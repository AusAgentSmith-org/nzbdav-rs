use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;

pub fn sab_router() -> Router<AppState> {
    Router::new()
        .route(
            "/api",
            get(super::handler::sab_api).post(super::handler::sab_api),
        )
        .route(
            "/api/history/{id}/retry",
            post(super::handler::rest_retry_history),
        )
}
