//! On-demand Usenet streaming layer.
//!
//! Provides async streams that fetch articles from Usenet via `nzb-nntp`
//! connection pools and decode them via `nzb-decode`. No data is stored
//! locally — every read fetches from Usenet on demand.

pub mod aes_decoder_stream;
pub mod error;
pub mod multi_segment_stream;
pub mod multipart_file_stream;
pub mod nzb_file_stream;
pub mod prioritized_semaphore;
pub mod provider;
pub mod seekable_segment_stream;

pub use aes_decoder_stream::AesDecoderStream;
pub use error::{Result, StreamError};
pub use multi_segment_stream::MultiSegmentStream;
pub use multipart_file_stream::DavMultipartFileStream;
pub use nzb_file_stream::NzbFileStream;
pub use prioritized_semaphore::{PrioritizedPermit, PrioritizedSemaphore};
pub use provider::{UsenetArticleProvider, YencHeaders};
pub use seekable_segment_stream::SeekableSegmentStream;

// Re-export nzb_nntp so consumers can use the same ConnectionPool/ServerConfig types
pub use nzb_nntp;
