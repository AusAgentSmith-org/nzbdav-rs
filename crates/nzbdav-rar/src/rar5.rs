//! RAR5 format header parser.

use std::io::{Read, Seek, SeekFrom};

use crate::crypto::{Rar5EncryptionHeader, decrypt_rar5_headers, derive_rar5_key};
use crate::error::{RarError, Result};
use crate::header::{
    ArchiveHeader, EndArchiveHeader, FileHeader, RarEncryption, RarHeader, ServiceHeader,
};
use crate::RAR5_MAGIC;

/// Read a RAR5 variable-length integer (vint).
/// Each byte: bits 0-6 are data, bit 7 is continuation flag.
/// Little-endian: first byte is least significant.
fn read_vint<R: Read>(reader: &mut R) -> Result<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte)?;
        let b = byte[0];
        result |= u64::from(b & 0x7F) << shift;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 63 {
            return Err(RarError::TruncatedHeader(0));
        }
    }
    Ok(result)
}

/// Read a 4-byte little-endian u32.
fn read_u32_le<R: Read>(reader: &mut R) -> Result<u32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

/// Parse all headers from a RAR5 archive.
/// The reader should be positioned at the start of the stream (magic is read first).
///
/// If the archive has encrypted headers (type 4 encryption header), the `password`
/// parameter is used to derive the AES-256-CBC key and decrypt all subsequent headers.
/// When no password is provided and encrypted headers are encountered, returns
/// `RarError::EncryptedHeaders`.
pub fn parse_rar5<R: Read + Seek>(reader: &mut R, password: Option<&str>) -> Result<Vec<RarHeader>> {
    // Read and verify magic
    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic)?;
    if magic != RAR5_MAGIC {
        return Err(RarError::SignatureNotFound);
    }

    let mut headers = Vec::new();

    loop {
        let header_start = reader.stream_position()?;

        // Try to read header CRC32
        let expected_crc = match read_u32_le(reader) {
            Ok(v) => v,
            Err(RarError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        };

        // Record position after CRC (start of CRC-checked data)
        let crc_data_start = reader.stream_position()?;

        let header_size = read_vint(reader)?;

        // header_size is the number of bytes AFTER the header_size vint.
        // The CRC covers: header_size_vint + header_size bytes.
        let header_size_vint_len = reader.stream_position()? - crc_data_start;
        let total_header_bytes = header_size_vint_len + header_size;

        let header_type = read_vint(reader)?;
        let header_flags = read_vint(reader)?;

        let extra_area_size = if header_flags & 0x0001 != 0 {
            read_vint(reader)?
        } else {
            0
        };

        let data_area_size = if header_flags & 0x0002 != 0 {
            read_vint(reader)?
        } else {
            0
        };

        // Read the rest of the header into a buffer for CRC checking
        let current_pos = reader.stream_position()?;
        let consumed_from_crc_start = current_pos - crc_data_start;
        let remaining_header = total_header_bytes.saturating_sub(consumed_from_crc_start);

        // We need to read remaining header bytes for CRC and parsing
        let mut remaining_buf = vec![0u8; remaining_header as usize];
        reader.read_exact(&mut remaining_buf)?;

        // Verify CRC32 over the entire header (from crc_data_start for total_header_bytes)
        {
            let end_pos = reader.stream_position()?;
            reader.seek(SeekFrom::Start(crc_data_start))?;
            let mut crc_buf = vec![0u8; total_header_bytes as usize];
            reader.read_exact(&mut crc_buf)?;
            let actual_crc = crc32fast::hash(&crc_buf);
            if actual_crc != expected_crc {
                return Err(RarError::InvalidHeaderCrc(header_start));
            }
            reader.seek(SeekFrom::Start(end_pos))?;
        }

        // Parse type-specific fields from remaining_buf
        let mut cursor = std::io::Cursor::new(&remaining_buf);

        match header_type {
            // Encryption header (type 4) — headers after this are encrypted
            4 => {
                let enc_header = parse_encryption_header(&mut cursor)?;

                let password = password.ok_or_else(|| {
                    tracing::warn!(
                        "RAR5 archive has encrypted headers — cannot parse without password"
                    );
                    RarError::EncryptedHeaders
                })?;

                tracing::debug!(
                    lg2_count = enc_header.lg2_count,
                    has_psw_check = enc_header.has_psw_check,
                    "decrypting RAR5 encrypted headers"
                );

                // Derive the AES-256 key from the password
                let derived = derive_rar5_key(password, &enc_header)?;

                // Read remaining data from the reader — everything after this
                // encryption header is AES-256-CBC encrypted.
                let mut encrypted_data = Vec::new();
                reader.read_to_end(&mut encrypted_data)?;

                if encrypted_data.len() < 16 {
                    return Err(RarError::DecryptionError(
                        "encrypted header data too short (need at least 16 bytes for IV)"
                            .to_string(),
                    ));
                }

                // First 16 bytes of the encrypted data block are the IV
                let mut iv = [0u8; 16];
                iv.copy_from_slice(&encrypted_data[..16]);
                let ciphertext = &encrypted_data[16..];

                // Decrypt the headers
                let decrypted = decrypt_rar5_headers(ciphertext, &derived.key, &iv)?;

                // Parse headers from the decrypted data.
                // The decrypted block contains regular RAR5 headers
                // (CRC + header_size + type + ...) without magic.
                let mut dec_cursor = std::io::Cursor::new(decrypted);
                let decrypted_headers = parse_rar5_headers_from_data(&mut dec_cursor)?;
                headers.extend(decrypted_headers);

                // All remaining headers have been parsed from the decrypted block
                break;
            }
            // Archive header
            1 => {
                let archive_flags = read_vint(&mut cursor)?;
                let is_volume = archive_flags & 0x0001 != 0;
                let has_volume_number = archive_flags & 0x0002 != 0;
                let volume_number = if has_volume_number {
                    Some(read_vint(&mut cursor)? as i32)
                } else {
                    None
                };
                headers.push(RarHeader::Archive(ArchiveHeader {
                    is_first_volume: !is_volume || volume_number == Some(0),
                    volume_number,
                }));
            }
            // File header
            2 => {
                let fh = parse_rar5_file_header(
                    &mut cursor,
                    extra_area_size,
                    data_area_size,
                    remaining_header,
                    reader,
                )?;
                headers.push(RarHeader::File(fh));
            }
            // Service header
            3 => {
                let fh = parse_rar5_file_header(
                    &mut cursor,
                    extra_area_size,
                    data_area_size,
                    remaining_header,
                    reader,
                )?;
                headers.push(RarHeader::Service(ServiceHeader {
                    name: fh.filename,
                    data_size: fh.data_size,
                }));
            }
            // End archive
            5 => {
                let end_flags = read_vint(&mut cursor)?;
                let has_next_volume = end_flags & 0x0001 != 0;
                headers.push(RarHeader::EndArchive(EndArchiveHeader {
                    volume_number: if has_next_volume { Some(1) } else { None },
                }));
                break;
            }
            // Unknown header type — skip
            _ => {
                tracing::warn!("unknown RAR5 header type {} at offset {}", header_type, header_start);
            }
        }

        // Seek past data area if present.
        // For multi-volume RARs or partial data (header-only parsing),
        // the seek target may be past the end of the reader — that's OK,
        // the next read_u32_le will hit EOF and we'll break out of the loop.
        if data_area_size > 0 {
            let data_start = crc_data_start + total_header_bytes;
            let target = data_start + data_area_size;
            if reader.seek(SeekFrom::Start(target)).is_err() {
                break; // Past end of data — no more headers
            }
        }
    }

    Ok(headers)
}

