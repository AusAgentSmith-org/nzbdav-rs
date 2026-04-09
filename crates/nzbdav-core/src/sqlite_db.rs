//! `SqliteDavDatabase` — implements `DavDatabase` for SQLite via rusqlite.
//!
//! Wraps synchronous rusqlite calls in `spawn_blocking` to satisfy the async trait.

use std::sync::Arc;

use chrono::{DateTime, NaiveDateTime, Utc};
use parking_lot::Mutex;
use rusqlite::Connection;
use uuid::Uuid;

use crate::database::DavDatabase;
use crate::error::{DavError, Result};
use crate::models::{DavItem, HistoryItem, QueueItem};
use crate::{blob_store, dav_items, history_items, queue_items};

/// SQLite-backed implementation of `DavDatabase`.
///
/// Uses `Arc<Mutex<Connection>>` for thread-safe access and `spawn_blocking`
/// to avoid blocking the async runtime.
#[derive(Clone)]
pub struct SqliteDavDatabase {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteDavDatabase {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    /// Access the raw connection (for use by code that hasn't been migrated to the trait).
    pub fn connection(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }
}

/// Helper macro to run a synchronous closure on the connection via `spawn_blocking`.
macro_rules! blocking {
    ($self:expr, |$conn:ident| $body:expr) => {{
        let db = Arc::clone(&$self.conn);
        tokio::task::spawn_blocking(move || {
            let $conn = db.lock();
            $body
        })
        .await
        .map_err(|e| DavError::Other(format!("spawn_blocking join error: {e}")))?
    }};
}

#[async_trait::async_trait]
impl DavDatabase for SqliteDavDatabase {
    // ── DavItem ────────────────────────────────────────────────────────

    async fn insert_dav_item(&self, item: &DavItem) -> Result<()> {
        let item = item.clone();
        blocking!(self, |conn| dav_items::insert(&conn, &item))
    }

    async fn get_dav_item_by_id(&self, id: Uuid) -> Result<Option<DavItem>> {
        blocking!(self, |conn| dav_items::get_by_id(&conn, id))
    }

    async fn get_dav_item_by_path(&self, path: &str) -> Result<Option<DavItem>> {
        let path = path.to_string();
        blocking!(self, |conn| dav_items::get_by_path(&conn, &path))
    }

    async fn get_dav_children(&self, parent_id: Uuid) -> Result<Vec<DavItem>> {
        blocking!(self, |conn| dav_items::get_children(&conn, parent_id))
    }

    async fn get_dav_children_by_path(&self, parent_path: &str) -> Result<Vec<DavItem>> {
        let parent_path = parent_path.to_string();
        blocking!(self, |conn| dav_items::get_children_by_path(
            &conn,
            &parent_path
        ))
    }

    async fn delete_dav_item(&self, id: Uuid) -> Result<()> {
        blocking!(self, |conn| dav_items::delete(&conn, id))
    }

    async fn delete_dav_items_by_history(&self, history_item_id: Uuid) -> Result<()> {
        blocking!(self, |conn| dav_items::delete_by_history_item_id(
            &conn,
            history_item_id
        ))
    }

    async fn move_dav_item(
        &self,
        id: Uuid,
        new_name: &str,
        new_path: &str,
        new_parent_id: Uuid,
    ) -> Result<()> {
        let new_name = new_name.to_string();
        let new_path = new_path.to_string();
        blocking!(self, |conn| {
            conn.execute(
                "UPDATE dav_items SET name = ?1, path = ?2, parent_id = ?3 WHERE id = ?4",
                rusqlite::params![
                    new_name,
                    new_path,
                    new_parent_id.to_string(),
                    id.to_string()
                ],
            )
            .map_err(DavError::Sqlite)?;
            Ok(())
        })
    }

    async fn update_dav_health_check(
        &self,
        id: Uuid,
        last: DateTime<Utc>,
        next: DateTime<Utc>,
    ) -> Result<()> {
        blocking!(self, |conn| dav_items::update_health_check(
            &conn, id, last, next
        ))
    }

