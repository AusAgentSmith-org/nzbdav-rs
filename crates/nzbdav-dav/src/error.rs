use thiserror::Error;

#[derive(Error, Debug)]
pub enum DavServerError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("method not allowed: {0}")]
    MethodNotAllowed(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("precondition failed: {0}")]
    PreconditionFailed(String),

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("invalid range: {0}")]
    InvalidRange(String),

    #[error(transparent)]
    Stream(#[from] nzbdav_stream::error::StreamError),

    #[error(transparent)]
    Core(#[from] nzbdav_core::error::DavError),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, DavServerError>;
