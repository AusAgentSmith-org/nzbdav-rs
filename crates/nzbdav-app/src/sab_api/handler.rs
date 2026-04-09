use axum::extract::{Multipart, Path, Query, State};
use axum::response::Json;
use chrono::Utc;
use serde::Deserialize;
use tracing::{info, warn};
use uuid::Uuid;

use nzb_core::nzb_parser::parse_nzb;
use nzbdav_core::blob_store::BlobStore;
use nzbdav_core::models::{DownloadStatus, QueueItem};
use nzbdav_core::{dav_items, history_items, queue_items};

use crate::state::AppState;

use super::models::{
    AddFileResponse, CategoriesResponse, HistoryData, HistoryResponse, HistorySlot, QueueSlot,
    SimpleResponse, VersionResponse,
};

#[derive(Deserialize)]
pub struct ApiParams {
    pub mode: String,
    #[allow(dead_code)]
    pub apikey: Option<String>,
    #[allow(dead_code)]
    pub output: Option<String>,
    // Queue/history params
    pub name: Option<String>,
    pub value: Option<String>,
    pub cat: Option<String>,
    pub priority: Option<i32>,
    pub password: Option<String>,
    pub start: Option<i64>,
    pub limit: Option<i64>,
    /// Sonarr sends del_files=1 on delete; accepted and ignored.
    #[allow(dead_code)]
    pub del_files: Option<i32>,
}

/// Main SABnzbd-compatible API endpoint. Dispatches by `mode` query parameter.
pub async fn sab_api(
    State(state): State<AppState>,
    Query(params): Query<ApiParams>,
    multipart: Option<Multipart>,
) -> Json<serde_json::Value> {
    match params.mode.as_str() {
        "addfile" => handle_addfile(state, params, multipart).await,
        "addurl" => handle_addurl(state, params).await,
        "queue" => handle_queue(state, params),
        "history" => handle_history(state, params),
        "retry" => handle_retry(state, params),
        "version" => {
            // Report SABnzbd-compatible version (>= 0.7.0) so Sonarr/Radarr accept us
            Json(
                serde_json::to_value(VersionResponse {
                    version: "4.0.0".to_string(),
                })
                .unwrap(),
            )
        }
        "status" | "fullstatus" => {
            // SABnzbd-compatible fullstatus — Sonarr parses specific nested fields
            Json(serde_json::json!({
                "status": {
                    "version": "4.0.0",
                    "paused": false,
                    "pause_int": "0",
                    "kbpersec": "0.00",
                    "speed": "0 ",
                    "mbleft": "0.00",
                    "mb": "0.00",
                    "noofslots": 0,
                    "noofslots_total": 0,
                    "timeleft": "0:00:00",
                    "eta": "unknown",
                    "diskspace1": "999.99",
                    "diskspacetotal1": "999.99",
                    "diskspace2": "999.99",
                    "diskspacetotal2": "999.99",
                    "have_warnings": "0",
                    "loadavg": "",
                    "cache_art": "0",
                    "cache_size": "0 B",
                    "finishaction": null,
                    "quota": "",
                    "left_quota": "",
                    "speedlimit": "",
                    "servers": []
                }
            }))
        }
        "get_config" => {
            // SABnzbd-compatible config structure — Sonarr parses misc and servers
            let categories = state.config.get_categories();
            let cats: Vec<serde_json::Value> = categories.iter()
                .map(|c| serde_json::json!({"name": c, "order": 0, "dir": "", "newzbin": "", "priority": 0}))
                .collect();
            Json(serde_json::json!({
                "config": {
                    "misc": {
                        "port": "8080",
                        "host": "0.0.0.0",
                        "api_key": "",
                        "complete_dir": "/content",
                        "dirscan_dir": "",
                        "download_dir": "/content",
                        "history_retention": "all",
                        "pause_on_post_processing": 0,
                        "pre_check": 0,
                        "quota_size": "",
                        "quota_day": "",
                        "quota_period": "m",
                        "refresh_rate": 1,
                        "bandwidth_max": "",
                        "cache_limit": "512M"
                    },
                    "categories": cats,
                    "servers": []
                }
            }))
        }
        "get_cats" => {
            let categories = state.config.get_categories();
            Json(serde_json::to_value(CategoriesResponse { categories }).unwrap())
        }
        unknown => {
            warn!(mode = %unknown, "unknown SAB API mode");
            Json(
                serde_json::to_value(SimpleResponse {
                    status: false,
                    error: Some(format!("unknown mode: {unknown}")),
                })
                .unwrap(),
            )
        }
    }
}

