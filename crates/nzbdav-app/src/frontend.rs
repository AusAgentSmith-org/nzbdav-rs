use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../../frontend/dist"]
struct FrontendAssets;

/// Serve the SPA index page.
pub async fn frontend_index() -> Response {
    serve_file("index.html")
}

/// Serve an embedded frontend asset, falling back to `index.html` for SPA routing.
pub async fn frontend_handler(Path(path): Path<String>) -> Response {
    // Try exact path first, then fall back to index.html for client-side routing
    if FrontendAssets::get(&path).is_some() {
        serve_file(&path)
    } else {
        serve_file("index.html")
    }
}

/// Fallback handler for unmatched routes — serves static assets or the SPA index.
pub async fn frontend_fallback(req: axum::extract::Request) -> Response {
    let path = req.uri().path().trim_start_matches('/');
    if FrontendAssets::get(path).is_some() {
        serve_file(path)
    } else {
        serve_file("index.html")
    }
}

fn serve_file(path: &str) -> Response {
    match FrontendAssets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime.as_ref().to_string()),
                    (
                        header::CACHE_CONTROL,
                        "no-cache, no-store, must-revalidate".to_string(),
                    ),
                    (header::PRAGMA, "no-cache".to_string()),
                ],
                content.data.to_vec(),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}
