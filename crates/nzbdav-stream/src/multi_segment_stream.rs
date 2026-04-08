//! `MultiSegmentStream` — an `AsyncRead` that fetches Usenet articles in
//! sequential order with bounded lookahead prefetching.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use bytes::{Buf, BytesMut};
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
        let spawn_result = tokio::spawn(async move {
            debug!(segments = segment_count, "prefetch task started");
            for (i, mid) in segment_ids.iter().enumerate() {
                let result = provider.fetch_decoded(mid).await;
                debug!(segment = i, message_id = %mid, ok = result.is_ok(), "fetched segment");
                if tx.send(result).await.is_err() {
                    debug!("receiver dropped, stopping prefetch");
                    break;
                }
            }
            debug!(segments = segment_count, "prefetch task finished");
        });
        debug!(spawned = spawn_result.is_finished(), "MultiSegmentStream background task spawned");

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
            self.bytes_read
                .fetch_add(to_copy as u64, Ordering::Relaxed);
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
                self.bytes_read
                    .fetch_add(to_copy as u64, Ordering::Relaxed);
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