/// Handle `mode=addfile` -- accept NZB upload via multipart form.
async fn handle_addfile(
    state: AppState,
    params: ApiParams,
    multipart: Option<Multipart>,
) -> Json<serde_json::Value> {
    let Some(mut multipart) = multipart else {
        return Json(
            serde_json::to_value(SimpleResponse {
                status: false,
                error: Some("multipart body required for addfile".to_string()),
            })
            .unwrap(),
        );
    };

    // Extract the NZB file from multipart fields.
    let mut nzb_data: Option<Vec<u8>> = None;
    let mut nzb_filename: Option<String> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name = field.name().unwrap_or("").to_string();
        if field_name == "nzbfile" || field_name == "name" {
            nzb_filename = field.file_name().map(|s| s.to_string());
            match field.bytes().await {
                Ok(bytes) => nzb_data = Some(bytes.to_vec()),
                Err(e) => {
                    warn!(error = %e, "failed to read multipart field");
                    return Json(
                        serde_json::to_value(SimpleResponse {
                            status: false,
                            error: Some(format!("failed to read upload: {e}")),
                        })
                        .unwrap(),
                    );
                }
            }
            break;
        }
    }

    let Some(data) = nzb_data else {
        return Json(
            serde_json::to_value(SimpleResponse {
                status: false,
                error: Some("no NZB file found in upload".to_string()),
            })
            .unwrap(),
        );
    };

    let filename = nzb_filename.unwrap_or_else(|| "unknown.nzb".to_string());
    let category = params.cat.unwrap_or_default();
    let priority = params.priority.unwrap_or(0);

    enqueue_nzb(
        state,
        &filename,
        &data,
        &category,
        priority,
        params.password.as_deref(),
    )
}

/// Handle `mode=addurl` -- fetch NZB from a URL and enqueue it.
async fn handle_addurl(state: AppState, params: ApiParams) -> Json<serde_json::Value> {
    let Some(url) = params.name.or(params.value) else {
        return Json(
            serde_json::to_value(SimpleResponse {
                status: false,
                error: Some("URL is required (pass as 'name' or 'value' parameter)".to_string()),
            })
            .unwrap(),
        );
    };

    let client = reqwest::Client::new();
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, url = %url, "failed to fetch NZB from URL");
            return Json(
                serde_json::to_value(SimpleResponse {
                    status: false,
                    error: Some(format!("failed to fetch URL: {e}")),
                })
                .unwrap(),
            );
        }
    };

    if !resp.status().is_success() {
        return Json(
            serde_json::to_value(SimpleResponse {
                status: false,
                error: Some(format!("HTTP {} fetching URL", resp.status())),
            })
            .unwrap(),
        );
    }

    let data = match resp.bytes().await {
        Ok(b) => b.to_vec(),
        Err(e) => {
            warn!(error = %e, "failed to read NZB response body");
            return Json(
                serde_json::to_value(SimpleResponse {
                    status: false,
                    error: Some(format!("failed to read response: {e}")),
                })
                .unwrap(),
            );
        }
    };

    // Extract filename from URL path, falling back to "download.nzb".
    let filename = url
        .rsplit('/')
        .next()
        .filter(|s| s.ends_with(".nzb"))
        .unwrap_or("download.nzb")
        .to_string();

    let category = params.cat.unwrap_or_default();
    let priority = params.priority.unwrap_or(0);

    enqueue_nzb(
        state,
        &filename,
        &data,
        &category,
        priority,
        params.password.as_deref(),
    )
}

