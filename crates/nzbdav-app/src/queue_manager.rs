use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use parking_lot::Mutex;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use uuid::Uuid;

use nzbdav_core::config::ConfigManager;
use nzbdav_core::database::DavDatabase;
use nzbdav_core::models::{DownloadStatus, HistoryItem, QueueItem};
use nzbdav_core::sqlite_db::SqliteDavDatabase;
use nzbdav_pipeline::queue_item_processor::{PipelineConfig, QueueItemProcessor};
use nzbdav_stream::UsenetArticleProvider;

/// Live status of the queue processing loop.
#[derive(Debug, Clone, Default)]
pub struct QueueStatus {
    pub is_processing: bool,
    pub current_job: Option<String>,
    pub active_ids: HashSet<Uuid>,
    pub items_processed: u64,
    pub last_error: Option<String>,
}

/// Spawn the queue manager on a dedicated thread with its own single-threaded
/// tokio runtime. This avoids `Send` issues with `rusqlite::Connection`.
pub fn spawn_queue_manager(
    db_path: String,
    provider: Arc<UsenetArticleProvider>,
    config: ConfigManager,
    cancel: CancellationToken,
) -> (std::thread::JoinHandle<()>, watch::Receiver<QueueStatus>) {
    let (status_tx, status_rx) = watch::channel(QueueStatus::default());
    let max_concurrent = config.get_max_concurrent_queue();

    let pipeline_config = PipelineConfig {
        article_buffer_size: config.get_article_buffer_size(),
        file_blocklist: config.get_file_blocklist(),
        ensure_importable_video: config.is_ensure_importable_video_enabled(),
        webdav_base_url: config.get_or_default("api.webdav-base-url", "http://localhost:8080"),
    };

    let handle = std::thread::Builder::new()
        .name("queue-manager".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build queue manager runtime");

            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let processor = Arc::new(QueueItemProcessor::new(provider, pipeline_config));
                run_loop(db_path, processor, cancel, status_tx, max_concurrent).await;
            });
        })
        .expect("failed to spawn queue manager thread");

    (handle, status_rx)
}

async fn run_loop(
    db_path: String,
    processor: Arc<QueueItemProcessor>,
    cancel: CancellationToken,
    status_tx: watch::Sender<QueueStatus>,
    max_concurrent: usize,
) {
    info!(max_concurrent, "queue manager started");

    let mut active: tokio::task::JoinSet<Uuid> = tokio::task::JoinSet::new();
    let mut active_ids: HashSet<Uuid> = HashSet::new();
    let poll_interval = Duration::from_secs(3);

    loop {
        // Reap completed tasks
        while let Some(result) = active.try_join_next() {
            match result {
                Ok(id) => {
                    active_ids.remove(&id);
                    status_tx.send_modify(|s| {
                        s.active_ids.remove(&id);
                    });
                }
                Err(e) => {
                    error!(error = %e, "queue task panicked");
                }
            }
        }

        if active.is_empty() {
            status_tx.send_modify(|s| {
                s.is_processing = false;
                s.current_job = None;
                s.active_ids.clear();
            });
        }

        if active.len() < max_concurrent {
            let dav_db = match open_sqlite_db(&db_path) {
                Some(db) => db,
                None => {
                    tokio::select! {
                        () = cancel.cancelled() => break,
                        () = tokio::time::sleep(poll_interval) => continue,
                    }
                }
            };

            let exclude: Vec<Uuid> = active_ids.iter().copied().collect();
            match dav_db.get_next_queue_item(&exclude).await {
                Ok(Some(item)) => {
                    let job_name = item.job_name.clone();
                    let item_id = item.id;
                    active_ids.insert(item_id);

                    info!(
                        job_name = %job_name,
                        active = active.len() + 1,
                        max_concurrent,
                        "starting queue item"
                    );

                    status_tx.send_modify(|s| {
                        s.is_processing = true;
                        s.current_job = Some(job_name.clone());
                        s.active_ids.insert(item_id);
                    });

                    let db_path = db_path.clone();
                    let processor = Arc::clone(&processor);
                    let status_tx = status_tx.clone();

                    active.spawn_local(async move {
                        process_single_item(&db_path, &processor, item, &status_tx).await;
                        item_id
                    });

                    continue;
                }
                Ok(None) => {}
                Err(e) => {
                    error!(error = %e, "failed to fetch next queue item");
                    status_tx.send_modify(|s| s.last_error = Some(e.to_string()));
                }
            }
        }

        tokio::select! {
            () = cancel.cancelled() => break,
            Some(result) = active.join_next() => {
                match result {
                    Ok(id) => {
                        active_ids.remove(&id);
                        status_tx.send_modify(|s| { s.active_ids.remove(&id); });
                    }
                    Err(e) => {
                        error!(error = %e, "queue task panicked");
                    }
                }
            }
            () = tokio::time::sleep(poll_interval) => {}
        }
    }

    while let Some(result) = active.join_next().await {
        match result {
            Ok(id) => {
                active_ids.remove(&id);
            }
            Err(e) => {
                error!(error = %e, "queue task panicked during shutdown");
            }
        }
    }

    info!("queue manager stopped");
}

