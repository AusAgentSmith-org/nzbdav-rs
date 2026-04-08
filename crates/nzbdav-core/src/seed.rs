use chrono::Utc;
use rusqlite::Connection;
use uuid::Uuid;

use crate::blob_store::BlobStore;
use crate::dav_items;
use crate::error::Result;
use crate::models::{DavItem, ItemSubType, ItemType};

/// Namespace UUID for deterministic v5 IDs (generated once, stable forever).
const NAMESPACE: Uuid = Uuid::from_bytes([
    0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30,
    0xc8,
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
        created_at: Utc::now().naive_utc(),
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
pub fn seed_root_items(conn: &Connection) -> Result<()> {
    let root_path = "/";
    // Check if root already exists
    if dav_items::get_by_path(conn, root_path)?.is_some() {
        return Ok(());
    }

    let root = root_item("", root_path, ItemSubType::WebdavRoot, None);
    let root_id = root.id;
    dav_items::insert(conn, &root)?;

    let children = [
        ("nzbs", "/nzbs/", ItemSubType::NzbsRoot),
        ("content", "/content/", ItemSubType::ContentRoot),
        ("completed-symlinks", "/completed-symlinks/", ItemSubType::SymlinkRoot),
        (".ids", "/.ids/", ItemSubType::IdsRoot),
    ];

    for (name, path, sub_type) in children {
        let item = root_item(name, path, sub_type, Some(root_id));
        dav_items::insert(conn, &item)?;
    }

    // Seed README.txt files in key directories.
    let readmes: &[(&str, &str, &str)] = &[
        ("/content/README.txt", "/content/", CONTENT_README),
        ("/nzbs/README.txt", "/nzbs/", NZBS_README),
        ("/.ids/README.txt", "/.ids/", IDS_README),
    ];

    for &(readme_path, parent_path, content) in readmes {
        let parent = dav_items::get_by_path(conn, parent_path)?
            .expect("parent directory must exist after seeding");
        let blob_id = Uuid::new_v5(&NAMESPACE, readme_path.as_bytes());
        BlobStore::put_nzb_blob(conn, blob_id, content.as_bytes())?;

        let item = DavItem {
            id: Uuid::new_v5(&NAMESPACE, format!("readme:{readme_path}").as_bytes()),
            id_prefix: "R".to_string(),
            created_at: Utc::now().naive_utc(),
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
        dav_items::insert(conn, &item)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn test_seed_creates_root_items() {
        let conn = open_in_memory().unwrap();
        seed_root_items(&conn).unwrap();

        let root = dav_items::get_by_path(&conn, "/").unwrap().unwrap();
        assert_eq!(root.sub_type, ItemSubType::WebdavRoot);

        let children = dav_items::get_children(&conn, root.id).unwrap();
        assert_eq!(children.len(), 4);

        let nzbs = dav_items::get_by_path(&conn, "/nzbs/").unwrap().unwrap();
        assert_eq!(nzbs.sub_type, ItemSubType::NzbsRoot);

        let content = dav_items::get_by_path(&conn, "/content/").unwrap().unwrap();
        assert_eq!(content.sub_type, ItemSubType::ContentRoot);

        let symlinks = dav_items::get_by_path(&conn, "/completed-symlinks/").unwrap().unwrap();
        assert_eq!(symlinks.sub_type, ItemSubType::SymlinkRoot);

        let ids = dav_items::get_by_path(&conn, "/.ids/").unwrap().unwrap();
        assert_eq!(ids.sub_type, ItemSubType::IdsRoot);

        // README files should exist in content, nzbs, and .ids directories.
        let content_readme = dav_items::get_by_path(&conn, "/content/README.txt").unwrap().unwrap();
        assert_eq!(content_readme.sub_type, ItemSubType::ReadmeFile);
        assert!(content_readme.nzb_blob_id.is_some());
        assert!(content_readme.file_size.unwrap() > 0);

        let nzbs_readme = dav_items::get_by_path(&conn, "/nzbs/README.txt").unwrap().unwrap();
        assert_eq!(nzbs_readme.sub_type, ItemSubType::ReadmeFile);

        let ids_readme = dav_items::get_by_path(&conn, "/.ids/README.txt").unwrap().unwrap();
        assert_eq!(ids_readme.sub_type, ItemSubType::ReadmeFile);
    }

    #[test]
    fn test_seed_is_idempotent() {
        let conn = open_in_memory().unwrap();
        seed_root_items(&conn).unwrap();
        seed_root_items(&conn).unwrap(); // Should not error

        // Root still has exactly 4 directory children.
        let children = dav_items::get_children(
            &conn,
            dav_items::get_by_path(&conn, "/").unwrap().unwrap().id,
        )
        .unwrap();
        assert_eq!(children.len(), 4);

        // Still exactly one README per directory.
        let content_children = dav_items::get_children_by_path(&conn, "/content/").unwrap();
        let readme_count = content_children
            .iter()
            .filter(|c| c.name == "README.txt")
            .count();
        assert_eq!(readme_count, 1);
    }

    #[test]
    fn test_seed_deterministic_ids() {
        let conn1 = open_in_memory().unwrap();
        seed_root_items(&conn1).unwrap();
        let root1 = dav_items::get_by_path(&conn1, "/").unwrap().unwrap();

        let conn2 = open_in_memory().unwrap();
        seed_root_items(&conn2).unwrap();
        let root2 = dav_items::get_by_path(&conn2, "/").unwrap().unwrap();

        assert_eq!(root1.id, root2.id);
    }
}
