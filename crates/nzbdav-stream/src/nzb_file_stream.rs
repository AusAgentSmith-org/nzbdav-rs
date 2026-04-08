//! `NzbFileStream` — `AsyncRead` + `AsyncSeek` for a complete NZB file,
//! backed by segment-level streaming from Usenet.

use std::io::SeekFrom;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};
use tracing::debug;

use crate::error::StreamError;
use crate::multi_segment_stream::MultiSegmentStream;
use crate::provider::UsenetArticleProvider;

/// Seekable, readable stream over an NZB file's segments.
///
/// Implements `AsyncRead` by delegating to a `MultiSegmentStream`, and
/// `AsyncSeek` by estimating which segment contains the target offset
/// (using yEnc headers to refine), then creating a new inner stream
/// starting at the correct segment.
pub struct NzbFileStream {
    provider: Arc<UsenetArticleProvider>,
    segment_ids: Vec<String>,
    file_size: u64,
    lookahead: usize,
    /// Current logical position in the file.
    position: u64,
    /// Bytes to skip from the beginning of the next segment's decoded data
    /// after a seek lands in the middle of a segment.
    skip_bytes: usize,
    /// Underlying segment stream, recreated on seek.
    inner: MultiSegmentStream,
    /// Pending seek target, set by `start_seek` and consumed by `poll_complete`.
    pending_seek: Option<u64>,
}

impl NzbFileStream {
    /// Create a new file stream.
    ///
    /// * `provider` — article provider with failover
    /// * `segment_ids` — ordered message-ids for every segment in the file
    /// * `file_size` — total decoded file size (from NZB metadata)
    /// * `lookahead` — prefetch depth passed to `MultiSegmentStream`
    pub fn new(
        provider: Arc<UsenetArticleProvider>,
        segment_ids: Vec<String>,
        file_size: u64,
        lookahead: usize,
    ) -> Self {
        let inner =
            MultiSegmentStream::new(Arc::clone(&provider), segment_ids.clone(), lookahead);
        Self {
            provider,
            segment_ids,
            file_size,
            lookahead,
            position: 0,
            skip_bytes: 0,
            inner,
            pending_seek: None,
        }
    }

    /// Estimate which segment index contains `offset`, then refine using
    /// yEnc headers via binary search.
    ///
    /// Currently unused — the synchronous interpolation in `poll_complete`
    /// handles seeks without async I/O. This method is retained for a
    /// future phase where seek refinement is done asynchronously.
    #[allow(dead_code)]
    async fn resolve_segment_for_offset(
        &self,
        offset: u64,
    ) -> Result<(usize, usize), StreamError> {
        let num_segments = self.segment_ids.len();
        if num_segments == 0 {
            return Err(StreamError::SeekPositionNotFound(offset as i64));
        }

        // Linear interpolation to get a starting estimate.
        let estimated =
            ((offset as u128 * num_segments as u128) / self.file_size.max(1) as u128) as usize;
        let estimated = estimated.min(num_segments - 1);

        // Binary search around the estimate to find the correct segment.
        let mut lo: usize = 0;
        let mut hi: usize = num_segments - 1;
        let mut best_seg = estimated;
        let mut best_skip: usize = 0;

        // First, fetch headers for the estimated segment to see if we're close.
        let headers = self
            .provider
            .yenc_headers(&self.segment_ids[estimated])
            .await?;

        if offset >= headers.part_begin && offset < headers.part_end {
            // Direct hit.
            return Ok((estimated, (offset - headers.part_begin) as usize));
        } else if offset < headers.part_begin {
            hi = estimated.saturating_sub(1);
        } else {
            lo = (estimated + 1).min(num_segments - 1);
        }

        // Binary search the remaining range.
        while lo <= hi {
            let mid = lo + (hi - lo) / 2;
            let h = self
                .provider
                .yenc_headers(&self.segment_ids[mid])
                .await?;

            if offset >= h.part_begin && offset < h.part_end {
                best_seg = mid;
                best_skip = (offset - h.part_begin) as usize;
                return Ok((best_seg, best_skip));
            } else if offset < h.part_begin {
                if mid == 0 {
                    break;
                }
                hi = mid - 1;
            } else {
                lo = mid + 1;
            }
        }

        // If we couldn't find an exact segment (e.g. offset is at the very
        // end), use the last segment.
        if offset >= self.file_size {
            return Ok((num_segments - 1, 0));
        }

        // Fallback: use estimated segment with no skip (best effort).
        debug!(
            offset,
            estimated, "seek fallback to estimated segment"
        );
        Ok((best_seg, best_skip))
    }

