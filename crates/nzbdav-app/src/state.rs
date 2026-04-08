use std::sync::Arc;

use nzbdav_core::config::ConfigManager;
use nzbdav_stream::provider::UsenetArticleProvider;
use parking_lot::Mutex;
use rusqlite::Connection;

/// Shared application state.
#[derive(Clone)]
#[allow(dead_code)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub config: ConfigManager,
    pub provider: Arc<UsenetArticleProvider>,
    pub version: &'static str,
}
