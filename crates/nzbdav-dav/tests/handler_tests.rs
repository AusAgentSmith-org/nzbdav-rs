//! Functional tests for WebDAV handlers using an in-memory mock database.

mod common;

use common::{
    MockDavDatabase, make_dir_item, make_nzb_file_item, make_root_item, send_dav, test_router,
};
use http::StatusCode;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Seed a minimal filesystem: root "/" with /nzbs/ and /content/ directories.
async fn seeded_db() -> MockDavDatabase {
    let db = MockDavDatabase::new();
    let root = make_root_item();
    let root_id = root.id;
    db.seed_item(root).await;
    db.seed_item(make_dir_item("nzbs", "/nzbs/", root_id)).await;
    db.seed_item(make_dir_item("content", "/content/", root_id))
        .await;
    db
}

// ---------------------------------------------------------------------------
// OPTIONS
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_options_returns_dav_headers() {
    let db = seeded_db().await;
    let router = test_router(db);

    let (status, headers, body) = send_dav(&router, "OPTIONS", "/", vec![], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["dav"], "1, 2");
    assert!(headers["allow"].to_str().unwrap().contains("PROPFIND"));
    assert!(body.is_empty());
}

// ---------------------------------------------------------------------------
// PROPFIND
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_propfind_root_depth_0() {
    let db = seeded_db().await;
    let router = test_router(db);

    let (status, headers, body) =
        send_dav(&router, "PROPFIND", "/", vec![("depth", "0")], None).await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(headers["content-type"].to_str().unwrap().contains("xml"));

    let xml = String::from_utf8(body).unwrap();
    // Depth 0: only the root itself, no children.
    let response_count = xml.matches("<D:response>").count();
    assert_eq!(
        response_count, 1,
        "depth=0 should return 1 response element"
    );
    assert!(
        xml.contains("<D:collection/>"),
        "root should be a collection"
    );
}

#[tokio::test]
async fn test_propfind_root_depth_1() {
    let db = seeded_db().await;
    let router = test_router(db);

    let (status, _, body) = send_dav(&router, "PROPFIND", "/", vec![("depth", "1")], None).await;
    assert_eq!(status, StatusCode::MULTI_STATUS);

    let xml = String::from_utf8(body).unwrap();
    // Depth 1: root + /nzbs/ + /content/ = 3 responses.
    let response_count = xml.matches("<D:response>").count();
    assert_eq!(
        response_count, 3,
        "depth=1 should return root + 2 children, got xml:\n{xml}"
    );
}

