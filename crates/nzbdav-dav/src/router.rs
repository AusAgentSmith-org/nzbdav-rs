//! WebDAV router — dispatches by HTTP method via an axum fallback handler.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::handler;
use crate::store::DatabaseStore;

/// Build the WebDAV router. Mount at `/` or wherever WebDAV should serve.
pub fn dav_router(store: Arc<DatabaseStore>) -> Router {
    Router::new().fallback(dav_handler).with_state(store)
}

/// Single handler that dispatches by HTTP method.
async fn dav_handler(State(store): State<Arc<DatabaseStore>>, req: Request) -> Response {
    match req.method().as_str() {
        "PROPFIND" => handler::propfind(State(store), req).await,
        "GET" => handler::get(State(store), req).await,
        "HEAD" => handler::head(State(store), req).await,
        "PUT" => handler::put(State(store), req).await,
        "MKCOL" => handler::mkcol(State(store), req).await,
        "DELETE" => handler::delete(State(store), req).await,
        "MOVE" => handler::r#move(State(store), req).await,
        "COPY" => handler::copy(State(store), req).await,
        "OPTIONS" => handler::options().await,
        _ => (StatusCode::METHOD_NOT_ALLOWED, "Method not allowed").into_response(),
    }
}
