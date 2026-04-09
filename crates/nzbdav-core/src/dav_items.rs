use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{params, Connection, Row};
use uuid::Uuid;

use crate::error::{DavError, Result};
use crate::models::{DavItem, ItemSubType, ItemType};

fn row_to_dav_item(row: &Row) -> rusqlite::Result<DavItem> {
    let id: String = row.get("id")?;
    let parent_id: Option<String> = row.get("parent_id")?;
    let item_type_i: i32 = row.get("type")?;
    let sub_type_i: i32 = row.get("sub_type")?;
    let created_at_str: String = row.get("created_at")?;
    let release_date_str: Option<String> = row.get("release_date")?;
    let last_health_check_str: Option<String> = row.get("last_health_check")?;
    let next_health_check_str: Option<String> = row.get("next_health_check")?;
    let history_item_id: Option<String> = row.get("history_item_id")?;
    let file_blob_id: Option<String> = row.get("file_blob_id")?;
    let nzb_blob_id: Option<String> = row.get("nzb_blob_id")?;

    let parse_uuid = |s: String| -> rusqlite::Result<Uuid> {
        Uuid::parse_str(&s)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
    };

    let parse_rfc3339 = |s: String| -> rusqlite::Result<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&s)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
    };

    Ok(DavItem {
        id: parse_uuid(id)?,
        id_prefix: row.get("id_prefix")?,
        created_at: NaiveDateTime::parse_from_str(&created_at_str, "%Y-%m-%d %H:%M:%S")
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?,
        parent_id: parent_id.map(parse_uuid).transpose()?,
        name: row.get("name")?,
        file_size: row.get("file_size")?,
        item_type: ItemType::try_from(item_type_i)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Integer, e.into()))?,
        sub_type: ItemSubType::try_from(sub_type_i)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Integer, e.into()))?,
        path: row.get("path")?,
        release_date: release_date_str.map(parse_rfc3339).transpose()?,
        last_health_check: last_health_check_str.map(parse_rfc3339).transpose()?,
        next_health_check: next_health_check_str.map(parse_rfc3339).transpose()?,
        history_item_id: history_item_id.map(parse_uuid).transpose()?,
        file_blob_id: file_blob_id.map(parse_uuid).transpose()?,
        nzb_blob_id: nzb_blob_id.map(parse_uuid).transpose()?,
    })
}

const COLUMNS: &str = "id, id_prefix, created_at, parent_id, name, file_size, type, sub_type, path, release_date, last_health_check, next_health_check, history_item_id, file_blob_id, nzb_blob_id";

pub fn insert(conn: &Connection, item: &DavItem) -> Result<()> {
    conn.execute(
        &format!("INSERT OR REPLACE INTO dav_items ({COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"),
        params![
            item.id.to_string(),
            item.id_prefix,
            item.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            item.parent_id.map(|u| u.to_string()),
            item.name,
            item.file_size,
            item.item_type as i32,
            item.sub_type as i32,
            item.path,
            item.release_date.map(|d| d.to_rfc3339()),
            item.last_health_check.map(|d| d.to_rfc3339()),
            item.next_health_check.map(|d| d.to_rfc3339()),
            item.history_item_id.map(|u| u.to_string()),
            item.file_blob_id.map(|u| u.to_string()),
            item.nzb_blob_id.map(|u| u.to_string()),
        ],
    )?;
    Ok(())
}

pub fn get_by_id(conn: &Connection, id: Uuid) -> Result<Option<DavItem>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM dav_items WHERE id = ?1"))?;
    let mut rows = stmt.query_map(params![id.to_string()], row_to_dav_item)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

pub fn get_by_path(conn: &Connection, path: &str) -> Result<Option<DavItem>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM dav_items WHERE path = ?1"))?;
    let mut rows = stmt.query_map(params![path], row_to_dav_item)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

pub fn get_children(conn: &Connection, parent_id: Uuid) -> Result<Vec<DavItem>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM dav_items WHERE parent_id = ?1"
    ))?;
    let rows = stmt.query_map(params![parent_id.to_string()], row_to_dav_item)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

