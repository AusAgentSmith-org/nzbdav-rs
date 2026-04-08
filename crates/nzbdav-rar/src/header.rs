//! RAR header types shared between RAR4 and RAR5 parsers.

#[derive(Debug, Clone)]
pub enum RarHeader {
    Archive(ArchiveHeader),
    File(FileHeader),
    EndArchive(EndArchiveHeader),
    Service(ServiceHeader),
}

#[derive(Debug, Clone)]
pub struct ArchiveHeader {
    pub is_first_volume: bool,
    pub volume_number: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct FileHeader {
    pub filename: String,
    pub uncompressed_size: u64,
    pub compressed_size: u64,
    pub compression_method: u8,
    /// Offset in the stream where file data begins.
    pub data_start_position: u64,
    /// Size of the data region (= compressed_size for m0/store).
    pub data_size: u64,
    pub is_directory: bool,
    pub is_encrypted: bool,
    pub is_solid: bool,
    pub volume_number: Option<i32>,
    pub encryption: Option<RarEncryption>,
}

#[derive(Debug, Clone)]
pub struct EndArchiveHeader {
    pub volume_number: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct ServiceHeader {
    pub name: String,
    pub data_size: u64,
}

#[derive(Debug, Clone)]
pub enum RarEncryption {
    Rar4 {
        salt: [u8; 8],
    },
    Rar5 {
        lg2_count: u8,
        salt: Vec<u8>,
        use_psw_check: bool,
        psw_check: Vec<u8>,
        iv: Vec<u8>,
    },
}
