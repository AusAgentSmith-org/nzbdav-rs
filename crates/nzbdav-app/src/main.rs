//! nzbdav-rs -- Usenet virtual filesystem with WebDAV serving.

mod auth;
mod cli;
mod frontend;
mod log_capture;
mod queue_manager;
mod sab_api;
mod server_api;
mod settings_api;
mod state;
mod status_broadcaster;
mod websocket;

use std::sync::Arc;

use axum::extract::State;
use axum::response::Json;
use axum::Router;
use axum::middleware;
use axum::routing::get;
use clap::Parser;
use parking_lot::Mutex;
use tokio::net::TcpListener;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

use nzbdav_core::config::ConfigManager;
use nzbdav_core::{db, seed};
use nzbdav_dav::{DatabaseStore, dav_router};
use nzbdav_stream::UsenetArticleProvider;

use cli::Cli;
use log_capture::{CaptureLayer, LogBuffer};
use state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // 1. Log capture buffer (created before tracing init)
    let log_buffer = LogBuffer::new();

    // 2. Tracing — fmt layer + capture layer
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&cli.log_level));
    // We'll set up the capture layer's WS sender after creating the WS manager.
    // For now, init with just the buffer capture (no WS broadcast yet).
    let capture_layer = CaptureLayer::new(log_buffer.clone());
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .with(capture_layer)
        .init();

    tracing::info!("nzbdav-rs starting");

    // 3. Database
    let conn = db::open(&cli.db_path)?;
    seed::seed_root_items(&conn)?;
    let config = ConfigManager::new();
    config.load_from_db(&conn)?;
    let db = Arc::new(Mutex::new(conn));

    // 4. Usenet provider
    let provider = Arc::new(UsenetArticleProvider::new(vec![]));

    // 5. Build shared state
    let app_state = AppState {
        db: db.clone(),
        config: config.clone(),
        provider: provider.clone(),
        version: env!("CARGO_PKG_VERSION"),
    };

    let dav_store = Arc::new(DatabaseStore::new(
        db.clone(),
        provider.clone(),
        config.get_article_buffer_size(),
    ));

    // 6. Load saved server configs
    server_api::init_pools_from_config(&app_state);

    // 7. Background tasks
    let cancel = CancellationToken::new();

    let qm_db_path = cli.db_path.clone();
    let (_qm_thread, qm_status_rx) = queue_manager::spawn_queue_manager(
        qm_db_path,
        provider.clone(),
        config.clone(),
        cancel.clone(),
    );

    // 8. WebSocket + status broadcaster
    let ws_manager = websocket::WebSocketManager::new();
    let ws_tx = ws_manager.sender();
    let ws_tx_for_route = ws_tx.clone();
    let broadcaster_cancel = cancel.clone();
    tokio::spawn(async move {
        status_broadcaster::run_status_broadcaster(qm_status_rx, ws_tx, broadcaster_cancel).await;
    });

    // 9. Build router (pass log_buffer for the /api/logs endpoint)
    let app = build_router(app_state, dav_store, ws_tx_for_route, log_buffer, &cli);

    // 10. Start server
    let addr = format!("{}:{}", cli.host, cli.port);
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("Listening on {addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    cancel.cancel();
    tracing::info!("Shutdown complete");
    Ok(())
}

/// GET /api/logs — return recent log lines as JSON array.
async fn get_logs(State(buffer): State<LogBuffer>) -> Json<Vec<log_capture::LogLine>> {
    Json(buffer.get_all())
}

/// GET /api/debug/stream/:message_id — fetch a single segment and return raw bytes.
/// Used to verify the streaming layer works in isolation.
async fn debug_stream_segment(
    State(state): State<AppState>,
    axum::extract::Path(message_id): axum::extract::Path<String>,
) -> axum::response::Response {
    use axum::body::Body;
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;
    use tokio_util::io::ReaderStream;

    tracing::info!(message_id = %message_id, "debug: fetching single segment");

    // Test 1: Direct fetch via provider
    match state.provider.fetch_decoded(&message_id).await {
        Ok(data) => {
            let len = data.len();
            let first16 = data.iter().take(16).map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ");
            let zeros = data.iter().filter(|&&b| b == 0).count();
            tracing::info!(bytes = len, first_16 = %first16, zeros, "debug: direct fetch OK");

            // Test 2: Also test via MultiSegmentStream
            let stream = nzbdav_stream::MultiSegmentStream::new(
                state.provider.clone(),
                vec![message_id.clone()],
                2,
            );
            let body = Body::from_stream(ReaderStream::new(stream));

            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/octet-stream")],
                body,
            ).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "debug: fetch failed");
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Fetch failed: {e}")).into_response()
        }
    }
}

fn build_router(
    state: AppState,
    dav_store: Arc<DatabaseStore>,
    ws_tx: tokio::sync::broadcast::Sender<websocket::WsEvent>,
    log_buffer: LogBuffer,
    cli: &Cli,
) -> Router {
    use axum::routing::{post, put};

    // SAB API
    let api_key = cli.api_key.clone();
    let sab = sab_api::router::sab_router().layer(middleware::from_fn(move |req, next| {
        let key = api_key.clone();
        async move { auth::api_key_auth(key, req, next).await }
    }));

    // Server management REST API
    let servers = Router::new()
        .route("/api/servers", get(server_api::list_servers).post(server_api::add_server))
        .route("/api/servers/{id}", put(server_api::update_server).delete(server_api::delete_server))
        .route("/api/servers/{id}/test", post(server_api::test_server));

    // Debug endpoint for stream testing
    let debug_routes = Router::new()
        .route("/api/debug/stream/{message_id}", get(debug_stream_segment));

    // Settings + backup API
    let settings = Router::new()
        .route("/api/settings", get(settings_api::get_settings).put(settings_api::update_settings))
        .route("/api/backup", get(settings_api::backup_database));

    // Logs API
    let logs = Router::new()
        .route("/api/logs", get(get_logs))
        .with_state(log_buffer);

    // WebSocket
    let ws_route = Router::new()
        .route("/ws", get(websocket::WebSocketManager::ws_handler))
        .with_state(ws_tx);

    // WebDAV
    let webdav_user = cli.webdav_user.clone();
    let webdav_pass = cli.webdav_pass.clone();
    let dav = dav_router(dav_store).layer(middleware::from_fn(move |req, next| {
        let user = webdav_user.clone();
        let pass = webdav_pass.clone();
        async move { auth::basic_auth(user, pass, req, next).await }
    }));

    Router::new()
        .merge(sab.with_state(state.clone()))
        .merge(servers.with_state(state.clone()))
        .merge(settings.with_state(state.clone()))
        .merge(debug_routes.with_state(state.clone()))
        .merge(logs)
        .merge(ws_route)
        .nest("/dav", dav)
        .route("/", get(frontend::frontend_index))
        .fallback(get(frontend::frontend_fallback))
}

async fn shutdown_signal() {
    let ctrl_c = signal::ctrl_c();
    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => { tracing::info!("Ctrl+C received"); }
        _ = terminate => { tracing::info!("SIGTERM received"); }
    }
}
