//! Unified RAR parser entry point.

use std::io::{Read, Seek, SeekFrom};

use crate::error::{RarError, Result};
use crate::header::{FileHeader, RarHeader};
use crate::{detect_version, RarVersion};

/// Parse a RAR archive and return only `FileHeader` entries with store (m0) compression.
///
/// Returns an error if any file header uses a non-store compression method.
/// For password-protected RAR5 archives with encrypted headers, pass the
/// password to decrypt the header block.
pub fn parse_headers<R: Read + Seek>(
    reader: &mut R,
    password: Option<&str>,
) -> Result<Vec<FileHeader>> {
    let all = parse_all_headers(reader, password)?;
    let mut files = Vec::new();
    for header in all {
        if let RarHeader::File(fh) = header {
            if fh.compression_method != 0 {
                return Err(RarError::UnsupportedCompressionMethod(
                    fh.compression_method,
                ));
            }
            files.push(fh);
        }
    }
    Ok(files)
}

/// Parse a RAR archive and return all headers (archive, file, service, end).
///
/// For password-protected RAR5 archives with encrypted headers, pass the
/// password to decrypt the header block. RAR4 ignores the password parameter.
pub fn parse_all_headers<R: Read + Seek>(
    reader: &mut R,
    password: Option<&str>,
) -> Result<Vec<RarHeader>> {
    // Read enough bytes to detect version
    let mut magic_buf = [0u8; 8];
    let bytes_read = reader.read(&mut magic_buf)?;

    let version = detect_version(&magic_buf[..bytes_read]).ok_or(RarError::SignatureNotFound)?;

    // Seek back to start so the format-specific parser can read the magic
    reader.seek(SeekFrom::Start(0))?;

    match version {
        RarVersion::Rar5 => crate::rar5::parse_rar5(reader, password),
        RarVersion::Rar4 => crate::rar4::parse_rar4(reader),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parse_headers_filters_to_file_headers() {
        // Build a minimal RAR5 archive using the test helper from rar5 module
        // We'll construct it inline here for independence
        let data = crate::rar5::tests_helper::build_minimal_rar5_store();
        let mut cursor = Cursor::new(&data[..]);
        let files = parse_headers(&mut cursor, None).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].filename, "test.txt");
        assert_eq!(files[0].compression_method, 0);
    }

    #[test]
    fn parse_headers_rejects_non_store_compression() {
        let data = crate::rar5::tests_helper::build_rar5_with_method(1);
        let mut cursor = Cursor::new(&data[..]);
        let result = parse_headers(&mut cursor, None);
        assert!(matches!(
            result,
            Err(RarError::UnsupportedCompressionMethod(..))
        ));
    }

    #[test]
    fn parse_all_headers_returns_all() {
        let data = crate::rar5::tests_helper::build_minimal_rar5_store();
        let mut cursor = Cursor::new(&data[..]);
        let all = parse_all_headers(&mut cursor, None).unwrap();
        // Should have archive + file + end = 3
        assert_eq!(all.len(), 3);
        assert!(matches!(all[0], RarHeader::Archive(_)));
        assert!(matches!(all[1], RarHeader::File(_)));
        assert!(matches!(all[2], RarHeader::EndArchive(_)));
    }

    #[test]
    fn invalid_magic_rejected() {
        let data = vec![0u8; 16];
        let mut cursor = Cursor::new(&data[..]);
        let result = parse_all_headers(&mut cursor, None);
        assert!(matches!(result, Err(RarError::SignatureNotFound)));
    }
}
