use reqwest::Client;
use tracing::debug;

use crate::error::{ArrError, Result};

/// Client for the rclone RC (remote control) HTTP API.
pub struct RcloneClient {
    client: Client,
    base_url: String,
}

impl RcloneClient {
    pub fn new(base_url: String) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Tell rclone to forget a specific path from its VFS cache.
    /// POST /vfs/forget with dir parameter.
    pub async fn vfs_forget(&self, path: &str) -> Result<()> {
        let url = format!("{}/vfs/forget", self.base_url);
        let resp = self
            .client
            .post(&url)
            .query(&[("dir", path)])
            .send()
            .await
            .map_err(|e| ArrError::Unreachable(format!("rclone unreachable: {e}")))?;

        resp.error_for_status()?;
        debug!(path, "rclone vfs/forget completed");
        Ok(())
    }

    /// Refresh the VFS for a specific directory.
    /// POST /vfs/refresh with dir parameter.
    pub async fn vfs_refresh(&self, path: &str) -> Result<()> {
        let url = format!("{}/vfs/refresh", self.base_url);
        let resp = self
            .client
            .post(&url)
            .query(&[("dir", path)])
            .send()
            .await
            .map_err(|e| ArrError::Unreachable(format!("rclone unreachable: {e}")))?;

        resp.error_for_status()?;
        debug!(path, "rclone vfs/refresh completed");
        Ok(())
    }
}