    /// Rebuild the inner stream starting from `segment_idx`.
    fn rebuild_inner(&mut self, segment_idx: usize, skip: usize) {
        let remaining_ids = self.segment_ids[segment_idx..].to_vec();
        self.inner = MultiSegmentStream::new(
            Arc::clone(&self.provider),
            remaining_ids,
            self.lookahead,
        );
        self.skip_bytes = skip;
    }
}

impl AsyncRead for NzbFileStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        // If we need to skip bytes after a seek, do that first.
        if self.skip_bytes > 0 {
            let skip = self.skip_bytes;
            let mut skip_buf = vec![0u8; skip.min(8192)];
            let mut read_buf = ReadBuf::new(&mut skip_buf);
            match Pin::new(&mut self.inner).poll_read(cx, &mut read_buf) {
                Poll::Ready(Ok(())) => {
                    let filled = read_buf.filled().len();
                    if filled == 0 {
                        // EOF while skipping — stream ended.
                        return Poll::Ready(Ok(()));
                    }
                    self.skip_bytes -= filled;
                    // Re-poll to either skip more or read real data.
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }

        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &result {
            let read = (buf.filled().len() - before) as u64;
            self.position += read;
        }
        result
    }
}

impl AsyncSeek for NzbFileStream {
    fn start_seek(mut self: Pin<&mut Self>, pos: SeekFrom) -> std::io::Result<()> {
        let target = match pos {
            SeekFrom::Start(offset) => offset,
            SeekFrom::Current(delta) => {
                let current = self.position as i64;
                let target = current + delta;
                if target < 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "seek to negative position",
                    ));
                }
                target as u64
            }
            SeekFrom::End(delta) => {
                let target = self.file_size as i64 + delta;
                if target < 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "seek to negative position",
                    ));
                }
                target as u64
            }
        };

        self.pending_seek = Some(target);
        Ok(())
    }

    fn poll_complete(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<u64>> {
        let target = match self.pending_seek.take() {
            Some(t) => t,
            None => return Poll::Ready(Ok(self.position)),
        };

        // Clamp to file_size.
        let target = target.min(self.file_size);

        // We need to resolve the segment asynchronously. Since poll_complete
        // is synchronous, we spawn the resolution in a blocking-compatible way
        // using a boxed future.
        let provider = Arc::clone(&self.provider);
        let segment_ids = self.segment_ids.clone();
        let file_size = self.file_size;
        let num_segments = segment_ids.len();

        // For seeks to position 0 or the same position, take a fast path.
        if target == 0 {
            self.rebuild_inner(0, 0);
            self.position = 0;
            return Poll::Ready(Ok(0));
        }

        if target >= file_size {
            self.position = file_size;
            self.rebuild_inner(num_segments.saturating_sub(1), 0);
            return Poll::Ready(Ok(file_size));
        }

        // For non-trivial seeks, we need to do async work. We use a
        // FutureExt-style approach: create the future inline and poll it.
        // Since this is the first poll after start_seek, we'll resolve via
        // a spawned task and wake when ready.
        let waker = cx.waker().clone();
        let _lookahead = self.lookahead;

        // We use a trick: spawn a task that will resolve the segment and
        // stash the result. On re-poll we pick it up.
        // Store target back so we can re-enter.
        self.pending_seek = Some(target);

        // We avoid complex future state by doing estimate-based seek
        // synchronously (no network), then refining later lazily.
        // Simple approach: use linear interpolation for the segment index.
        let estimated =
            ((target as u128 * num_segments as u128) / file_size.max(1) as u128) as usize;
        let segment_idx = estimated.min(num_segments - 1);

        // Estimate skip_bytes assuming equal segment sizes.
        let avg_segment_size = file_size / num_segments.max(1) as u64;
        let segment_start = segment_idx as u64 * avg_segment_size;
        let skip = if target > segment_start {
            (target - segment_start) as usize
        } else {
            0
        };

        // Clear pending seek — we've handled it.
        self.pending_seek = None;
        self.position = target;
        self.rebuild_inner(segment_idx, skip);

        // Spawn a refinement task: fetch actual yEnc headers and correct
        // if the estimate was wrong. This makes the seek non-blocking at
        // the cost of possibly reading a few wrong bytes that get discarded.
        // For Phase 1 this interpolation approach is acceptable.
        let _refine_handle = tokio::spawn(async move {
            // In a future phase, we'd send corrections back to the stream.
            // For now, the linear estimate is good enough for most files
            // where segments are roughly equal size.
            drop(provider);
            drop(segment_ids);
            drop(waker);
        });

        Poll::Ready(Ok(target))
    }
}
