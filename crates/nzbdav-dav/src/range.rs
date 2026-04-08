//! HTTP Range header parsing for 206 Partial Content responses.

use crate::error::{DavServerError, Result};

/// A parsed byte range from an HTTP `Range` header.
#[derive(Debug, Clone, Copy)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64, // inclusive
}

impl ByteRange {
    /// Number of bytes in this range (inclusive).
    pub fn length(&self) -> u64 {
        self.end - self.start + 1
    }
}

/// Parse an HTTP `Range` header value.
///
/// Supports:
/// - `bytes=start-end`  (both inclusive)
/// - `bytes=start-`     (from start to end of file)
/// - `bytes=-suffix`    (last N bytes)
///
/// Returns an error on malformed input or unsatisfiable ranges.
pub fn parse_range(header: &str, file_size: u64) -> Result<Vec<ByteRange>> {
    let header = header.trim();
    let rest = header
        .strip_prefix("bytes=")
        .ok_or_else(|| DavServerError::InvalidRange("missing 'bytes=' prefix".into()))?;

    let mut ranges = Vec::new();

    for part in rest.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        let range = if let Some(suffix) = part.strip_prefix('-') {
            // Suffix range: bytes=-N
            let n: u64 = suffix
                .parse()
                .map_err(|_| DavServerError::InvalidRange(format!("invalid suffix: {part}")))?;
            if n == 0 || n > file_size {
                return Err(DavServerError::InvalidRange(format!(
                    "unsatisfiable suffix range: {part}"
                )));
            }
            ByteRange {
                start: file_size - n,
                end: file_size - 1,
            }
        } else if let Some((start_s, end_s)) = part.split_once('-') {
            let start: u64 = start_s
                .parse()
                .map_err(|_| DavServerError::InvalidRange(format!("invalid start: {part}")))?;

            if end_s.is_empty() {
                // Open-ended: bytes=N-
                if start >= file_size {
                    return Err(DavServerError::InvalidRange(format!(
                        "start beyond file size: {part}"
                    )));
                }
                ByteRange {
                    start,
                    end: file_size - 1,
                }
            } else {
                // Full range: bytes=N-M
                let end: u64 = end_s.parse().map_err(|_| {
                    DavServerError::InvalidRange(format!("invalid end: {part}"))
                })?;
                if start > end || start >= file_size {
                    return Err(DavServerError::InvalidRange(format!(
                        "unsatisfiable range: {part}"
                    )));
                }
                ByteRange {
                    start,
                    end: end.min(file_size - 1),
                }
            }
        } else {
            return Err(DavServerError::InvalidRange(format!(
                "malformed range spec: {part}"
            )));
        };

        ranges.push(range);
    }

    if ranges.is_empty() {
        return Err(DavServerError::InvalidRange("no ranges specified".into()));
    }

    Ok(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_full_range() {
        let ranges = parse_range("bytes=0-499", 1000).unwrap();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges[0].end, 499);
        assert_eq!(ranges[0].length(), 500);
    }

    #[test]
    fn test_open_ended() {
        let ranges = parse_range("bytes=500-", 1000).unwrap();
        assert_eq!(ranges[0].start, 500);
        assert_eq!(ranges[0].end, 999);
    }

    #[test]
    fn test_suffix() {
        let ranges = parse_range("bytes=-200", 1000).unwrap();
        assert_eq!(ranges[0].start, 800);
        assert_eq!(ranges[0].end, 999);
    }

    #[test]
    fn test_clamp_end() {
        let ranges = parse_range("bytes=900-2000", 1000).unwrap();
        assert_eq!(ranges[0].end, 999);
    }

    #[test]
    fn test_invalid_no_prefix() {
        assert!(parse_range("0-499", 1000).is_err());
    }

    #[test]
    fn test_start_beyond_size() {
        assert!(parse_range("bytes=1000-", 1000).is_err());
    }
}