/// Shared logic: parse an NZB, store it, and insert a queue item.
fn enqueue_nzb(
    state: AppState,
    filename: &str,
    data: &[u8],
    category: &str,
    priority: i32,
    password: Option<&str>,
) -> Json<serde_json::Value> {
    // Embed password in the filename using {{password}} convention so
    // the pipeline's get_nzb_password() picks it up during processing.
    let stored_filename = match password {
        Some(pw) if !pw.is_empty() && nzbdav_core::util::get_nzb_password(filename).is_none() => {
            let base = filename.trim_end_matches(".nzb");
            format!("{base} {{{{{pw}}}}}.nzb")
        }
        _ => filename.to_string(),
    };
    let job_name = nzbdav_core::util::get_job_name(&stored_filename);
    let nzb_job = match parse_nzb(&job_name, data) {
        Ok(job) => job,
        Err(e) => {
            warn!(error = %e, filename = %filename, "failed to parse NZB");
            return Json(
                serde_json::to_value(SimpleResponse {
                    status: false,
                    error: Some(format!("invalid NZB: {e}")),
                })
                .unwrap(),
            );
        }
    };

    let total_segment_bytes: u64 = nzb_job.files.iter().map(|f| f.bytes).sum();
    let item_id = Uuid::new_v4();

    let item = QueueItem {
        id: item_id,
        created_at: Utc::now().naive_utc(),
        file_name: stored_filename.clone(),
        job_name,
        nzb_file_size: data.len() as i64,
        total_segment_bytes: total_segment_bytes as i64,
        category: category.to_string(),
        priority,
        post_processing: -1,
        pause_until: None,
    };

    {
        let conn = state.db.lock();
        if let Err(e) = BlobStore::put_nzb_blob(&conn, item_id, data) {
            warn!(error = %e, "failed to store NZB blob");
            return Json(
                serde_json::to_value(SimpleResponse {
                    status: false,
                    error: Some(format!("failed to store NZB: {e}")),
                })
                .unwrap(),
            );
        }
        if let Err(e) = queue_items::insert(&conn, &item) {
            warn!(error = %e, "failed to insert queue item");
            return Json(
                serde_json::to_value(SimpleResponse {
                    status: false,
                    error: Some(format!("failed to enqueue: {e}")),
                })
                .unwrap(),
            );
        }
    }

    info!(
        nzo_id = %item.id,
        filename = %stored_filename,
        total_segment_bytes,
        has_password = password.is_some(),
        "NZB added to queue"
    );

    Json(
        serde_json::to_value(AddFileResponse {
            status: true,
            nzo_ids: vec![item.id.to_string()],
        })
        .unwrap(),
    )
}

