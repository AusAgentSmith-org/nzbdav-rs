use thiserror::Error;

#[derive(Error, Debug)]
pub enum RarError {
    #[error("RAR signature not found")]
    SignatureNotFound,

    #[error("unsupported RAR compression method: {0} (only m0/store is supported)")]
    UnsupportedCompressionMethod(u8),

    #[error("password-protected RAR archives cannot be solid")]
    EncryptedSolidArchive,

    #[error("archive has encrypted headers — password required to read file list")]
    EncryptedHeaders,

    #[error("incorrect password for encrypted archive")]
    IncorrectPassword,

    #[error("unsupported encryption version: {0}")]
    UnsupportedEncryptionVersion(u64),

    #[error("decryption failed: {0}")]
    DecryptionError(String),

    #[error("truncated header at offset {0}")]
    TruncatedHeader(u64),

    #[error("invalid header CRC at offset {0}")]
    InvalidHeaderCrc(u64),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, RarError>;
