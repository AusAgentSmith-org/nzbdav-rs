use thiserror::Error;

#[derive(Error, Debug)]
pub enum DavError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("blob not found: {0}")]
    BlobNotFound(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("item not found: {0}")]
    ItemNotFound(String),

    #[error("duplicate item: {0}")]
    DuplicateItem(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, DavError>;
