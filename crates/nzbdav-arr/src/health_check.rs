use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use parking_lot::Mutex;
use rusqlite::Connection;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use nzbdav_core::blob_store::BlobStore;
use nzbdav_core::dav_items;
use nzbdav_core::models::{
    DavItem, DavMultipartFile, DavNzbFile, HealthCheckResult, HealthResult, ItemSubType, ItemType,
    RepairStatus,
};
use nzbdav_stream::UsenetArticleProvider;

use crate::error::Result;

/// Outcome of a single item health check.
#[derive(Debug)]
pub enum HealthCheckOutcome {
    Healthy,
    Unhealthy {
        missing_articles: usize,
        total_articles: usize,
    },
    Error(String),
}

/// Background service that verifies article existence on Usenet servers.
pub struct HealthCheckService {
    db: Arc<Mutex<Connection>>,
    provider: Arc<UsenetArticleProvider>,
    cancel: CancellationToken,
    /// How often to scan for items needing health checks.
    check_interval: Duration,
    /// Maximum articles to STAT per check cycle.
    max_articles_per_cycle: usize,
}

impl HealthCheckService {
    pub fn new(
        db: Arc<Mutex<Connection>>,
        provider: Arc<UsenetArticleProvider>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            db,
            provider,
            cancel,
            check_interval: Duration::from_secs(3600),
            max_articles_per_cycle: 500,
        }
    }

    /// Run the health check loop until cancelled.
    pub async fn run(&self) {
        info!(
            interval_secs = self.check_interval.as_secs(),
            max_articles = self.max_articles_per_cycle,
            "health check service started",
        );

        loop {
            if self.cancel.is_cancelled() {
                info!("health check service shutting down");
                return;
            }

            if let Err(e) = self.run_cycle().await {
                error!(error = %e, "health check cycle failed");
            }

            tokio::select! {
                () = tokio::time::sleep(self.check_interval) => {}
                () = self.cancel.cancelled() => {
                    info!("health check service shutting down");
                    return;
                }
            }
        }
    }

    /// Execute one check cycle: find items needing checks, STAT their articles.
    async fn run_cycle(&self) -> Result<()> {
        let items = self.get_items_needing_check();
        if items.is_empty() {
            debug!("no items need health checks");
            return Ok(());
        }

        info!(
            count = items.len(),
            "checking items for article availability"
        );

        for item in &items {
            if self.cancel.is_cancelled() {
                break;
            }

            let outcome = self.check_item(item).await;

            let now = Utc::now();
            let (health_result, message) = match &outcome {
                HealthCheckOutcome::Healthy => {
                    debug!(path = %item.path, "item healthy");
                    (HealthResult::Healthy, "All articles available".to_string())
                }
                HealthCheckOutcome::Unhealthy {
                    missing_articles,
                    total_articles,
                } => {
                    warn!(
                        path = %item.path,
                        missing = missing_articles,
                        total = total_articles,
                        "item unhealthy",
                    );
                    (
                        HealthResult::Unhealthy,
                        format!("{missing_articles}/{total_articles} articles missing"),
                    )
                }
                HealthCheckOutcome::Error(msg) => {
                    error!(path = %item.path, error = %msg, "health check error");
                    (HealthResult::Unhealthy, msg.clone())
                }
            };

            // Exponential backoff: healthy items checked less frequently over time.
            let check_count = self.consecutive_healthy_count(&outcome);
            let backoff_hours = 24i64 * (1 << check_count.min(4)); // 24h, 48h, 96h, ...max ~384h
            let next_check = now + chrono::Duration::hours(backoff_hours);

            // Update the dav_item timestamps.
            let db = self.db.lock();
            if let Err(e) = dav_items::update_health_check(&db, item.id, now, next_check) {
                error!(id = %item.id, error = %e, "failed to update health check timestamps");
            }

            // Insert a health check result record.
            let result = HealthCheckResult {
                id: Uuid::new_v4(),
                dav_item_id: item.id,
                path: item.path.clone(),
                created_at: now,
                result: health_result,
                repair_status: RepairStatus::None,
                message,
            };
            if let Err(e) = self.insert_health_check_result(&db, &result) {
                error!(id = %item.id, error = %e, "failed to insert health check result");
            }
        }

        Ok(())
    }

    /// Check a single DavItem's articles for availability.
    pub async fn check_item(&self, item: &DavItem) -> HealthCheckOutcome {
        let segment_ids = match self.get_segment_ids(item) {
            Ok(ids) => ids,
            Err(e) => return HealthCheckOutcome::Error(e.to_string()),
        };

        if segment_ids.is_empty() {
            return HealthCheckOutcome::Healthy;
        }

        let total = segment_ids.len();
        let to_check = segment_ids
            .into_iter()
            .take(self.max_articles_per_cycle)
            .collect::<Vec<_>>();

        let mut missing = 0usize;
        for seg_id in &to_check {
            match self.provider.stat(seg_id).await {
                Ok(true) => {}
                Ok(false) => missing += 1,
                Err(e) => {
                    return HealthCheckOutcome::Error(format!("STAT failed for {seg_id}: {e}"));
                }
            }
        }

        if missing == 0 {
            HealthCheckOutcome::Healthy
        } else {
            HealthCheckOutcome::Unhealthy {
                missing_articles: missing,
                total_articles: total,
            }
        }
    }

    /// Query items where `next_health_check <= now()` or is NULL, limited to UsenetFile types.
    fn get_items_needing_check(&self) -> Vec<DavItem> {
        let db = self.db.lock();
        let now = Utc::now().to_rfc3339();
        let sql = format!(
            "SELECT {} FROM dav_items WHERE type = ?1 AND (next_health_check IS NULL OR next_health_check <= ?2) LIMIT 100",
            "id, id_prefix, created_at, parent_id, name, file_size, type, sub_type, path, release_date, last_health_check, next_health_check, history_item_id, file_blob_id, nzb_blob_id"
        );
        let result = db.prepare(&sql).and_then(|mut stmt| {
            let rows =
                stmt.query_map(rusqlite::params![ItemType::UsenetFile as i32, now], |row| {
                    // Re-use the same row mapping pattern as dav_items module
                    let id: String = row.get("id")?;
                    let parent_id: Option<String> = row.get("parent_id")?;
                    let item_type_i: i32 = row.get("type")?;
                    let sub_type_i: i32 = row.get("sub_type")?;
                    let created_at_str: String = row.get("created_at")?;
                    let release_date_str: Option<String> = row.get("release_date")?;
                    let last_health_check_str: Option<String> = row.get("last_health_check")?;
                    let next_health_check_str: Option<String> = row.get("next_health_check")?;
                    let history_item_id: Option<String> = row.get("history_item_id")?;
                    let file_blob_id: Option<String> = row.get("file_blob_id")?;
                    let nzb_blob_id: Option<String> = row.get("nzb_blob_id")?;

                    let parse_uuid = |s: String| -> rusqlite::Result<uuid::Uuid> {
                        uuid::Uuid::parse_str(&s).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })
                    };

                    let parse_rfc3339 =
                        |s: String| -> rusqlite::Result<chrono::DateTime<chrono::Utc>> {
                            chrono::DateTime::parse_from_rfc3339(&s)
                                .map(|dt| dt.with_timezone(&chrono::Utc))
                                .map_err(|e| {
                                    rusqlite::Error::FromSqlConversionFailure(
                                        0,
                                        rusqlite::types::Type::Text,
                                        Box::new(e),
                                    )
                                })
                        };

                    Ok(DavItem {
                        id: parse_uuid(id)?,
                        id_prefix: row.get("id_prefix")?,
                        created_at: chrono::NaiveDateTime::parse_from_str(
                            &created_at_str,
                            "%Y-%m-%d %H:%M:%S",
                        )
                        .map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                        parent_id: parent_id.map(parse_uuid).transpose()?,
                        name: row.get("name")?,
                        file_size: row.get("file_size")?,
                        item_type: ItemType::try_from(item_type_i).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Integer,
                                e.into(),
                            )
                        })?,
                        sub_type: ItemSubType::try_from(sub_type_i).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Integer,
                                e.into(),
                            )
                        })?,
                        path: row.get("path")?,
                        release_date: release_date_str.map(parse_rfc3339).transpose()?,
                        last_health_check: last_health_check_str.map(parse_rfc3339).transpose()?,
                        next_health_check: next_health_check_str.map(parse_rfc3339).transpose()?,
                        history_item_id: history_item_id.map(parse_uuid).transpose()?,
                        file_blob_id: file_blob_id.map(parse_uuid).transpose()?,
                        nzb_blob_id: nzb_blob_id.map(parse_uuid).transpose()?,
                    })
                })?;
            let mut items = Vec::new();
            for row in rows {
                match row {
                    Ok(item) => items.push(item),
                    Err(e) => warn!(error = %e, "failed to parse dav_item row"),
                }
            }
            Ok(items)
        });

        match result {
            Ok(items) => items,
            Err(e) => {
                error!(error = %e, "failed to query items needing health check");
                Vec::new()
            }
        }
    }

    /// Extract segment IDs from an item's file blob.
    fn get_segment_ids(&self, item: &DavItem) -> std::result::Result<Vec<String>, String> {
        let blob_id = item
            .file_blob_id
            .ok_or_else(|| "item has no file_blob_id".to_string())?;

        let db = self.db.lock();
        let blob_data = BlobStore::get_file_blob(&db, blob_id)
            .map_err(|e| format!("failed to read blob {blob_id}: {e}"))?;

        match item.sub_type {
            ItemSubType::NzbFile => {
                let nzb_file: DavNzbFile = bincode::deserialize(&blob_data)
                    .map_err(|e| format!("failed to deserialize NzbFile blob: {e}"))?;
                Ok(nzb_file.segment_ids)
            }
            ItemSubType::MultipartFile => {
                let mp_file: DavMultipartFile = bincode::deserialize(&blob_data)
                    .map_err(|e| format!("failed to deserialize MultipartFile blob: {e}"))?;
                let ids: Vec<String> = mp_file
                    .file_parts
                    .into_iter()
                    .flat_map(|p| p.segment_ids)
                    .collect();
                Ok(ids)
            }
            ItemSubType::RarFile => {
                // RarFile uses the same blob format as NzbFile
                let nzb_file: DavNzbFile = bincode::deserialize(&blob_data)
                    .map_err(|e| format!("failed to deserialize RarFile blob: {e}"))?;
                Ok(nzb_file.segment_ids)
            }
            other => Err(format!("unsupported sub_type for health check: {other:?}")),
        }
    }

    /// Insert a health check result row.
    fn insert_health_check_result(
        &self,
        conn: &Connection,
        result: &HealthCheckResult,
    ) -> std::result::Result<(), String> {
        conn.execute(
            "INSERT INTO health_check_results (id, dav_item_id, path, created_at, result, repair_status, message) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                result.id.to_string(),
                result.dav_item_id.to_string(),
                result.path,
                result.created_at.to_rfc3339(),
                result.result as i32,
                result.repair_status as i32,
                result.message,
            ],
        )
        .map_err(|e| format!("SQL insert failed: {e}"))?;
        Ok(())
    }

    fn consecutive_healthy_count(&self, outcome: &HealthCheckOutcome) -> u32 {
        match outcome {
            HealthCheckOutcome::Healthy => 1,
            _ => 0,
        }
    }
}