/// Parse the RAR5 encryption header body (type 4).
///
/// Layout:
/// - Encryption version (vint) — must be 0
/// - Encryption flags (vint) — bit 0 = has_password_check
/// - KDF count (1 byte) — lg2_count (iterations = 1 << lg2_count)
/// - Salt (16 bytes)
/// - If has_password_check: password check value (8 bytes) + check sum (4 bytes)
fn parse_encryption_header<R: Read>(reader: &mut R) -> Result<Rar5EncryptionHeader> {
    let version = read_vint(reader)?;
    if version != 0 {
        return Err(RarError::UnsupportedEncryptionVersion(version));
    }

    let enc_flags = read_vint(reader)?;
    let has_psw_check = enc_flags & 0x0001 != 0;

    let mut lg2_count_buf = [0u8; 1];
    reader.read_exact(&mut lg2_count_buf)?;
    let lg2_count = lg2_count_buf[0];

    let mut salt = vec![0u8; 16];
    reader.read_exact(&mut salt)?;

    let (psw_check, psw_check_sum) = if has_psw_check {
        let mut check = vec![0u8; 8];
        reader.read_exact(&mut check)?;
        let mut sum = vec![0u8; 4];
        reader.read_exact(&mut sum)?;
        (check, sum)
    } else {
        (Vec::new(), Vec::new())
    };

    Ok(Rar5EncryptionHeader {
        lg2_count,
        salt,
        has_psw_check,
        psw_check,
        psw_check_sum,
    })
}

