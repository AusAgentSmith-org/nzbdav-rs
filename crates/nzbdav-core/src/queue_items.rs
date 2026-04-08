use chrono::NaiveDateTime;
use rusqlite::{params, Connection, Row};
use uuid::Uuid;

use crate::error::Result;
use crate::models::QueueItem;

fn row_to_queue_item(row: &Row) -> rusqlite::Result<QueueItem> {
    let id: String = row.get("id")?;
    let created_at_str: String = row.get("created_at")?;
    let pause_until_str: Option<String> = row.get("pause_until")?;

    Ok(QueueItem {
        id: Uuid::parse_str(&id)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?,
        created_at: NaiveDateTime::parse_from_str(&created_at_str, "%Y-%m-%d %H:%M:%S")
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?,
        file_name: row.get("file_name")?,
        job_name: row.get("job_name")?,
        nzb_file_size: row.get("nzb_file_size")?,
        total_segment_bytes: row.get("total_segment_bytes")?,
        category: row.get("category")?,
        priority: row.get("priority")?,
        post_processing: row.get("post_processing")?,
        pause_until: pause_until_str
            .map(|s| NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S"))
            .transpose()
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?,
    })
}

const COLUMNS: &str = "id, created_at, file_name, job_name, nzb_file_size, total_segment_bytes, category, priority, post_processing, pause_until";

pub fn insert(conn: &Connection, item: &QueueItem) -> Result<()> {
    conn.execute(
        &format!("INSERT INTO queue_items ({COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"),
        params![
            item.id.to_string(),
            item.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            item.file_name,
            item.job_name,
            item.nzb_file_size,
            item.total_segment_bytes,
            item.category,
            item.priority,
            item.post_processing,
            item.pause_until.map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string()),
        ],
    )?;
    Ok(())
}

pub fn get_by_id(conn: &Connection, id: Uuid) -> Result<Option<QueueItem>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM queue_items WHERE id = ?1"))?;
    let mut rows = stmt.query_map(params![id.to_string()], row_to_queue_item)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

pub fn get_next(conn: &Connection) -> Result<Option<QueueItem>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM queue_items \
         WHERE pause_until IS NULL OR pause_until <= datetime('now') \
         ORDER BY priority DESC, created_at ASC \
         LIMIT 1"
    ))?;
    let mut rows = stmt.query_map([], row_to_queue_item)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

pub fn get_next_excluding(conn: &Connection, exclude_ids: &[Uuid]) -> Result<Option<QueueItem>> {
    if exclude_ids.is_empty() {
        return get_next(conn);
    }
    let placeholders: Vec<String> = exclude_ids.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect();
    let sql = format!(
        "SELECT {COLUMNS} FROM queue_items \
         WHERE (pause_until IS NULL OR pause_until <= datetime('now')) \
         AND id NOT IN ({}) \
         ORDER BY priority DESC, created_at ASC \
         LIMIT 1",
        placeholders.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let id_strings: Vec<String> = exclude_ids.iter().map(|id| id.to_string()).collect();
    let params: Vec<&dyn rusqlite::ToSql> = id_strings.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    let mut rows = stmt.query_map(params.as_slice(), row_to_queue_item)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

pub fn list(conn: &Connection) -> Result<Vec<QueueItem>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM queue_items ORDER BY priority DESC, created_at ASC"
    ))?;
    let rows = stmt.query_map([], row_to_queue_item)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

pub fn list_paginated(conn: &Connection, offset: i64, limit: i64) -> Result<Vec<QueueItem>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM queue_items ORDER BY priority DESC, created_at ASC LIMIT ?1 OFFSET ?2"
    ))?;
    let rows = stmt.query_map(params![limit, offset], row_to_queue_item)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

pub fn update_pause_until(
    conn: &Connection,
    id: Uuid,
    pause_until: Option<NaiveDateTime>,
) -> Result<()> {
    conn.execute(
        "UPDATE queue_items SET pause_until = ?1 WHERE id = ?2",
        params![
            pause_until.map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string()),
            id.to_string(),
        ],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: Uuid) -> Result<()> {
    conn.execute(
        "DELETE FROM queue_items WHERE id = ?1",
        params![id.to_string()],
    )?;
    Ok(())
}

