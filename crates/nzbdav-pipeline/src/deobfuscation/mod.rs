//! Deobfuscation pipeline — resolve real filenames for NZB files.
//!
//! Many Usenet posts use obfuscated filenames (hashes, UUIDs) in the NZB
//! subject line. The deobfuscation pipeline resolves real filenames by:
//!
//! 1. Fetching the first segment of each file to detect file types and compute
//!    a 16KB MD5 hash.
//! 2. Finding and parsing the smallest PAR2 index file for its file descriptors
//!    (which contain real filenames and 16KB hashes).
//! 3. Matching NZB files to PAR2 descriptors by comparing 16KB MD5 hashes.

pub mod fetch_first_segments;
pub mod get_file_infos;
pub mod get_par2_file_descriptors;

/// Info about an NZB file after first-segment analysis.
#[derive(Debug, Clone)]
pub struct NzbFileInfo {
    /// Original NZB file index.
    pub file_index: usize,
    /// Filename from NZB subject.
    pub subject_name: String,
    /// Filename from yEnc header (first segment).
    pub yenc_name: Option<String>,
    /// Real filename (from PAR2 match, or best guess).
    pub resolved_name: String,
    /// Total file size in bytes.
    pub file_size: u64,
    /// Segment message IDs in order.
    pub segment_ids: Vec<String>,
    /// Is this a RAR file? (detected from magic bytes).
    pub is_rar: bool,
    /// Is this a PAR2 file?
    pub is_par2: bool,
    /// First 16KB of file data (for MD5 matching).
    pub first_16k: Option<Vec<u8>>,
    /// MD5 of first 16KB.
    pub hash_16k: Option<[u8; 16]>,
}
