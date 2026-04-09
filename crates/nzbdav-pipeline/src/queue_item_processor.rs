use std::sync::Arc;

use rusqlite::Connection;
use tracing::{debug, info};
use uuid::Uuid;

use nzb_core::models::NzbJob;
use nzb_core::nzb_parser::parse_nzb;
use nzbdav_core::blob_store::BlobStore;
use nzbdav_core::config::ConfigManager;
use nzbdav_core::dav_items;
use nzbdav_core::models::{
    DavItem, DavMultipartFile, DavNzbFile, ItemSubType, ItemType, QueueItem,
};
use nzbdav_core::util::get_nzb_password;
use nzbdav_stream::provider::UsenetArticleProvider;

use crate::aggregators::{aggregate_plain_files, aggregate_rar_files};
use crate::deobfuscation::{
    fetch_first_segments::fetch_first_segments, get_file_infos::get_file_infos,
    get_par2_file_descriptors::get_par2_file_descriptors,
};
use crate::error::{PipelineError, Result};
use crate::post_processors::{
    create_strm_items, ensure_importable_video, filter_blocklisted, rename_duplicates,
};
use crate::processors::{process_plain_files, process_rar_files};

/// Result of processing a single queue item.
pub struct ProcessResult {
    /// Number of DAV items created (files + directories).
    pub items_created: usize,
    /// UUID of the job directory created under `/content/{category}/`.
    pub job_dir_id: Uuid,
}

/// Orchestrates the full NZB processing pipeline for a single queue item.
///
/// Pipeline stages:
/// 1. Parse NZB XML into an [`NzbJob`]
/// 2. Create the job directory in the virtual filesystem
/// 3. Deobfuscation (first-segment fetch, PAR2 descriptors, filename resolution)
/// 4. File processing (RAR header reading, plain file validation)
/// 5. Aggregation (group RAR volumes, create file entries)
/// 6. Post-processing (rename duplicates, blocklist filtering, video check, STRM)
/// 7. Persist results to the database
pub struct QueueItemProcessor {
    provider: Arc<UsenetArticleProvider>,
    config: ConfigManager,
}

impl QueueItemProcessor {
    pub fn new(provider: Arc<UsenetArticleProvider>, config: ConfigManager) -> Self {
        Self { provider, config }
    }