fn open_sqlite_db(db_path: &str) -> Option<SqliteDavDatabase> {
    match nzbdav_core::db::open(db_path) {
        Ok(conn) => Some(SqliteDavDatabase::new(Arc::new(Mutex::new(conn)))),
        Err(e) => {
            error!(error = %e, "failed to open DB for queue poll");
            None
        }
    }
}

async fn process_single_item(
    db_path: &str,
    processor: &QueueItemProcessor,
    item: QueueItem,
    status_tx: &watch::Sender<QueueStatus>,
) {
    let dav_db = match open_sqlite_db(db_path) {
        Some(db) => db,
        None => return,
    };

    let job_name = item.job_name.clone();
    let item_id = item.id;

    // Load NZB blob
    let nzb_data = match dav_db.get_nzb_blob(item_id).await {
        Ok(data) => data,
        Err(e) => {
            error!(job_name = %job_name, error = %e, "NZB blob not found — failing");
            move_to_history(
                &dav_db,
                &item,
                DownloadStatus::Failed,
                Some(format!("NZB blob not found: {e}")),
                None,
            )
            .await;
            status_tx.send_modify(|s| {
                s.last_error = Some(e.to_string());
            });
            return;
        }
    };

    let result = processor.process(&dav_db, &item, &nzb_data).await;

    match result {
        Ok(pr) => {
            info!(job_name = %job_name, items_created = pr.items_created, "processed successfully");
            move_to_history(
                &dav_db,
                &item,
                DownloadStatus::Completed,
                None,
                Some(pr.job_dir_id),
            )
            .await;
            status_tx.send_modify(|s| {
                s.items_processed += 1;
                s.last_error = None;
            });
        }
        Err(e) => {
            if e.is_retryable() {
                warn!(job_name = %job_name, error = %e, "retryable error — pausing 60s");
                let pause_until = Utc::now().naive_utc() + chrono::Duration::seconds(60);
                if let Err(ue) = dav_db
                    .update_queue_pause_until(item_id, Some(pause_until))
                    .await
                {
                    error!(error = %ue, "failed to set pause_until");
                }
            } else {
                error!(job_name = %job_name, error = %e, "non-retryable error — failing");
                move_to_history(
                    &dav_db,
                    &item,
                    DownloadStatus::Failed,
                    Some(e.to_string()),
                    None,
                )
                .await;
            }
            status_tx.send_modify(|s| {
                s.last_error = Some(e.to_string());
            });
        }
    }
}

async fn move_to_history(
    db: &dyn DavDatabase,
    item: &QueueItem,
    status: DownloadStatus,
    fail_message: Option<String>,
    download_dir_id: Option<Uuid>,
) {
    let history = HistoryItem {
        id: Uuid::new_v4(),
        created_at: Utc::now().naive_utc(),
        file_name: item.file_name.clone(),
        job_name: item.job_name.clone(),
        category: item.category.clone(),
        download_status: status,
        total_segment_bytes: item.total_segment_bytes,
        download_time_seconds: 0,
        fail_message,
        download_dir_id,
        nzb_blob_id: Some(item.id),
    };

    if let Err(e) = db.insert_history_item(&history).await {
        error!(error = %e, "failed to insert history item");
    }
    if let Err(e) = db.delete_queue_item(item.id).await {
        error!(error = %e, "failed to delete queue item");
    }
}