pub fn count(conn: &Connection) -> Result<i64> {
    let c: i64 = conn.query_row("SELECT COUNT(*) FROM queue_items", [], |row| row.get(0))?;
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;
    use chrono::Utc;

    fn make_queue_item(priority: i32) -> QueueItem {
        QueueItem {
            id: Uuid::new_v4(),
            created_at: Utc::now().naive_utc(),
            file_name: "test.nzb".to_string(),
            job_name: "test".to_string(),
            nzb_file_size: 1024,
            total_segment_bytes: 10240,
            category: "default".to_string(),
            priority,
            post_processing: -1,
            pause_until: None,
        }
    }

    #[test]
    fn test_insert_and_get_by_id() {
        let conn = open_in_memory().unwrap();
        let item = make_queue_item(0);
        insert(&conn, &item).unwrap();
        let fetched = get_by_id(&conn, item.id).unwrap().unwrap();
        assert_eq!(fetched.id, item.id);
        assert_eq!(fetched.file_name, "test.nzb");
    }

    #[test]
    fn test_get_next_priority_ordering() {
        let conn = open_in_memory().unwrap();
        let low = make_queue_item(0);
        let high = make_queue_item(10);
        insert(&conn, &low).unwrap();
        insert(&conn, &high).unwrap();

        let next = get_next(&conn).unwrap().unwrap();
        assert_eq!(next.id, high.id);
    }

    #[test]
    fn test_get_next_skips_paused() {
        let conn = open_in_memory().unwrap();
        let mut paused = make_queue_item(10);
        paused.pause_until = Some(
            Utc::now().naive_utc() + chrono::Duration::hours(1),
        );
        let ready = make_queue_item(0);
        insert(&conn, &paused).unwrap();
        insert(&conn, &ready).unwrap();

        let next = get_next(&conn).unwrap().unwrap();
        assert_eq!(next.id, ready.id);
    }

    #[test]
    fn test_get_next_empty() {
        let conn = open_in_memory().unwrap();
        assert!(get_next(&conn).unwrap().is_none());
    }

    #[test]
    fn test_list() {
        let conn = open_in_memory().unwrap();
        insert(&conn, &make_queue_item(0)).unwrap();
        insert(&conn, &make_queue_item(5)).unwrap();
        let items = list(&conn).unwrap();
        assert_eq!(items.len(), 2);
        // Higher priority first
        assert!(items[0].priority >= items[1].priority);
    }

    #[test]
    fn test_update_pause_until() {
        let conn = open_in_memory().unwrap();
        let item = make_queue_item(0);
        insert(&conn, &item).unwrap();

        let pause = Utc::now().naive_utc() + chrono::Duration::hours(1);
        update_pause_until(&conn, item.id, Some(pause)).unwrap();

        let fetched = get_by_id(&conn, item.id).unwrap().unwrap();
        assert!(fetched.pause_until.is_some());

        // Clear pause
        update_pause_until(&conn, item.id, None).unwrap();
        let fetched = get_by_id(&conn, item.id).unwrap().unwrap();
        assert!(fetched.pause_until.is_none());
    }

    #[test]
    fn test_delete() {
        let conn = open_in_memory().unwrap();
        let item = make_queue_item(0);
        insert(&conn, &item).unwrap();
        delete(&conn, item.id).unwrap();
        assert!(get_by_id(&conn, item.id).unwrap().is_none());
    }

    #[test]
    fn test_count() {
        let conn = open_in_memory().unwrap();
        assert_eq!(count(&conn).unwrap(), 0);
        insert(&conn, &make_queue_item(0)).unwrap();
        insert(&conn, &make_queue_item(0)).unwrap();
        assert_eq!(count(&conn).unwrap(), 2);
    }

    #[test]
    fn test_get_next_excluding() {
        let conn = open_in_memory().unwrap();
        let high = make_queue_item(10);
        let low = make_queue_item(0);
        insert(&conn, &high).unwrap();
        insert(&conn, &low).unwrap();

        // Excluding the high-priority item should return the low one
        let next = get_next_excluding(&conn, &[high.id]).unwrap().unwrap();
        assert_eq!(next.id, low.id);

        // Excluding both should return None
        assert!(get_next_excluding(&conn, &[high.id, low.id]).unwrap().is_none());

        // Empty exclusion list should behave like get_next
        let next = get_next_excluding(&conn, &[]).unwrap().unwrap();
        assert_eq!(next.id, high.id);
    }

    #[test]
    fn test_list_paginated() {
        let conn = open_in_memory().unwrap();
        for i in 0..5 {
            insert(&conn, &make_queue_item(i)).unwrap();
        }

        // First page
        let page1 = list_paginated(&conn, 0, 2).unwrap();
        assert_eq!(page1.len(), 2);
        // Highest priority first
        assert_eq!(page1[0].priority, 4);
        assert_eq!(page1[1].priority, 3);

        // Second page
        let page2 = list_paginated(&conn, 2, 2).unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].priority, 2);

        // Beyond end
        let page3 = list_paginated(&conn, 10, 2).unwrap();
        assert!(page3.is_empty());
    }
}
