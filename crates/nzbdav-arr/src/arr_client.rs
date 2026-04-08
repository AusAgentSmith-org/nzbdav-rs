use reqwest::{Client, Method, RequestBuilder};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::error::{ArrError, Result};

/// Configuration for an Arr instance (Radarr or Sonarr).
#[derive(Debug, Clone)]
pub struct ArrConfig {
    pub name: String,
    pub base_url: String,
    pub api_key: String,
}

pub struct ArrClient {
    config: ArrConfig,
    client: Client,
}

#[derive(Debug, Deserialize)]
pub struct DownloadClient {
    pub id: i32,
    pub name: String,
    pub implementation: String,
    pub enable: bool,
}

#[derive(Debug, Deserialize)]
pub struct ArrQueueItem {
    pub id: i32,
    pub title: String,
    pub status: String,
    #[serde(rename = "downloadId")]
    pub download_id: Option<String>,
    #[serde(rename = "trackedDownloadStatus")]
    pub tracked_download_status: Option<String>,
    #[serde(rename = "trackedDownloadState")]
    pub tracked_download_state: Option<String>,
    #[serde(rename = "errorMessage")]
    pub error_message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ArrQueueResponse {
    pub records: Vec<ArrQueueItem>,
    #[serde(rename = "totalRecords")]
    pub total_records: i32,
}

#[derive(Debug, Serialize)]
struct CommandBody {
    name: String,
}

#[derive(Debug, Serialize)]
struct BulkDeleteBody {
    ids: Vec<i32>,
}

impl ArrClient {
    pub fn new(config: ArrConfig) -> Self {
        Self {
            config,
            client: Client::new(),
        }
    }

    /// The display name of this Arr instance.
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// GET /api/v3/downloadclient
    pub async fn get_download_clients(&self) -> Result<Vec<DownloadClient>> {
        let resp = self
            .request(Method::GET, "downloadclient")
            .send()
            .await
            .map_err(|e| ArrError::Unreachable(e.to_string()))?;

        let clients: Vec<DownloadClient> = resp.error_for_status()?.json().await?;
        debug!(
            instance = %self.config.name,
            count = clients.len(),
            "fetched download clients",
        );
        Ok(clients)
    }

    /// POST /api/v3/command with name: "RefreshMonitoredDownloads"
    pub async fn refresh_monitored_downloads(&self) -> Result<()> {
        let body = CommandBody {
            name: "RefreshMonitoredDownloads".to_string(),
        };
        let resp = self
            .request(Method::POST, "command")
            .json(&body)
            .send()
            .await
            .map_err(|e| ArrError::Unreachable(e.to_string()))?;

        resp.error_for_status()?;
        debug!(instance = %self.config.name, "refreshed monitored downloads");
        Ok(())
    }

    /// GET /api/v3/queue?page=1&pageSize=100
    pub async fn get_queue(&self) -> Result<ArrQueueResponse> {
        let resp = self
            .request(Method::GET, "queue")
            .query(&[("page", "1"), ("pageSize", "100")])
            .send()
            .await
            .map_err(|e| ArrError::Unreachable(e.to_string()))?;

        let queue: ArrQueueResponse = resp.error_for_status()?.json().await?;
        debug!(
            instance = %self.config.name,
            total = queue.total_records,
            "fetched queue",
        );
        Ok(queue)
    }

    /// DELETE /api/v3/queue/{id}?removeFromClient=true&blocklist={blocklist}
    pub async fn delete_queue_item(&self, id: i32, blocklist: bool) -> Result<()> {
        let path = format!("queue/{id}");
        let resp = self
            .request(Method::DELETE, &path)
            .query(&[
                ("removeFromClient", "true"),
                ("blocklist", if blocklist { "true" } else { "false" }),
            ])
            .send()
            .await
            .map_err(|e| ArrError::Unreachable(e.to_string()))?;

        resp.error_for_status()?;
        debug!(
            instance = %self.config.name,
            id,
            blocklist,
            "deleted queue item",
        );
        Ok(())
    }

    /// DELETE /api/v3/queue/bulk with ids
    pub async fn delete_queue_bulk(&self, ids: &[i32], blocklist: bool) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let body = BulkDeleteBody {
            ids: ids.to_vec(),
        };
        let resp = self
            .request(Method::DELETE, "queue/bulk")
            .query(&[
                ("removeFromClient", "true"),
                ("blocklist", if blocklist { "true" } else { "false" }),
            ])
            .json(&body)
            .send()
            .await
            .map_err(|e| ArrError::Unreachable(e.to_string()))?;

        resp.error_for_status()?;
        warn!(
            instance = %self.config.name,
            count = ids.len(),
            blocklist,
            "bulk-deleted queue items",
        );
        Ok(())
    }

    /// Build a [`RequestBuilder`] with the API key header and full URL.
    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let url = format!(
            "{}/api/v3/{}",
            self.config.base_url.trim_end_matches('/'),
            path,
        );
        self.client
            .request(method, &url)
            .header("X-Api-Key", &self.config.api_key)
    }
}
