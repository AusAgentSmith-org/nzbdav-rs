//! `MultiSegmentStream` — an `AsyncRead` that fetches Usenet articles in
//! sequential order with bounded lookahead prefetching.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use bytes::{Buf, BytesMut};
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::FuturesOrdered;
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::mpsc;
use tracing::{debug, error};

use crate::error::StreamError;
use crate::provider::UsenetArticleProvider;

/// An `AsyncRead` stream that fetches and decodes articles in order.
///
/// On creation a background tokio task is spawned that fetches segments
/// through the provider and sends decoded payloads through a bounded
/// channel. The channel capacity equals `lookahead`, providing natural
/// backpressure: the fetcher won't race too far ahead of the reader.
pub struct MultiSegmentStream {
    receiver: mpsc::Receiver<Result<Vec<u8>, StreamError>>,
    buffer: BytesMut,
    bytes_read: Arc<AtomicU64>,
    done: bool,
}

impl MultiSegmentStream {
    /// Create a new stream that will fetch the given segment message-ids
    /// in order.
    ///
    /// * `provider` — article fetch + decode backend
    /// * `segment_ids` — ordered list of article message-ids
    /// * `lookahead` — how many segments to prefetch ahead of the reader
    pub fn new(
        provider: Arc<UsenetArticleProvider>,
        segment_ids: Vec<String>,
        lookahead: usize,
    ) -> Self {
        let (tx, rx) = mpsc::channel(lookahead);
        let bytes_read = Arc::new(AtomicU64::new(0));

        let segment_count = segment_ids.len();
        // Parallel, in-order prefetch. Up to `lookahead` fetches can be
        // in flight at once; FuturesOrdered yields results in the original
        // submission order, preserving the stream's byte ordering.
        let concurrency = lookahead.max(1);
        let spawn_result = tokio::spawn(async move {
            debug!(
                segments = segment_count,
                concurrency, "prefetch task started"
            );
            let mut pending: FuturesOrdered<BoxFuture<'static, Result<Vec<u8>, StreamError>>> =
                FuturesOrdered::new();
            let mut next_to_spawn = 0usize;
            let spawn_one = |idx: usize,
                             provider: &Arc<UsenetArticleProvider>,
                             segment_ids: &Vec<String>|
             -> BoxFuture<'static, Result<Vec<u8>, StreamError>> {
                let p = Arc::clone(provider);
                let mid = segment_ids[idx].clone();
                Box::pin(async move { p.fetch_decoded(&mid).await })
            };

            while next_to_spawn < segment_ids.len() && pending.len() < concurrency {
                pending.push_back(spawn_one(next_to_spawn, &provider, &segment_ids));
                next_to_spawn += 1;
            }

            while let Some(result) = pending.next().await {
                if tx.send(result).await.is_err() {
                    debug!("receiver dropped, stopping prefetch");
                    return;
                }
                if next_to_spawn < segment_ids.len() {
                    pending.push_back(spawn_one(next_to_spawn, &provider, &segment_ids));
                    next_to_spawn += 1;
                }
            }
            debug!(segments = segment_count, "prefetch task finished");
        });
        debug!(
            spawned = spawn_result.is_finished(),
            "MultiSegmentStream background task spawned"
        );

        Self {
            receiver: rx,
            buffer: BytesMut::new(),
            bytes_read,
            done: false,
        }
    }

    /// Total bytes that have been read out of this stream so far.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read.load(Ordering::Relaxed)
    }
}

