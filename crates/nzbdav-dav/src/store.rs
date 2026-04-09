//! `DatabaseStore` — WebDAV storage backed by `DavDatabase` + Usenet streaming.

use std::sync::Arc;

use axum::body::Body;
use tokio_util::io::ReaderStream;
use tracing::debug;
use uuid::Uuid;

use nzbdav_core::database::DavDatabase;
use nzbdav_core::models::{DavItem, DavMultipartFile, DavNzbFile, ItemSubType, ItemType};
use nzbdav_stream::{AesDecoderStream, DavMultipartFileStream, UsenetArticleProvider};

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

/// WebDAV storage backed by a `DavDatabase` for metadata and Usenet for file content.
pub struct DatabaseStore {
    db: Arc<dyn DavDatabase>,
    provider: Arc<UsenetArticleProvider>,
    lookahead: usize,
}

impl DatabaseStore {
    pub fn new(
        db: Arc<dyn DavDatabase>,
        provider: Arc<UsenetArticleProvider>,
        lookahead: usize,
    ) -> Self {
        Self {
            db,
            provider,
            lookahead,
        }
    }

    /// Access the database.
    pub fn db(&self) -> &dyn DavDatabase {
        &*self.db
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
    pub async fn get_node(&self, path: &str) -> Result<Option<DavNode>> {
        // Check real items first.
        if let Some(item) = self.db.get_dav_item_by_path(path).await? {
            return Ok(Some(DavNode::from_item(item)));
        }

        // Try with trailing slash (directories in DB have trailing slash).
        if !path.ends_with('/') {
            let with_slash = format!("{path}/");
            if let Some(item) = self.db.get_dav_item_by_path(&with_slash).await? {
                return Ok(Some(DavNode::from_item(item)));
            }
        }

        // Virtual queue items under /nzbs/<filename>
        if let Some(filename) = path.strip_prefix("/nzbs/")
            && !filename.is_empty()
            && !filename.contains('/')
        {
            let queue = self.db.list_queue_items().await?;
            if let Some(qi) = queue.into_iter().find(|qi| qi.file_name == filename) {
                return Ok(Some(DavNode::from_item(queue_item_to_dav_item(&qi))));
            }
        }

        Ok(None)
    }

    /// List the immediate children of a collection path.
    pub async fn list_children(&self, path: &str) -> Result<Vec<DavNode>> {
        // Normalize: try with trailing slash if not present
        let normalized = if path.ends_with('/') || path == "/" {
            path.to_string()
        } else {
            format!("{path}/")
        };
        let mut children: Vec<DavNode> = self
            .db
            .get_dav_children_by_path(&normalized)
            .await?
            .into_iter()
            .map(DavNode::from_item)
            .collect();

        // /nzbs/ additionally shows queue items as virtual NZB files.
        let nzbs_check = normalized.trim_end_matches('/');
        if nzbs_check == "/nzbs" {
            let queue = self.db.list_queue_items().await?;
            for qi in &queue {
                children.push(DavNode::from_item(queue_item_to_dav_item(qi)));
            }
        }

        Ok(children)
    }

    /// Build a streaming `Body` for a file item.
    pub async fn get_body(&self, item: &DavItem) -> Result<Body> {
        let blob_id = item
            .file_blob_id
            .ok_or_else(|| DavServerError::NotFound("item has no file blob".into()))?;

        let blob_data = self.db.get_file_blob(blob_id).await?;
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
    pub async fn put_nzb(&self, path: &str, data: &[u8]) -> Result<()> {
        let (parent_path, name) = split_path(path);

        let parent = self
            .db
            .get_dav_item_by_path(parent_path)
            .await?
            .ok_or_else(|| DavServerError::Conflict(format!("parent not found: {parent_path}")))?;

        // If item already exists at this path, replace it.
        if let Some(existing) = self.db.get_dav_item_by_path(path).await? {
            if let Some(blob_id) = existing.nzb_blob_id {
                let _ = self.db.delete_nzb_blob(blob_id).await;
            }
            self.db.delete_dav_item(existing.id).await?;
        }

        let blob_id = Uuid::new_v4();
        self.db.put_nzb_blob(blob_id, data).await?;

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
        self.db.insert_dav_item(&item).await?;
        debug!(path, "PUT NZB stored");
        Ok(())
    }

    /// Delete an item (and its children via CASCADE).
    pub async fn delete(&self, path: &str) -> Result<()> {
        // Check for real item first.
        if let Some(item) = self.db.get_dav_item_by_path(path).await? {
            self.db.delete_dav_item(item.id).await?;
            debug!(path, "DELETE complete");
            return Ok(());
        }

        // Virtual queue items under /nzbs/<filename>.
        if let Some(filename) = path.strip_prefix("/nzbs/")
            && !filename.is_empty()
            && !filename.contains('/')
        {
            let queue = self.db.list_queue_items().await?;
            if let Some(qi) = queue.into_iter().find(|qi| qi.file_name == filename) {
                let _ = self.db.delete_nzb_blob(qi.id).await;
                self.db.delete_queue_item(qi.id).await?;
                debug!(path, "DELETE queue item complete");
                return Ok(());
            }
        }

        Err(DavServerError::NotFound(path.into()))
    }

    /// Create a new collection (directory).
    pub async fn mkcol(&self, path: &str) -> Result<()> {
        let (parent_path, name) = split_path(path);

        let parent = self
            .db
            .get_dav_item_by_path(parent_path)
            .await?
            .ok_or_else(|| DavServerError::Conflict(format!("parent not found: {parent_path}")))?;

        if parent.item_type != ItemType::Directory {
            return Err(DavServerError::Conflict(
                "parent is not a collection".into(),
            ));
        }

        if self.db.get_dav_item_by_path(path).await?.is_some() {
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
        self.db.insert_dav_item(&item).await?;
        debug!(path, "MKCOL created");
        Ok(())
    }

    /// Move (rename) an item from one path to another.
    pub async fn move_item(&self, from: &str, to: &str) -> Result<()> {
        let item = self
            .db
            .get_dav_item_by_path(from)
            .await?
            .ok_or_else(|| DavServerError::NotFound(from.into()))?;

        let (new_parent_path, new_name) = split_path(to);
        let new_parent = self
            .db
            .get_dav_item_by_path(new_parent_path)
            .await?
            .ok_or_else(|| {
                DavServerError::Conflict(format!("dest parent not found: {new_parent_path}"))
            })?;

        // Delete anything already at the destination.
        if let Some(existing) = self.db.get_dav_item_by_path(to).await? {
            self.db.delete_dav_item(existing.id).await?;
        }

        self.db
            .move_dav_item(item.id, new_name, to, new_parent.id)
            .await?;

        debug!(from, to, "MOVE complete");
        Ok(())
    }

    /// Copy an item from one path to another (shallow copy — same blob refs).
    pub async fn copy_item(&self, from: &str, to: &str) -> Result<()> {
        let source = self
            .db
            .get_dav_item_by_path(from)
            .await?
            .ok_or_else(|| DavServerError::NotFound(from.into()))?;

        let (new_parent_path, new_name) = split_path(to);
        let new_parent = self
            .db
            .get_dav_item_by_path(new_parent_path)
            .await?
            .ok_or_else(|| {
                DavServerError::Conflict(format!("dest parent not found: {new_parent_path}"))
            })?;

        // Delete anything already at the destination.
        if let Some(existing) = self.db.get_dav_item_by_path(to).await? {
            self.db.delete_dav_item(existing.id).await?;
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
        self.db.insert_dav_item(&copy).await?;
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
        let nzb_mime = mime_from_name("file.nzb");
        assert!(!nzb_mime.is_empty());
        assert_eq!(mime_from_name("file.mp4"), "video/mp4");
    }
}
