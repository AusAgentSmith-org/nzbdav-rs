use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

#[derive(Serialize, Deserialize)]
pub struct SettingsResponse {
    pub categories: String,
    pub file_blocklist: String,
    pub ensure_importable_video: bool,
    pub duplicate_nzb_behavior: String,
    pub import_strategy: String,
    pub webdav_enforce_readonly: bool,
    pub strm_base_url: String,
    pub max_concurrent_queue: usize,
}

/// GET /api/settings -- return all settings.
pub async fn get_settings(State(state): State<AppState>) -> Json<SettingsResponse> {
    Json(SettingsResponse {
        categories: state.config.get_or_default("api.categories", ""),
        file_blocklist: state
            .config
            .get_or_default("api.download-file-blocklist", ""),
        ensure_importable_video: state.config.is_ensure_importable_video_enabled(),
        duplicate_nzb_behavior: state.config.get_duplicate_nzb_behavior(),
        import_strategy: state.config.get_import_strategy(),
        webdav_enforce_readonly: state
            .config
            .get("webdav.enforce-readonly")
            .map(|v| v == "true")
            .unwrap_or(false),
        strm_base_url: state.config.get_or_default("api.strm-base-url", ""),
        max_concurrent_queue: state.config.get_max_concurrent_queue(),
    })
}

/// PUT /api/settings -- update settings.
pub async fn update_settings(
    State(state): State<AppState>,
    Json(settings): Json<SettingsResponse>,
) -> Result<Json<SettingsResponse>, StatusCode> {
    let conn = state.db.lock();
    state
        .config
        .set(&conn, "api.categories", &settings.categories)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .config
        .set(
            &conn,
            "api.download-file-blocklist",
            &settings.file_blocklist,
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .config
        .set(
            &conn,
            "api.ensure-importable-video",
            if settings.ensure_importable_video {
                "true"
            } else {
                "false"
            },
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .config
        .set(
            &conn,
            "api.duplicate-nzb-behavior",
            &settings.duplicate_nzb_behavior,
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .config
        .set(&conn, "api.import-strategy", &settings.import_strategy)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .config
        .set(
            &conn,
            "webdav.enforce-readonly",
            if settings.webdav_enforce_readonly {
                "true"
            } else {
                "false"
            },
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .config
        .set(&conn, "api.strm-base-url", &settings.strm_base_url)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .config
        .set(
            &conn,
            "queue.max-concurrent",
            &settings.max_concurrent_queue.to_string(),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    drop(conn);
    Ok(Json(settings))
}

/// GET /api/backup -- download a copy of the SQLite database.
pub async fn backup_database(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.db.lock();
    let mut backup_conn = match rusqlite::Connection::open_in_memory() {
        Ok(c) => c,
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    {
        let backup = match rusqlite::backup::Backup::new(&conn, &mut backup_conn) {
            Ok(b) => b,
            Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        };
        if backup
            .run_to_completion(100, std::time::Duration::from_millis(10), None)
            .is_err()
        {
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }
    drop(conn);

    let tmp = std::env::temp_dir().join("nzbdav_backup.db");
    let tmp_path = tmp.display().to_string();
    if backup_conn
        .execute_batch(&format!("VACUUM INTO '{tmp_path}'"))
        .is_err()
    {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    let data = match std::fs::read(&tmp) {
        Ok(d) => d,
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let _ = std::fs::remove_file(&tmp);

    Ok((
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/x-sqlite3".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"nzbdav-backup.db\"".to_string(),
            ),
        ],
        data,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
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
            .route("/api/settings", get(get_settings).put(update_settings))
            .with_state(state)
    }

    #[tokio::test]
    async fn test_get_settings_defaults() {
        let app = test_router().await;
        let resp = app
            .oneshot(Request::get("/api/settings").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let s: SettingsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(s.categories, "");
        assert!(s.ensure_importable_video);
        assert_eq!(s.duplicate_nzb_behavior, "increment");
        assert_eq!(s.import_strategy, "symlinks");
        assert!(!s.webdav_enforce_readonly);
        assert_eq!(s.max_concurrent_queue, 2);
    }

    #[tokio::test]
    async fn test_update_settings() {
        let app = test_router().await;
        let body = serde_json::json!({
            "categories": "tv, movies",
            "file_blocklist": "*.nfo",
            "ensure_importable_video": false,
            "duplicate_nzb_behavior": "reject",
            "import_strategy": "copy",
            "webdav_enforce_readonly": true,
            "strm_base_url": "http://example.com/dav",
            "max_concurrent_queue": 4,
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/settings")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let s: SettingsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(s.categories, "tv, movies");
        assert!(!s.ensure_importable_video);
        assert_eq!(s.duplicate_nzb_behavior, "reject");
        assert!(s.webdav_enforce_readonly);
        // Regression: STRM base URL must round-trip through the same config
        // key that the queue pipeline reads (`api.strm-base-url`).
        assert_eq!(s.strm_base_url, "http://example.com/dav");
    }

    /// Settings API and queue pipeline must share the same config key so the
    /// UI toggle actually controls STRM generation.
    #[tokio::test]
    async fn strm_base_url_persists_to_api_strm_base_url_key() {
        let state = test_state().await;
        {
            let conn = state.db.lock();
            state
                .config
                .set(&conn, "api.strm-base-url", "http://strm.test/dav")
                .unwrap();
        }
        let app = Router::new()
            .route("/api/settings", get(get_settings).put(update_settings))
            .with_state(state.clone());
        let resp = app
            .oneshot(Request::get("/api/settings").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let s: SettingsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(s.strm_base_url, "http://strm.test/dav");
    }
}
