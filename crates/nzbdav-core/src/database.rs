use chrono::{DateTime, NaiveDateTime, Utc};
use uuid::Uuid;

use crate::error::Result;
use crate::models::{DavItem, HistoryItem, QueueItem};

/// Database abstraction for nzbdav storage.
///
/// Implementations exist for SQLite (standalone, behind `sqlite` feature)
/// and PostgreSQL (StackArr integration, in stackarr-core).
#[async_trait::async_trait]
pub trait DavDatabase: Send + Sync {
    // ── DavItem operations ─────────────────────────────────────────────

    /// Insert or replace a DAV item.
    async fn insert_dav_item(&self, item: &DavItem) -> Result<()>;

    /// Look up a DAV item by its UUID.
    async fn get_dav_item_by_id(&self, id: Uuid) -> Result<Option<DavItem>>;

    /// Look up a DAV item by its virtual filesystem path.
    async fn get_dav_item_by_path(&self, path: &str) -> Result<Option<DavItem>>;

    /// List direct children of a parent item.
    async fn get_dav_children(&self, parent_id: Uuid) -> Result<Vec<DavItem>>;

    /// List direct children by parent path (joins on parent).
    async fn get_dav_children_by_path(&self, parent_path: &str) -> Result<Vec<DavItem>>;

    /// Delete a DAV item (children cascade).
    async fn delete_dav_item(&self, id: Uuid) -> Result<()>;

    /// Delete all DAV items linked to a history item.
    async fn delete_dav_items_by_history(&self, history_item_id: Uuid) -> Result<()>;

    /// Move/rename a DAV item to a new path and parent.
    async fn move_dav_item(
        &self,
        id: Uuid,
        new_name: &str,
        new_path: &str,
        new_parent_id: Uuid,
    ) -> Result<()>;

    /// Update health check timestamps on a DAV item.
    async fn update_dav_health_check(
        &self,
        id: Uuid,
        last: DateTime<Utc>,
        next: DateTime<Utc>,
    ) -> Result<()>;

    // ── Blob operations ────────────────────────────────────────────────

    /// Retrieve file metadata blob (DavMultipartFile / DavNzbFile serialized as bincode).
    async fn get_file_blob(&self, id: Uuid) -> Result<Vec<u8>>;

    /// Store file metadata blob.
    async fn put_file_blob(&self, id: Uuid, data: &[u8]) -> Result<()>;

    /// Retrieve raw NZB XML blob.
    async fn get_nzb_blob(&self, id: Uuid) -> Result<Vec<u8>>;

    /// Store raw NZB XML blob.
    async fn put_nzb_blob(&self, id: Uuid, data: &[u8]) -> Result<()>;

    /// Delete a raw NZB XML blob.
    async fn delete_nzb_blob(&self, id: Uuid) -> Result<()>;

    // ── Queue operations ───────────────────────────────────────────────

    /// List all queue items ordered by priority DESC, created_at ASC.
    async fn list_queue_items(&self) -> Result<Vec<QueueItem>>;

    /// Get the next eligible queue item, excluding the given IDs.
    async fn get_next_queue_item(&self, exclude_ids: &[Uuid]) -> Result<Option<QueueItem>>;

    /// Insert a queue item.
    async fn insert_queue_item(&self, item: &QueueItem) -> Result<()>;

    /// Delete a queue item.
    async fn delete_queue_item(&self, id: Uuid) -> Result<()>;

    /// Update the pause_until timestamp on a queue item.
    async fn update_queue_pause_until(
        &self,
        id: Uuid,
        pause_until: Option<NaiveDateTime>,
    ) -> Result<()>;

    /// Count total queue items.
    async fn count_queue_items(&self) -> Result<i64>;

    // ── History operations ─────────────────────────────────────────────

    /// Insert a history item.
    async fn insert_history_item(&self, item: &HistoryItem) -> Result<()>;

    /// List history items, newest first.
    async fn list_history_items(&self, offset: i64, limit: i64) -> Result<Vec<HistoryItem>>;

    /// Delete a history item.
    async fn delete_history_item(&self, id: Uuid) -> Result<()>;

    /// Delete all history items.
    async fn delete_all_history_items(&self) -> Result<()>;

    /// Count total history items.
    async fn count_history_items(&self) -> Result<i64>;

    // ── Config operations ──────────────────────────────────────────────

    /// Load all config key-value pairs.
    async fn load_config_items(&self) -> Result<Vec<(String, String)>>;

    /// Set a config value (upsert).
    async fn set_config_item(&self, key: &str, value: &str) -> Result<()>;
}
