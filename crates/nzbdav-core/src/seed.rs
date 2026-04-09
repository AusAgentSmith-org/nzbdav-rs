use uuid::Uuid;

use crate::database::DavDatabase;
use crate::error::Result;
use crate::models::{DavItem, ItemSubType, ItemType};

/// Namespace UUID for deterministic v5 IDs (generated once, stable forever).
const NAMESPACE: Uuid = Uuid::from_bytes([
    0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8,
]);

const CONTENT_README: &str = "\
About /content

This directory contains all streamable files that have finished processing.
Files are organized by category and job name.

Files are served by streaming directly from Usenet on demand - no local storage is used.
Content can be accessed via any WebDAV client or via rclone mount.
";

const NZBS_README: &str = "\
About /nzbs

This directory mirrors the nzbdav queue.
- NZBs currently in the queue can be downloaded from this directory.
- Upload an NZB file here to add it to the processing queue.
- Delete an NZB to remove it from the queue.
";

const IDS_README: &str = "\
About /.ids

This directory provides ID-based file access.
Files can be accessed by their UUID without knowing the full path.
";

fn root_item(name: &str, path: &str, sub_type: ItemSubType, parent_id: Option<Uuid>) -> DavItem {
    DavItem {
        id: Uuid::new_v5(&NAMESPACE, path.as_bytes()),
        id_prefix: name.chars().next().unwrap_or('_').to_string(),
        created_at: chrono::Utc::now().naive_utc(),
        parent_id,
        name: name.to_string(),
        file_size: None,
        item_type: ItemType::Directory,
        sub_type,
        path: path.to_string(),
        release_date: None,
        last_health_check: None,
        next_health_check: None,
        history_item_id: None,
        file_blob_id: None,
        nzb_blob_id: None,
    }
}

/// Seed the root virtual filesystem directories if they don't already exist.
///
/// Works with any `DavDatabase` implementation (SQLite, PostgreSQL, etc.).
pub async fn seed_root_items(db: &dyn DavDatabase) -> Result<()> {
    let root_path = "/";
    // Check if root already exists
    if db.get_dav_item_by_path(root_path).await?.is_some() {
        return Ok(());
    }

    let root = root_item("", root_path, ItemSubType::WebdavRoot, None);
    let root_id = root.id;
    db.insert_dav_item(&root).await?;

    let children = [
        ("nzbs", "/nzbs/", ItemSubType::NzbsRoot),
        ("content", "/content/", ItemSubType::ContentRoot),
        (
            "completed-symlinks",
            "/completed-symlinks/",
            ItemSubType::SymlinkRoot,
        ),
        (".ids", "/.ids/", ItemSubType::IdsRoot),
    ];

    for (name, path, sub_type) in children {
        let item = root_item(name, path, sub_type, Some(root_id));
        db.insert_dav_item(&item).await?;
    }

    // Seed README.txt files in key directories.
    let readmes: &[(&str, &str, &str)] = &[
        ("/content/README.txt", "/content/", CONTENT_README),
        ("/nzbs/README.txt", "/nzbs/", NZBS_README),
        ("/.ids/README.txt", "/.ids/", IDS_README),
    ];

    for &(readme_path, parent_path, content) in readmes {
        let parent = db
            .get_dav_item_by_path(parent_path)
            .await?
            .expect("parent directory must exist after seeding");
        let blob_id = Uuid::new_v5(&NAMESPACE, readme_path.as_bytes());
        db.put_nzb_blob(blob_id, content.as_bytes()).await?;

        let item = DavItem {
            id: Uuid::new_v5(&NAMESPACE, format!("readme:{readme_path}").as_bytes()),
            id_prefix: "R".to_string(),
            created_at: chrono::Utc::now().naive_utc(),
            parent_id: Some(parent.id),
            name: "README.txt".to_string(),
            file_size: Some(content.len() as i64),
            item_type: ItemType::UsenetFile,
            sub_type: ItemSubType::ReadmeFile,
            path: readme_path.to_string(),
            release_date: None,
            last_health_check: None,
            next_health_check: None,
            history_item_id: None,
            file_blob_id: None,
            nzb_blob_id: Some(blob_id),
        };
        db.insert_dav_item(&item).await?;
    }

    Ok(())
}
