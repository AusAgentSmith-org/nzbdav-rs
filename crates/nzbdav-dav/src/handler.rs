//! Axum handlers for each WebDAV method.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use percent_encoding::percent_decode_str;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use tracing::{debug, info, warn};

use nzbdav_core::models::{DavMultipartFile, ItemSubType};

use crate::error::DavServerError;
use crate::propfind::multistatus_xml;
use crate::range::parse_range;
use crate::store::DatabaseStore;

pub type DavState = Arc<DatabaseStore>;

/// PROPFIND handler — directory listing (depth 0 or 1).
pub async fn propfind(State(store): State<DavState>, req: Request) -> Response {
    let path = decode_path(req.uri().path());
    let depth = req
        .headers()
        .get("depth")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("1");

    debug!(path, depth, "PROPFIND");

    let node = match store.get_node(&path).await {
        Ok(Some(n)) => n,
        Ok(None) => return (StatusCode::NOT_FOUND, "Not Found").into_response(),
        Err(e) => return error_response(e),
    };

    let mut nodes = vec![node];

    if depth != "0" {
        match store.list_children(&path).await {
            Ok(children) => nodes.extend(children),
            Err(e) => return error_response(e),
        }
    }

    let xml = multistatus_xml(&nodes, &path);

    Response::builder()
        .status(StatusCode::MULTI_STATUS)
        .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
        .body(Body::from(xml))
        .unwrap()
}

/// GET handler — file content with Range support.
pub async fn get(State(store): State<DavState>, req: Request) -> Response {
    let path = decode_path(req.uri().path());
    let range_header = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    let node = match store.get_node(&path).await {
        Ok(Some(n)) => n,
        Ok(None) => return (StatusCode::NOT_FOUND, "Not Found").into_response(),
        Err(e) => return error_response(e),
    };

    if node.is_collection {
        return (StatusCode::METHOD_NOT_ALLOWED, "Cannot GET a collection").into_response();
    }

    let raw_size = node.item.file_size.unwrap_or(0) as u64;
    let file_size = if node.item.sub_type == ItemSubType::NzbFile {
        (raw_size as f64 * 0.97) as u64
    } else {
        raw_size
    };

    info!(
        path = %path,
        sub_type = ?node.item.sub_type,
        file_size,
        has_file_blob = node.item.file_blob_id.is_some(),
        has_nzb_blob = node.item.nzb_blob_id.is_some(),
        range = ?range_header,
        "GET file request"
    );

    if node.item.file_blob_id.is_some() {
        return serve_streaming(store, &node, file_size, range_header).await;
    }

    if node.item.nzb_blob_id.is_some() {
        return serve_nzb_blob(store, &node).await;
    }

    (StatusCode::NOT_FOUND, "No content available").into_response()
}

/// HEAD handler — same as GET but no body.
pub async fn head(State(store): State<DavState>, req: Request) -> Response {
    let path = decode_path(req.uri().path());
    debug!(path, "HEAD");

    let node = match store.get_node(&path).await {
        Ok(Some(n)) => n,
        Ok(None) => return (StatusCode::NOT_FOUND, "Not Found").into_response(),
        Err(e) => return error_response(e),
    };

    if node.is_collection {
        return (StatusCode::METHOD_NOT_ALLOWED, "Cannot HEAD a collection").into_response();
    }

    let raw_size = node.item.file_size.unwrap_or(0) as u64;
    let file_size = if node.item.sub_type == ItemSubType::NzbFile {
        (raw_size as f64 * 0.97) as u64
    } else {
        raw_size
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, file_size)
        .header(header::CONTENT_TYPE, &node.content_type)
        .header(header::ETAG, &node.etag)
        .header(header::ACCEPT_RANGES, "bytes")
        .body(Body::empty())
        .unwrap()
}

/// PUT handler — upload NZB file.
pub async fn put(State(store): State<DavState>, req: Request) -> Response {
    let path = decode_path(req.uri().path());
    debug!(path, "PUT");

    let body_bytes = match axum::body::to_bytes(req.into_body(), 256 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            warn!("PUT body read error: {e}");
            return (StatusCode::BAD_REQUEST, "Failed to read body").into_response();
        }
    };

    match store.put_nzb(&path, &body_bytes).await {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e) => error_response(e),
    }
}

