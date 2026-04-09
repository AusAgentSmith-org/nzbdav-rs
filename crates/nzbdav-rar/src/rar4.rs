//! RAR4 format header parser.

use std::io::{Read, Seek, SeekFrom};

use crate::RAR4_MAGIC;
use crate::error::{RarError, Result};
use crate::header::{
    ArchiveHeader, EndArchiveHeader, FileHeader, RarEncryption, RarHeader, ServiceHeader,
};

// RAR4 header type constants
const RAR4_ARCHIVE: u8 = 0x73;
const RAR4_FILE: u8 = 0x74;
const RAR4_SERVICE: u8 = 0x7A;
const RAR4_END_ARCHIVE: u8 = 0x7B;

/// Flags common to all RAR4 headers.
const RAR4_LONG_BLOCK: u16 = 0x8000;

/// Read a 2-byte little-endian u16.
fn read_u16_le<R: Read>(reader: &mut R) -> Result<u16> {
    let mut buf = [0u8; 2];
    reader.read_exact(&mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

/// Read a 4-byte little-endian u32.
fn read_u32_le<R: Read>(reader: &mut R) -> Result<u32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

/// Parse all headers from a RAR4 archive.
pub fn parse_rar4<R: Read + Seek>(reader: &mut R) -> Result<Vec<RarHeader>> {
    // Read and verify magic
    let mut magic = [0u8; 7];
    reader.read_exact(&mut magic)?;
    if magic != RAR4_MAGIC {
        return Err(RarError::SignatureNotFound);
    }

    let mut headers = Vec::new();

    loop {
        let header_start = reader.stream_position()?;

        // Read base header: CRC(2) + type(1) + flags(2) + size(2) = 7 bytes
        let mut base_header = [0u8; 7];
        match reader.read_exact(&mut base_header) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(RarError::Io(e)),
        }

        let _header_crc = u16::from_le_bytes([base_header[0], base_header[1]]);
        let header_type = base_header[2];
        let flags = u16::from_le_bytes([base_header[3], base_header[4]]);
        let header_size = u16::from_le_bytes([base_header[5], base_header[6]]) as u64;

        // ADD_SIZE: 4 bytes if LONG_BLOCK flag set (for file headers this is the packed data size)
        let add_size = if flags & RAR4_LONG_BLOCK != 0 {
            // Read from just after base header
            let pos = reader.stream_position()?;
            reader.seek(SeekFrom::Start(header_start + 7))?;
            let v = read_u32_le(reader)? as u64;
            reader.seek(SeekFrom::Start(pos))?;
            v
        } else {
            0
        };

        // Read the full header into a buffer for parsing type-specific fields
        reader.seek(SeekFrom::Start(header_start + 7))?;
        let remaining = header_size.saturating_sub(7);
        let mut body = vec![0u8; remaining as usize];
        reader.read_exact(&mut body)?;

        match header_type {
            RAR4_ARCHIVE => {
                // Archive header body: reserved1(2) + reserved2(4) = 6 bytes
                let is_volume = flags & 0x0001 != 0;
                let is_first_volume = flags & 0x0100 != 0;
                headers.push(RarHeader::Archive(ArchiveHeader {
                    is_first_volume: !is_volume || is_first_volume,
                    volume_number: None,
                }));
            }
            RAR4_FILE => {
                let fh = parse_rar4_file_header(&body, flags, header_start, header_size)?;
                // Seek past packed data
                let total_packed = fh.compressed_size;
                reader.seek(SeekFrom::Start(header_start + header_size + total_packed))?;
                headers.push(RarHeader::File(fh));
                continue; // skip the seek below
            }
            RAR4_SERVICE => {
                let fh = parse_rar4_file_header(&body, flags, header_start, header_size)?;
                let data_size = fh.data_size;
                reader.seek(SeekFrom::Start(header_start + header_size + data_size))?;
                headers.push(RarHeader::Service(ServiceHeader {
                    name: fh.filename,
                    data_size: fh.data_size,
                }));
                continue;
            }
            RAR4_END_ARCHIVE => {
                headers.push(RarHeader::EndArchive(EndArchiveHeader {
                    volume_number: None,
                }));
                break;
            }
            _ => {
                tracing::warn!(
                    "unknown RAR4 header type 0x{:02x} at offset {}",
                    header_type,
                    header_start
                );
            }
        }

        // Seek past any add_size data
        if add_size > 0 {
            reader.seek(SeekFrom::Start(header_start + header_size + add_size))?;
        }
    }

    Ok(headers)
}

/// Parse a RAR4 file/service header from the body bytes (after base 7-byte header).
fn parse_rar4_file_header(
    body: &[u8],
    flags: u16,
    header_start: u64,
    header_size: u64,
) -> Result<FileHeader> {
    let mut cursor = std::io::Cursor::new(body);

    let packed_size_low = read_u32_le(&mut cursor)? as u64;
    let unpacked_size_low = read_u32_le(&mut cursor)? as u64;

    let mut host_os_buf = [0u8; 1];
    cursor.read_exact(&mut host_os_buf)?;

    let _crc32 = read_u32_le(&mut cursor)?;
    let _datetime = read_u32_le(&mut cursor)?;

    let mut unpack_ver = [0u8; 1];
    cursor.read_exact(&mut unpack_ver)?;

    let mut method_buf = [0u8; 1];
    cursor.read_exact(&mut method_buf)?;
    let method = method_buf[0];

    let name_len = read_u16_le(&mut cursor)? as usize;
    let _attrs = read_u32_le(&mut cursor)?;

    // High parts of sizes if flag 0x0100 set
    let (packed_high, unpacked_high) = if flags & 0x0100 != 0 {
        let ph = read_u32_le(&mut cursor)? as u64;
        let uh = read_u32_le(&mut cursor)? as u64;
        (ph, uh)
    } else {
        (0, 0)
    };

    let packed_size = packed_size_low | (packed_high << 32);
    let unpacked_size = unpacked_size_low | (unpacked_high << 32);

    // Read filename
    let mut name_buf = vec![0u8; name_len];
    cursor.read_exact(&mut name_buf)?;
    let filename = String::from_utf8_lossy(&name_buf).into_owned();

    // Encryption: check flag 0x0004
    let is_encrypted = flags & 0x0004 != 0;
    let encryption = if is_encrypted && flags & 0x0200 != 0 {
        let mut salt = [0u8; 8];
        cursor.read_exact(&mut salt)?;
        Some(RarEncryption::Rar4 { salt })
    } else {
        None
    };

    let is_solid = flags & 0x0010 != 0;
    let is_directory = flags & 0x0020 != 0;

    // Map RAR4 method: 0x30 = store (m0)
    let compression_method = if method == 0x30 { 0 } else { method - 0x30 + 1 };

    let data_start_position = header_start + header_size;

    Ok(FileHeader {
        filename,
        uncompressed_size: unpacked_size,
        compressed_size: packed_size,
        compression_method,
        data_start_position,
        data_size: packed_size,
        is_directory,
        is_encrypted,
        is_solid,
        volume_number: None,
        encryption,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Build a minimal RAR4 archive: magic + archive header + file header + end archive.
    fn build_minimal_rar4() -> Vec<u8> {
        let mut out = Vec::new();

        // Magic
        out.extend_from_slice(RAR4_MAGIC);

        // --- Archive header (0x73) ---
        // body: 2 reserved + 4 reserved = 6 bytes
        // header size = 7 (base) + 6 (body) = 13
        {
            let header_type: u8 = RAR4_ARCHIVE;
            let flags: u16 = 0; // not a volume
            let size: u16 = 13;
            let body: [u8; 6] = [0; 6]; // reserved bytes

            // Build header for CRC: type(1) + flags(2) + size(2) + body(6) = 11 bytes
            let mut crc_data = Vec::new();
            crc_data.push(header_type);
            crc_data.extend_from_slice(&flags.to_le_bytes());
            crc_data.extend_from_slice(&size.to_le_bytes());
            crc_data.extend_from_slice(&body);

            let crc = crc32fast::hash(&crc_data) as u16; // RAR4 uses lower 16 bits
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&crc_data);
        }

        // --- File header (0x74) ---
        {
            let filename = b"test.txt";
            let packed_size: u32 = 5; // "hello"
            let unpacked_size: u32 = 5;
            let method: u8 = 0x30; // store
            let name_len: u16 = filename.len() as u16;

            // flags: LONG_BLOCK (0x8000) so add_size is present
            let flags: u16 = 0x8000;

            // Build body
            let mut body = Vec::new();
            body.extend_from_slice(&packed_size.to_le_bytes()); // packed_size_low
            body.extend_from_slice(&unpacked_size.to_le_bytes()); // unpacked_size_low
            body.push(0); // host_os
            body.extend_from_slice(&0u32.to_le_bytes()); // crc32
            body.extend_from_slice(&0u32.to_le_bytes()); // datetime
            body.push(29); // unpack_ver
            body.push(method); // method
            body.extend_from_slice(&name_len.to_le_bytes()); // name_len
            body.extend_from_slice(&0u32.to_le_bytes()); // attrs
            body.extend_from_slice(filename); // name

            let size: u16 = (7 + body.len()) as u16;

            // Build CRC data: type + flags + size + body
            let mut crc_data = Vec::new();
            crc_data.push(RAR4_FILE);
            crc_data.extend_from_slice(&flags.to_le_bytes());
            crc_data.extend_from_slice(&size.to_le_bytes());
            crc_data.extend_from_slice(&body);

            let crc = crc32fast::hash(&crc_data) as u16;
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&crc_data);

            // Data area
            out.extend_from_slice(b"hello");
        }

        // --- End archive header (0x7B) ---
        {
            let header_type: u8 = RAR4_END_ARCHIVE;
            let flags: u16 = 0;
            let size: u16 = 7; // base header only

            let mut crc_data = Vec::new();
            crc_data.push(header_type);
            crc_data.extend_from_slice(&flags.to_le_bytes());
            crc_data.extend_from_slice(&size.to_le_bytes());

            let crc = crc32fast::hash(&crc_data) as u16;
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&crc_data);
        }

        out
    }

    #[test]
    fn parse_minimal_rar4() {
        let data = build_minimal_rar4();
        let mut cursor = Cursor::new(&data[..]);
        let headers = parse_rar4(&mut cursor).unwrap();

        assert_eq!(headers.len(), 3);

        // Archive header
        match &headers[0] {
            RarHeader::Archive(ah) => {
                assert!(ah.is_first_volume);
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
                assert_eq!(fh.compression_method, 0); // store
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
}
