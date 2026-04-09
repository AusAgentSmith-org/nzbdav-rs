use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// API key auth middleware for the SAB API.
///
/// Checks `apikey` query param or `X-Api-Key` header.
/// If `expected` is `None`, all requests pass through (no auth configured).
pub async fn api_key_auth(
    expected: Option<String>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(expected_key) = expected else {
        return Ok(next.run(req).await);
    };

    // Check query parameter first
    if let Some(query) = req.uri().query() {
        for pair in query.split('&') {
            if let Some(value) = pair.strip_prefix("apikey=") {
                let decoded = percent_encoding::percent_decode_str(value).decode_utf8_lossy();
                if decoded == expected_key {
                    return Ok(next.run(req).await);
                }
            }
        }
    }

    // Check X-Api-Key header
    if let Some(header_val) = req.headers().get("x-api-key")
        && let Ok(val) = header_val.to_str()
        && val == expected_key
    {
        return Ok(next.run(req).await);
    }

    Err(StatusCode::UNAUTHORIZED)
}

/// Basic auth middleware for WebDAV.
///
/// Decodes `Authorization: Basic base64(user:pass)` header.
/// If neither `expected_user` nor `expected_pass` is configured, all requests
/// pass through.
pub async fn basic_auth(
    expected_user: Option<String>,
    expected_pass: Option<String>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    // No auth configured -- pass through
    if expected_user.is_none() && expected_pass.is_none() {
        return Ok(next.run(req).await);
    }

    let unauthorized = || {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"nzbdav\"")],
            "Unauthorized",
        )
            .into_response()
    };

    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let Some(auth_header) = auth_header else {
        return Ok(unauthorized());
    };

    let Some(encoded) = auth_header.strip_prefix("Basic ") else {
        return Ok(unauthorized());
    };

    let decoded = STANDARD
        .decode(encoded.trim())
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let decoded_str = String::from_utf8(decoded).map_err(|_| StatusCode::BAD_REQUEST)?;

    let Some((user, pass)) = decoded_str.split_once(':') else {
        return Ok(unauthorized());
    };

    let user_ok = expected_user
        .as_ref()
        .is_none_or(|expected| expected == user);
    let pass_ok = expected_pass
        .as_ref()
        .is_none_or(|expected| expected == pass);

    if user_ok && pass_ok {
        Ok(next.run(req).await)
    } else {
        Ok(unauthorized())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::middleware;
    use axum::routing::get;
    use tower::ServiceExt;

    /// A trivial handler that returns 200 OK if the middleware passes.
    async fn ok_handler() -> &'static str {
        "ok"
    }

    // -- api_key_auth tests --

    fn api_key_router(expected: Option<String>) -> Router {
        Router::new()
            .route("/test", get(ok_handler))
            .layer(middleware::from_fn(move |req, next| {
                let key = expected.clone();
                async move { api_key_auth(key, req, next).await }
            }))
    }

    #[tokio::test]
    async fn test_api_key_no_key_configured() {
        let app = api_key_router(None);
        let resp = app
            .oneshot(Request::get("/test").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_api_key_valid_query_param() {
        let app = api_key_router(Some("secret123".to_string()));
        let resp = app
            .oneshot(
                Request::get("/test?apikey=secret123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_api_key_valid_header() {
        let app = api_key_router(Some("secret123".to_string()));
        let resp = app
            .oneshot(
                Request::get("/test")
                    .header("x-api-key", "secret123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_api_key_invalid() {
        let app = api_key_router(Some("secret123".to_string()));
        let resp = app
            .oneshot(
                Request::get("/test?apikey=wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_api_key_missing_when_required() {
        let app = api_key_router(Some("secret123".to_string()));
        let resp = app
            .oneshot(Request::get("/test").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // -- basic_auth tests --

    fn basic_auth_router(user: Option<String>, pass: Option<String>) -> Router {
        Router::new()
            .route("/test", get(ok_handler))
            .layer(middleware::from_fn(move |req, next| {
                let u = user.clone();
                let p = pass.clone();
                async move { basic_auth(u, p, req, next).await }
            }))
    }

    fn encode_basic(user: &str, pass: &str) -> String {
        format!("Basic {}", STANDARD.encode(format!("{user}:{pass}")))
    }

    #[tokio::test]
    async fn test_basic_auth_no_creds_configured() {
        let app = basic_auth_router(None, None);
        let resp = app
            .oneshot(Request::get("/test").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_basic_auth_valid() {
        let app = basic_auth_router(Some("admin".to_string()), Some("pass".to_string()));
        let resp = app
            .oneshot(
                Request::get("/test")
                    .header("authorization", encode_basic("admin", "pass"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_basic_auth_invalid_password() {
        let app = basic_auth_router(Some("admin".to_string()), Some("pass".to_string()));
        let resp = app
            .oneshot(
                Request::get("/test")
                    .header("authorization", encode_basic("admin", "wrong"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_basic_auth_invalid_user() {
        let app = basic_auth_router(Some("admin".to_string()), Some("pass".to_string()));
        let resp = app
            .oneshot(
                Request::get("/test")
                    .header("authorization", encode_basic("wrong", "pass"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_basic_auth_missing_header() {
        let app = basic_auth_router(Some("admin".to_string()), Some("pass".to_string()));
        let resp = app
            .oneshot(Request::get("/test").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_basic_auth_only_user_configured() {
        let app = basic_auth_router(Some("admin".to_string()), None);
        let resp = app
            .oneshot(
                Request::get("/test")
                    .header("authorization", encode_basic("admin", "anything"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_basic_auth_only_pass_configured() {
        let app = basic_auth_router(None, Some("secret".to_string()));
        let resp = app
            .oneshot(
                Request::get("/test")
                    .header("authorization", encode_basic("anyone", "secret"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
