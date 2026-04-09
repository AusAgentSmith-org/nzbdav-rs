//! `DatabaseStore` — concrete WebDAV storage backed by SQLite + Usenet streaming.

use std::sync::Arc;

use axum::body::Body;
use parking_lot::Mutex;
use rusqlite::Connection;
use tokio_util::io::ReaderStream;
use tracing::debug;
use uuid::Uuid;

use nzbdav_core::blob_store::BlobStore;
use nzbdav_core::models::{DavItem, DavMultipartFile, DavNzbFile, ItemSubType, ItemType};
use nzbdav_core::{dav_items, dav_items::get_by_path, queue_items};
use nzbdav_stream::{
    AesDecoderStream, DavMultipartFileStream, NzbFileStream, UsenetArticleProvider,
};

use crate::error::{DavServerError, Result};

/// A node in the WebDAV virtual filesystem.
#[derive(Debug, Clone)]
pub struct DavNode {
    pub item: DavItem,
    pub is_collection: bool,
    pub content_type: String,
    pub etag: String,
}

impl DavNode {
    pub fn from_item(item: DavItem) -> Self {
        let is_collection = item.item_type == ItemType::Directory;
        let content_type = if is_collection {
            "httpd/unix-directory".to_string()
        } else {
            mime_from_name(&item.name)
        };
        let etag = format!("\"{}\"", item.id);
        Self {
            item,
            is_collection,
            content_type,
            etag,
        }
    }
}

/// Guess a MIME type from a filename.
fn mime_from_name(name: &str) -> String {
    mime_guess::from_path(name)
        .first_or_octet_stream()
        .to_string()
}

/// WebDAV storage backed by SQLite for metadata and Usenet for file content.
pub struct DatabaseStore {
    db: Arc<Mutex<Connection>>,
    provider: Arc<UsenetArticleProvider>,
    lookahead: usize,
}

impl DatabaseStore {
    pub fn new(
        db: Arc<Mutex<Connection>>,
        provider: Arc<UsenetArticleProvider>,
        lookahead: usize,
    ) -> Self {
        Self {
            db,
            provider,
            lookahead,
        }
    }

