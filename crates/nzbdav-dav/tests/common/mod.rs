//! Test helpers: in-memory mock `DavDatabase` and router builder.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use chrono::{DateTime, NaiveDateTime, Utc};
use http_body_util::BodyExt;
use tokio::sync::RwLock;
use uuid::Uuid;

use nzbdav_core::database::DavDatabase;
use nzbdav_core::error::{DavError, Result};
use nzbdav_core::models::{DavItem, HistoryItem, ItemSubType, ItemType, QueueItem};
use nzbdav_dav::DatabaseStore;

// ---------------------------------------------------------------------------
// MockDavDatabase
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Inner {
    pub items: HashMap<String, DavItem>,     // path -> DavItem
    pub items_by_id: HashMap<Uuid, DavItem>, // id -> DavItem
    pub nzb_blobs: HashMap<Uuid, Vec<u8>>,   // blob_id -> data
    pub file_blobs: HashMap<Uuid, Vec<u8>>,  // blob_id -> data
    pub queue: Vec<QueueItem>,
}

pub struct MockDavDatabase {
    pub inner: RwLock<Inner>,
}

impl MockDavDatabase {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
        }
    }

    /// Seed a DavItem into the mock.
    pub async fn seed_item(&self, item: DavItem) {
        let mut inner = self.inner.write().await;
        inner.items.insert(item.path.clone(), item.clone());
        inner.items_by_id.insert(item.id, item);
    }

    /// Seed an NZB blob.
    pub async fn seed_nzb_blob(&self, id: Uuid, data: Vec<u8>) {
        self.inner.write().await.nzb_blobs.insert(id, data);
    }

    /// Seed a queue item.
    pub async fn seed_queue_item(&self, qi: QueueItem) {
        self.inner.write().await.queue.push(qi);
    }

    /// Check if an item exists at path.
    pub async fn has_item(&self, path: &str) -> bool {
        self.inner.read().await.items.contains_key(path)
    }

    /// Get item count.
    pub async fn item_count(&self) -> usize {
        self.inner.read().await.items.len()
    }
}

#[async_trait]
impl DavDatabase for MockDavDatabase {
    // -- DavItem operations --

    async fn insert_dav_item(&self, item: &DavItem) -> Result<()> {
        let mut inner = self.inner.write().await;
        inner.items.insert(item.path.clone(), item.clone());
        inner.items_by_id.insert(item.id, item.clone());
        Ok(())
    }

    async fn get_dav_item_by_id(&self, id: Uuid) -> Result<Option<DavItem>> {
        Ok(self.inner.read().await.items_by_id.get(&id).cloned())
    }

    async fn get_dav_item_by_path(&self, path: &str) -> Result<Option<DavItem>> {
        Ok(self.inner.read().await.items.get(path).cloned())
    }

    async fn get_dav_children(&self, parent_id: Uuid) -> Result<Vec<DavItem>> {
        Ok(self
            .inner
            .read()
            .await
            .items
            .values()
            .filter(|i| i.parent_id == Some(parent_id))
            .cloned()
            .collect())
    }

    async fn get_dav_children_by_path(&self, parent_path: &str) -> Result<Vec<DavItem>> {
        let inner = self.inner.read().await;
        let parent = inner.items.get(parent_path);
        let Some(parent) = parent else {
            return Ok(vec![]);
        };
        let pid = parent.id;
        Ok(inner
            .items
            .values()
            .filter(|i| i.parent_id == Some(pid))
            .cloned()
            .collect())
    }

    async fn delete_dav_item(&self, id: Uuid) -> Result<()> {
        let mut inner = self.inner.write().await;
        if let Some(item) = inner.items_by_id.remove(&id) {
            inner.items.remove(&item.path);
        }
        // Cascade: remove children
        let child_ids: Vec<Uuid> = inner
            .items
            .values()
            .filter(|i| i.parent_id == Some(id))
            .map(|i| i.id)
            .collect();
        for cid in child_ids {
            if let Some(child) = inner.items_by_id.remove(&cid) {
                inner.items.remove(&child.path);
            }
        }
        Ok(())
    }

    async fn delete_dav_items_by_history(&self, _history_item_id: Uuid) -> Result<()> {
        Ok(())
    }

    async fn move_dav_item(
        &self,
        id: Uuid,
        new_name: &str,
        new_path: &str,
        new_parent_id: Uuid,
    ) -> Result<()> {
        let mut inner = self.inner.write().await;
        if let Some(mut item) = inner.items_by_id.remove(&id) {
            inner.items.remove(&item.path);
            item.name = new_name.to_string();
            item.path = new_path.to_string();
            item.parent_id = Some(new_parent_id);
            inner.items.insert(item.path.clone(), item.clone());
            inner.items_by_id.insert(id, item);
        }
        Ok(())
    }

    async fn update_dav_health_check(
        &self,
        _id: Uuid,
        _last: DateTime<Utc>,
        _next: DateTime<Utc>,
    ) -> Result<()> {
        Ok(())
    }

    // -- Blob operations --

    async fn get_file_blob(&self, id: Uuid) -> Result<Vec<u8>> {
        self.inner
            .read()
            .await
            .file_blobs
            .get(&id)
            .cloned()
            .ok_or_else(|| DavError::BlobNotFound(id.to_string()))
    }

    async fn put_file_blob(&self, id: Uuid, data: &[u8]) -> Result<()> {
        self.inner
            .write()
            .await
            .file_blobs
            .insert(id, data.to_vec());
        Ok(())
    }