    /// Process a single queue item through the full pipeline.
    pub async fn process(
        &self,
        conn: &Connection,
        queue_item: &QueueItem,
        nzb_data: &[u8],
    ) -> Result<ProcessResult> {
        // ── 1. Parse NZB ────────────────────────────────────────────────
        let job: NzbJob = parse_nzb(&queue_item.job_name, nzb_data)
            .map_err(|e| PipelineError::Other(format!("NZB parse error: {e}")))?;

        info!(
            job_name = %queue_item.job_name,
            file_count = job.files.len(),
            "parsed NZB"
        );

        // ── 2. Create job directory ─────────────────────────────────────
        let category = &queue_item.category;
        let content_root_path = "/content/";

        // If category is non-empty, create a subdirectory for it; otherwise
        // place jobs directly under /content/.
        let parent_path = if category.is_empty() {
            content_root_path.to_string()
        } else {
            let category_path = format!("{content_root_path}{category}/");
            let _category_dir =
                ensure_directory(conn, &category_path, category, content_root_path)?;
            category_path
        };
        let job_dir_path = format!("{parent_path}{}/", queue_item.job_name);

        // Ensure the job directory exists (may already exist from a previous failed attempt).
        let job_dir = ensure_directory_with_history(
            conn,
            &job_dir_path,
            &queue_item.job_name,
            &parent_path,
            Some(queue_item.id),
        )?;

        debug!(
            job_dir_id = %job_dir.id,
            path = %job_dir_path,
            "created job directory"
        );

        // ── 3. Deobfuscation ────────────────────────────────────────────
        let mut file_infos = fetch_first_segments(&self.provider, &job).await?;

        let par2_files = get_par2_file_descriptors(&self.provider, &file_infos).await?;

        get_file_infos(&mut file_infos, &par2_files);

        info!(file_count = file_infos.len(), "deobfuscation complete");

        // ── 4. Split files into RAR vs plain ────────────────────────────
        let rar_files: Vec<_> = file_infos.iter().filter(|f| f.is_rar).cloned().collect();
        let plain_files: Vec<_> = file_infos
            .iter()
            .filter(|f| !f.is_rar && !f.is_par2)
            .cloned()
            .collect();

        // ── 5. Process ──────────────────────────────────────────────────
        // Extract password: first check NZB filename {{password}}, then NZB XML metadata
        let password = get_nzb_password(&queue_item.file_name).or_else(|| job.password.clone());
        if password.is_some() {
            info!(
                job_name = %queue_item.job_name,
                "NZB password detected"
            );
        }
        let lookahead = self.config.get_article_buffer_size();
        let processed_rar =
            process_rar_files(&self.provider, &rar_files, lookahead, password.as_deref()).await?;
        let processed_plain = process_plain_files(&plain_files);

        debug!(
            rar_files = processed_rar.len(),
            plain_files = processed_plain.len(),
            "processing complete"
        );

        // ── 6. Aggregate ────────────────────────────────────────────────
        let rar_aggregated = aggregate_rar_files(
            &processed_rar,
            job_dir.id,
            &job_dir_path,
            password.as_deref(),
        )?;
        let plain_aggregated = aggregate_plain_files(&processed_plain, job_dir.id, &job_dir_path);

        // Collect all DavItems for post-processing.
        let mut items: Vec<DavItem> = Vec::new();
        let mut multipart_blobs: Vec<(Uuid, DavMultipartFile)> = Vec::new();
        let mut nzb_file_blobs: Vec<(Uuid, DavNzbFile)> = Vec::new();

        for (dav_item, multipart) in &rar_aggregated {
            items.push(dav_item.clone());
            multipart_blobs.push((dav_item.id, multipart.clone()));
        }
        for (dav_item, nzb_file) in &plain_aggregated {
            items.push(dav_item.clone());
            nzb_file_blobs.push((dav_item.id, nzb_file.clone()));
        }

        // ── 7. Post-process ─────────────────────────────────────────────
        // 7a. Rename duplicates
        rename_duplicates(&mut items);

        // 7b. Filter blocklisted files
        let blocklist = self.config.get_file_blocklist();
        let items = filter_blocklisted(items, &blocklist);

        // 7c. Ensure at least one importable video (if enabled)
        if self.config.is_ensure_importable_video_enabled() {
            ensure_importable_video(&items)?;
        }

        // 7d. Create STRM files for video items
        let webdav_base_url = self
            .config
            .get_or_default("api.webdav-base-url", "http://localhost:8080");
        let strm_items = create_strm_items(&items, job_dir.id, &job_dir_path, &webdav_base_url);

        // ── 8. Persist to database ──────────────────────────────────────
        // Collect the IDs of items that survived post-processing (blocklist
        // may have removed some).
        let surviving_ids: std::collections::HashSet<Uuid> = items.iter().map(|i| i.id).collect();

        let mut total_created: usize = 0;

        // Insert file items + blobs.
        for mut item in items {
            // Store multipart blob if this item has one.
            if let Some((_id, multipart)) = multipart_blobs.iter().find(|(id, _)| *id == item.id) {
                let blob_id = Uuid::new_v4();
                let blob_data = bincode::serialize(multipart)
                    .map_err(|e| PipelineError::Other(format!("bincode error: {e}")))?;
                BlobStore::put_file_blob(conn, blob_id, &blob_data)?;
                item.file_blob_id = Some(blob_id);
            }

            // Store NZB file blob if this item has one.
            if let Some((_id, nzb_file)) = nzb_file_blobs.iter().find(|(id, _)| *id == item.id) {
                // Only persist if the item survived blocklist filtering.
                if !surviving_ids.contains(&item.id) {
                    continue;
                }
                let blob_id = Uuid::new_v4();
                let blob_data = bincode::serialize(nzb_file)
                    .map_err(|e| PipelineError::Other(format!("bincode error: {e}")))?;
                BlobStore::put_file_blob(conn, blob_id, &blob_data)?;
                item.file_blob_id = Some(blob_id);
            }

            dav_items::insert(conn, &item)?;
            total_created += 1;
        }

        // Insert STRM items.
        for item in &strm_items {
            dav_items::insert(conn, item)?;
            total_created += 1;
        }

        info!(
            items_created = total_created,
            job_dir_id = %job_dir.id,
            "pipeline complete"
        );

        Ok(ProcessResult {
            items_created: total_created,
            job_dir_id: job_dir.id,
        })
    }
}

// ── Helper functions ────────────────────────────────────────────────────────

/// Ensure a directory exists at `path`; create it if missing.
fn ensure_directory(
    conn: &Connection,
    path: &str,
    name: &str,
    parent_path: &str,
) -> Result<DavItem> {
    if let Some(existing) = dav_items::get_by_path(conn, path)? {
        return Ok(existing);
    }

    // Find the parent.
    let parent = dav_items::get_by_path(conn, parent_path)?.ok_or_else(|| {
        PipelineError::Other(format!("parent directory not found: {parent_path}"))
    })?;

    create_directory(conn, path, name, parent.id, None)
}

/// Ensure a directory exists, with an optional history_item_id.
/// Used for job directories which may already exist from a failed retry.
fn ensure_directory_with_history(
    conn: &Connection,
    path: &str,
    name: &str,
    parent_path: &str,
    history_item_id: Option<Uuid>,
) -> Result<DavItem> {
    if let Some(existing) = dav_items::get_by_path(conn, path)? {
        return Ok(existing);
    }

    let parent = dav_items::get_by_path(conn, parent_path)?.ok_or_else(|| {
        PipelineError::Other(format!("parent directory not found: {parent_path}"))
    })?;

    create_directory(conn, path, name, parent.id, history_item_id)
}

/// Create and insert a new directory `DavItem`.
fn create_directory(
    conn: &Connection,
    path: &str,
    name: &str,
    parent_id: Uuid,
    history_item_id: Option<Uuid>,
) -> Result<DavItem> {
    let item = DavItem {
        id: Uuid::new_v4(),
        id_prefix: name
            .chars()
            .next()
            .unwrap_or('d')
            .to_ascii_lowercase()
            .to_string(),
        created_at: chrono::Utc::now().naive_utc(),
        parent_id: Some(parent_id),
        name: name.to_string(),
        file_size: None,
        item_type: ItemType::Directory,
        sub_type: ItemSubType::Directory,
        path: path.to_string(),
        release_date: None,
        last_health_check: None,
        next_health_check: None,
        history_item_id,
        file_blob_id: None,
        nzb_blob_id: None,
    };

    dav_items::insert(conn, &item)?;
    Ok(item)
}