/// Handle `mode=queue` -- list queue or delete item.
fn handle_queue(state: AppState, params: ApiParams) -> Json<serde_json::Value> {
    let conn = state.db.lock();

    // Delete action.
    if params.name.as_deref() == Some("delete") || params.name.as_deref() == Some("purge") {
        if let Some(value) = &params.value {
            let id = match Uuid::parse_str(value) {
                Ok(id) => id,
                Err(_) => {
                    return Json(
                        serde_json::to_value(SimpleResponse {
                            status: false,
                            error: Some("invalid id".to_string()),
                        })
                        .unwrap(),
                    );
                }
            };
            if let Err(e) = queue_items::delete(&conn, id) {
                warn!(error = %e, %id, "failed to delete queue item");
                return Json(
                    serde_json::to_value(SimpleResponse {
                        status: false,
                        error: Some(format!("database error: {e}")),
                    })
                    .unwrap(),
                );
            }
        }
        return Json(
            serde_json::to_value(SimpleResponse {
                status: true,
                error: None,
            })
            .unwrap(),
        );
    }

    // List queue with pagination.
    let total = match queue_items::count(&conn) {
        Ok(c) => c as usize,
        Err(e) => {
            warn!(error = %e, "failed to count queue");
            return Json(
                serde_json::to_value(SimpleResponse {
                    status: false,
                    error: Some(format!("database error: {e}")),
                })
                .unwrap(),
            );
        }
    };
    let offset = params.start.unwrap_or(0);
    let limit = params.limit.unwrap_or(50);
    let items = match queue_items::list_paginated(&conn, offset, limit) {
        Ok(items) => items,
        Err(e) => {
            warn!(error = %e, "failed to list queue");
            return Json(
                serde_json::to_value(SimpleResponse {
                    status: false,
                    error: Some(format!("database error: {e}")),
                })
                .unwrap(),
            );
        }
    };
    let qs = state.queue_status.borrow();
    let slots: Vec<QueueSlot> = items
        .iter()
        .map(|item| {
            let mb = format!("{:.2}", item.total_segment_bytes as f64 / 1_048_576.0);
            let status = if qs.active_ids.contains(&item.id) {
                "Processing".to_string()
            } else {
                "Queued".to_string()
            };
            QueueSlot {
                nzo_id: item.id.to_string(),
                filename: item.job_name.clone(),
                cat: item.category.clone(),
                priority: priority_to_string(item.priority),
                status,
                mb,
                mbleft: "0.00".to_string(),
                percentage: "100".to_string(),
                timeleft: "0:00:00".to_string(),
            }
        })
        .collect();
    drop(qs);

    let page_count = slots.len();
    // SABnzbd-compatible queue with all fields Sonarr expects
    Json(serde_json::json!({
        "queue": {
            "status": if total > 0 { "Downloading" } else { "Idle" },
            "paused": false,
            "noofslots": page_count,
            "noofslots_total": total,
            "speed": "0",
            "kbpersec": "0.00",
            "size": "0 B",
            "sizeleft": "0 B",
            "mbleft": "0.00",
            "mb": "0.00",
            "timeleft": "0:00:00",
            "eta": "unknown",
            "diskspace1": "999.99",
            "diskspacetotal1": "999.99",
            "slots": slots.iter().map(|s| serde_json::to_value(s).unwrap()).collect::<Vec<_>>(),
        }
    }))
}

/// Handle `mode=history` -- list history or delete item.
fn handle_history(state: AppState, params: ApiParams) -> Json<serde_json::Value> {
    let conn = state.db.lock();

    // Delete action.
    if params.name.as_deref() == Some("delete") {
        if let Some(value) = &params.value {
            if value == "all" {
                if let Err(e) = history_items::delete_all(&conn) {
                    warn!(error = %e, "failed to clear history");
                    return Json(
                        serde_json::to_value(SimpleResponse {
                            status: false,
                            error: Some(format!("database error: {e}")),
                        })
                        .unwrap(),
                    );
                }
            } else {
                let id = match Uuid::parse_str(value) {
                    Ok(id) => id,
                    Err(_) => {
                        return Json(
                            serde_json::to_value(SimpleResponse {
                                status: false,
                                error: Some("invalid id".to_string()),
                            })
                            .unwrap(),
                        );
                    }
                };
                if let Err(e) = history_items::delete(&conn, id) {
                    warn!(error = %e, %id, "failed to delete history item");
                    return Json(
                        serde_json::to_value(SimpleResponse {
                            status: false,
                            error: Some(format!("database error: {e}")),
                        })
                        .unwrap(),
                    );
                }
            }
        }
        return Json(
            serde_json::to_value(SimpleResponse {
                status: true,
                error: None,
            })
            .unwrap(),
        );
    }

    // List history with pagination.
    let offset = params.start.unwrap_or(0);
    let limit = params.limit.unwrap_or(50);
    let items = match history_items::list(&conn, offset, limit) {
        Ok(items) => items,
        Err(e) => {
            warn!(error = %e, "failed to list history");
            return Json(
                serde_json::to_value(SimpleResponse {
                    status: false,
                    error: Some(format!("database error: {e}")),
                })
                .unwrap(),
            );
        }
    };
    let total = match history_items::count(&conn) {
        Ok(c) => c as usize,
        Err(e) => {
            warn!(error = %e, "failed to count history");
            return Json(
                serde_json::to_value(SimpleResponse {
                    status: false,
                    error: Some(format!("database error: {e}")),
                })
                .unwrap(),
            );
        }
    };

    let slots: Vec<HistorySlot> = items
        .iter()
        .map(|item| {
            let status = match item.download_status {
                DownloadStatus::Completed => "Completed",
                DownloadStatus::Failed => "Failed",
            };
            let storage = item
                .download_dir_id
                .and_then(|id| dav_items::get_by_id(&conn, id).ok().flatten())
                .map(|dav| dav.path)
                .unwrap_or_default();
            HistorySlot {
                nzo_id: item.id.to_string(),
                name: item.job_name.clone(),
                category: item.category.clone(),
                status: status.to_string(),
                fail_message: item.fail_message.clone().unwrap_or_default(),
                bytes: item.total_segment_bytes,
                download_time: item.download_time_seconds,
                completed: item.created_at.and_utc().timestamp(),
                storage,
            }
        })
        .collect();

    Json(
        serde_json::to_value(HistoryResponse {
            history: HistoryData {
                noofslots: total,
                slots,
            },
        })
        .unwrap(),
    )
}

