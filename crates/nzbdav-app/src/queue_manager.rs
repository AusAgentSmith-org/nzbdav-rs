use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use rusqlite::Connection;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use uuid::Uuid;

use nzbdav_core::blob_store::BlobStore;
use nzbdav_core::config::ConfigManager;
use nzbdav_core::models::{DownloadStatus, HistoryItem, QueueItem};
use nzbdav_core::{history_items, queue_items};
use nzbdav_pipeline::queue_item_processor::QueueItemProcessor;
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

    let handle = std::thread::Builder::new()
        .name("queue-manager".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build queue manager runtime");

            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let processor = Arc::new(QueueItemProcessor::new(provider, config));
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

        // Update status based on active tasks
        if active.is_empty() {
            status_tx.send_modify(|s| {
                s.is_processing = false;
                s.current_job = None;
                s.active_ids.clear();
            });
        }

        // If below limit, try to grab next item
        if active.len() < max_concurrent {
            let conn = match nzbdav_core::db::open(&db_path) {
                Ok(c) => c,
                Err(e) => {
                    error!(error = %e, "failed to open DB for queue poll");
                    tokio::select! {
                        () = cancel.cancelled() => break,
                        () = tokio::time::sleep(poll_interval) => continue,
                    }
                }
            };

            let exclude: Vec<Uuid> = active_ids.iter().copied().collect();
            match queue_items::get_next_excluding(&conn, &exclude) {
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

                    continue; // Try to grab another immediately
                }
                Ok(None) => {} // Queue empty, wait
                Err(e) => {
                    error!(error = %e, "failed to fetch next queue item");
                    status_tx.send_modify(|s| s.last_error = Some(e.to_string()));
                }
            }
            drop(conn);
        }

        // Wait for a task to complete, cancellation, or poll timer
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

    // Drain remaining tasks on shutdown
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

async fn process_single_item(
    db_path: &str,
    processor: &QueueItemProcessor,
    item: QueueItem,
    status_tx: &watch::Sender<QueueStatus>,
) {
    let conn = match nzbdav_core::db::open(db_path) {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "failed to open DB for queue item processing");
            return;
        }
    };

    let job_name = item.job_name.clone();
    let item_id = item.id;

    // Load NZB blob (stored with queue item's UUID as key)
    let nzb_data = match BlobStore::get_nzb_blob(&conn, item_id) {
        Ok(data) => data,
        Err(e) => {
            error!(job_name = %job_name, error = %e, "NZB blob not found — failing");
            move_to_history(
                &conn,
                &item,
                DownloadStatus::Failed,
                Some(format!("NZB blob not found: {e}")),
                None,
            );
            status_tx.send_modify(|s| {
                s.last_error = Some(e.to_string());
            });
            return;
        }
    };

    // Run the pipeline
    let result = processor.process(&conn, &item, &nzb_data).await;

    match result {
        Ok(pr) => {
            info!(job_name = %job_name, items_created = pr.items_created, "processed successfully");
            move_to_history(
                &conn,
                &item,
                DownloadStatus::Completed,
                None,
                Some(pr.job_dir_id),
            );
            status_tx.send_modify(|s| {
                s.items_processed += 1;
                s.last_error = None;
            });
        }
        Err(e) => {
            if e.is_retryable() {
                warn!(job_name = %job_name, error = %e, "retryable error — pausing 60s");
                let pause_until = Utc::now().naive_utc() + chrono::Duration::seconds(60);
                if let Err(ue) = queue_items::update_pause_until(&conn, item_id, Some(pause_until))
                {
                    error!(error = %ue, "failed to set pause_until");
                }
            } else {
                error!(job_name = %job_name, error = %e, "non-retryable error — failing");
                move_to_history(
                    &conn,
                    &item,
                    DownloadStatus::Failed,
                    Some(e.to_string()),
                    None,
                );
            }
            status_tx.send_modify(|s| {
                s.last_error = Some(e.to_string());
            });
        }
    }
}

fn move_to_history(
    conn: &Connection,
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

    if let Err(e) = history_items::insert(conn, &history) {
        error!(error = %e, "failed to insert history item");
    }
    if let Err(e) = queue_items::delete(conn, item.id) {
        error!(error = %e, "failed to delete queue item");
    }
}
