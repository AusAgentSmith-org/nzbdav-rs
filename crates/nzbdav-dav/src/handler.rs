//! Axum handlers for each WebDAV method.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use percent_encoding::percent_decode_str;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use tracing::{debug, warn};

use nzbdav_core::models::ItemSubType;

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

    let node = match store.get_node(&path) {
        Ok(Some(n)) => n,
        Ok(None) => return (StatusCode::NOT_FOUND, "Not Found").into_response(),
        Err(e) => return error_response(e),
    };

    let mut nodes = vec![node];

    if depth != "0" {
        match store.list_children(&path) {
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

    debug!(path, "GET");

    let node = match store.get_node(&path) {
        Ok(Some(n)) => n,
        Ok(None) => return (StatusCode::NOT_FOUND, "Not Found").into_response(),
        Err(e) => return error_response(e),
    };

    if node.is_collection {
        return (StatusCode::METHOD_NOT_ALLOWED, "Cannot GET a collection").into_response();
    }

    let raw_size = node.item.file_size.unwrap_or(0) as u64;
    // Stored file_size is raw yEnc segment bytes (~3% larger than decoded content).
    // Estimate decoded size for HTTP Content-Length and Range calculations.
    let file_size = if node.item.sub_type == ItemSubType::NzbFile {
        (raw_size as f64 * 0.97) as u64
    } else {
        raw_size
    };

    // For items that have a file_blob_id, stream from Usenet.
    // For items that only have an nzb_blob_id (raw NZB XML), serve the blob directly.
    if node.item.file_blob_id.is_some() {
        return serve_streaming(store, &node, file_size, range_header).await;
    }

    // Serve raw NZB XML for items with only nzb_blob_id
    if node.item.nzb_blob_id.is_some() {
        return serve_nzb_blob(store, &node);
    }

    (StatusCode::NOT_FOUND, "No content available").into_response()
}

/// HEAD handler — same as GET but no body.
pub async fn head(State(store): State<DavState>, req: Request) -> Response {
    let path = decode_path(req.uri().path());
    debug!(path, "HEAD");

    let node = match store.get_node(&path) {
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

    let body_bytes = match axum::body::to_bytes(req.into_body(), 50 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            warn!("PUT body read error: {e}");
            return (StatusCode::BAD_REQUEST, "Failed to read body").into_response();
        }
    };

    match store.put_nzb(&path, &body_bytes) {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e) => error_response(e),
    }
}

/// MKCOL handler — create directory.
pub async fn mkcol(State(store): State<DavState>, req: Request) -> Response {
    let path = decode_path(req.uri().path());
    debug!(path, "MKCOL");

    match store.mkcol(&path) {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e) => error_response(e),
    }
}

/// DELETE handler — remove item.
pub async fn delete(State(store): State<DavState>, req: Request) -> Response {
    let path = decode_path(req.uri().path());
    debug!(path, "DELETE");

    match store.delete(&path) {
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

    match store.move_item(&path, &destination) {
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

    match store.copy_item(&path, &destination) {
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
        // Parse the range and serve partial content.
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

        // We only support a single range for streaming.
        let range = ranges[0];

        // For NzbFile (plain files), use SeekableSegmentStream with Range support.
        if node.item.sub_type == ItemSubType::NzbFile {
            if let Some(blob_id) = node.item.file_blob_id {
                let blob_data = {
                    let conn = store.db_conn();
                    nzbdav_core::blob_store::BlobStore::get_file_blob(&conn, blob_id)
                };
                if let Ok(blob_data) = blob_data {
                    if let Ok(meta) =
                        bincode::deserialize::<nzbdav_core::models::DavNzbFile>(&blob_data)
                    {
                        let mut stream = nzbdav_stream::SeekableSegmentStream::new(
                            store.provider(),
                            meta.segment_ids,
                            file_size,
                            store.lookahead(),
                        );

                        use tokio::io::AsyncSeekExt;
                        if let Ok(_) = stream.seek(std::io::SeekFrom::Start(range.start)).await {
                            // Don't use Take — the seek starts at approximately
                            // the right position and streams to EOF. No Content-Length
                            // since the decoded byte count is approximate.
                            let body = Body::from_stream(ReaderStream::new(stream));
                            let content_range =
                                format!("bytes {}-{}/{file_size}", range.start, range.end);
                            return Response::builder()
                                .status(StatusCode::PARTIAL_CONTENT)
                                .header(header::CONTENT_TYPE, &node.content_type)
                                .header(header::CONTENT_RANGE, content_range)
                                .header(header::ETAG, &node.etag)
                                .header(header::ACCEPT_RANGES, "bytes")
                                .body(body)
                                .unwrap();
                        }
                    }
                }
            }
        }

        // Fallback: serve full body (chunked, no Content-Length).
        match store.get_body(&node.item) {
            Ok(body) => {
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, &node.content_type)
                    .header(header::ETAG, &node.etag)
                    .body(body)
                    .unwrap();
            }
            Err(e) => return error_response(e),
        }
        match build_ranged_body(store, node, &range).await {
            Ok(ranged_body) => {
                let content_range = format!("bytes {}-{}/{file_size}", range.start, range.end);
                Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_TYPE, &node.content_type)
                    .header(header::CONTENT_LENGTH, range.length())
                    .header(header::CONTENT_RANGE, content_range)
                    .header(header::ETAG, &node.etag)
                    .header(header::ACCEPT_RANGES, "bytes")
                    .body(ranged_body)
                    .unwrap()
            }
            Err(e) => {
                warn!("range stream failed: {e}");
                error_response(e)
            }
        }
    } else {
        // No range — serve full body (chunked, no Content-Length for streaming).
        match store.get_body(&node.item) {
            Ok(body) => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, &node.content_type)
                .header(header::ETAG, &node.etag)
                .body(body)
                .unwrap(),
            Err(e) => error_response(e),
        }
    }
}