/// Handle `mode=retry` -- re-queue a history item.
fn handle_retry(state: AppState, params: ApiParams) -> Json<serde_json::Value> {
    let Some(value) = &params.value else {
        return Json(
            serde_json::to_value(SimpleResponse {
                status: false,
                error: Some("value parameter (nzo_id) is required".to_string()),
            })
            .unwrap(),
        );
    };

    let id = match Uuid::parse_str(value) {
        Ok(id) => id,
        Err(_) => {
            return Json(
                serde_json::to_value(SimpleResponse {
                    status: false,
                    error: Some("invalid id".to_string()),
                })
                .unwrap(),
            );
        }
    };

    retry_history_item(&state, id)
}

/// REST endpoint: POST /api/history/{id}/retry
pub async fn rest_retry_history(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return Json(
                serde_json::to_value(SimpleResponse {
                    status: false,
                    error: Some("invalid id".to_string()),
                })
                .unwrap(),
            );
        }
    };

    retry_history_item(&state, id)
}

/// Shared retry logic: look up history item, get NZB blob, re-enqueue, delete history entry.
fn retry_history_item(state: &AppState, id: Uuid) -> Json<serde_json::Value> {
    let conn = state.db.lock();

    let history_item = match history_items::get_by_id(&conn, id) {
        Ok(Some(item)) => item,
        Ok(None) => {
            return Json(
                serde_json::to_value(SimpleResponse {
                    status: false,
                    error: Some("history item not found".to_string()),
                })
                .unwrap(),
            );
        }
        Err(e) => {
            warn!(error = %e, %id, "failed to look up history item");
            return Json(
                serde_json::to_value(SimpleResponse {
                    status: false,
                    error: Some(format!("database error: {e}")),
                })
                .unwrap(),
            );
        }
    };

    let blob_id = match history_item.nzb_blob_id {
        Some(bid) => bid,
        None => {
            return Json(
                serde_json::to_value(SimpleResponse {
                    status: false,
                    error: Some("NZB blob not available for this history item".to_string()),
                })
                .unwrap(),
            );
        }
    };

    let nzb_data = match BlobStore::get_nzb_blob(&conn, blob_id) {
        Ok(data) => data,
        Err(e) => {
            warn!(error = %e, %blob_id, "NZB blob missing for retry");
            return Json(
                serde_json::to_value(SimpleResponse {
                    status: false,
                    error: Some(
                        "NZB blob has been cleaned up and is no longer available".to_string(),
                    ),
                })
                .unwrap(),
            );
        }
    };

    let new_id = Uuid::new_v4();
    let queue_item = QueueItem {
        id: new_id,
        created_at: Utc::now().naive_utc(),
        file_name: history_item.file_name.clone(),
        job_name: history_item.job_name.clone(),
        nzb_file_size: nzb_data.len() as i64,
        total_segment_bytes: history_item.total_segment_bytes,
        category: history_item.category.clone(),
        priority: 0,
        post_processing: -1,
        pause_until: None,
    };

    if let Err(e) = BlobStore::put_nzb_blob(&conn, new_id, &nzb_data) {
        warn!(error = %e, "failed to store NZB blob for retry");
        return Json(
            serde_json::to_value(SimpleResponse {
                status: false,
                error: Some(format!("failed to store NZB: {e}")),
            })
            .unwrap(),
        );
    }

    if let Err(e) = queue_items::insert(&conn, &queue_item) {
        warn!(error = %e, "failed to insert retried queue item");
        return Json(
            serde_json::to_value(SimpleResponse {
                status: false,
                error: Some(format!("failed to enqueue: {e}")),
            })
            .unwrap(),
        );
    }

    if let Err(e) = history_items::delete(&conn, id) {
        warn!(error = %e, %id, "failed to delete history item after retry");
        // Non-fatal: item is re-queued, history entry is stale but harmless.
    }

    info!(
        old_nzo_id = %id,
        new_nzo_id = %new_id,
        job_name = %queue_item.job_name,
        "history item retried and re-queued"
    );

    Json(
        serde_json::to_value(AddFileResponse {
            status: true,
            nzo_ids: vec![new_id.to_string()],
        })
        .unwrap(),
    )
}

