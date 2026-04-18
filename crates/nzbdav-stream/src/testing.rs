//! Test helpers: fake article backend + yEnc fixture builder.
//!
//! Exposed publicly so integration tests in other workspace crates can inject
//! canned article responses without standing up an NNTP server. Not intended
//! for production use.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use nzb_decode::decode_yenc;
use tokio::sync::Notify;

use crate::error::{Result, StreamError};
use crate::provider::{ArticleBackend, UsenetArticleProvider, YencHeaders};

/// Minimal yEnc article encoder used to build realistic fixture articles.
///
/// Mirrors the wire format produced by `yenc-simd::encode_article` but kept
/// inline here so the testing module has no extra dependency. Output includes
/// `=ybegin`, `=ypart`, encoded body, and `=yend` lines terminated by CRLF.
fn encode_yenc_article(
    raw: &[u8],
    filename: &str,
    part: u32,
    total_parts: u32,
    file_offset: u64,
    total_file_size: u64,
) -> Vec<u8> {
    use crc32fast::Hasher;
    const LINE_WIDTH: usize = 128;

    let mut hasher = Hasher::new();
    hasher.update(raw);
    let crc = hasher.finalize();

    let mut out = Vec::with_capacity(raw.len() * 11 / 10 + 256);
    if total_parts > 1 {
        out.extend_from_slice(
            format!(
                "=ybegin part={part} line={LINE_WIDTH} size={total_file_size} name={filename}\r\n"
            )
            .as_bytes(),
        );
        let begin = file_offset + 1;
        let end = file_offset + raw.len() as u64;
        out.extend_from_slice(format!("=ypart begin={begin} end={end}\r\n").as_bytes());
    } else {
        out.extend_from_slice(
            format!("=ybegin line={LINE_WIDTH} size={total_file_size} name={filename}\r\n")
                .as_bytes(),
        );
    }

    let mut line_pos: usize = 0;
    for &byte in raw {
        let encoded = byte.wrapping_add(42);
        let escape = matches!(encoded, 0x00 | 0x0A | 0x0D | 0x3D)
            || (line_pos == 0 && matches!(encoded, 0x09 | 0x20 | 0x2E));
        if escape {
            out.push(b'=');
            out.push(encoded.wrapping_add(64));
            line_pos += 2;
        } else {
            out.push(encoded);
            line_pos += 1;
        }
        if line_pos >= LINE_WIDTH {
            out.extend_from_slice(b"\r\n");
            line_pos = 0;
        }
    }
    if line_pos > 0 {
        out.extend_from_slice(b"\r\n");
    }

    if total_parts > 1 {
        out.extend_from_slice(
            format!("=yend size={} part={part} pcrc32={crc:08x}\r\n", raw.len()).as_bytes(),
        );
    } else {
        out.extend_from_slice(format!("=yend size={} crc32={crc:08x}\r\n", raw.len()).as_bytes());
    }
    out
}

/// One canned article: the raw yEnc-encoded body exactly as it would come
/// off the wire, plus the plaintext bytes it decodes to (for assertions).
#[derive(Clone)]
pub struct FakeArticle {
    pub raw_yenc: Vec<u8>,
    pub decoded: Vec<u8>,
    pub part_begin: u64,
    pub part_end: u64,
    pub total_size: u64,
}

/// Build a yEnc-encoded multipart article for one segment of a file.
///
/// * `file_data` — full plaintext file bytes
/// * `segments` — slice of `(part_index, offset, length)` tuples describing
///   where each part lives in the file. `part_index` is 1-based.
///
/// Returns the canned articles in order.
pub fn build_yenc_articles(
    filename: &str,
    file_data: &[u8],
    segments: &[(u32, u64, usize)],
) -> Vec<FakeArticle> {
    let total_parts = segments.len() as u32;
    let total_size = file_data.len() as u64;
    segments
        .iter()
        .map(|&(part, offset, len)| {
            let slice = &file_data[offset as usize..offset as usize + len];
            let raw = encode_yenc_article(slice, filename, part, total_parts, offset, total_size);
            let part_begin = offset + 1;
            let part_end = offset + len as u64;
            FakeArticle {
                raw_yenc: raw,
                decoded: slice.to_vec(),
                part_begin,
                part_end,
                total_size,
            }
        })
        .collect()
}

/// In-memory article backend that returns canned yEnc-decoded payloads.
///
/// * Simulates per-fetch latency via `delay`.
/// * Tracks the maximum number of in-flight `fetch_decoded` calls observed,
///   so tests can assert on intra-file parallelism.
pub struct MockArticleBackend {
    articles: HashMap<String, FakeArticle>,
    delay: std::time::Duration,
    in_flight: AtomicUsize,
    peak_in_flight: AtomicUsize,
    fetch_count: AtomicUsize,
    gate: Option<Arc<Notify>>,
}

