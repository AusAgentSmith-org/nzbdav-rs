use std::sync::Arc;
use std::time::Instant;

use tracing::{debug, info, warn};
use uuid::Uuid;

use nzb_core::models::NzbJob;
use nzb_core::nzb_parser::parse_nzb;
use nzbdav_core::database::DavDatabase;
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

/// Configuration for the pipeline (extracted from ConfigManager to avoid coupling).
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub article_buffer_size: usize,
    pub file_blocklist: Vec<String>,
    pub ensure_importable_video: bool,
    pub webdav_base_url: String,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            article_buffer_size: 40,
            file_blocklist: Vec::new(),
            ensure_importable_video: true,
            webdav_base_url: "http://localhost:8080".to_string(),
        }
    }
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
    config: PipelineConfig,
}

impl QueueItemProcessor {
    pub fn new(provider: Arc<UsenetArticleProvider>, config: PipelineConfig) -> Self {
        Self { provider, config }
    }

    /// Process a single queue item through the full pipeline.
    pub async fn process(
        &self,
        db: &dyn DavDatabase,
        queue_item: &QueueItem,
        nzb_data: &[u8],
    ) -> Result<ProcessResult> {
        let pipeline_start = Instant::now();

        // ── 1. Parse NZB ────────────────────────────────────────────────
        let job: NzbJob = parse_nzb(&queue_item.job_name, nzb_data)
            .map_err(|e| PipelineError::Other(format!("NZB parse error: {e}")))?;

        info!(
            job_name = %queue_item.job_name,
            file_count = job.files.len(),
            "parsed NZB"
        );

        // ── 1b. Check for incomplete NZB (missing early segments) ──────
        let mut incomplete_count = 0usize;
        let total_files = job.files.len();
        for nzb_file in &job.files {
            if nzb_file.articles.is_empty() {
                continue;
            }
            let min_seg = nzb_file
                .articles
                .iter()
                .map(|a| a.segment_number)
                .min()
                .unwrap_or(1);
            let max_seg = nzb_file
                .articles
                .iter()
                .map(|a| a.segment_number)
                .max()
                .unwrap_or(1);
            let expected = (max_seg - min_seg + 1) as usize;
            let present = nzb_file.articles.len();
            let missing_start = min_seg > 1;

            if missing_start || present < expected / 2 {
                incomplete_count += 1;
                debug!(
                    file = %nzb_file.filename,
                    segments = %format!("{min_seg}-{max_seg}"),
                    present,
                    expected,
                    missing_start,
                    "incomplete file in NZB"
                );
            }
        }
        if incomplete_count > 0 {
            let pct = incomplete_count * 100 / total_files.max(1);
            if pct > 50 {
                warn!(
                    job_name = %queue_item.job_name,
                    incomplete = incomplete_count,
                    total = total_files,
                    "NZB is mostly incomplete ({pct}% of files missing segments)"
                );
                return Err(PipelineError::IncompleteNzb(format!(
                    "{incomplete_count}/{total_files} files ({pct}%) have missing segments"
                )));
            }
            info!(
                job_name = %queue_item.job_name,
                incomplete = incomplete_count,
                total = total_files,
                "skipping {incomplete_count} incomplete file(s) ({pct}%)"
            );
        }

        // ── 2. Create job directory ─────────────────────────────────────
        let category = &queue_item.category;
        let content_root_path = "/content/";

        let parent_path = if category.is_empty() {
            content_root_path.to_string()
        } else {
            let category_path = format!("{content_root_path}{category}/");
            let _category_dir =
                ensure_directory(db, &category_path, category, content_root_path).await?;
            category_path
        };
        let job_dir_path = format!("{parent_path}{}/", queue_item.job_name);

        let job_dir = ensure_directory_with_history(
            db,
            &job_dir_path,
            &queue_item.job_name,
            &parent_path,
            Some(queue_item.id),
        )
        .await?;

        debug!(
            job_dir_id = %job_dir.id,
            path = %job_dir_path,
            "created job directory"
        );

        // ── 3. Deobfuscation ────────────────────────────────────────────
        let stage_start = Instant::now();
        let mut file_infos = fetch_first_segments(&self.provider, &job).await?;
        info!(
            job_name = %queue_item.job_name,
            elapsed_ms = stage_start.elapsed().as_millis() as u64,
            files = file_infos.len(),
            "first segment fetch complete"
        );

        let stage_start = Instant::now();
        let par2_files = get_par2_file_descriptors(&self.provider, &file_infos).await?;
        info!(
            job_name = %queue_item.job_name,
            elapsed_ms = stage_start.elapsed().as_millis() as u64,
            descriptors = par2_files.len(),
            "PAR2 descriptor fetch complete"
        );

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
        let password = get_nzb_password(&queue_item.file_name).or_else(|| job.password.clone());
        if password.is_some() {
            info!(
                job_name = %queue_item.job_name,
                "NZB password detected"
            );
        }
        let lookahead = self.config.article_buffer_size;
        let stage_start = Instant::now();
        let processed_rar =
            process_rar_files(&self.provider, &rar_files, lookahead, password.as_deref()).await?;
        let processed_plain = process_plain_files(&plain_files);

        info!(
            job_name = %queue_item.job_name,
            elapsed_ms = stage_start.elapsed().as_millis() as u64,
            rar_files = processed_rar.len(),
            plain_files = processed_plain.len(),
            "file processing complete"
        );

        // ── 6. Aggregate ────────────────────────────────────────────────
        let stage_start = Instant::now();
        let rar_aggregated = aggregate_rar_files(
            &processed_rar,
            job_dir.id,
            &job_dir_path,
            password.as_deref(),
        )?;
        let plain_aggregated = aggregate_plain_files(&processed_plain, job_dir.id, &job_dir_path);
        info!(
            job_name = %queue_item.job_name,
            elapsed_ms = stage_start.elapsed().as_millis() as u64,
            rar_items = rar_aggregated.len(),
            plain_items = plain_aggregated.len(),
            "aggregation complete"
        );

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
        let pre_filter_count = items.len();
        rename_duplicates(&mut items);

        let items = filter_blocklisted(items, &self.config.file_blocklist);
        let blocked_count = pre_filter_count - items.len();
        if blocked_count > 0 {
            info!(
                job_name = %queue_item.job_name,
                blocked = blocked_count,
                remaining = items.len(),
                "blocklist filtered files"
            );
        }

        if self.config.ensure_importable_video {
            ensure_importable_video(&items)?;
        }

        let strm_items = create_strm_items(
            &items,
            job_dir.id,
            &job_dir_path,
            &self.config.webdav_base_url,
        );
        if !strm_items.is_empty() {
            info!(
                job_name = %queue_item.job_name,
                strm_files = strm_items.len(),
                "created STRM files"
            );
        }

        // ── 8. Persist to database ──────────────────────────────────────
        let stage_start = Instant::now();
        let surviving_ids: std::collections::HashSet<Uuid> = items.iter().map(|i| i.id).collect();

        let mut total_created: usize = 0;

        for mut item in items {
            if let Some((_id, multipart)) = multipart_blobs.iter().find(|(id, _)| *id == item.id) {
                let blob_id = Uuid::new_v4();
                let blob_data = bincode::serialize(multipart)
                    .map_err(|e| PipelineError::Other(format!("bincode error: {e}")))?;
                db.put_file_blob(blob_id, &blob_data).await?;
                item.file_blob_id = Some(blob_id);
            }

            if let Some((_id, nzb_file)) = nzb_file_blobs.iter().find(|(id, _)| *id == item.id) {
                if !surviving_ids.contains(&item.id) {
                    continue;
                }
                let blob_id = Uuid::new_v4();
                let blob_data = bincode::serialize(nzb_file)
                    .map_err(|e| PipelineError::Other(format!("bincode error: {e}")))?;
                db.put_file_blob(blob_id, &blob_data).await?;
                item.file_blob_id = Some(blob_id);
            }

            db.insert_dav_item(&item).await?;
            total_created += 1;
        }

        for item in &strm_items {
            db.insert_dav_item(item).await?;
            total_created += 1;
        }

        info!(
            job_name = %queue_item.job_name,
            elapsed_ms = stage_start.elapsed().as_millis() as u64,
            items_persisted = total_created,
            "database persistence complete"
        );

        info!(
            job_name = %queue_item.job_name,
            items_created = total_created,
            total_elapsed_ms = pipeline_start.elapsed().as_millis() as u64,
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
async fn ensure_directory(
    db: &dyn DavDatabase,
    path: &str,
    name: &str,
    parent_path: &str,
) -> Result<DavItem> {
    if let Some(existing) = db.get_dav_item_by_path(path).await? {
        return Ok(existing);
    }

    let parent = db.get_dav_item_by_path(parent_path).await?.ok_or_else(|| {
        PipelineError::Other(format!("parent directory not found: {parent_path}"))
    })?;

    create_directory(db, path, name, parent.id, None).await
}

/// Ensure a directory exists, with an optional history_item_id.
async fn ensure_directory_with_history(
    db: &dyn DavDatabase,
    path: &str,
    name: &str,
    parent_path: &str,
    history_item_id: Option<Uuid>,
) -> Result<DavItem> {
    if let Some(existing) = db.get_dav_item_by_path(path).await? {
        return Ok(existing);
    }

    let parent = db.get_dav_item_by_path(parent_path).await?.ok_or_else(|| {
        PipelineError::Other(format!("parent directory not found: {parent_path}"))
    })?;

    create_directory(db, path, name, parent.id, history_item_id).await
}

/// Create and insert a new directory `DavItem`.
async fn create_directory(
    db: &dyn DavDatabase,
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

    db.insert_dav_item(&item).await?;
    Ok(item)
}