    // ── Blobs ──────────────────────────────────────────────────────────

    async fn get_file_blob(&self, id: Uuid) -> Result<Vec<u8>> {
        blocking!(self, |conn| blob_store::BlobStore::get_file_blob(&conn, id))
    }

    async fn put_file_blob(&self, id: Uuid, data: &[u8]) -> Result<()> {
        let data = data.to_vec();
        blocking!(self, |conn| blob_store::BlobStore::put_file_blob(
            &conn, id, &data
        ))
    }

    async fn get_nzb_blob(&self, id: Uuid) -> Result<Vec<u8>> {
        blocking!(self, |conn| blob_store::BlobStore::get_nzb_blob(&conn, id))
    }

    async fn put_nzb_blob(&self, id: Uuid, data: &[u8]) -> Result<()> {
        let data = data.to_vec();
        blocking!(self, |conn| blob_store::BlobStore::put_nzb_blob(
            &conn, id, &data
        ))
    }

    async fn delete_nzb_blob(&self, id: Uuid) -> Result<()> {
        blocking!(self, |conn| blob_store::BlobStore::delete(
            &conn,
            "nzb_blobs",
            id
        ))
    }

    // ── Queue ──────────────────────────────────────────────────────────

    async fn list_queue_items(&self) -> Result<Vec<QueueItem>> {
        blocking!(self, |conn| queue_items::list(&conn))
    }

    async fn get_next_queue_item(&self, exclude_ids: &[Uuid]) -> Result<Option<QueueItem>> {
        let exclude_ids = exclude_ids.to_vec();
        blocking!(self, |conn| queue_items::get_next_excluding(
            &conn,
            &exclude_ids
        ))
    }

    async fn insert_queue_item(&self, item: &QueueItem) -> Result<()> {
        let item = item.clone();
        blocking!(self, |conn| queue_items::insert(&conn, &item))
    }

    async fn delete_queue_item(&self, id: Uuid) -> Result<()> {
        blocking!(self, |conn| queue_items::delete(&conn, id))
    }

    async fn update_queue_pause_until(
        &self,
        id: Uuid,
        pause_until: Option<NaiveDateTime>,
    ) -> Result<()> {
        blocking!(self, |conn| queue_items::update_pause_until(
            &conn,
            id,
            pause_until
        ))
    }

    async fn count_queue_items(&self) -> Result<i64> {
        blocking!(self, |conn| queue_items::count(&conn))
    }

    // ── History ────────────────────────────────────────────────────────

    async fn insert_history_item(&self, item: &HistoryItem) -> Result<()> {
        let item = item.clone();
        blocking!(self, |conn| history_items::insert(&conn, &item))
    }

    async fn list_history_items(&self, offset: i64, limit: i64) -> Result<Vec<HistoryItem>> {
        blocking!(self, |conn| history_items::list(&conn, offset, limit))
    }

    async fn delete_history_item(&self, id: Uuid) -> Result<()> {
        blocking!(self, |conn| history_items::delete(&conn, id))
    }

    async fn delete_all_history_items(&self) -> Result<()> {
        blocking!(self, |conn| history_items::delete_all(&conn))
    }

    async fn count_history_items(&self) -> Result<i64> {
        blocking!(self, |conn| history_items::count(&conn))
    }

    // ── Config ─────────────────────────────────────────────────────────

    async fn load_config_items(&self) -> Result<Vec<(String, String)>> {
        blocking!(self, |conn| {
            let mut stmt = conn
                .prepare("SELECT key, value FROM config_items")
                .map_err(DavError::Sqlite)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(DavError::Sqlite)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row.map_err(DavError::Sqlite)?);
            }
            Ok(items)
        })
    }

    async fn set_config_item(&self, key: &str, value: &str) -> Result<()> {
        let key = key.to_string();
        let value = value.to_string();
        blocking!(self, |conn| {
            conn.execute(
                "INSERT INTO config_items (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                rusqlite::params![key, value],
            )
            .map_err(DavError::Sqlite)?;
            Ok(())
        })
    }
}
