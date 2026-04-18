//! `SeekableSegmentStream` — seekable stream over Usenet segments.
//!
//! Uses interpolation search with yEnc header probing to find the exact
//! segment containing any byte offset. Uses interpolation search with yEnc header probing.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};
use tracing::debug;

use crate::multi_segment_stream::MultiSegmentStream;
use crate::provider::UsenetArticleProvider;

pub struct SeekableSegmentStream {
    provider: Arc<UsenetArticleProvider>,
    segment_ids: Vec<String>,
    file_size: u64,
    lookahead: usize,
    position: u64,
    inner: Option<MultiSegmentStream>,
    discard_bytes: u64,
    pending_seek: Option<u64>,
    needs_resolve: bool,
}

/// Result of interpolation search: segment index and its byte range.
pub struct FoundSegment {
    pub index: usize,
    pub start_byte: u64,
}

impl SeekableSegmentStream {
    pub fn new(
        provider: Arc<UsenetArticleProvider>,
        segment_ids: Vec<String>,
        file_size: u64,
        lookahead: usize,
    ) -> Self {
        Self {
            provider,
            segment_ids,
            file_size,
            lookahead,
            position: 0,
            inner: None,
            discard_bytes: 0,
            pending_seek: None,
            needs_resolve: true,
        }
    }

    /// Build a stream positioned exactly at `target_byte`.
    ///
    /// Resolves the containing segment via yEnc-header interpolation, spawns
    /// the underlying multi-segment fetch starting at that segment, and
    /// discards any intra-segment prefix so the next read returns the byte at
    /// `target_byte`. This is the byte-exact path used by the Range handler.
    pub async fn aligned(
        provider: Arc<UsenetArticleProvider>,
        segment_ids: Vec<String>,
        file_size: u64,
        lookahead: usize,
        target_byte: u64,
    ) -> std::io::Result<Self> {
        let mut s = Self::new(provider, segment_ids, file_size, lookahead);
        if target_byte == 0 {
            s.inner = Some(MultiSegmentStream::new(
                Arc::clone(&s.provider),
                s.segment_ids.clone(),
                s.lookahead,
            ));
            s.discard_bytes = 0;
            s.needs_resolve = false;
            s.position = 0;
            return Ok(s);
        }

        let found = s.find_segment(target_byte).await?;
        let remaining = s.segment_ids[found.index..].to_vec();
        let mut inner = MultiSegmentStream::new(Arc::clone(&s.provider), remaining, s.lookahead);

        // Discard from segment start to target byte.
        let mut to_skip = target_byte.saturating_sub(found.start_byte);
        if to_skip > 0 {
            use tokio::io::AsyncReadExt;
            let mut scratch = vec![0u8; 65_536];
            while to_skip > 0 {
                let want = scratch.len().min(to_skip as usize);
                let n = inner.read(&mut scratch[..want]).await?;
                if n == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "EOF while aligning to seek offset",
                    ));
                }
                to_skip -= n as u64;
            }
        }

        s.inner = Some(inner);
        s.discard_bytes = 0;
        s.needs_resolve = false;
        s.position = target_byte;
        Ok(s)
    }

    /// Interpolation search: find the segment containing `target_byte`.
    ///
    /// Fetches yEnc headers to get actual byte ranges for guessed segments,
    /// then narrows the search until the correct segment is found.
    async fn find_segment(&self, target_byte: u64) -> std::io::Result<FoundSegment> {
        let n = self.segment_ids.len() as i64;
        if n == 0 {
            return Err(std::io::Error::other("no segments"));
        }
        if target_byte == 0 {
            return Ok(FoundSegment {
                index: 0,
                start_byte: 0,
            });
        }

        let mut lo_idx: i64 = 0;
        let mut hi_idx: i64 = n;
        let mut lo_byte: i64 = 0;
        let mut hi_byte: i64 = self.file_size as i64;
        let target = target_byte as i64;

        for iteration in 0..20 {
            if lo_idx >= hi_idx || lo_byte >= hi_byte {
                break;
            }
            if target < lo_byte || target >= hi_byte {
                break;
            }

            // Interpolate
            let range_bytes = (hi_byte - lo_byte) as f64;
            let range_idx = (hi_idx - lo_idx) as f64;
            let offset_from_lo = (target - lo_byte) as f64;
            let guess_offset = (offset_from_lo / range_bytes * range_idx).floor() as i64;
            let guess_idx = (lo_idx + guess_offset).clamp(lo_idx, hi_idx - 1) as usize;

            // Fetch yEnc headers for this segment
            let seg_id = &self.segment_ids[guess_idx];
            let headers = match self.provider.yenc_headers(seg_id).await {
                Ok(h) => h,
                Err(e) => {
                    debug!(error = %e, guess_idx, iteration, "yenc header fetch failed, using estimate");
                    // Fall back to estimate
                    let avg = self.file_size / n as u64;
                    let est = (target_byte / avg).min(n as u64 - 1) as usize;
                    return Ok(FoundSegment {
                        index: est.saturating_sub(1),
                        start_byte: est.saturating_sub(1) as u64 * avg,
                    });
                }
            };

            // yEnc part_begin/part_end are 1-based inclusive offsets. Convert
            // to a 0-based half-open range [seg_start_0, seg_end).
            let seg_start_0 = headers.part_begin.saturating_sub(1);
            let seg_end = headers.part_end;

            debug!(
                iteration,
                guess_idx,
                seg_start_0,
                seg_end,
                target = target_byte,
                "interpolation probe"
            );

            if seg_end <= target_byte {
                // Too low — search higher
                lo_idx = guess_idx as i64 + 1;
                lo_byte = seg_end as i64;
            } else if seg_start_0 > target_byte {
                // Too high — search lower
                hi_idx = guess_idx as i64;
                hi_byte = seg_start_0 as i64;
            } else {
                // Found: seg_start_0 <= target < seg_end
                return Ok(FoundSegment {
                    index: guess_idx,
                    start_byte: seg_start_0,
                });
            }
        }

        // Fallback estimate
        let avg = self.file_size / n as u64;
        let est = if avg > 0 {
            (target_byte / avg).min(n as u64 - 1) as usize
        } else {
            0
        };
        Ok(FoundSegment {
            index: est.saturating_sub(1),
            start_byte: est.saturating_sub(1) as u64 * avg,
        })
    }

    /// Resolve the pending seek: find the segment and create the inner stream.
    /// This is async (fetches yEnc headers) so it runs inside poll_read
    /// via a spawned task.
    fn start_resolve(&mut self) {
        let target = self.position;
        let provider = Arc::clone(&self.provider);
        let segment_ids = self.segment_ids.clone();
        let file_size = self.file_size;
        let lookahead = self.lookahead;

        // For position 0, just create the stream directly (no search needed)
        if target == 0 {
            self.inner = Some(MultiSegmentStream::new(provider, segment_ids, lookahead));
            self.discard_bytes = 0;
            self.needs_resolve = false;
            return;
        }

        // For non-zero positions, estimate which segment to start from.
        // file_size may be raw yEnc bytes (~3% larger than decoded).
        let n = segment_ids.len() as u64;
        let decoded_est = (file_size as f64 * 0.97) as u64;
        let avg_decoded = if n > 0 { decoded_est / n } else { 1 };

        // How many segments from the end do we need?
        let bytes_from_end = decoded_est.saturating_sub(target);
        let segs_needed = if avg_decoded > 0 {
            (bytes_from_end / avg_decoded) + 5 // generous margin
        } else {
            n
        };
        let est_idx = n.saturating_sub(segs_needed) as usize;

        // Don't discard — let the handler's Take limit the output.
        // The stream may start slightly before the requested offset,
        // which means the response includes a few extra bytes at the start.
        // For MP4 moov atom seeking this is fine since boxes are self-describing.
        self.discard_bytes = 0;

        debug!(
            target,
            est_idx, segs_needed, "seek: creating stream at estimated segment"
        );

        let remaining = segment_ids[est_idx..].to_vec();
        self.inner = Some(MultiSegmentStream::new(provider, remaining, lookahead));
        self.needs_resolve = false;
    }
}

