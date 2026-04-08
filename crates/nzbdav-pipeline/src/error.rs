use thiserror::Error;

#[derive(Error, Debug)]
pub enum PipelineError {
    #[error("unsupported RAR compression method (only m0/store supported)")]
    UnsupportedRarCompression,

    #[error("unsupported 7z compression method")]
    Unsupported7zCompression,

    #[error("password-protected 7z archives are not supported")]
    PasswordProtected7z,

    #[error("could not determine part number for RAR file")]
    UnknownRarPartNumber,

    #[error("article not found: {0}")]
    ArticleNotFound(String),

    #[error("non-retryable download error: {0}")]
    NonRetryable(String),

    #[error("retryable download error: {0}")]
    Retryable(String),

    #[error("no importable video file found")]
    NoImportableVideo,

    #[error(transparent)]
    Rar(#[from] nzbdav_rar::error::RarError),

    #[error(transparent)]
    Stream(#[from] nzbdav_stream::error::StreamError),

    #[error(transparent)]
    Core(#[from] nzbdav_core::error::DavError),

    #[error("{0}")]
    Other(String),
}

impl PipelineError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Retryable(_))
    }
}

pub type Result<T> = std::result::Result<T, PipelineError>;