    /// Access the database connection (locked).
    pub fn db_conn(&self) -> parking_lot::MutexGuard<'_, Connection> {
        self.db.lock()
    }

    /// Access the Usenet article provider.
    pub fn provider(&self) -> Arc<UsenetArticleProvider> {
        Arc::clone(&self.provider)
    }

    /// Lookahead depth for prefetching.
    pub fn lookahead(&self) -> usize {
        self.lookahead
    }

    /// Resolve a WebDAV path to a `DavNode`.
    pub fn get_node(&self, path: &str) -> Result<Option<DavNode>> {
        let conn = self.db.lock();

        // Check real items first.
        if let Some(item) = get_by_path(&conn, path)? {
            return Ok(Some(DavNode::from_item(item)));
        }

        // Try with trailing slash (directories in DB have trailing slash).
        if !path.ends_with('/') {
            let with_slash = format!("{path}/");
            if let Some(item) = get_by_path(&conn, &with_slash)? {
                return Ok(Some(DavNode::from_item(item)));
            }
        }

        // Virtual queue items under /nzbs/<filename>
        if let Some(filename) = path.strip_prefix("/nzbs/")
            && !filename.is_empty()
            && !filename.contains('/')
        {
            let queue = queue_items::list(&conn)?;
            if let Some(qi) = queue.into_iter().find(|qi| qi.file_name == filename) {
                return Ok(Some(DavNode::from_item(queue_item_to_dav_item(&qi))));
            }
        }

        Ok(None)
    }

    /// List the immediate children of a collection path.
    pub fn list_children(&self, path: &str) -> Result<Vec<DavNode>> {
        let conn = self.db.lock();
        // Normalize: try with trailing slash if not present
        let normalized = if path.ends_with('/') || path == "/" {
            path.to_string()
        } else {
            format!("{path}/")
        };
        let mut children: Vec<DavNode> = dav_items::get_children_by_path(&conn, &normalized)?
            .into_iter()
            .map(DavNode::from_item)
            .collect();

        // /nzbs/ additionally shows queue items as virtual NZB files.
        let nzbs_check = normalized.trim_end_matches('/');
        if nzbs_check == "/nzbs" {
            let queue = queue_items::list(&conn)?;
            for qi in &queue {
                children.push(DavNode::from_item(queue_item_to_dav_item(qi)));
            }
        }

        Ok(children)
    }

    /// Build a streaming `Body` for a file item.
    ///
    /// Deserializes the file blob metadata and constructs the appropriate
    /// Usenet stream (`DavMultipartFileStream` or `NzbFileStream`), then
    /// wraps it in an axum `Body`.
    pub fn get_body(&self, item: &DavItem) -> Result<Body> {
        let blob_id = item
            .file_blob_id
            .ok_or_else(|| DavServerError::NotFound("item has no file blob".into()))?;

        let blob_data = {
            let conn = self.db.lock();
            BlobStore::get_file_blob(&conn, blob_id)?
        };

        let file_size = item.file_size.unwrap_or(0) as u64;

        match item.sub_type {
            ItemSubType::MultipartFile => {
                let meta: DavMultipartFile = bincode::deserialize(&blob_data).map_err(|e| {
                    DavServerError::Other(format!("failed to deserialize multipart blob: {e}"))
                })?;

                let stream = DavMultipartFileStream::new(
                    Arc::clone(&self.provider),
                    meta.file_parts,
                    file_size,
                    self.lookahead,
                );

                if let Some(aes) = meta.aes_params {
                    let decoded_size = aes.decoded_size as u64;
                    let aes_stream = AesDecoderStream::new(stream, &aes.key, &aes.iv, decoded_size);
                    Ok(Body::from_stream(ReaderStream::new(aes_stream)))
                } else {
                    Ok(Body::from_stream(ReaderStream::new(stream)))
                }
            }
            ItemSubType::NzbFile => {
                let meta: DavNzbFile = bincode::deserialize(&blob_data).map_err(|e| {
                    DavServerError::Other(format!("failed to deserialize nzb blob: {e}"))
                })?;

                let stream = nzbdav_stream::SeekableSegmentStream::new(
                    Arc::clone(&self.provider),
                    meta.segment_ids,
                    file_size,
                    self.lookahead,
                );

                Ok(Body::from_stream(ReaderStream::new(stream)))
            }
            other => Err(DavServerError::Other(format!(
                "unsupported sub_type for streaming: {other:?}"
            ))),
        }
    }

    /// Store an NZB XML file under `/nzbs/`.
    pub fn put_nzb(&self, path: &str, data: &[u8]) -> Result<()> {
        let (parent_path, name) = split_path(path);

        let conn = self.db.lock();
        let parent = get_by_path(&conn, parent_path)?
            .ok_or_else(|| DavServerError::Conflict(format!("parent not found: {parent_path}")))?;

        // If item already exists at this path, replace it.
        if let Some(existing) = get_by_path(&conn, path)? {
            // Delete old blob if present
            if let Some(blob_id) = existing.nzb_blob_id {
                let _ = BlobStore::delete(&conn, "nzb_blobs", blob_id);
            }
            dav_items::delete(&conn, existing.id)?;
        }

        let blob_id = Uuid::new_v4();
        BlobStore::put_nzb_blob(&conn, blob_id, data)?;

        let item = DavItem {
            id: Uuid::new_v4(),
            id_prefix: name.chars().next().unwrap_or('_').to_string(),
            created_at: chrono::Utc::now().naive_utc(),
            parent_id: Some(parent.id),
            name: name.to_string(),
            file_size: Some(data.len() as i64),
            item_type: ItemType::UsenetFile,
            sub_type: ItemSubType::NzbFile,
            path: path.to_string(),
            release_date: None,
            last_health_check: None,
            next_health_check: None,
            history_item_id: None,
            file_blob_id: None,
            nzb_blob_id: Some(blob_id),
        };
        dav_items::insert(&conn, &item)?;
        debug!(path, "PUT NZB stored");
        Ok(())
    }

    /// Delete an item (and its children via CASCADE).
    ///
    /// For virtual queue items under `/nzbs/`, this removes the queue entry
    /// and its NZB blob instead of deleting a dav_item.
    pub fn delete(&self, path: &str) -> Result<()> {
        let conn = self.db.lock();

        // Check for real item first.
        if let Some(item) = get_by_path(&conn, path)? {
            dav_items::delete(&conn, item.id)?;
            debug!(path, "DELETE complete");
            return Ok(());
        }

        // Virtual queue items under /nzbs/<filename>.
        if let Some(filename) = path.strip_prefix("/nzbs/")
            && !filename.is_empty()
            && !filename.contains('/')
        {
            let queue = queue_items::list(&conn)?;
            if let Some(qi) = queue.into_iter().find(|qi| qi.file_name == filename) {
                let _ = BlobStore::delete(&conn, "nzb_blobs", qi.id);
                queue_items::delete(&conn, qi.id)?;
                debug!(path, "DELETE queue item complete");
                return Ok(());
            }
        }

        Err(DavServerError::NotFound(path.into()))
    }

    /// Create a new collection (directory).
    pub fn mkcol(&self, path: &str) -> Result<()> {
        let (parent_path, name) = split_path(path);

        let conn = self.db.lock();

        // Check the parent exists and is a collection.
        let parent = get_by_path(&conn, parent_path)?
            .ok_or_else(|| DavServerError::Conflict(format!("parent not found: {parent_path}")))?;
        if parent.item_type != ItemType::Directory {
            return Err(DavServerError::Conflict(
                "parent is not a collection".into(),
            ));
        }

        // Conflict if already exists.
        if get_by_path(&conn, path)?.is_some() {
            return Err(DavServerError::MethodNotAllowed(
                "collection already exists".into(),
            ));
        }

        let item = DavItem {
            id: Uuid::new_v4(),
            id_prefix: name.chars().next().unwrap_or('_').to_string(),
            created_at: chrono::Utc::now().naive_utc(),
            parent_id: Some(parent.id),
            name: name.to_string(),
            file_size: None,
            item_type: ItemType::Directory,
            sub_type: ItemSubType::Directory,
            path: path.to_string(),
            release_date: None,
            last_health_check: None,
            next_health_check: None,
            history_item_id: None,
            file_blob_id: None,
            nzb_blob_id: None,
        };
        dav_items::insert(&conn, &item)?;
        debug!(path, "MKCOL created");
        Ok(())
    }

    /// Move (rename) an item from one path to another.
    pub fn move_item(&self, from: &str, to: &str) -> Result<()> {
        let conn = self.db.lock();
        let item =
            get_by_path(&conn, from)?.ok_or_else(|| DavServerError::NotFound(from.into()))?;

        let (new_parent_path, new_name) = split_path(to);
        let new_parent = get_by_path(&conn, new_parent_path)?.ok_or_else(|| {
            DavServerError::Conflict(format!("dest parent not found: {new_parent_path}"))
        })?;

        // Delete anything already at the destination.
        if let Some(existing) = get_by_path(&conn, to)? {
            dav_items::delete(&conn, existing.id)?;
        }

        conn.execute(
            "UPDATE dav_items SET name = ?1, path = ?2, parent_id = ?3 WHERE id = ?4",
            rusqlite::params![new_name, to, new_parent.id.to_string(), item.id.to_string()],
        )
        .map_err(nzbdav_core::error::DavError::Database)?;

        debug!(from, to, "MOVE complete");
        Ok(())
    }

    /// Copy an item from one path to another (shallow copy — same blob refs).
    pub fn copy_item(&self, from: &str, to: &str) -> Result<()> {
        let conn = self.db.lock();
        let source =
            get_by_path(&conn, from)?.ok_or_else(|| DavServerError::NotFound(from.into()))?;

        let (new_parent_path, new_name) = split_path(to);
        let new_parent = get_by_path(&conn, new_parent_path)?.ok_or_else(|| {
            DavServerError::Conflict(format!("dest parent not found: {new_parent_path}"))
        })?;

        // Delete anything already at the destination.
        if let Some(existing) = get_by_path(&conn, to)? {
            dav_items::delete(&conn, existing.id)?;
        }

        let copy = DavItem {
            id: Uuid::new_v4(),
            id_prefix: new_name.chars().next().unwrap_or('_').to_string(),
            created_at: chrono::Utc::now().naive_utc(),
            parent_id: Some(new_parent.id),
            name: new_name.to_string(),
            path: to.to_string(),
            ..source
        };
        dav_items::insert(&conn, &copy)?;
        debug!(from, to, "COPY complete");
        Ok(())
    }
}

