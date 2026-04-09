use chrono::NaiveDateTime;
use rusqlite::{params, Connection, Row};
use uuid::Uuid;

use crate::error::Result;
use crate::models::{DownloadStatus, HistoryItem};

fn row_to_history_item(row: &Row) -> rusqlite::Result<HistoryItem> {
    let id: String = row.get("id")?;
    let created_at_str: String = row.get("created_at")?;
    let status_i: i32 = row.get("download_status")?;
    let download_dir_id: Option<String> = row.get("download_dir_id")?;
    let nzb_blob_id: Option<String> = row.get("nzb_blob_id")?;

    let parse_uuid = |s: String| -> rusqlite::Result<Uuid> {
        Uuid::parse_str(&s)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
    };

    Ok(HistoryItem {
        id: parse_uuid(id)?,
        created_at: NaiveDateTime::parse_from_str(&created_at_str, "%Y-%m-%d %H:%M:%S")
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?,
        file_name: row.get("file_name")?,
        job_name: row.get("job_name")?,
        category: row.get("category")?,
        download_status: DownloadStatus::try_from(status_i)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Integer, e.into()))?,
        total_segment_bytes: row.get("total_segment_bytes")?,
        download_time_seconds: row.get("download_time_seconds")?,
        fail_message: row.get("fail_message")?,
        download_dir_id: download_dir_id.map(parse_uuid).transpose()?,
        nzb_blob_id: nzb_blob_id.map(parse_uuid).transpose()?,
    })
}

const COLUMNS: &str = "id, created_at, file_name, job_name, category, download_status, total_segment_bytes, download_time_seconds, fail_message, download_dir_id, nzb_blob_id";

pub fn insert(conn: &Connection, item: &HistoryItem) -> Result<()> {
    conn.execute(
        &format!("INSERT INTO history_items ({COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)"),
        params![
            item.id.to_string(),
            item.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            item.file_name,
            item.job_name,
            item.category,
            item.download_status as i32,
            item.total_segment_bytes,
            item.download_time_seconds,
            item.fail_message,
            item.download_dir_id.map(|u| u.to_string()),
            item.nzb_blob_id.map(|u| u.to_string()),
        ],
    )?;
    Ok(())
}

pub fn get_by_id(conn: &Connection, id: Uuid) -> Result<Option<HistoryItem>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM history_items WHERE id = ?1"))?;
    let mut rows = stmt.query_map(params![id.to_string()], row_to_history_item)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

pub fn list(conn: &Connection, offset: i64, limit: i64) -> Result<Vec<HistoryItem>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM history_items ORDER BY created_at DESC LIMIT ?1 OFFSET ?2"
    ))?;
    let rows = stmt.query_map(params![limit, offset], row_to_history_item)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

pub fn delete(conn: &Connection, id: Uuid) -> Result<()> {
    conn.execute(
        "DELETE FROM history_items WHERE id = ?1",
        params![id.to_string()],
    )?;
    Ok(())
}

pub fn delete_all(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM history_items", [])?;
    Ok(())
}

pub fn count(conn: &Connection) -> Result<i64> {
    let c: i64 = conn.query_row("SELECT COUNT(*) FROM history_items", [], |row| row.get(0))?;
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;
    use chrono::Utc;

    fn make_history_item(status: DownloadStatus) -> HistoryItem {
        HistoryItem {
            id: Uuid::new_v4(),
            created_at: Utc::now().naive_utc(),
            file_name: "test.nzb".to_string(),
            job_name: "test".to_string(),
            category: "default".to_string(),
            download_status: status,
            total_segment_bytes: 10240,
            download_time_seconds: 60,
            fail_message: None,
            download_dir_id: None,
            nzb_blob_id: None,
        }
    }

    #[test]
    fn test_insert_and_get_by_id() {
        let conn = open_in_memory().unwrap();
        let item = make_history_item(DownloadStatus::Completed);
        insert(&conn, &item).unwrap();
        let fetched = get_by_id(&conn, item.id).unwrap().unwrap();
        assert_eq!(fetched.id, item.id);
        assert_eq!(fetched.download_status, DownloadStatus::Completed);
    }

    #[test]
    fn test_list_pagination() {
        let conn = open_in_memory().unwrap();
        for _ in 0..5 {
            insert(&conn, &make_history_item(DownloadStatus::Completed)).unwrap();
        }
        let page1 = list(&conn, 0, 2).unwrap();
        assert_eq!(page1.len(), 2);

        let page2 = list(&conn, 2, 2).unwrap();
        assert_eq!(page2.len(), 2);

        let page3 = list(&conn, 4, 2).unwrap();
        assert_eq!(page3.len(), 1);
    }

    #[test]
    fn test_delete() {
        let conn = open_in_memory().unwrap();
        let item = make_history_item(DownloadStatus::Failed);
        insert(&conn, &item).unwrap();
        delete(&conn, item.id).unwrap();
        assert!(get_by_id(&conn, item.id).unwrap().is_none());
    }

    #[test]
    fn test_count() {
        let conn = open_in_memory().unwrap();
        assert_eq!(count(&conn).unwrap(), 0);
        insert(&conn, &make_history_item(DownloadStatus::Completed)).unwrap();
        insert(&conn, &make_history_item(DownloadStatus::Failed)).unwrap();
        assert_eq!(count(&conn).unwrap(), 2);
    }

    #[test]
    fn test_failed_with_message() {
        let conn = open_in_memory().unwrap();
        let mut item = make_history_item(DownloadStatus::Failed);
        item.fail_message = Some("disk full".to_string());
        insert(&conn, &item).unwrap();
        let fetched = get_by_id(&conn, item.id).unwrap().unwrap();
        assert_eq!(fetched.fail_message.as_deref(), Some("disk full"));
    }
}