#[tokio::test]
async fn test_propfind_not_found() {
    let db = seeded_db().await;
    let router = test_router(db);

    let (status, _, _) = send_dav(
        &router,
        "PROPFIND",
        "/nonexistent",
        vec![("depth", "0")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// GET
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_get_nzb_blob() {
    let db = seeded_db().await;
    let nzbs_id = {
        // Find the /nzbs/ directory ID
        let root = db.inner.read().await.items.get("/nzbs/").unwrap().clone();
        root.id
    };

    let blob_id = Uuid::new_v4();
    let nzb_xml = b"<nzb><file subject=\"test\"></file></nzb>";
    db.seed_nzb_blob(blob_id, nzb_xml.to_vec()).await;
    db.seed_item(make_nzb_file_item(
        "test.nzb",
        "/nzbs/test.nzb",
        nzbs_id,
        nzb_xml.len() as i64,
        blob_id,
    ))
    .await;

    let router = test_router(db);

    let (status, _, body) = send_dav(&router, "GET", "/nzbs/test.nzb", vec![], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, nzb_xml);
}

#[tokio::test]
async fn test_get_collection_returns_405() {
    let db = seeded_db().await;
    let router = test_router(db);

    let (status, _, _) = send_dav(&router, "GET", "/content/", vec![], None).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn test_get_not_found() {
    let db = seeded_db().await;
    let router = test_router(db);

    let (status, _, _) = send_dav(&router, "GET", "/nonexistent.txt", vec![], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// HEAD
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_head_returns_headers_no_body() {
    let db = seeded_db().await;
    let nzbs_id = db.inner.read().await.items.get("/nzbs/").unwrap().id;

    let blob_id = Uuid::new_v4();
    db.seed_nzb_blob(blob_id, b"<nzb/>".to_vec()).await;
    db.seed_item(make_nzb_file_item(
        "head.nzb",
        "/nzbs/head.nzb",
        nzbs_id,
        1000,
        blob_id,
    ))
    .await;

    let router = test_router(db);

    let (status, headers, body) = send_dav(&router, "HEAD", "/nzbs/head.nzb", vec![], None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty(), "HEAD should have empty body");
    assert!(headers.contains_key("content-length"));
    assert!(headers.contains_key("etag"));
    assert_eq!(headers["accept-ranges"].to_str().unwrap(), "bytes");
}

#[tokio::test]
async fn test_head_collection_returns_405() {
    let db = seeded_db().await;
    let router = test_router(db);

    let (status, _, _) = send_dav(&router, "HEAD", "/content/", vec![], None).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

// ---------------------------------------------------------------------------
// PUT
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_put_creates_nzb() {
    let db = seeded_db().await;
    let router = test_router(db);

    let nzb_body = b"<nzb><file subject=\"new\"></file></nzb>";
    let (status, _, _) = send_dav(&router, "PUT", "/nzbs/new.nzb", vec![], Some(nzb_body)).await;
    assert_eq!(status, StatusCode::CREATED);
}

// ---------------------------------------------------------------------------
// MKCOL
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_mkcol_creates_directory() {
    let db = seeded_db().await;
    let router = test_router(db);

    let (status, _, _) = send_dav(&router, "MKCOL", "/content/newdir/", vec![], None).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn test_mkcol_already_exists() {
    let db = seeded_db().await;
    let router = test_router(db);

    let (status, _, _) = send_dav(&router, "MKCOL", "/nzbs/", vec![], None).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

// ---------------------------------------------------------------------------
// DELETE
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_delete_item() {
    let db = seeded_db().await;
    let nzbs_id = db.inner.read().await.items.get("/nzbs/").unwrap().id;

    let blob_id = Uuid::new_v4();
    db.seed_nzb_blob(blob_id, b"<nzb/>".to_vec()).await;
    db.seed_item(make_nzb_file_item(
        "del.nzb",
        "/nzbs/del.nzb",
        nzbs_id,
        100,
        blob_id,
    ))
    .await;

    let router = test_router(db);

    let (status, _, _) = send_dav(&router, "DELETE", "/nzbs/del.nzb", vec![], None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn test_delete_not_found() {
    let db = seeded_db().await;
    let router = test_router(db);

    let (status, _, _) = send_dav(&router, "DELETE", "/nonexistent.txt", vec![], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// MOVE
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_move_item() {
    let db = seeded_db().await;
    let content_id = db.inner.read().await.items.get("/content/").unwrap().id;

    db.seed_item(make_dir_item("movies", "/content/movies/", content_id))
        .await;

    let router = test_router(db);

    let (status, _, _) = send_dav(
        &router,
        "MOVE",
        "/content/movies/",
        vec![("destination", "/content/films/")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn test_move_missing_destination() {
    let db = seeded_db().await;
    let router = test_router(db);

    let (status, _, _) = send_dav(&router, "MOVE", "/content/", vec![], None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// COPY
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_copy_item() {
    let db = seeded_db().await;
    let content_id = db.inner.read().await.items.get("/content/").unwrap().id;

    db.seed_item(make_dir_item("src", "/content/src/", content_id))
        .await;

    let router = test_router(db);

    let (status, _, _) = send_dav(
        &router,
        "COPY",
        "/content/src/",
        vec![("destination", "/content/dst/")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

// ---------------------------------------------------------------------------
// Unsupported method
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_unsupported_method() {
    let db = seeded_db().await;
    let router = test_router(db);

    let (status, _, _) = send_dav(&router, "PATCH", "/", vec![], None).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}
