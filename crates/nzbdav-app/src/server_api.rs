//! REST API for managing Usenet server configurations.
//!
//! Servers are stored as JSON in the `config_items` table under key
//! `usenet.servers`. When servers are added, removed, or modified the
//! connection pools are rebuilt and the shared `UsenetArticleProvider` is
//! swapped atomically.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use nzb_nntp::{ConnectionPool, ServerConfig};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::state::AppState;

/// Wire-format for a server in the API (subset of nzb_nntp::ServerConfig).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEntry {
    pub id: String,
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_true")]
    pub ssl: bool,
    #[serde(default = "default_true")]
    pub ssl_verify: bool,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default = "default_connections")]
    pub connections: u16,
    #[serde(default)]
    pub priority: u8,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub retention: u32,
}

fn default_port() -> u16 {
    563
}
fn default_true() -> bool {
    true
}
fn default_connections() -> u16 {
    8
}

impl ServerEntry {
    fn to_server_config(&self) -> ServerConfig {
        ServerConfig {
            id: self.id.clone(),
            name: self.name.clone(),
            host: self.host.clone(),
            port: self.port,
            ssl: self.ssl,
            ssl_verify: self.ssl_verify,
            username: self.username.clone(),
            password: self.password.clone(),
            connections: self.connections,
            priority: self.priority,
            enabled: self.enabled,
            retention: self.retention,
            ..ServerConfig::default()
        }
    }
}

const SERVERS_KEY: &str = "usenet.servers";

/// Load saved servers from the config DB.
fn load_servers(state: &AppState) -> Vec<ServerEntry> {
    let json = state.config.get(SERVERS_KEY).unwrap_or_else(|| "[]".to_string());
    serde_json::from_str(&json).unwrap_or_default()
}

/// Save servers to the config DB and rebuild connection pools.
fn save_and_rebuild(state: &AppState, servers: &[ServerEntry]) {
    let json = serde_json::to_string(servers).unwrap_or_else(|_| "[]".to_string());
    let conn = state.db.lock();
    if let Err(e) = state.config.set(&conn, SERVERS_KEY, &json) {
        warn!(error = %e, "failed to save server config");
    }
    drop(conn);

    // Build new pools from enabled servers, sorted by priority
    let mut sorted: Vec<_> = servers.iter().filter(|s| s.enabled).cloned().collect();
    sorted.sort_by_key(|s| s.priority);
    let pools: Vec<Arc<ConnectionPool>> = sorted
        .iter()
        .map(|s| Arc::new(ConnectionPool::new(Arc::new(s.to_server_config()))))
        .collect();

    info!(
        count = pools.len(),
        "rebuilt connection pools from {} enabled servers",
        pools.len()
    );
    state.provider.replace_pools(pools);
}

/// GET /api/servers — list all configured servers.
pub async fn list_servers(State(state): State<AppState>) -> Json<Vec<ServerEntry>> {
    Json(load_servers(&state))
}

/// POST /api/servers — add a new server.
pub async fn add_server(
    State(state): State<AppState>,
    Json(mut entry): Json<ServerEntry>,
) -> Result<Json<ServerEntry>, StatusCode> {
    if entry.host.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if entry.id.is_empty() {
        entry.id = uuid::Uuid::new_v4().to_string();
    }
    if entry.name.is_empty() {
        entry.name = entry.host.clone();
    }

    let mut servers = load_servers(&state);
    servers.push(entry.clone());
    save_and_rebuild(&state, &servers);

    info!(id = %entry.id, name = %entry.name, host = %entry.host, "server added");
    Ok(Json(entry))
}

