//! Pure Rust RAR4/RAR5 header-only parser.
//!
//! nzbdav only needs to read archive headers to discover contained files —
//! it never decompresses data (files must be stored with compression method m0).
//! Pure Rust implementation — no external archive libraries required.

pub mod crypto;
pub mod error;
pub mod header;
pub mod parser;
pub mod rar4;
pub mod rar5;

pub use error::{RarError, Result};
pub use header::{ArchiveHeader, EndArchiveHeader, FileHeader, RarHeader, ServiceHeader};
pub use parser::{parse_all_headers, parse_headers};

/// RAR4 magic bytes: `Rar!\x1a\x07\x00`
pub const RAR4_MAGIC: &[u8] = &[0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x00];

/// RAR5 magic bytes: `Rar!\x1a\x07\x01\x00`
pub const RAR5_MAGIC: &[u8] = &[0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x01, 0x00];

pub fn detect_version(header: &[u8]) -> Option<RarVersion> {
    if header.len() >= RAR5_MAGIC.len() && header[..RAR5_MAGIC.len()] == *RAR5_MAGIC {
        Some(RarVersion::Rar5)
    } else if header.len() >= RAR4_MAGIC.len() && header[..RAR4_MAGIC.len()] == *RAR4_MAGIC {
        Some(RarVersion::Rar4)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RarVersion {
    Rar4,
    Rar5,
}