/// Parse RAR5 headers from a decrypted data buffer (no magic prefix).
///
/// This is used after decrypting the encrypted header block. The decrypted
/// data contains normal RAR5 headers (CRC + header_size + ...) without the
/// 8-byte magic prefix. Parsing stops at EOF or end-archive header.
fn parse_rar5_headers_from_data<R: Read + Seek>(reader: &mut R) -> Result<Vec<RarHeader>> {
    let mut headers = Vec::new();

    loop {
        let header_start = reader.stream_position()?;

        // Try to read header CRC32
        let expected_crc = match read_u32_le(reader) {
            Ok(v) => v,
            Err(RarError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        };

        let crc_data_start = reader.stream_position()?;

        let header_size = match read_vint(reader) {
            Ok(v) => v,
            Err(RarError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        };

        let header_size_vint_len = reader.stream_position()? - crc_data_start;
        let total_header_bytes = header_size_vint_len + header_size;

        let header_type = read_vint(reader)?;
        let header_flags = read_vint(reader)?;

        let extra_area_size = if header_flags & 0x0001 != 0 {
            read_vint(reader)?
        } else {
            0
        };

        let data_area_size = if header_flags & 0x0002 != 0 {
            read_vint(reader)?
        } else {
            0
        };

        let current_pos = reader.stream_position()?;
        let consumed_from_crc_start = current_pos - crc_data_start;
        let remaining_header = total_header_bytes.saturating_sub(consumed_from_crc_start);

        let mut remaining_buf = vec![0u8; remaining_header as usize];
        reader.read_exact(&mut remaining_buf)?;

        // Verify CRC32 over the header
        {
            let end_pos = reader.stream_position()?;
            reader.seek(SeekFrom::Start(crc_data_start))?;
            let mut crc_buf = vec![0u8; total_header_bytes as usize];
            reader.read_exact(&mut crc_buf)?;
            let actual_crc = crc32fast::hash(&crc_buf);
            if actual_crc != expected_crc {
                // In decrypted data, a CRC mismatch likely means we've hit
                // padding bytes at the end of the decrypted block.
                tracing::debug!(
                    "CRC mismatch in decrypted block at offset {} — likely padding, stopping",
                    header_start
                );
                break;
            }
            reader.seek(SeekFrom::Start(end_pos))?;
        }

        let mut cursor = std::io::Cursor::new(&remaining_buf);

        match header_type {
            1 => {
                let archive_flags = read_vint(&mut cursor)?;
                let is_volume = archive_flags & 0x0001 != 0;
                let has_volume_number = archive_flags & 0x0002 != 0;
                let volume_number = if has_volume_number {
                    Some(read_vint(&mut cursor)? as i32)
                } else {
                    None
                };
                headers.push(RarHeader::Archive(ArchiveHeader {
                    is_first_volume: !is_volume || volume_number == Some(0),
                    volume_number,
                }));
            }
            2 => {
                let fh = parse_rar5_file_header(
                    &mut cursor,
                    extra_area_size,
                    data_area_size,
                    remaining_header,
                    reader,
                )?;
                headers.push(RarHeader::File(fh));
            }
            3 => {
                let fh = parse_rar5_file_header(
                    &mut cursor,
                    extra_area_size,
                    data_area_size,
                    remaining_header,
                    reader,
                )?;
                headers.push(RarHeader::Service(ServiceHeader {
                    name: fh.filename,
                    data_size: fh.data_size,
                }));
            }
            5 => {
                let end_flags = read_vint(&mut cursor)?;
                let has_next_volume = end_flags & 0x0001 != 0;
                headers.push(RarHeader::EndArchive(EndArchiveHeader {
                    volume_number: if has_next_volume { Some(1) } else { None },
                }));
                break;
            }
            _ => {
                tracing::debug!(
                    "unknown header type {} in decrypted block at offset {}",
                    header_type,
                    header_start
                );
            }
        }

        // Seek past data area if present
        if data_area_size > 0 {
            let data_start = crc_data_start + total_header_bytes;
            let target = data_start + data_area_size;
            if reader.seek(SeekFrom::Start(target)).is_err() {
                break;
            }
        }
    }

    Ok(headers)
}

/// Parse file/service header fields from the type-specific portion.
fn parse_rar5_file_header<R: Read + Seek>(
    cursor: &mut std::io::Cursor<&Vec<u8>>,
    extra_area_size: u64,
    data_area_size: u64,
    _remaining_header: u64,
    outer_reader: &mut R,
) -> Result<FileHeader> {
    let file_flags = read_vint(cursor)?;
    let is_directory = file_flags & 0x0001 != 0;
    let has_unix_timestamps = file_flags & 0x0002 != 0;
    let has_crc32 = file_flags & 0x0004 != 0;
    let has_unknown_unpacked_size = file_flags & 0x0008 != 0;

    let unpacked_size = if has_unknown_unpacked_size {
        0
    } else {
        read_vint(cursor)?
    };

    let _attributes = read_vint(cursor)?;

    if has_unix_timestamps {
        let mut _mtime_buf = [0u8; 4];
        cursor.read_exact(&mut _mtime_buf)?;
    }

    if has_crc32 {
        let mut _data_crc = [0u8; 4];
        cursor.read_exact(&mut _data_crc)?;
    }

    let compression_info = read_vint(cursor)?;
    let method = ((compression_info >> 7) & 0x0F) as u8;
    let is_solid = compression_info & 0x0040 != 0;

    let _host_os = read_vint(cursor)?;
    let name_len = read_vint(cursor)?;
    let mut name_buf = vec![0u8; name_len as usize];
    cursor.read_exact(&mut name_buf)?;
    let filename = String::from_utf8_lossy(&name_buf).into_owned();

    // Parse extra area for encryption info
    let mut encryption = None;
    let mut is_encrypted = false;
    if extra_area_size > 0 {
        let extra_start = cursor.position();
        let extra_end = extra_start + extra_area_size;
        while cursor.position() < extra_end {
            let extra_size = match read_vint(cursor) {
                Ok(v) => v,
                Err(_) => break,
            };
            let extra_field_start = cursor.position();
            let extra_type = match read_vint(cursor) {
                Ok(v) => v,
                Err(_) => break,
            };

            if extra_type == 1 {
                // Encryption record
                is_encrypted = true;
                let _version = read_vint(cursor)?;
                let enc_flags = read_vint(cursor)?;
                let has_psw_check = enc_flags & 0x0001 != 0;
                let mut lg2_count = [0u8; 1];
                cursor.read_exact(&mut lg2_count)?;
                let mut salt = vec![0u8; 16];
                cursor.read_exact(&mut salt)?;
                let mut iv = vec![0u8; 16];
                cursor.read_exact(&mut iv)?;
                let psw_check = if has_psw_check {
                    let mut buf = vec![0u8; 12];
                    cursor.read_exact(&mut buf)?;
                    buf
                } else {
                    Vec::new()
                };
                encryption = Some(RarEncryption::Rar5 {
                    lg2_count: lg2_count[0],
                    salt,
                    use_psw_check: has_psw_check,
                    psw_check,
                    iv,
                });
            }

            // Skip to end of this extra field
            cursor.set_position(extra_field_start + extra_size);
        }
    }

    // Calculate data start position — it's right after the current header in the outer stream
    let data_start_position = outer_reader.stream_position()?;

    Ok(FileHeader {
        filename,
        uncompressed_size: unpacked_size,
        compressed_size: data_area_size,
        compression_method: method,
        data_start_position,
        data_size: data_area_size,
        is_directory,
        is_encrypted,
        is_solid,
        volume_number: None,
        encryption,
    })
}

/// Test helper functions for building synthetic RAR5 archives.
/// Exposed for use by other module tests (e.g., parser).
#[doc(hidden)]
pub mod tests_helper {
    use crate::RAR5_MAGIC;

    /// Build a minimal RAR5 archive with store compression (method 0).
    pub fn build_minimal_rar5_store() -> Vec<u8> {
        build_rar5_with_method(0)
    }

    /// Build a minimal RAR5 archive with the given compression method.
    pub fn build_rar5_with_method(method: u8) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(RAR5_MAGIC);

        // Archive header (type 1)
        {
            // header_size = bytes after the header_size vint
            // Contents after vint: type(1) + flags(1) + archive_flags(1) = 3 bytes
            let inner = vec![1u8, 0, 0]; // type=archive, flags=0, archive_flags=0
            let size_vint = vec![inner.len() as u8]; // header_size vint
            let mut header_data = Vec::new();
            header_data.extend_from_slice(&size_vint);
            header_data.extend_from_slice(&inner);
            // CRC covers the entire header_data (vint + inner)
            let crc = crc32fast::hash(&header_data);
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&header_data);
        }

        // File header (type 2)
        {
            let filename = b"test.txt";
            let mut inner = vec![
                2u8, // type = file
                0x02, // flags = has data area
                5,    // data area size = 5
                0x04, // file_flags = has_crc32
                5,    // unpacked_size = 5
                0,    // attributes = 0
            ];
            let data_crc = crc32fast::hash(b"hello");
            inner.extend_from_slice(&data_crc.to_le_bytes());
            // compression_info: method in bits 7-10
            let compression_info: u64 = (method as u64) << 7;
            // Encode as vint
            let mut ci = compression_info;
            loop {
                let mut byte = (ci & 0x7F) as u8;
                ci >>= 7;
                if ci > 0 {
                    byte |= 0x80;
                }
                inner.push(byte);
                if ci == 0 {
                    break;
                }
            }
            inner.push(0u8); // host_os = 0
            inner.push(filename.len() as u8); // name_len
            inner.extend_from_slice(filename);

            // header_size = bytes after the header_size vint = inner.len()
            let header_size_val = inner.len();
            assert!(header_size_val < 128);
            let size_vint = vec![header_size_val as u8];
            let mut header_data = Vec::new();
            header_data.extend_from_slice(&size_vint);
            header_data.extend_from_slice(&inner);
            // CRC covers vint + inner
            let crc = crc32fast::hash(&header_data);
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&header_data);
            out.extend_from_slice(b"hello");
        }

        // End archive header (type 5)
        {
            // inner: type=end(1) + flags(1) + end_flags(1) = 3 bytes
            let inner = vec![5u8, 0, 0]; // type=end, flags=0, end_flags=0
            let size_vint = vec![inner.len() as u8];
            let mut header_data = Vec::new();
            header_data.extend_from_slice(&size_vint);
            header_data.extend_from_slice(&inner);
            let crc = crc32fast::hash(&header_data);
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&header_data);
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn read_vint_single_byte() {
        let data = [0x05u8];
        let mut cursor = Cursor::new(&data[..]);
        assert_eq!(read_vint(&mut cursor).unwrap(), 5);
    }

    #[test]
    fn read_vint_multi_byte() {
        // 128 = 0x80 in vint: first byte 0x80 (continuation, data=0), second byte 0x01 (data=1, shift by 7 = 128)
        let data = [0x80u8, 0x01];
        let mut cursor = Cursor::new(&data[..]);
        assert_eq!(read_vint(&mut cursor).unwrap(), 128);
    }

    #[test]
    fn read_vint_value_255() {
        // 255 = 0x7F | (0x01 << 7) → bytes: 0xFF, 0x01
        let data = [0xFFu8, 0x01];
        let mut cursor = Cursor::new(&data[..]);
        assert_eq!(read_vint(&mut cursor).unwrap(), 255);
    }

    /// Build a minimal RAR5 archive with: magic + archive header + file header + end archive.
    fn build_minimal_rar5() -> Vec<u8> {
        let mut out = Vec::new();

        // Magic
        out.extend_from_slice(RAR5_MAGIC);

        // --- Archive header (type 1) ---
        {
            let inner = vec![1u8, 0, 0]; // type=archive, flags=0, archive_flags=0
            let size_vint = vec![inner.len() as u8];
            let mut header_data = Vec::new();
            header_data.extend_from_slice(&size_vint);
            header_data.extend_from_slice(&inner);
            let crc = crc32fast::hash(&header_data);
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&header_data);
        }

        // --- File header (type 2) ---
        {
            let filename = b"test.txt";
            let mut inner = vec![
                2u8, 0x02, 5, 0x04, 5, 0, // type, flags, data_size, file_flags, unpack_size, attrs
            ];
            let data_crc = crc32fast::hash(b"hello");
            inner.extend_from_slice(&data_crc.to_le_bytes());
            inner.push(0u8); // compression_info = 0 (store)
            inner.push(0u8); // host_os
            inner.push(filename.len() as u8);
            inner.extend_from_slice(filename);

            let header_size_val = inner.len();
            assert!(header_size_val < 128);
            let size_vint = vec![header_size_val as u8];
            let mut header_data = Vec::new();
            header_data.extend_from_slice(&size_vint);
            header_data.extend_from_slice(&inner);

            let crc = crc32fast::hash(&header_data);
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&header_data);
            out.extend_from_slice(b"hello");
        }

        // --- End archive header (type 5) ---
        {
            let inner = vec![5u8, 0, 0]; // type=end, flags=0, end_flags=0
            let size_vint = vec![inner.len() as u8];
            let mut header_data = Vec::new();
            header_data.extend_from_slice(&size_vint);
            header_data.extend_from_slice(&inner);
            let crc = crc32fast::hash(&header_data);
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&header_data);
        }

        out
    }

    #[test]
    fn parse_minimal_rar5() {
        let data = build_minimal_rar5();
        let mut cursor = Cursor::new(&data[..]);
        let headers = parse_rar5(&mut cursor, None).unwrap();

        assert_eq!(headers.len(), 3);

        // Archive header
        match &headers[0] {
            RarHeader::Archive(ah) => {
                assert!(ah.volume_number.is_none());
            }
            other => panic!("expected Archive header, got {:?}", other),
        }

        // File header
        match &headers[1] {
            RarHeader::File(fh) => {
                assert_eq!(fh.filename, "test.txt");
                assert_eq!(fh.uncompressed_size, 5);
                assert_eq!(fh.compressed_size, 5);
                assert_eq!(fh.compression_method, 0);
                assert!(!fh.is_directory);
                assert!(!fh.is_encrypted);
                assert!(!fh.is_solid);
            }
            other => panic!("expected File header, got {:?}", other),
        }

        // End archive
        match &headers[2] {
            RarHeader::EndArchive(ea) => {
                assert!(ea.volume_number.is_none());
            }
            other => panic!("expected EndArchive header, got {:?}", other),
        }
    }

    /// Build a RAR5 stream that starts with the magic then an encryption header (type 4).
    fn build_rar5_with_encryption_header() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(RAR5_MAGIC);

        // Encryption header (type 4)
        // inner: type=4, flags=0, then encryption body
        let mut inner = Vec::new();
        inner.push(4u8); // type = encryption
        inner.push(0u8); // flags = 0 (no extra area, no data area)
        // Encryption body:
        inner.push(0u8); // encryption version = 0
        inner.push(0u8); // encryption flags = 0 (no password check)
        inner.push(15u8); // lg2_count = 15
        inner.extend_from_slice(&[0u8; 16]); // salt (16 bytes of zeros)

        let header_size_val = inner.len();
        assert!(header_size_val < 128);
        let size_vint = vec![header_size_val as u8];
        let mut header_data = Vec::new();
        header_data.extend_from_slice(&size_vint);
        header_data.extend_from_slice(&inner);
        let crc = crc32fast::hash(&header_data);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&header_data);

        out
    }

    #[test]
    fn test_encrypted_headers_no_password() {
        let data = build_rar5_with_encryption_header();
        let mut cursor = Cursor::new(&data[..]);
        let result = parse_rar5(&mut cursor, None);
        assert!(result.is_err(), "should fail without password");
        assert!(
            matches!(result, Err(RarError::EncryptedHeaders)),
            "expected EncryptedHeaders error, got: {result:?}"
        );
    }

    #[test]
    fn test_encrypted_headers_detected() {
        // Verify that header type 4 is recognized as the encryption header
        // by checking the error message differs from other parse errors.
        let data = build_rar5_with_encryption_header();
        let mut cursor = Cursor::new(&data[..]);
        let err = parse_rar5(&mut cursor, None).unwrap_err();
        // The error should specifically be EncryptedHeaders, not a generic parse failure
        assert_eq!(
            format!("{err}"),
            "archive has encrypted headers \u{2014} password required to read file list"
        );
    }

    #[test]
    fn invalid_crc_rejected() {
        let mut data = build_minimal_rar5();
        // Corrupt the CRC of the first header (bytes 8..12)
        data[8] ^= 0xFF;
        let mut cursor = Cursor::new(&data[..]);
        let result = parse_rar5(&mut cursor, None);
        assert!(matches!(result, Err(RarError::InvalidHeaderCrc(_))));
    }
}