/// Build a ranged body by seeking and taking the appropriate bytes.
async fn build_ranged_body(
    store: DavState,
    node: &crate::store::DavNode,
    range: &crate::range::ByteRange,
) -> Result<Body, DavServerError> {
    let blob_id = node
        .item
        .file_blob_id
        .ok_or_else(|| DavServerError::NotFound("no file blob".into()))?;

    let blob_data = {
        let conn = store.db_conn();
        nzbdav_core::blob_store::BlobStore::get_file_blob(&conn, blob_id)?
    };

    let file_size = node.item.file_size.unwrap_or(0) as u64;

    match node.item.sub_type {
        ItemSubType::MultipartFile => {
            let meta: nzbdav_core::models::DavMultipartFile = bincode::deserialize(&blob_data)
                .map_err(|e| DavServerError::Other(format!("deserialize error: {e}")))?;

            let mut stream = nzbdav_stream::DavMultipartFileStream::new(
                store.provider(),
                meta.file_parts,
                file_size,
                store.lookahead(),
            );

            if let Some(aes) = meta.aes_params {
                let decoded_size = aes.decoded_size as u64;
                let mut aes_stream =
                    nzbdav_stream::AesDecoderStream::new(stream, &aes.key, &aes.iv, decoded_size);
                aes_stream
                    .seek(std::io::SeekFrom::Start(range.start))
                    .await
                    .map_err(|e| DavServerError::Other(format!("seek error: {e}")))?;
                let take = aes_stream.take(range.length());
                Ok(Body::from_stream(ReaderStream::new(take)))
            } else {
                stream
                    .seek(std::io::SeekFrom::Start(range.start))
                    .await
                    .map_err(|e| DavServerError::Other(format!("seek error: {e}")))?;
                let take = stream.take(range.length());
                Ok(Body::from_stream(ReaderStream::new(take)))
            }
        }
        ItemSubType::NzbFile => {
            let meta: nzbdav_core::models::DavNzbFile = bincode::deserialize(&blob_data)
                .map_err(|e| DavServerError::Other(format!("deserialize error: {e}")))?;

            let mut stream = nzbdav_stream::NzbFileStream::new(
                store.provider(),
                meta.segment_ids,
                file_size,
                store.lookahead(),
            );

            stream
                .seek(std::io::SeekFrom::Start(range.start))
                .await
                .map_err(|e| DavServerError::Other(format!("seek error: {e}")))?;
            let take = stream.take(range.length());
            Ok(Body::from_stream(ReaderStream::new(take)))
        }
        other => Err(DavServerError::Other(format!(
            "unsupported sub_type for range: {other:?}"
        ))),
    }
}

/// Serve raw NZB XML blob directly.
fn serve_nzb_blob(store: DavState, node: &crate::store::DavNode) -> Response {
    let blob_id = match node.item.nzb_blob_id {
        Some(id) => id,
        None => return (StatusCode::NOT_FOUND, "No NZB blob").into_response(),
    };

    let data = {
        let conn = store.db_conn();
        match nzbdav_core::blob_store::BlobStore::get_nzb_blob(&conn, blob_id) {
            Ok(d) => d,
            Err(e) => return error_response(e.into()),
        }
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
    // Destination may be an absolute URL or just a path.
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