/// MKCOL handler — create directory.
pub async fn mkcol(State(store): State<DavState>, req: Request) -> Response {
    let path = decode_path(req.uri().path());
    debug!(path, "MKCOL");

    match store.mkcol(&path).await {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e) => error_response(e),
    }
}

/// DELETE handler — remove item.
pub async fn delete(State(store): State<DavState>, req: Request) -> Response {
    let path = decode_path(req.uri().path());
    debug!(path, "DELETE");

    match store.delete(&path).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => error_response(e),
    }
}

/// MOVE handler — rename/move item.
pub async fn r#move(State(store): State<DavState>, req: Request) -> Response {
    let destination = match extract_destination(&req) {
        Some(d) => d,
        None => return (StatusCode::BAD_REQUEST, "Missing Destination header").into_response(),
    };
    let path = decode_path(req.uri().path());
    debug!(path, destination, "MOVE");

    match store.move_item(&path, &destination).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => error_response(e),
    }
}

/// COPY handler — copy item.
pub async fn copy(State(store): State<DavState>, req: Request) -> Response {
    let destination = match extract_destination(&req) {
        Some(d) => d,
        None => return (StatusCode::BAD_REQUEST, "Missing Destination header").into_response(),
    };
    let path = decode_path(req.uri().path());
    debug!(path, destination, "COPY");

    match store.copy_item(&path, &destination).await {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e) => error_response(e),
    }
}

/// OPTIONS handler — advertise DAV capabilities.
pub async fn options() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(
            "Allow",
            "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, MKCOL, MOVE, COPY",
        )
        .header("DAV", "1, 2")
        .header(header::CONTENT_LENGTH, "0")
        .body(Body::empty())
        .unwrap()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Serve a streaming body from Usenet with optional Range support.
async fn serve_streaming(
    store: DavState,
    node: &crate::store::DavNode,
    file_size: u64,
    range_header: Option<String>,
) -> Response {
    if let Some(range_val) = range_header {
        let ranges = match parse_range(&range_val, file_size) {
            Ok(r) => r,
            Err(_) => {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{file_size}"))
                    .body(Body::empty())
                    .unwrap();
            }
        };

        let range = ranges[0];
        let content_length = range.end - range.start + 1;
        let content_range = format!("bytes {}-{}/{file_size}", range.start, range.end);

        if let Some(blob_id) = node.item.file_blob_id
            && let Ok(blob_data) = store.db().get_file_blob(blob_id).await
        {
            // MultipartFile (RAR-extracted): DavMultipartFileStream with seek.
            if node.item.sub_type == ItemSubType::MultipartFile
                && let Ok(meta) = bincode::deserialize::<DavMultipartFile>(&blob_data)
            {
                info!(
                    parts = meta.file_parts.len(),
                    encrypted = meta.aes_params.is_some(),
                    range_start = range.start,
                    "streaming MultipartFile"
                );

                // Encrypted multipart + Range is unsafe today: seeking the
                // raw ciphertext stream before wrapping in AesDecoderStream
                // desynchronises CBC state. Decline the range and serve the
                // full decrypted body instead — correctness beats seekability.
                if meta.aes_params.is_some() {
                    warn!("Range request on encrypted file — serving full body (200 OK)");
                    return serve_full_body(store, node, file_size).await;
                }

                let mut stream = nzbdav_stream::DavMultipartFileStream::new(
                    store.provider(),
                    meta.file_parts,
                    file_size,
                    store.lookahead(),
                );

                if stream
                    .seek(std::io::SeekFrom::Start(range.start))
                    .await
                    .is_ok()
                {
                    let body = Body::from_stream(ReaderStream::new(stream.take(content_length)));

                    return Response::builder()
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header(header::CONTENT_TYPE, &node.content_type)
                        .header(header::CONTENT_RANGE, &content_range)
                        .header(header::CONTENT_LENGTH, content_length)
                        .header(header::ETAG, &node.etag)
                        .header(header::ACCEPT_RANGES, "bytes")
                        .body(body)
                        .unwrap();
                }

                return range_not_satisfiable(file_size);
            }

            // NzbFile (plain files): SeekableSegmentStream with exact seek.
            if node.item.sub_type == ItemSubType::NzbFile
                && let Ok(meta) =
                    bincode::deserialize::<nzbdav_core::models::DavNzbFile>(&blob_data)
            {
                match nzbdav_stream::SeekableSegmentStream::aligned(
                    store.provider(),
                    meta.segment_ids,
                    file_size,
                    store.lookahead(),
                    range.start,
                )
                .await
                {
                    Ok(stream) => {
                        return Response::builder()
                            .status(StatusCode::PARTIAL_CONTENT)
                            .header(header::CONTENT_TYPE, &node.content_type)
                            .header(header::CONTENT_RANGE, &content_range)
                            .header(header::CONTENT_LENGTH, content_length)
                            .header(header::ETAG, &node.etag)
                            .header(header::ACCEPT_RANGES, "bytes")
                            .body(Body::from_stream(ReaderStream::new(
                                stream.take(content_length),
                            )))
                            .unwrap();
                    }
                    Err(e) => {
                        warn!(error = %e, "failed to align seekable stream, returning 416");
                        return range_not_satisfiable(file_size);
                    }
                }
            }
        }

        // Fallthrough: we saw a Range header but had no streamable source.
        // Don't emit a 200 body — that corrupts client-side seek state.
        range_not_satisfiable(file_size)
    } else {
        // No Range header: serve full body.
        match store.get_body(&node.item).await {
            Ok(body) => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, &node.content_type)
                .header(header::CONTENT_LENGTH, file_size)
                .header(header::ETAG, &node.etag)
                .header(header::ACCEPT_RANGES, "bytes")
                .body(body)
                .unwrap(),
            Err(e) => error_response(e),
        }
    }
}

