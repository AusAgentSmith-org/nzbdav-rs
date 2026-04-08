//! `DavMultipartFileStream` — a virtual file stream composed from multiple
//! `FilePart`s, each mapping a byte range in the virtual file to a byte range
//! within decoded Usenet segment data.

use std::io::SeekFrom;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};
use tracing::debug;

use nzbdav_core::models::FilePart;

use crate::multi_segment_stream::MultiSegmentStream;
use crate::provider::UsenetArticleProvider;

/// A virtual file stream composed from multiple `FilePart`s.
///
/// Each `FilePart` maps a range within the virtual file (`file_part_byte_range`)
/// to a range within decoded Usenet segment data (`segment_id_byte_range`).
/// The stream transparently fetches segments, skips to the correct offset
/// within segment data, and concatenates parts to present a contiguous file.
pub struct DavMultipartFileStream {
    provider: Arc<UsenetArticleProvider>,
    file_parts: Vec<FilePart>,
    file_size: u64,
    position: u64,
    lookahead: usize,
    // Current active part state
    current_part_index: usize,
    inner_stream: Option<MultiSegmentStream>,
    /// Bytes still to skip at the start of the current part's segment data.
    skip_bytes: u64,
    /// Bytes remaining to read from the current part.
    part_bytes_remaining: u64,
    /// Pending seek target, set by `start_seek` and consumed by `poll_complete`.
    pending_seek: Option<u64>,
}

impl DavMultipartFileStream {
    /// Create a new multipart file stream.
    ///
    /// * `provider` — article provider with failover
    /// * `file_parts` — ordered list of file parts composing the virtual file
    /// * `file_size` — total virtual file size
    /// * `lookahead` — prefetch depth passed to inner `MultiSegmentStream`s
    pub fn new(
        provider: Arc<UsenetArticleProvider>,
        file_parts: Vec<FilePart>,
        file_size: u64,
        lookahead: usize,
    ) -> Self {
        Self {
            provider,
            file_parts,
            file_size,
            position: 0,
            lookahead,
            current_part_index: 0,
            inner_stream: None,
            skip_bytes: 0,
            part_bytes_remaining: 0,
            pending_seek: None,
        }
    }

    /// Find the part index that contains the given virtual file position.
    fn find_part_for_position(&self, pos: u64) -> Option<usize> {
        let pos_i64 = pos as i64;
        self.file_parts
            .iter()
            .position(|p| p.file_part_byte_range.contains(pos_i64))
    }

    /// Set up the state for the part at `part_index` at the given virtual
    /// file `position`. Does NOT create the inner stream yet — that happens
    /// lazily in `poll_read` to ensure the `tokio::spawn` runs in the right
    /// async context.
    fn setup_part(&mut self, part_index: usize, position: u64) {
        let part = &self.file_parts[part_index];

        // How far into this part's virtual range are we?
        let offset_in_part = position as i64 - part.file_part_byte_range.start;

        // Total bytes available from this part's segment data.
        let part_data_size = part.segment_id_byte_range.size();

        // Bytes to skip = segment_id_byte_range.start + offset within part.
        let skip = part.segment_id_byte_range.start + offset_in_part;

        // Bytes remaining in this part from the current position.
        let remaining = part_data_size - offset_in_part;

        self.current_part_index = part_index;
        self.skip_bytes = skip.max(0) as u64;
        self.part_bytes_remaining = remaining.max(0) as u64;
        // Don't create the stream here — defer to poll_read.
        self.inner_stream = None;
    }

    /// Ensure the inner stream exists for the current part. Creates it
    /// lazily so that `tokio::spawn` runs inside the poll context.
    fn ensure_inner_stream(&mut self) {
        if self.inner_stream.is_none() && self.current_part_index < self.file_parts.len() {
            let part = &self.file_parts[self.current_part_index];
            self.inner_stream = Some(MultiSegmentStream::new(
                Arc::clone(&self.provider),
                part.segment_ids.clone(),
                self.lookahead,
            ));
        }
    }