/// DELETE /api/servers/:id — remove a server.
pub async fn delete_server(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> StatusCode {
    let mut servers = load_servers(&state);
    let before = servers.len();
    servers.retain(|s| s.id != id);
    if servers.len() == before {
        return StatusCode::NOT_FOUND;
    }
    save_and_rebuild(&state, &servers);
    info!(id = %id, "server removed");
    StatusCode::NO_CONTENT
}

/// PUT /api/servers/:id — update a server.
pub async fn update_server(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(entry): Json<ServerEntry>,
) -> Result<Json<ServerEntry>, StatusCode> {
    let mut servers = load_servers(&state);
    let Some(existing) = servers.iter_mut().find(|s| s.id == id) else {
        return Err(StatusCode::NOT_FOUND);
    };
    *existing = ServerEntry { id, ..entry };
    let updated = existing.clone();
    save_and_rebuild(&state, &servers);
    info!(id = %updated.id, name = %updated.name, "server updated");
    Ok(Json(updated))
}

/// POST /api/servers/:id/test — test connectivity to a server.
pub async fn test_server(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<TestResult>, StatusCode> {
    let servers = load_servers(&state);
    let Some(entry) = servers.iter().find(|s| s.id == id) else {
        return Err(StatusCode::NOT_FOUND);
    };
    let config = Arc::new(entry.to_server_config());
    let pool = ConnectionPool::new(config);

    match pool.acquire().await {
        Ok(pooled) => {
            pool.release(pooled);
            Ok(Json(TestResult {
                success: true,
                message: "Connection successful".to_string(),
            }))
        }
        Err(e) => Ok(Json(TestResult {
            success: false,
            message: format!("Connection failed: {e}"),
        })),
    }
}

#[derive(Serialize)]
pub struct TestResult {
    pub success: bool,
    pub message: String,
}

/// Initialize connection pools from saved server configs at startup.
pub fn init_pools_from_config(state: &AppState) {
    let servers = load_servers(state);
    if servers.is_empty() {
        return;
    }
    let mut sorted: Vec<_> = servers.iter().filter(|s| s.enabled).cloned().collect();
    sorted.sort_by_key(|s| s.priority);
    let pools: Vec<Arc<ConnectionPool>> = sorted
        .iter()
        .map(|s| Arc::new(ConnectionPool::new(Arc::new(s.to_server_config()))))
        .collect();
    info!(
        count = pools.len(),
        "initialized {} connection pools from config",
        pools.len()
    );
    state.provider.replace_pools(pools);
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get, put};
    use axum::Router;
    use tower::ServiceExt;

    use crate::state::AppState;

    fn test_state() -> AppState {
        let conn = nzbdav_core::db::open(":memory:").unwrap();
        nzbdav_core::seed::seed_root_items(&conn).unwrap();
        let db = Arc::new(parking_lot::Mutex::new(conn));
        let config = nzbdav_core::config::ConfigManager::new();
        let provider = Arc::new(nzbdav_stream::UsenetArticleProvider::new(vec![]));
        let (_, queue_status) = tokio::sync::watch::channel(crate::queue_manager::QueueStatus::default());
        AppState {
            db,
            config,
            provider,
            version: "0.1.0-test",
            queue_status,
        }
    }

    fn test_router() -> Router {
        let state = test_state();
        Router::new()
            .route("/api/servers", get(list_servers).post(add_server))
            .route(
                "/api/servers/{id}",
                put(update_server).delete(delete_server),
            )
            .with_state(state)
    }

    fn sample_server_json() -> serde_json::Value {
        serde_json::json!({
            "id": "",
            "name": "Test Server",
            "host": "news.example.com",
            "port": 563,
            "ssl": true,
            "ssl_verify": true,
            "username": "user",
            "password": "pass",
            "connections": 8,
            "priority": 0,
            "enabled": true,
            "retention": 3000
        })
    }

    #[tokio::test]
    async fn test_list_empty() {
        let app = test_router();
        let resp = app
            .oneshot(Request::get("/api/servers").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v, serde_json::json!([]));
    }

    #[tokio::test]
    async fn test_add_server() {
        let app = test_router();
        let resp = app
            .oneshot(
                Request::post("/api/servers")
                    .header("content-type", "application/json")
                    .body(Body::from(sample_server_json().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["name"], "Test Server");
        assert_eq!(v["host"], "news.example.com");
        // Should have auto-generated an id
        assert!(!v["id"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_add_server_empty_host() {
        let app = test_router();
        let mut json = sample_server_json();
        json["host"] = serde_json::json!("");
        let resp = app
            .oneshot(
                Request::post("/api/servers")
                    .header("content-type", "application/json")
                    .body(Body::from(json.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_add_and_list() {
        // We need shared state across requests, so build state once
        let state = test_state();
        let app = Router::new()
            .route("/api/servers", get(list_servers).post(add_server))
            .with_state(state);

        // Add
        let resp = app
            .clone()
            .oneshot(
                Request::post("/api/servers")
                    .header("content-type", "application/json")
                    .body(Body::from(sample_server_json().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        // List
        let resp = app
            .oneshot(Request::get("/api/servers").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["host"], "news.example.com");
    }

    #[tokio::test]
    async fn test_delete_server() {
        let state = test_state();
        let app = Router::new()
            .route("/api/servers", get(list_servers).post(add_server))
            .route(
                "/api/servers/{id}",
                put(update_server).delete(delete_server),
            )
            .with_state(state);

        // Add
        let resp = app
            .clone()
            .oneshot(
                Request::post("/api/servers")
                    .header("content-type", "application/json")
                    .body(Body::from(sample_server_json().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let added: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = added["id"].as_str().unwrap();

        // Delete
        let resp = app
            .clone()
            .oneshot(
                Request::delete(format!("/api/servers/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NO_CONTENT);

        // List should be empty
        let resp = app
            .oneshot(Request::get("/api/servers").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_delete_not_found() {
        let app = test_router();
        let resp = app
            .oneshot(
                Request::delete("/api/servers/00000000-0000-0000-0000-000000000000")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_update_server() {
        let state = test_state();
        let app = Router::new()
            .route("/api/servers", get(list_servers).post(add_server))
            .route(
                "/api/servers/{id}",
                put(update_server).delete(delete_server),
            )
            .with_state(state);

        // Add
        let resp = app
            .clone()
            .oneshot(
                Request::post("/api/servers")
                    .header("content-type", "application/json")
                    .body(Body::from(sample_server_json().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let added: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = added["id"].as_str().unwrap();

        // Update
        let mut updated_json = sample_server_json();
        updated_json["id"] = serde_json::json!(id);
        updated_json["name"] = serde_json::json!("Updated Server");
        updated_json["host"] = serde_json::json!("updated.example.com");
        let resp = app
            .clone()
            .oneshot(
                Request::put(format!("/api/servers/{id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(updated_json.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["name"], "Updated Server");
        assert_eq!(v["host"], "updated.example.com");
    }

    #[tokio::test]
    async fn test_update_not_found() {
        let app = test_router();
        let resp = app
            .oneshot(
                Request::put("/api/servers/00000000-0000-0000-0000-000000000000")
                    .header("content-type", "application/json")
                    .body(Body::from(sample_server_json().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    }
}
