use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::arr_client::ArrClient;
use crate::error::Result;

/// Action to take when a stuck item is detected.
#[derive(Debug, Clone)]
pub enum StuckAction {
    /// Remove from Arr queue.
    Remove,
    /// Remove and blocklist.
    RemoveAndBlocklist,
    /// Just log a warning.
    Warn,
}

/// An item in the Arr queue that appears to be stuck or errored.
#[derive(Debug)]
pub struct StuckItem {
    pub id: i32,
    pub title: String,
    pub error_message: Option<String>,
}

/// Monitors Arr instances for stuck queue items and performs configurable actions.
pub struct ArrMonitoringService {
    clients: Vec<ArrClient>,
    cancel: CancellationToken,
    check_interval: Duration,
    stuck_action: StuckAction,
}

impl ArrMonitoringService {
    pub fn new(clients: Vec<ArrClient>, cancel: CancellationToken) -> Self {
        Self {
            clients,
            cancel,
            check_interval: Duration::from_secs(300), // 5 minutes
            stuck_action: StuckAction::Warn,
        }
    }

    /// Override the default stuck action.
    pub fn with_stuck_action(mut self, action: StuckAction) -> Self {
        self.stuck_action = action;
        self
    }

    /// Run the monitoring loop until cancelled.
    pub async fn run(&self) {
        if self.clients.is_empty() {
            info!("no arr instances configured, monitoring disabled");
            return;
        }

        info!(
            interval_secs = self.check_interval.as_secs(),
            instances = self.clients.len(),
            "arr monitoring service started",
        );

        loop {
            if self.cancel.is_cancelled() {
                info!("arr monitoring service shutting down");
                return;
            }

            for client in &self.clients {
                match self.check_instance(client).await {
                    Ok(stuck) if stuck.is_empty() => {
                        debug!(instance = %client.name(), "no stuck items");
                    }
                    Ok(stuck) => {
                        warn!(
                            instance = %client.name(),
                            count = stuck.len(),
                            "found stuck items",
                        );
                        for item in &stuck {
                            self.handle_stuck_item(client, item).await;
                        }
                    }
                    Err(e) => {
                        error!(
                            instance = %client.name(),
                            error = %e,
                            "failed to check arr instance",
                        );
                    }
                }
            }

            tokio::select! {
                () = tokio::time::sleep(self.check_interval) => {}
                () = self.cancel.cancelled() => {
                    info!("arr monitoring service shutting down");
                    return;
                }
            }
        }
    }

    /// Check a single Arr instance for stuck items.
    async fn check_instance(&self, client: &ArrClient) -> Result<Vec<StuckItem>> {
        let queue = client.get_queue().await?;

        let stuck: Vec<StuckItem> = queue
            .records
            .into_iter()
            .filter(|item| {
                let has_warning = item
                    .tracked_download_status
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case("warning"));
                let has_error = item.error_message.is_some();
                has_warning || has_error
            })
            .map(|item| StuckItem {
                id: item.id,
                title: item.title,
                error_message: item.error_message,
            })
            .collect();

        Ok(stuck)
    }

    /// Handle a single stuck item based on the configured action.
    async fn handle_stuck_item(&self, client: &ArrClient, item: &StuckItem) {
        match self.stuck_action {
            StuckAction::Remove => {
                info!(
                    instance = %client.name(),
                    id = item.id,
                    title = %item.title,
                    "removing stuck item from queue",
                );
                if let Err(e) = client.delete_queue_item(item.id, false).await {
                    error!(id = item.id, error = %e, "failed to remove stuck item");
                }
            }
            StuckAction::RemoveAndBlocklist => {
                info!(
                    instance = %client.name(),
                    id = item.id,
                    title = %item.title,
                    "removing and blocklisting stuck item",
                );
                if let Err(e) = client.delete_queue_item(item.id, true).await {
                    error!(id = item.id, error = %e, "failed to remove+blocklist stuck item");
                }
            }
            StuckAction::Warn => {
                warn!(
                    instance = %client.name(),
                    id = item.id,
                    title = %item.title,
                    error = ?item.error_message,
                    "stuck item detected (warn-only mode)",
                );
            }
        }
    }
}
