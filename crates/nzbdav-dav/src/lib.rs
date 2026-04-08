//! WebDAV server crate for nzbdav-rs.
//!
//! Implements the WebDAV protocol subset needed for virtual filesystem serving:
//! PROPFIND, GET/HEAD (with Range), PUT, MKCOL, DELETE, MOVE, COPY, OPTIONS.
//!
//! Built as custom axum handlers rather than a full WebDAV framework,
//! since nzbdav only needs a focused subset of the protocol.

pub mod error;
pub mod handler;
pub mod propfind;
pub mod range;
pub mod router;
pub mod store;

pub use router::dav_router;
pub use store::DatabaseStore;