pub fn get_children_by_path(conn: &Connection, parent_path: &str) -> Result<Vec<DavItem>> {
    let prefixed = COLUMNS
        .split(", ")
        .map(|col| format!("c.{col}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn.prepare(&format!(
        "SELECT {prefixed} FROM dav_items c \
         INNER JOIN dav_items p ON c.parent_id = p.id \
         WHERE p.path = ?1"
    ))?;
    let rows = stmt.query_map(params![parent_path], row_to_dav_item)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

pub fn delete(conn: &Connection, id: Uuid) -> Result<()> {
    conn.execute("DELETE FROM dav_items WHERE id = ?1", params![id.to_string()])?;
    Ok(())
}

pub fn delete_by_history_item_id(conn: &Connection, history_item_id: Uuid) -> Result<()> {
    conn.execute(
        "DELETE FROM dav_items WHERE history_item_id = ?1",
        params![history_item_id.to_string()],
    )?;
    Ok(())
}

pub fn update_health_check(
    conn: &Connection,
    id: Uuid,
    last: DateTime<Utc>,
    next: DateTime<Utc>,
) -> Result<()> {
    let updated = conn.execute(
        "UPDATE dav_items SET last_health_check = ?1, next_health_check = ?2 WHERE id = ?3",
        params![last.to_rfc3339(), next.to_rfc3339(), id.to_string()],
    )?;
    if updated == 0 {
        return Err(DavError::ItemNotFound(id.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;
    use crate::models::{ItemSubType, ItemType};
    use chrono::Utc;

    fn make_dir_item(name: &str, path: &str, parent_id: Option<Uuid>) -> DavItem {
        DavItem {
            id: Uuid::new_v4(),
            id_prefix: name[..1.min(name.len())].to_string(),
            created_at: Utc::now().naive_utc(),
            parent_id,
            name: name.to_string(),
            file_size: None,
            item_type: ItemType::Directory,
            sub_type: ItemSubType::Directory,
            path: path.to_string(),
            release_date: None,
            last_health_check: None,
            next_health_check: None,
            history_item_id: None,
            file_blob_id: None,
            nzb_blob_id: None,
        }
    }

    #[test]
    fn test_insert_and_get_by_id() {
        let conn = open_in_memory().unwrap();
        let item = make_dir_item("root", "/", None);
        insert(&conn, &item).unwrap();
        let fetched = get_by_id(&conn, item.id).unwrap().unwrap();
        assert_eq!(fetched.id, item.id);
        assert_eq!(fetched.name, "root");
        assert_eq!(fetched.path, "/");
    }

    #[test]
    fn test_get_by_path() {
        let conn = open_in_memory().unwrap();
        let item = make_dir_item("root", "/", None);
        insert(&conn, &item).unwrap();
        let fetched = get_by_path(&conn, "/").unwrap().unwrap();
        assert_eq!(fetched.id, item.id);
    }

    #[test]
    fn test_get_by_path_not_found() {
        let conn = open_in_memory().unwrap();
        let result = get_by_path(&conn, "/nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_children() {
        let conn = open_in_memory().unwrap();
        let root = make_dir_item("root", "/", None);
        insert(&conn, &root).unwrap();

        let child1 = make_dir_item("nzbs", "/nzbs/", Some(root.id));
        let child2 = make_dir_item("content", "/content/", Some(root.id));
        insert(&conn, &child1).unwrap();
        insert(&conn, &child2).unwrap();

        let children = get_children(&conn, root.id).unwrap();
        assert_eq!(children.len(), 2);
    }

    #[test]
    fn test_get_children_by_path() {
        let conn = open_in_memory().unwrap();
        let root = make_dir_item("root", "/", None);
        insert(&conn, &root).unwrap();

        let child = make_dir_item("nzbs", "/nzbs/", Some(root.id));
        insert(&conn, &child).unwrap();

        let children = get_children_by_path(&conn, "/").unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "nzbs");
    }

    #[test]
    fn test_delete() {
        let conn = open_in_memory().unwrap();
        let item = make_dir_item("root", "/", None);
        insert(&conn, &item).unwrap();
        delete(&conn, item.id).unwrap();
        assert!(get_by_id(&conn, item.id).unwrap().is_none());
    }

    #[test]
    fn test_delete_cascades() {
        let conn = open_in_memory().unwrap();
        let root = make_dir_item("root", "/", None);
        insert(&conn, &root).unwrap();
        let child = make_dir_item("nzbs", "/nzbs/", Some(root.id));
        insert(&conn, &child).unwrap();

        delete(&conn, root.id).unwrap();
        assert!(get_by_id(&conn, child.id).unwrap().is_none());
    }

    #[test]
    fn test_delete_by_history_item_id() {
        let conn = open_in_memory().unwrap();
        let history_id = Uuid::new_v4();
        let mut item = make_dir_item("file", "/file", None);
        item.history_item_id = Some(history_id);
        insert(&conn, &item).unwrap();

        delete_by_history_item_id(&conn, history_id).unwrap();
        assert!(get_by_id(&conn, item.id).unwrap().is_none());
    }

    #[test]
    fn test_update_health_check() {
        let conn = open_in_memory().unwrap();
        let item = make_dir_item("root", "/", None);
        insert(&conn, &item).unwrap();

        let last = Utc::now();
        let next = last + chrono::Duration::hours(24);
        update_health_check(&conn, item.id, last, next).unwrap();

        let fetched = get_by_id(&conn, item.id).unwrap().unwrap();
        assert!(fetched.last_health_check.is_some());
        assert!(fetched.next_health_check.is_some());
    }

    #[test]
    fn test_update_health_check_not_found() {
        let conn = open_in_memory().unwrap();
        let result = update_health_check(&conn, Uuid::new_v4(), Utc::now(), Utc::now());
        assert!(result.is_err());
    }

    #[test]
    fn test_optional_dates_roundtrip() {
        let conn = open_in_memory().unwrap();
        let mut item = make_dir_item("file", "/file", None);
        item.release_date = Some(Utc::now());
        item.item_type = ItemType::UsenetFile;
        item.sub_type = ItemSubType::NzbFile;
        insert(&conn, &item).unwrap();

        let fetched = get_by_id(&conn, item.id).unwrap().unwrap();
        assert!(fetched.release_date.is_some());
    }
}