/// Build a 416 Range Not Satisfiable response. Used whenever we cannot
/// produce byte-exact partial content — never silently fall back to 200.
fn range_not_satisfiable(file_size: u64) -> Response {
    Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(header::CONTENT_RANGE, format!("bytes */{file_size}"))
        .body(Body::empty())
        .unwrap()
}

/// Serve a full 200 OK body — used when a Range header arrives but the
/// resource cannot be range-served correctly (e.g. encrypted multipart).
async fn serve_full_body(
    store: DavState,
    node: &crate::store::DavNode,
    file_size: u64,
) -> Response {
    match store.get_body(&node.item).await {
        Ok(body) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, &node.content_type)
            .header(header::CONTENT_LENGTH, file_size)
            .header(header::ETAG, &node.etag)
            .header(header::ACCEPT_RANGES, "bytes")
            .body(body)
            .unwrap(),
        Err(e) => error_response(e),
    }
}

/// Serve raw NZB XML blob directly.
async fn serve_nzb_blob(store: DavState, node: &crate::store::DavNode) -> Response {
    let blob_id = match node.item.nzb_blob_id {
        Some(id) => id,
        None => return (StatusCode::NOT_FOUND, "No NZB blob").into_response(),
    };

    let data = match store.db().get_nzb_blob(blob_id).await {
        Ok(d) => d,
        Err(e) => return error_response(e.into()),
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, &node.content_type)
        .header(header::CONTENT_LENGTH, data.len())
        .header(header::ETAG, &node.etag)
        .body(Body::from(data))
        .unwrap()
}

/// Extract the Destination header (used by MOVE/COPY), returning just the path.
fn extract_destination(req: &Request) -> Option<String> {
    let val = req.headers().get("destination")?.to_str().ok()?;
    if let Ok(uri) = val.parse::<http::Uri>() {
        Some(decode_path(uri.path()))
    } else {
        Some(decode_path(val))
    }
}

/// Percent-decode a URI path.
fn decode_path(path: &str) -> String {
    percent_decode_str(path).decode_utf8_lossy().into_owned()
}

/// Map a `DavServerError` to an HTTP response.
fn error_response(err: DavServerError) -> Response {
    let (status, msg) = match &err {
        DavServerError::NotFound(m) => (StatusCode::NOT_FOUND, m.clone()),
        DavServerError::Conflict(m) => (StatusCode::CONFLICT, m.clone()),
        DavServerError::MethodNotAllowed(m) => (StatusCode::METHOD_NOT_ALLOWED, m.clone()),
        DavServerError::PreconditionFailed(m) => (StatusCode::PRECONDITION_FAILED, m.clone()),
        DavServerError::Forbidden(m) => (StatusCode::FORBIDDEN, m.clone()),
        DavServerError::InvalidRange(m) => (StatusCode::RANGE_NOT_SATISFIABLE, m.clone()),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };
    warn!(status = %status, error = %msg, "DAV error");
    (status, msg).into_response()
}
