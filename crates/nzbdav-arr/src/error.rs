use thiserror::Error;

#[derive(Error, Debug)]
pub enum ArrError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("arr instance unreachable: {0}")]
    Unreachable(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, ArrError>;
