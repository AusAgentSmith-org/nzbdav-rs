use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "nzbdav-rs",
    about = "Usenet virtual filesystem with WebDAV serving"
)]
pub struct Cli {
    /// Path to the SQLite database file
    #[arg(long, default_value = "nzbdav.db", env = "NZBDAV_DB")]
    pub db_path: String,

    /// HTTP listen address
    #[arg(long, default_value = "0.0.0.0", env = "NZBDAV_HOST")]
    pub host: String,

    /// HTTP listen port
    #[arg(long, default_value_t = 8080, env = "NZBDAV_PORT")]
    pub port: u16,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info", env = "NZBDAV_LOG")]
    pub log_level: String,

    /// API key for SABnzbd-compatible API
    #[arg(long, env = "NZBDAV_API_KEY")]
    pub api_key: Option<String>,

    /// WebDAV basic auth username
    #[arg(long, env = "NZBDAV_WEBDAV_USER")]
    pub webdav_user: Option<String>,

    /// WebDAV basic auth password
    #[arg(long, env = "NZBDAV_WEBDAV_PASS")]
    pub webdav_pass: Option<String>,
}
