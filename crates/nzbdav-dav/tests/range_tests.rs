//! Functional tests for GET + Range on plain NZB files and encrypted
//! multipart files. These verify byte-level correctness through the DAV
//! handler, backed by a canned-article mock provider.

mod common;

use common::{MockDavDatabase, make_dir_item, make_root_item, send_dav, test_router_with_provider};
use http::StatusCode;
use nzbdav_core::models::{DavItem, DavNzbFile, ItemSubType, ItemType};
use nzbdav_stream::testing::mock_provider_for_file;
use uuid::Uuid;

fn make_nzb_file_with_blob(name: &str, path: &str, parent_id: Uuid, file_size: i64) -> DavItem {
    DavItem {
        id: Uuid::new_v4(),
        id_prefix: name.chars().next().unwrap_or('_').to_string(),
        created_at: chrono::Utc::now().naive_utc(),
        parent_id: Some(parent_id),
        name: name.into(),
        file_size: Some(file_size),
        item_type: ItemType::UsenetFile,
        sub_type: ItemSubType::NzbFile,
        path: path.into(),
        release_date: None,
        last_health_check: None,
        next_health_check: None,
        history_item_id: None,
        file_blob_id: Some(Uuid::new_v4()),
        nzb_blob_id: None,
    }
}

/// Seed a known plaintext file as an NzbFile: returns (router, plaintext).
async fn seed_nzb_file(file_size: usize, n_segments: usize) -> (axum::Router, Vec<u8>) {
    let db = MockDavDatabase::new();
    let root = make_root_item();
    let root_id = root.id;
    db.seed_item(root).await;
    db.seed_item(make_dir_item("content", "/content/", root_id))
        .await;

    let plaintext: Vec<u8> = (0..file_size).map(|i| (i * 7 + 3) as u8).collect();
    let (provider, message_ids, _) =
        mock_provider_for_file("movie.mp4", plaintext.clone(), n_segments);

    // The handler treats NzbFile file_size as raw yEnc bytes and multiplies by
    // 0.97. To make the advertised size equal the real plaintext size, seed
    // the DB with file_size / 0.97 as the raw_size.
    let advertised_raw = (plaintext.len() as f64 / 0.97).ceil() as i64;
    let mut item =
        make_nzb_file_with_blob("movie.mp4", "/content/movie.mp4", root_id, advertised_raw);
    item.file_blob_id = Some(Uuid::new_v4());

    let blob_id = item.file_blob_id.unwrap();
    let meta = DavNzbFile {
        segment_ids: message_ids,
    };
    let blob = bincode::serialize(&meta).unwrap();
    db.seed_file_blob(blob_id, blob).await;
    db.seed_item(item).await;

    let router = test_router_with_provider(db, provider, 4);
    (router, plaintext)
}

// ---------------------------------------------------------------------------
// Finding #1: plain-file range path must be byte-exact.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn range_request_returns_exact_bytes_from_offset() {
    let (router, plaintext) = seed_nzb_file(16_384, 4).await;

    let start = 4_096u64;
    let end = 8_191u64;
    let (status, headers, body) = send_dav(
        &router,
        "GET",
        "/content/movie.mp4",
        vec![("Range", &format!("bytes={start}-{end}"))],
        None,
    )
    .await;

    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    let expected_len = (end - start + 1) as usize;
    let cl = headers
        .get(http::header::CONTENT_LENGTH)
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<usize>()
        .unwrap();
    assert_eq!(cl, expected_len, "Content-Length header mismatch");
    assert_eq!(
        body.len(),
        expected_len,
        "response body length does not match Content-Length"
    );
    assert_eq!(
        body,
        plaintext[start as usize..=end as usize],
        "range bytes mismatch"
    );
}

#[tokio::test]
async fn range_tail_request_returns_correct_suffix() {
    let (router, plaintext) = seed_nzb_file(8_192, 4).await;
    // Last 512 bytes.
    let (status, _headers, body) = send_dav(
        &router,
        "GET",
        "/content/movie.mp4",
        vec![("Range", "bytes=-512")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body.len(), 512);
    assert_eq!(body, plaintext[plaintext.len() - 512..]);
}

#[tokio::test]
async fn range_beyond_eof_returns_416() {
    let (router, _) = seed_nzb_file(1_024, 2).await;
    // Request a range starting past EOF — must be 416, not 200 with full body.
    let (status, _headers, _body) = send_dav(
        &router,
        "GET",
        "/content/movie.mp4",
        vec![("Range", "bytes=999999-")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
}

/// A range request that the handler cannot service MUST NOT silently fall
/// back to a 200 OK with the full body — that corrupts seeking in HTTP
/// clients. After the fix, a broken/unsatisfiable range must yield 416 (or
/// 206 with correct bytes), never 200.
#[tokio::test]
async fn range_request_never_falls_back_to_200() {
    let (router, _) = seed_nzb_file(4_096, 2).await;
    let (status, _headers, _body) = send_dav(
        &router,
        "GET",
        "/content/movie.mp4",
        vec![("Range", "bytes=100-200")],
        None,
    )
    .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "Range request must not fall back to 200 OK"
    );
    assert!(
        status == StatusCode::PARTIAL_CONTENT || status == StatusCode::RANGE_NOT_SATISFIABLE,
        "unexpected status: {status}"
    );
}