impl AsyncRead for MultiSegmentStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        // If we have buffered data, copy it out first.
        if !self.buffer.is_empty() {
            let to_copy = self.buffer.len().min(buf.remaining());
            buf.put_slice(&self.buffer[..to_copy]);
            self.buffer.advance(to_copy);
            self.bytes_read.fetch_add(to_copy as u64, Ordering::Relaxed);
            return Poll::Ready(Ok(()));
        }

        // If we already know we're done, signal EOF.
        if self.done {
            return Poll::Ready(Ok(()));
        }

        // Try to receive the next segment from the prefetch task.
        match self.receiver.poll_recv(cx) {
            Poll::Ready(Some(Ok(data))) => {
                let to_copy = data.len().min(buf.remaining());
                buf.put_slice(&data[..to_copy]);
                self.bytes_read.fetch_add(to_copy as u64, Ordering::Relaxed);
                // Buffer any remainder.
                if to_copy < data.len() {
                    self.buffer.extend_from_slice(&data[to_copy..]);
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Some(Err(e))) => {
                error!(error = %e, "segment fetch error");
                self.done = true;
                Poll::Ready(Err(std::io::Error::other(e)))
            }
            Poll::Ready(None) => {
                // Channel closed — all segments sent.
                self.done = true;
                Poll::Ready(Ok(()))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{MockArticleBackend, build_yenc_articles};
    use std::time::Duration;
    use tokio::io::AsyncReadExt;

    /// Stream reassembly must match the source file byte-for-byte.
    #[tokio::test]
    async fn multi_segment_stream_reassembles_file_exactly() {
        let data: Vec<u8> = (0..16384u32).flat_map(|n| n.to_le_bytes()).collect();
        let (provider, mids, original) =
            crate::testing::mock_provider_for_file("test.bin", data, 4);
        let mut stream = MultiSegmentStream::new(provider, mids, 4);
        let mut out = Vec::new();
        stream.read_to_end(&mut out).await.unwrap();
        assert_eq!(out, original);
    }

    /// The prefetch loop should fetch segments concurrently up to the
    /// lookahead window. Today it does not — one fetch at a time — so peak
    /// in-flight is 1. A fix using `FuturesOrdered` should raise this to
    /// roughly `lookahead`.
    #[tokio::test]
    async fn multi_segment_fetches_in_parallel_up_to_lookahead() {
        // Use a deliberate delay so sequential fetches take meaningful time
        // while parallel fetches can overlap.
        let data = vec![0u8; 4096];
        let segments = &[
            (1u32, 0u64, 1024),
            (2, 1024, 1024),
            (3, 2048, 1024),
            (4, 3072, 1024),
        ];
        let articles = build_yenc_articles("f.bin", &data, segments);
        let mids: Vec<String> = (0..articles.len())
            .map(|i| format!("m{i}@t.test"))
            .collect();

        let mut backend = MockArticleBackend::new().with_delay(Duration::from_millis(50));
        for (mid, art) in mids.iter().zip(articles) {
            backend.add(mid.clone(), art);
        }
        let backend_arc: std::sync::Arc<MockArticleBackend> = std::sync::Arc::new(backend);

        // Wrap the Arc in a provider via a small shim backend.
        struct Shim(std::sync::Arc<MockArticleBackend>);
        #[async_trait::async_trait]
        impl crate::provider::ArticleBackend for Shim {
            async fn fetch_decoded(&self, m: &str) -> Result<Vec<u8>, StreamError> {
                self.0.fetch_decoded(m).await
            }
            async fn stat(&self, m: &str) -> Result<bool, StreamError> {
                self.0.stat(m).await
            }
            async fn yenc_headers(
                &self,
                m: &str,
            ) -> Result<crate::provider::YencHeaders, StreamError> {
                self.0.yenc_headers(m).await
            }
            fn total_connections(&self) -> usize {
                self.0.total_connections()
            }
        }
        let provider = std::sync::Arc::new(crate::provider::UsenetArticleProvider::with_fake(
            std::sync::Arc::new(Shim(std::sync::Arc::clone(&backend_arc))),
        ));

        // lookahead=4, so all 4 segments should be able to be in flight at once.
        let mut stream = MultiSegmentStream::new(provider, mids, 4);
        let mut out = Vec::new();
        stream.read_to_end(&mut out).await.unwrap();

        assert_eq!(out.len(), 4096);
        let peak = backend_arc.peak_in_flight();
        assert!(
            peak >= 2,
            "expected >=2 concurrent fetches with lookahead=4, saw peak={peak}"
        );
    }
}