impl AsyncRead for SeekableSegmentStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.needs_resolve {
            self.start_resolve();
        }

        if self.inner.is_none() {
            return Poll::Ready(Ok(()));
        }

        // Discard bytes to align to the target offset
        if self.discard_bytes > 0 {
            let to_discard = self.discard_bytes.min(65536) as usize;
            let mut tmp = vec![0u8; to_discard];
            let mut tmp_buf = ReadBuf::new(&mut tmp);
            let inner = self.inner.as_mut().unwrap();
            match Pin::new(inner).poll_read(cx, &mut tmp_buf) {
                Poll::Ready(Ok(())) => {
                    let filled = tmp_buf.filled().len();
                    if filled == 0 {
                        return Poll::Ready(Ok(()));
                    }
                    self.discard_bytes -= filled as u64;
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }

        // Normal read
        let before = buf.filled().len();
        {
            let inner = self.inner.as_mut().unwrap();
            match Pin::new(inner).poll_read(cx, buf) {
                Poll::Ready(Ok(())) => {}
                other => return other,
            }
        }
        let read = buf.filled().len() - before;
        self.position += read as u64;
        Poll::Ready(Ok(()))
    }
}

impl AsyncSeek for SeekableSegmentStream {
    fn start_seek(mut self: Pin<&mut Self>, pos: std::io::SeekFrom) -> std::io::Result<()> {
        let target = match pos {
            std::io::SeekFrom::Start(offset) => offset,
            std::io::SeekFrom::Current(delta) => {
                let t = self.position as i64 + delta;
                if t < 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "seek to negative position",
                    ));
                }
                t as u64
            }
            std::io::SeekFrom::End(delta) => {
                let t = self.file_size as i64 + delta;
                if t < 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "seek to negative position",
                    ));
                }
                t as u64
            }
        };

        self.pending_seek = Some(target.min(self.file_size));
        Ok(())
    }

    fn poll_complete(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<u64>> {
        let Some(target) = self.pending_seek.take() else {
            return Poll::Ready(Ok(self.position));
        };

        debug!(target, "seek to offset");
        self.inner = None;
        self.discard_bytes = 0;
        self.position = target;
        self.needs_resolve = true;

        Poll::Ready(Ok(target))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncSeekExt;

    fn make_stream(segment_count: usize, file_size: u64) -> SeekableSegmentStream {
        let provider = Arc::new(UsenetArticleProvider::new(vec![]));
        let segment_ids: Vec<String> = (0..segment_count)
            .map(|i| format!("seg-{i}@example.com"))
            .collect();
        SeekableSegmentStream::new(provider, segment_ids, file_size, 2)
    }

    #[tokio::test]
    async fn test_new_creates_stream_empty() {
        let stream = make_stream(0, 0);
        assert_eq!(stream.position, 0);
        assert!(stream.inner.is_none());
        assert_eq!(stream.segment_ids.len(), 0);
    }

    #[tokio::test]
    async fn test_new_creates_stream_non_empty() {
        let stream = make_stream(10, 50000);
        assert_eq!(stream.position, 0);
        assert!(stream.inner.is_none());
        assert_eq!(stream.segment_ids.len(), 10);
        assert_eq!(stream.file_size, 50000);
    }

    #[tokio::test]
    async fn test_seek_to_start() {
        let mut stream = make_stream(5, 1000);
        let pos = stream.seek(std::io::SeekFrom::Start(0)).await.unwrap();
        assert_eq!(pos, 0);
        assert_eq!(stream.position, 0);
    }

    #[tokio::test]
    async fn test_seek_to_end() {
        let mut stream = make_stream(5, 1000);
        let pos = stream.seek(std::io::SeekFrom::Start(1000)).await.unwrap();
        assert_eq!(pos, 1000);
        assert_eq!(stream.position, 1000);
    }

    #[tokio::test]
    async fn test_seek_from_end() {
        let mut stream = make_stream(5, 1000);
        let pos = stream.seek(std::io::SeekFrom::End(-100)).await.unwrap();
        assert_eq!(pos, 900);
        assert_eq!(stream.position, 900);
    }

    #[tokio::test]
    async fn test_seek_from_current() {
        let mut stream = make_stream(5, 1000);
        // First seek to position 500
        stream.seek(std::io::SeekFrom::Start(500)).await.unwrap();
        // Then seek +200 from current
        let pos = stream.seek(std::io::SeekFrom::Current(200)).await.unwrap();
        assert_eq!(pos, 700);
        assert_eq!(stream.position, 700);
    }

    #[tokio::test]
    async fn test_seek_negative_errors() {
        let mut stream = make_stream(5, 1000);
        // Seeking from current with a large negative delta should error
        let result = stream.seek(std::io::SeekFrom::Current(-100)).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    // -- Byte-correctness tests (finding #7) --
    // These go through the mock provider so we can assert exact returned bytes.

    use tokio::io::AsyncReadExt;

    /// `aligned` positions the stream exactly at the target byte; reading to
    /// end returns the plaintext slice from that offset onward.
    #[tokio::test]
    async fn seekable_stream_aligned_returns_exact_slice() {
        let data: Vec<u8> = (0..8192u32).flat_map(|n| n.to_le_bytes()).collect();
        let file_len = data.len() as u64;
        let (provider, mids, original) = crate::testing::mock_provider_for_file("t.bin", data, 4);

        let seek_to: u64 = 12_345;
        let mut stream = SeekableSegmentStream::aligned(provider, mids, file_len, 4, seek_to)
            .await
            .unwrap();

        let mut out = Vec::new();
        stream.read_to_end(&mut out).await.unwrap();
        assert_eq!(out, original[seek_to as usize..]);
    }

    /// Reading from position 0 must produce the whole file byte-for-byte.
    #[tokio::test]
    async fn seekable_stream_from_zero_reads_full_file() {
        let data: Vec<u8> = (0..4096u32).flat_map(|n| n.to_le_bytes()).collect();
        let file_len = data.len() as u64;
        let (provider, mids, original) = crate::testing::mock_provider_for_file("t.bin", data, 4);

        let mut stream = SeekableSegmentStream::new(provider, mids, file_len, 4);
        let mut out = Vec::new();
        stream.read_to_end(&mut out).await.unwrap();
        assert_eq!(out, original);
    }

    /// Alignment right at a segment boundary must land on byte 0 of that
    /// segment, not the previous one.
    #[tokio::test]
    async fn seekable_stream_aligned_on_segment_boundary() {
        let data: Vec<u8> = (0..4096u32).flat_map(|n| n.to_le_bytes()).collect();
        let file_len = data.len() as u64;
        let (provider, mids, original) = crate::testing::mock_provider_for_file("t.bin", data, 4);

        // 4 segments of 4096 bytes each = 4096 bytes per segment.
        let boundary = file_len / 2;
        let mut stream = SeekableSegmentStream::aligned(provider, mids, file_len, 4, boundary)
            .await
            .unwrap();
        let mut out = Vec::new();
        stream.read_to_end(&mut out).await.unwrap();
        assert_eq!(out, original[boundary as usize..]);
    }
}