/// Convert integer priority to SABnzbd string.
fn priority_to_string(priority: i32) -> String {
    match priority {
        p if p <= 0 => "Low".to_string(),
        1 => "Normal".to_string(),
        2 => "High".to_string(),
        _ => "Force".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get, post};
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn test_state() -> AppState {
        let conn = nzbdav_core::db::open(":memory:").unwrap();
        let db = Arc::new(parking_lot::Mutex::new(conn));
        let sqlite_db = nzbdav_core::sqlite_db::SqliteDavDatabase::new(Arc::clone(&db));
        nzbdav_core::seed::seed_root_items(&sqlite_db)
            .await
            .unwrap();
        let config = nzbdav_core::config::ConfigManager::new();
        let provider = Arc::new(nzbdav_stream::UsenetArticleProvider::new(vec![]));
        let (_, queue_status) =
            tokio::sync::watch::channel(crate::queue_manager::QueueStatus::default());
        AppState {
            db,
            config,
            provider,
            version: "0.1.0-test",
            queue_status,
        }
    }

    async fn test_router() -> Router {
        let state = test_state().await;
        Router::new()
            .route("/api", get(sab_api).post(sab_api))
            .route("/api/history/{id}/retry", post(rest_retry_history))
            .with_state(state)
    }

    async fn get_json(uri: &str) -> serde_json::Value {
        let app = test_router().await;
        let resp = app
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn test_version() {
        let v = get_json("/api?mode=version").await;
        // Reports SABnzbd-compatible version (4.0.0) for Sonarr/Radarr compatibility
        assert_eq!(v["version"], "4.0.0");
    }

    #[tokio::test]
    async fn test_status() {
        let v = get_json("/api?mode=status").await;
        // fullstatus wraps in a "status" object for SABnzbd compatibility
        assert_eq!(v["status"]["version"], "4.0.0");
        assert_eq!(v["status"]["paused"], false);
    }

    #[tokio::test]
    async fn test_fullstatus() {
        let v = get_json("/api?mode=fullstatus").await;
        assert!(v["status"].is_object());
        assert_eq!(v["status"]["version"], "4.0.0");
    }

    #[tokio::test]
    async fn test_get_cats_empty() {
        let v = get_json("/api?mode=get_cats").await;
        let cats = v["categories"].as_array().unwrap();
        assert!(cats.is_empty());
    }

    #[tokio::test]
    async fn test_queue_empty() {
        let v = get_json("/api?mode=queue").await;
        assert_eq!(v["queue"]["noofslots"], 0);
        assert_eq!(v["queue"]["noofslots_total"], 0);
        assert_eq!(v["queue"]["status"], "Idle");
        assert!(v["queue"]["slots"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_history_empty() {
        let v = get_json("/api?mode=history").await;
        assert_eq!(v["history"]["noofslots"], 0);
        assert!(v["history"]["slots"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_unknown_mode() {
        let v = get_json("/api?mode=bogus").await;
        assert_eq!(v["status"], false);
        assert!(v["error"].as_str().unwrap().contains("unknown mode"));
    }

    #[tokio::test]
    async fn test_addurl_missing_url() {
        let v = get_json("/api?mode=addurl").await;
        assert_eq!(v["status"], false);
        assert!(v["error"].as_str().unwrap().contains("URL is required"));
    }

    #[tokio::test]
    async fn test_get_config() {
        let v = get_json("/api?mode=get_config").await;
        assert!(v["config"].is_object());
    }

    #[tokio::test]
    async fn test_addfile_no_multipart() {
        let app = test_router().await;
        let resp = app
            .oneshot(
                Request::post("/api?mode=addfile")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["status"], false);
        assert!(v["error"].as_str().unwrap().contains("multipart"));
    }

    #[test]
    fn test_priority_to_string_values() {
        assert_eq!(priority_to_string(-1), "Low");
        assert_eq!(priority_to_string(0), "Low");
        assert_eq!(priority_to_string(1), "Normal");
        assert_eq!(priority_to_string(2), "High");
        assert_eq!(priority_to_string(3), "Force");
        assert_eq!(priority_to_string(100), "Force");
    }

    #[tokio::test]
    async fn test_queue_delete_nonexistent_returns_success() {
        let random_id = Uuid::new_v4();
        let v = get_json(&format!("/api?mode=queue&name=delete&value={random_id}")).await;
        assert_eq!(v["status"], true);
        assert!(v.get("error").is_none() || v["error"].is_null());
    }

    #[tokio::test]
    async fn test_queue_delete_invalid_uuid_returns_json_error() {
        let v = get_json("/api?mode=queue&name=delete&value=not-a-uuid").await;
        assert_eq!(v["status"], false);
        assert!(v["error"].as_str().unwrap().contains("invalid id"));
    }

    #[tokio::test]
    async fn test_history_delete_nonexistent_returns_success() {
        let random_id = Uuid::new_v4();
        let v = get_json(&format!("/api?mode=history&name=delete&value={random_id}")).await;
        assert_eq!(v["status"], true);
        assert!(v.get("error").is_none() || v["error"].is_null());
    }

    #[tokio::test]
    async fn test_history_delete_invalid_uuid_returns_json_error() {
        let v = get_json("/api?mode=history&name=delete&value=not-a-uuid").await;
        assert_eq!(v["status"], false);
        assert!(v["error"].as_str().unwrap().contains("invalid id"));
    }

    #[tokio::test]
    async fn test_retry_missing_value() {
        let v = get_json("/api?mode=retry").await;
        assert_eq!(v["status"], false);
        assert!(v["error"].as_str().unwrap().contains("value"));
    }

    #[tokio::test]
    async fn test_retry_invalid_uuid() {
        let v = get_json("/api?mode=retry&value=garbage").await;
        assert_eq!(v["status"], false);
        assert!(v["error"].as_str().unwrap().contains("invalid id"));
    }

    #[tokio::test]
    async fn test_retry_nonexistent_history_item() {
        let random_id = Uuid::new_v4();
        let v = get_json(&format!("/api?mode=retry&value={random_id}")).await;
        assert_eq!(v["status"], false);
        assert!(v["error"].as_str().unwrap().contains("not found"));
    }
}
