use nzbdav_core::models::FilePart;

/// Result of processing a single file within an NZB.
#[derive(Debug, Clone)]
pub struct ProcessedFile {
    /// Filename (from RAR header, or NZB filename for plain files).
    pub filename: String,
    /// Uncompressed file size.
    pub file_size: u64,
    /// Is this a directory entry?
    pub is_directory: bool,
    /// Source: which NZB file this came from.
    pub source_file_index: usize,
    /// For RAR files: the archive volume number.
    pub volume_number: Option<i32>,
    /// File parts (segment IDs + byte ranges for streaming).
    pub file_parts: Vec<FilePart>,
    /// Is the archive encrypted?
    pub is_encrypted: bool,
    /// Encryption params if encrypted.
    pub encryption: Option<nzbdav_rar::header::RarEncryption>,
}

// Suppress unused field warnings — these types are part of the public API
// consumed by later pipeline stages.
const _: () = {
    fn _assert_fields(f: &ProcessedFile) {
        let _ = &f.filename;
        let _ = &f.file_size;
        let _ = &f.is_directory;
        let _ = &f.source_file_index;
        let _ = &f.volume_number;
        let _ = &f.file_parts;
        let _ = &f.is_encrypted;
        let _ = &f.encryption;
    }
};

