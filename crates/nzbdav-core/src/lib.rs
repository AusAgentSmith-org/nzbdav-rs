pub mod database;
pub mod error;
pub mod models;
pub mod seed;
pub mod util;

// SQLite-specific modules (direct rusqlite access)
#[cfg(feature = "sqlite")]
pub mod blob_store;
#[cfg(feature = "sqlite")]
pub mod config;
#[cfg(feature = "sqlite")]
pub mod dav_items;
#[cfg(feature = "sqlite")]
pub mod db;
#[cfg(feature = "sqlite")]
pub mod history_items;
#[cfg(feature = "sqlite")]
pub mod queue_items;
#[cfg(feature = "sqlite")]
pub mod sqlite_db;