impl MockArticleBackend {
    pub fn new() -> Self {
        Self {
            articles: HashMap::new(),
            delay: std::time::Duration::from_millis(0),
            in_flight: AtomicUsize::new(0),
            peak_in_flight: AtomicUsize::new(0),
            fetch_count: AtomicUsize::new(0),
            gate: None,
        }
    }

    /// Add one article keyed by message-id.
    pub fn add(&mut self, message_id: impl Into<String>, article: FakeArticle) {
        self.articles.insert(message_id.into(), article);
    }

    /// Set a per-fetch delay (useful for concurrency tests).
    pub fn with_delay(mut self, delay: std::time::Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Install a gate: fetches block on `notify_one` before returning.
    /// Lets tests observe concurrent in-flight calls deterministically.
    pub fn with_gate(mut self, gate: Arc<Notify>) -> Self {
        self.gate = Some(gate);
        self
    }

    pub fn peak_in_flight(&self) -> usize {
        self.peak_in_flight.load(Ordering::SeqCst)
    }

    pub fn fetch_count(&self) -> usize {
        self.fetch_count.load(Ordering::SeqCst)
    }

    /// Build a finished provider from this backend.
    pub fn into_provider(self) -> Arc<UsenetArticleProvider> {
        Arc::new(UsenetArticleProvider::with_fake(Arc::new(self)))
    }
}

impl Default for MockArticleBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ArticleBackend for MockArticleBackend {
    async fn fetch_decoded(&self, message_id: &str) -> Result<Vec<u8>> {
        let n = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_in_flight.fetch_max(n, Ordering::SeqCst);
        self.fetch_count.fetch_add(1, Ordering::SeqCst);

        if let Some(gate) = &self.gate {
            gate.notified().await;
        } else if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }

        let result = match self.articles.get(message_id) {
            Some(a) => {
                let decoded =
                    decode_yenc(&a.raw_yenc).map_err(|e| StreamError::YencDecode(e.to_string()))?;
                Ok(decoded.data)
            }
            None => Err(StreamError::ArticleNotFound(message_id.to_string())),
        };

        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        result
    }

    async fn stat(&self, message_id: &str) -> Result<bool> {
        Ok(self.articles.contains_key(message_id))
    }

    async fn yenc_headers(&self, message_id: &str) -> Result<YencHeaders> {
        match self.articles.get(message_id) {
            Some(a) => Ok(YencHeaders {
                part_begin: a.part_begin,
                part_end: a.part_end,
                total_size: a.total_size,
            }),
            None => Err(StreamError::ArticleNotFound(message_id.to_string())),
        }
    }

    fn total_connections(&self) -> usize {
        // Effectively unlimited for tests.
        usize::MAX / 2
    }
}

/// Convenience: build a mock provider serving a single file split across
/// `n_segments` equal-sized parts. Returns (provider, message_ids, file_bytes).
pub fn mock_provider_for_file(
    filename: &str,
    file_bytes: Vec<u8>,
    n_segments: usize,
) -> (Arc<UsenetArticleProvider>, Vec<String>, Vec<u8>) {
    assert!(n_segments > 0);
    let total = file_bytes.len();
    let seg_size = total.div_ceil(n_segments);

    let mut segments = Vec::new();
    let mut offset = 0usize;
    for i in 0..n_segments {
        let len = seg_size.min(total - offset);
        segments.push(((i + 1) as u32, offset as u64, len));
        offset += len;
        if offset >= total {
            break;
        }
    }

    let articles = build_yenc_articles(filename, &file_bytes, &segments);
    let message_ids: Vec<String> = (0..articles.len())
        .map(|i| format!("seg-{i}@mock.test"))
        .collect();

    let mut backend = MockArticleBackend::new();
    for (mid, art) in message_ids.iter().zip(articles.into_iter()) {
        backend.add(mid.clone(), art);
    }

    (backend.into_provider(), message_ids, file_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_provider_round_trips_bytes() {
        let data: Vec<u8> = (0..4096u32).flat_map(|n| n.to_le_bytes()).collect();
        let (provider, mids, original) = mock_provider_for_file("test.bin", data, 4);

        let mut reassembled = Vec::new();
        for mid in &mids {
            reassembled.extend(provider.fetch_decoded(mid).await.unwrap());
        }
        assert_eq!(reassembled, original);
    }

    #[tokio::test]
    async fn yenc_headers_report_correct_ranges() {
        let data = vec![0u8; 1000];
        let (provider, mids, _) = mock_provider_for_file("test.bin", data, 4);
        let h0 = provider.yenc_headers(&mids[0]).await.unwrap();
        assert_eq!(h0.part_begin, 1);
        assert_eq!(h0.part_end, 250);
        assert_eq!(h0.total_size, 1000);
        let h3 = provider.yenc_headers(&mids[3]).await.unwrap();
        assert_eq!(h3.part_begin, 751);
        assert_eq!(h3.part_end, 1000);
    }
}
