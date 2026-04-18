use rusqlite::Connection;

use crate::error::Result;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS dav_items (
    id TEXT PRIMARY KEY,
    id_prefix TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    parent_id TEXT,
    name TEXT NOT NULL,
    file_size INTEGER,
    type INTEGER NOT NULL,
    sub_type INTEGER NOT NULL,
    path TEXT NOT NULL,
    release_date TEXT,
    last_health_check TEXT,
    next_health_check TEXT,
    history_item_id TEXT,
    file_blob_id TEXT,
    nzb_blob_id TEXT,
    FOREIGN KEY (parent_id) REFERENCES dav_items(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_dav_items_parent_name ON dav_items(parent_id, name);
CREATE INDEX IF NOT EXISTS idx_dav_items_prefix ON dav_items(id_prefix, type);
CREATE INDEX IF NOT EXISTS idx_dav_items_type_created ON dav_items(type, created_at);
CREATE INDEX IF NOT EXISTS idx_dav_items_sub_type_created ON dav_items(sub_type, created_at);
CREATE INDEX IF NOT EXISTS idx_dav_items_history ON dav_items(history_item_id, type, created_at);
CREATE INDEX IF NOT EXISTS idx_dav_items_nzb_blob ON dav_items(nzb_blob_id);
CREATE INDEX IF NOT EXISTS idx_dav_items_path ON dav_items(path);

CREATE TABLE IF NOT EXISTS queue_items (
    id TEXT PRIMARY KEY,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    file_name TEXT NOT NULL,
    job_name TEXT NOT NULL,
    nzb_file_size INTEGER NOT NULL,
    total_segment_bytes INTEGER NOT NULL,
    category TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    post_processing INTEGER NOT NULL DEFAULT -1,
    pause_until TEXT
);
CREATE INDEX IF NOT EXISTS idx_queue_priority_created ON queue_items(priority DESC, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_queue_category ON queue_items(category);

CREATE TABLE IF NOT EXISTS history_items (
    id TEXT PRIMARY KEY,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    file_name TEXT NOT NULL,
    job_name TEXT NOT NULL,
    category TEXT NOT NULL,
    download_status INTEGER NOT NULL,
    total_segment_bytes INTEGER NOT NULL,
    download_time_seconds INTEGER NOT NULL,
    fail_message TEXT,
    download_dir_id TEXT,
    nzb_blob_id TEXT
);
CREATE INDEX IF NOT EXISTS idx_history_created ON history_items(created_at);
CREATE INDEX IF NOT EXISTS idx_history_category ON history_items(category, created_at);

CREATE TABLE IF NOT EXISTS health_check_results (
    id TEXT PRIMARY KEY,
    dav_item_id TEXT NOT NULL,
    path TEXT NOT NULL,
    created_at TEXT NOT NULL,
    result INTEGER NOT NULL,
    repair_status INTEGER NOT NULL,
    message TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_hcr_created ON health_check_results(created_at);

CREATE TABLE IF NOT EXISTS config_items (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS accounts (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS blobs (
    id TEXT PRIMARY KEY,
    data BLOB NOT NULL
);

CREATE TABLE IF NOT EXISTS nzb_blobs (
    id TEXT PRIMARY KEY,
    data BLOB NOT NULL
);
"#;

pub fn open(path: &str) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

#[cfg(test)]
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_creation() {
        let conn = open_in_memory().unwrap();
        // Verify tables exist
        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(count >= 7, "Expected at least 7 tables, got {count}");
    }

    /// Every WebDAV path lookup does `WHERE path = ?1`. Without an index on
    /// the `path` column, SQLite falls back to a full table scan — fine for
    /// tiny libraries, disastrous as the virtual tree grows. This test asks
    /// SQLite to explain the plan and asserts an index is used.
    #[test]
    fn dav_items_path_lookup_uses_index() {
        let conn = open_in_memory().unwrap();
        let plan: String = conn
            .query_row(
                "EXPLAIN QUERY PLAN SELECT id FROM dav_items WHERE path = ?1",
                ["/foo/bar"],
                |row| row.get::<_, String>(3),
            )
            .unwrap();
        assert!(
            plan.to_uppercase().contains("USING INDEX"),
            "expected path lookup to use an index, got plan: {plan}"
        );
    }
}