    /// Reset all part state (used on seek).
    fn reset_part_state(&mut self) {
        self.inner_stream = None;
        self.current_part_index = 0;
        self.skip_bytes = 0;
        self.part_bytes_remaining = 0;
    }
}

impl AsyncRead for DavMultipartFileStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        // EOF check.
        if self.position >= self.file_size {
            return Poll::Ready(Ok(()));
        }

        // If no part is set up, find the right one for current position.
        if self.part_bytes_remaining == 0 && self.skip_bytes == 0 {
            let pos = self.position;
            if let Some(idx) = self.find_part_for_position(pos) {
                self.setup_part(idx, pos);
            } else {
                return Poll::Ready(Ok(()));
            }
        }

        // Lazily create the inner stream (ensures tokio::spawn runs in poll context).
        self.ensure_inner_stream();

        // Skip bytes if needed (segment_id_byte_range.start + intra-part offset).
        if self.skip_bytes > 0 {
            let to_skip = self.skip_bytes.min(8192) as usize;
            let mut skip_buf = vec![0u8; to_skip];
            let mut read_buf = ReadBuf::new(&mut skip_buf);
            let inner = self.inner_stream.as_mut().unwrap();
            match Pin::new(inner).poll_read(cx, &mut read_buf) {
                Poll::Ready(Ok(())) => {
                    let filled = read_buf.filled().len();
                    if filled == 0 {
                        // Inner stream EOF while skipping — move to next part.
                        self.inner_stream = None;
                        cx.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                    self.skip_bytes -= filled as u64;
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }

        // If this part is exhausted, move to next.
        if self.part_bytes_remaining == 0 && self.skip_bytes == 0 {
            let next = self.current_part_index + 1;
            if next >= self.file_parts.len() {
                return Poll::Ready(Ok(()));
            }
            let next_start = self.file_parts[next].file_part_byte_range.start as u64;
            self.inner_stream = None; // Drop old stream before creating new one
            self.setup_part(next, next_start);
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }

        // Read from inner stream into a temporary buffer, then copy to caller.
        let remaining_in_buf = buf.remaining();
        let cap = self.part_bytes_remaining.min(remaining_in_buf as u64) as usize;

        if cap == 0 {
            return Poll::Ready(Ok(()));
        }

        let mut tmp = vec![0u8; cap.min(65536)];
        let mut limited_buf = ReadBuf::new(&mut tmp);

        let inner = self.inner_stream.as_mut().unwrap();
        match Pin::new(inner).poll_read(cx, &mut limited_buf) {
            Poll::Ready(Ok(())) => {
                let filled = limited_buf.filled().len();
                let first_bytes = if filled > 0 {
                    format!("{:02x?}", &limited_buf.filled()[..filled.min(8)])
                } else {
                    "empty".to_string()
                };
                tracing::debug!(
                    filled,
                    first_bytes = %first_bytes,
                    skip_bytes = self.skip_bytes,
                    part_remaining = self.part_bytes_remaining,
                    position = self.position,
                    part_idx = self.current_part_index,
                    "multipart: inner read result"
                );
                if filled == 0 {
                    self.inner_stream = None;
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
                // Copy the actual data from the temporary buffer to the caller's buf.
                buf.put_slice(limited_buf.filled());
                self.position += filled as u64;
                self.part_bytes_remaining -= filled as u64;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncSeek for DavMultipartFileStream {
    fn start_seek(mut self: Pin<&mut Self>, pos: SeekFrom) -> std::io::Result<()> {
        let target = match pos {
            SeekFrom::Start(offset) => offset,
            SeekFrom::Current(delta) => {
                let target = self.position as i64 + delta;
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
        _cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<u64>> {
        let target = match self.pending_seek.take() {
            Some(t) => t,
            None => return Poll::Ready(Ok(self.position)),
        };

        let target = target.min(self.file_size);

        debug!(target, "multipart file stream seeking");

        self.position = target;
        self.reset_part_state();
        // Don't set up the part here — poll_read will do it lazily,
        // ensuring tokio::spawn runs in the correct async context.

        Poll::Ready(Ok(target))
    }
}