/// Build a virtual `DavItem` from a `QueueItem` for display under `/nzbs/`.
fn queue_item_to_dav_item(qi: &nzbdav_core::models::QueueItem) -> DavItem {
    DavItem {
        id: qi.id,
        id_prefix: "q".to_string(),
        created_at: qi.created_at,
        parent_id: None,
        name: qi.file_name.clone(),
        file_size: Some(qi.nzb_file_size),
        item_type: ItemType::UsenetFile,
        sub_type: ItemSubType::NzbFile,
        path: format!("/nzbs/{}", qi.file_name),
        release_date: None,
        last_health_check: None,
        next_health_check: None,
        history_item_id: None,
        file_blob_id: None,
        nzb_blob_id: Some(qi.id),
    }
}

/// Split a path into (parent_path, name).
///
/// For `/content/movies/file.mkv` returns (`/content/movies/`, `file.mkv`).
/// For `/content/movies/` returns (`/content/`, `movies`).
fn split_path(path: &str) -> (&str, &str) {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(pos) => (&path[..=pos], &trimmed[pos + 1..]),
        None => ("/", trimmed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_path_file() {
        let (parent, name) = split_path("/content/movies/file.mkv");
        assert_eq!(parent, "/content/movies/");
        assert_eq!(name, "file.mkv");
    }

    #[test]
    fn test_split_path_directory() {
        let (parent, name) = split_path("/content/movies/");
        assert_eq!(parent, "/content/");
        assert_eq!(name, "movies");
    }

    #[test]
    fn test_split_path_root_child() {
        let (parent, name) = split_path("/nzbs/");
        assert_eq!(parent, "/");
        assert_eq!(name, "nzbs");
    }

    #[test]
    fn test_mime_from_name() {
        assert_eq!(mime_from_name("video.mkv"), "video/x-matroska");
        // mime_guess returns octet-stream for unknown extensions
        let nzb_mime = mime_from_name("file.nzb");
        assert!(!nzb_mime.is_empty());
        // Known extension should produce a real type
        assert_eq!(mime_from_name("file.mp4"), "video/mp4");
    }

    fn test_store() -> (DatabaseStore, Arc<Mutex<Connection>>) {
        let conn = nzbdav_core::db::open(":memory:").unwrap();
        nzbdav_core::seed::seed_root_items(&conn).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let provider = Arc::new(UsenetArticleProvider::new(vec![]));
        let store = DatabaseStore::new(db.clone(), provider, 4);
        (store, db)
    }

    #[test]
    fn test_get_node_root() {
        let (store, _db) = test_store();
        let node = store.get_node("/").unwrap();
        assert!(node.is_some(), "root node should exist after seeding");
        let node = node.unwrap();
        assert!(node.is_collection);
        assert_eq!(
            node.item.sub_type,
            nzbdav_core::models::ItemSubType::WebdavRoot
        );
    }

    #[test]
    fn test_get_node_not_found() {
        let (store, _db) = test_store();
        let node = store.get_node("/nonexistent").unwrap();
        assert!(node.is_none());
    }

    #[test]
    fn test_list_children_root() {
        let (store, _db) = test_store();
        let children = store.list_children("/").unwrap();
        assert_eq!(children.len(), 4, "root should have 4 seeded children");
        let names: Vec<&str> = children.iter().map(|c| c.item.name.as_str()).collect();
        assert!(names.contains(&"nzbs"));
        assert!(names.contains(&"content"));
        assert!(names.contains(&"completed-symlinks"));
        assert!(names.contains(&".ids"));
    }

    #[test]
    fn test_list_children_nzbs_includes_readme() {
        let (store, _db) = test_store();
        let children = store.list_children("/nzbs/").unwrap();
        // Should have at least the README.txt from seeding
        let readme = children.iter().find(|c| c.item.name == "README.txt");
        assert!(readme.is_some(), "/nzbs/ should contain README.txt");
    }

    #[test]
    fn test_list_children_nzbs_includes_queue_items() {
        let (store, db) = test_store();

        // Insert a queue item with NZB blob
        let qi = nzbdav_core::models::QueueItem {
            id: Uuid::new_v4(),
            created_at: chrono::Utc::now().naive_utc(),
            file_name: "test.nzb".to_string(),
            job_name: "test".to_string(),
            nzb_file_size: 100,
            total_segment_bytes: 5000,
            category: "default".to_string(),
            priority: 0,
            post_processing: -1,
            pause_until: None,
        };
        {
            let conn = db.lock();
            nzbdav_core::queue_items::insert(&conn, &qi).unwrap();
            nzbdav_core::blob_store::BlobStore::put_nzb_blob(&conn, qi.id, b"<nzb/>").unwrap();
        }

        let children = store.list_children("/nzbs/").unwrap();
        let nzb_node = children.iter().find(|c| c.item.name == "test.nzb");
        assert!(nzb_node.is_some(), "queue item should appear under /nzbs/");
        let nzb_node = nzb_node.unwrap();
        assert!(!nzb_node.is_collection);
        assert_eq!(nzb_node.item.file_size, Some(100));
    }

    #[test]
    fn test_get_node_virtual_queue_item() {
        let (store, db) = test_store();

        let qi = nzbdav_core::models::QueueItem {
            id: Uuid::new_v4(),
            created_at: chrono::Utc::now().naive_utc(),
            file_name: "download.nzb".to_string(),
            job_name: "download".to_string(),
            nzb_file_size: 200,
            total_segment_bytes: 8000,
            category: "".to_string(),
            priority: 0,
            post_processing: -1,
            pause_until: None,
        };
        {
            let conn = db.lock();
            nzbdav_core::queue_items::insert(&conn, &qi).unwrap();
        }

        let node = store.get_node("/nzbs/download.nzb").unwrap();
        assert!(node.is_some(), "virtual queue item should be resolvable");
        assert_eq!(node.unwrap().item.id, qi.id);
    }

    #[test]
    fn test_delete_virtual_queue_item() {
        let (store, db) = test_store();

        let qi = nzbdav_core::models::QueueItem {
            id: Uuid::new_v4(),
            created_at: chrono::Utc::now().naive_utc(),
            file_name: "removeme.nzb".to_string(),
            job_name: "removeme".to_string(),
            nzb_file_size: 50,
            total_segment_bytes: 1000,
            category: "".to_string(),
            priority: 0,
            post_processing: -1,
            pause_until: None,
        };
        {
            let conn = db.lock();
            nzbdav_core::queue_items::insert(&conn, &qi).unwrap();
            nzbdav_core::blob_store::BlobStore::put_nzb_blob(&conn, qi.id, b"<nzb/>").unwrap();
        }

        // Delete via WebDAV path
        store.delete("/nzbs/removeme.nzb").unwrap();

        // Queue item should be gone
        {
            let conn = db.lock();
            assert!(
                nzbdav_core::queue_items::get_by_id(&conn, qi.id)
                    .unwrap()
                    .is_none()
            );
        }

        // Node should no longer resolve
        assert!(store.get_node("/nzbs/removeme.nzb").unwrap().is_none());
    }

    #[test]
    fn test_get_node_readme() {
        let (store, _db) = test_store();
        let node = store.get_node("/content/README.txt").unwrap();
        assert!(node.is_some(), "README.txt should exist in /content/");
        let node = node.unwrap();
        assert!(!node.is_collection);
        assert_eq!(
            node.item.sub_type,
            nzbdav_core::models::ItemSubType::ReadmeFile
        );
        assert!(node.item.nzb_blob_id.is_some());
    }

    #[test]
    fn test_mkcol_and_list() {
        let (store, _db) = test_store();
        store.mkcol("/content/movies/").unwrap();

        let children = store.list_children("/content/").unwrap();
        // README.txt + movies directory
        assert_eq!(children.len(), 2);
        let movies = children
            .iter()
            .find(|c| c.item.name == "movies")
            .expect("movies dir");
        assert!(movies.is_collection);

        // Verify via get_node too
        let node = store.get_node("/content/movies/").unwrap();
        assert!(node.is_some());
    }

    #[test]
    fn test_delete_item() {
        let (store, _db) = test_store();
        store.mkcol("/content/temp/").unwrap();

        // Verify it exists
        assert!(store.get_node("/content/temp/").unwrap().is_some());

        // Delete it
        store.delete("/content/temp/").unwrap();

        // Verify it's gone
        assert!(store.get_node("/content/temp/").unwrap().is_none());
    }
}
