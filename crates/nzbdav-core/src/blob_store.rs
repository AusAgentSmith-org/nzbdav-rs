use rusqlite::Connection;
use uuid::Uuid;

use crate::error::{DavError, Result};

/// Store and retrieve binary blobs (file metadata, NZB XML).
pub struct BlobStore;

impl BlobStore {
    pub fn put(conn: &Connection, table: &str, id: Uuid, data: &[u8]) -> Result<()> {
        let sql = format!("INSERT OR REPLACE INTO {table} (id, data) VALUES (?1, ?2)");
        conn.execute(&sql, rusqlite::params![id.to_string(), data])?;
        Ok(())
    }

    pub fn get(conn: &Connection, table: &str, id: Uuid) -> Result<Vec<u8>> {
        let sql = format!("SELECT data FROM {table} WHERE id = ?1");
        conn.query_row(&sql, rusqlite::params![id.to_string()], |row| {
            row.get(0)
        })
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => DavError::BlobNotFound(id.to_string()),
            other => DavError::Database(other),
        })
    }

    pub fn delete(conn: &Connection, table: &str, id: Uuid) -> Result<()> {
        let sql = format!("DELETE FROM {table} WHERE id = ?1");
        conn.execute(&sql, rusqlite::params![id.to_string()])?;
        Ok(())
    }

    // Convenience methods for specific blob tables

    pub fn put_file_blob(conn: &Connection, id: Uuid, data: &[u8]) -> Result<()> {
        Self::put(conn, "blobs", id, data)
    }

    pub fn get_file_blob(conn: &Connection, id: Uuid) -> Result<Vec<u8>> {
        Self::get(conn, "blobs", id)
    }

    pub fn put_nzb_blob(conn: &Connection, id: Uuid, data: &[u8]) -> Result<()> {
        Self::put(conn, "nzb_blobs", id, data)
    }

    pub fn get_nzb_blob(conn: &Connection, id: Uuid) -> Result<Vec<u8>> {
        Self::get(conn, "nzb_blobs", id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn test_put_get_file_blob() {
        let conn = open_in_memory().unwrap();
        let id = Uuid::new_v4();
        let data = b"hello blob world";

        BlobStore::put_file_blob(&conn, id, data).unwrap();
        let fetched = BlobStore::get_file_blob(&conn, id).unwrap();
        assert_eq!(fetched, data);
    }

    #[test]
    fn test_get_missing_blob() {
        let conn = open_in_memory().unwrap();
        let id = Uuid::new_v4();
        let result = BlobStore::get_file_blob(&conn, id);
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), DavError::BlobNotFound(_)),
            "expected BlobNotFound error"
        );
    }

    #[test]
    fn test_put_get_nzb_blob() {
        let conn = open_in_memory().unwrap();
        let id = Uuid::new_v4();
        let data = b"<nzb>xml content</nzb>";

        BlobStore::put_nzb_blob(&conn, id, data).unwrap();
        let fetched = BlobStore::get_nzb_blob(&conn, id).unwrap();
        assert_eq!(fetched, data);
    }

    #[test]
    fn test_delete_blob() {
        let conn = open_in_memory().unwrap();
        let id = Uuid::new_v4();
        let data = b"temporary data";

        BlobStore::put_file_blob(&conn, id, data).unwrap();
        BlobStore::delete(&conn, "blobs", id).unwrap();

        let result = BlobStore::get_file_blob(&conn, id);
        assert!(matches!(result.unwrap_err(), DavError::BlobNotFound(_)));
    }
}
