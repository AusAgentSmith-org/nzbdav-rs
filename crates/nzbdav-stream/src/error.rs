use thiserror::Error;

#[derive(Error, Debug)]
pub enum StreamError {
    #[error("article not found: {0}")]
    ArticleNotFound(String),

    #[error("all servers exhausted for article: {0}")]
    AllServersExhausted(String),

    #[error("yenc decode error: {0}")]
    YencDecode(String),

    #[error("nntp error: {0}")]
    NntpError(String),

    #[error("channel closed")]
    ChannelClosed,

    #[error("seek position not found: offset {0}")]
    SeekPositionNotFound(i64),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("decryption error: {0}")]
    Decryption(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, StreamError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let e = StreamError::ArticleNotFound("abc123".into());
        assert_eq!(e.to_string(), "article not found: abc123");

        let e = StreamError::AllServersExhausted("abc123".into());
        assert_eq!(e.to_string(), "all servers exhausted for article: abc123");

        let e = StreamError::YencDecode("bad crc".into());
        assert_eq!(e.to_string(), "yenc decode error: bad crc");

        let e = StreamError::NntpError("timeout".into());
        assert_eq!(e.to_string(), "nntp error: timeout");

        let e = StreamError::ChannelClosed;
        assert_eq!(e.to_string(), "channel closed");

        let e = StreamError::SeekPositionNotFound(42);
        assert_eq!(e.to_string(), "seek position not found: offset 42");

        let e = StreamError::Decryption("bad key".into());
        assert_eq!(e.to_string(), "decryption error: bad key");

        let e = StreamError::Other("something".into());
        assert_eq!(e.to_string(), "something");
    }

    #[test]
    fn test_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let stream_err: StreamError = io_err.into();
        assert!(matches!(stream_err, StreamError::Io(_)));
        assert!(stream_err.to_string().contains("file missing"));
    }
}