    async fn get_nzb_blob(&self, id: Uuid) -> Result<Vec<u8>> {
        self.inner
            .read()
            .await
            .nzb_blobs
            .get(&id)
            .cloned()
            .ok_or_else(|| DavError::BlobNotFound(id.to_string()))
    }

    async fn put_nzb_blob(&self, id: Uuid, data: &[u8]) -> Result<()> {
        self.inner.write().await.nzb_blobs.insert(id, data.to_vec());
        Ok(())
    }

    async fn delete_nzb_blob(&self, id: Uuid) -> Result<()> {
        self.inner.write().await.nzb_blobs.remove(&id);
        Ok(())
    }

    // -- Queue operations --

    async fn list_queue_items(&self) -> Result<Vec<QueueItem>> {
        Ok(self.inner.read().await.queue.clone())
    }

    async fn get_next_queue_item(&self, _exclude_ids: &[Uuid]) -> Result<Option<QueueItem>> {
        Ok(self.inner.read().await.queue.first().cloned())
    }

    async fn insert_queue_item(&self, item: &QueueItem) -> Result<()> {
        self.inner.write().await.queue.push(item.clone());
        Ok(())
    }

    async fn delete_queue_item(&self, id: Uuid) -> Result<()> {
        self.inner.write().await.queue.retain(|qi| qi.id != id);
        Ok(())
    }

    async fn update_queue_pause_until(
        &self,
        _id: Uuid,
        _pause_until: Option<NaiveDateTime>,
    ) -> Result<()> {
        Ok(())
    }

    async fn count_queue_items(&self) -> Result<i64> {
        Ok(self.inner.read().await.queue.len() as i64)
    }

    // -- History operations --

    async fn insert_history_item(&self, _item: &HistoryItem) -> Result<()> {
        Ok(())
    }

    async fn list_history_items(&self, _offset: i64, _limit: i64) -> Result<Vec<HistoryItem>> {
        Ok(vec![])
    }

    async fn delete_history_item(&self, _id: Uuid) -> Result<()> {
        Ok(())
    }

    async fn delete_all_history_items(&self) -> Result<()> {
        Ok(())
    }

    async fn count_history_items(&self) -> Result<i64> {
        Ok(0)
    }

    // -- Config operations --

    async fn load_config_items(&self) -> Result<Vec<(String, String)>> {
        Ok(vec![])
    }

    async fn set_config_item(&self, _key: &str, _value: &str) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Test item builders
// ---------------------------------------------------------------------------

pub fn make_root_item() -> DavItem {
    let id = Uuid::new_v4();
    DavItem {
        id,
        id_prefix: "/".into(),
        created_at: Utc::now().naive_utc(),
        parent_id: None,
        name: "root".into(),
        file_size: None,
        item_type: ItemType::Directory,
        sub_type: ItemSubType::Directory,
        path: "/".into(),
        release_date: None,
        last_health_check: None,
        next_health_check: None,
        history_item_id: None,
        file_blob_id: None,
        nzb_blob_id: None,
    }
}

pub fn make_dir_item(name: &str, path: &str, parent_id: Uuid) -> DavItem {
    DavItem {
        id: Uuid::new_v4(),
        id_prefix: name.chars().next().unwrap_or('_').to_string(),
        created_at: Utc::now().naive_utc(),
        parent_id: Some(parent_id),
        name: name.into(),
        file_size: None,
        item_type: ItemType::Directory,
        sub_type: ItemSubType::Directory,
        path: path.into(),
        release_date: None,
        last_health_check: None,
        next_health_check: None,
        history_item_id: None,
        file_blob_id: None,
        nzb_blob_id: None,
    }
}

pub fn make_nzb_file_item(
    name: &str,
    path: &str,
    parent_id: Uuid,
    size: i64,
    nzb_blob_id: Uuid,
) -> DavItem {
    DavItem {
        id: Uuid::new_v4(),
        id_prefix: name.chars().next().unwrap_or('_').to_string(),
        created_at: Utc::now().naive_utc(),
        parent_id: Some(parent_id),
        name: name.into(),
        file_size: Some(size),
        item_type: ItemType::UsenetFile,
        sub_type: ItemSubType::NzbFile,
        path: path.into(),
        release_date: None,
        last_health_check: None,
        next_health_check: None,
        history_item_id: None,
        file_blob_id: None,
        nzb_blob_id: Some(nzb_blob_id),
    }
}

// ---------------------------------------------------------------------------
// Router builder + request helper
// ---------------------------------------------------------------------------

pub fn test_router(db: MockDavDatabase) -> Router {
    let provider = Arc::new(nzbdav_stream::UsenetArticleProvider::new(vec![]));
    let store = Arc::new(DatabaseStore::new(Arc::new(db), provider, 0));
    nzbdav_dav::dav_router(store)
}

/// Send a request to the router and return (status, headers, body bytes).
pub async fn send_dav(
    router: &Router,
    method: &str,
    path: &str,
    headers: Vec<(&str, &str)>,
    body: Option<&[u8]>,
) -> (http::StatusCode, http::HeaderMap, Vec<u8>) {
    use tower::ServiceExt;

    let mut builder = http::Request::builder().method(method).uri(path);
    for (k, v) in headers {
        builder = builder.header(k, v);
    }
    let req = if let Some(b) = body {
        builder.body(axum::body::Body::from(b.to_vec())).unwrap()
    } else {
        builder.body(axum::body::Body::empty()).unwrap()
    };

    let response = router.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body_bytes = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, headers, body_bytes)
}
