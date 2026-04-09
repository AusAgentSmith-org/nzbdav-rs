use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// In-memory config cache backed by SQLite `config_items` table.
#[derive(Clone)]
pub struct ConfigManager {
    cache: Arc<RwLock<HashMap<String, String>>>,
}

impl Default for ConfigManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigManager {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn load_from_db(&self, db: &rusqlite::Connection) -> crate::error::Result<()> {
        let mut stmt = db.prepare("SELECT key, value FROM config_items")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut cache = self.cache.write();
        cache.clear();
        for row in rows {
            let (key, value) = row?;
            cache.insert(key, value);
        }
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.cache.read().get(key).cloned()
    }

    pub fn get_or_default(&self, key: &str, default: &str) -> String {
        self.cache
            .read()
            .get(key)
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }

    pub fn set(
        &self,
        db: &rusqlite::Connection,
        key: &str,
        value: &str,
    ) -> crate::error::Result<()> {
        db.execute(
            "INSERT INTO config_items (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )?;
        self.cache
            .write()
            .insert(key.to_string(), value.to_string());
        Ok(())
    }

    // -- Typed accessors --

    pub fn get_max_download_connections(&self) -> usize {
        self.get("usenet.max-download-connections")
            .and_then(|v| v.parse().ok())
            .unwrap_or(15)
    }

    pub fn get_article_buffer_size(&self) -> usize {
        self.get("usenet.article-buffer-size")
            .and_then(|v| v.parse().ok())
            .unwrap_or(40)
    }

    pub fn get_categories(&self) -> Vec<String> {
        self.get_or_default("api.categories", "")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    pub fn get_duplicate_nzb_behavior(&self) -> String {
        self.get_or_default("api.duplicate-nzb-behavior", "increment")
    }

    pub fn get_import_strategy(&self) -> String {
        self.get_or_default("api.import-strategy", "symlinks")
    }

    pub fn get_file_blocklist(&self) -> Vec<String> {
        self.get_or_default("api.download-file-blocklist", "")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    pub fn is_ensure_importable_video_enabled(&self) -> bool {
        self.get("api.ensure-importable-video")
            .map(|v| v == "true")
            .unwrap_or(true)
    }

    pub fn get_max_concurrent_queue(&self) -> usize {
        self.get("queue.max-concurrent")
            .and_then(|v| v.parse().ok())
            .unwrap_or(2)
    }

    pub fn get_ensure_article_existence_categories(&self) -> Vec<String> {
        self.get_or_default("api.ensure-article-existence-categories", "")
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn test_new_default_values() {
        let cfg = ConfigManager::new();
        assert_eq!(cfg.get_max_download_connections(), 15);
        assert_eq!(cfg.get_article_buffer_size(), 40);
        assert!(cfg.get_categories().is_empty());
        assert_eq!(cfg.get_duplicate_nzb_behavior(), "increment");
        assert_eq!(cfg.get_import_strategy(), "symlinks");
        assert!(cfg.get_file_blocklist().is_empty());
        assert!(cfg.is_ensure_importable_video_enabled());
        assert!(cfg.get_ensure_article_existence_categories().is_empty());
        assert_eq!(cfg.get_max_concurrent_queue(), 2);
    }

    #[test]
    fn test_set_and_get() {
        let conn = open_in_memory().unwrap();
        let cfg = ConfigManager::new();

        cfg.set(&conn, "usenet.max-download-connections", "25")
            .unwrap();
        assert_eq!(cfg.get("usenet.max-download-connections").unwrap(), "25");
        assert_eq!(cfg.get_max_download_connections(), 25);

        // Overwrite
        cfg.set(&conn, "usenet.max-download-connections", "30")
            .unwrap();
        assert_eq!(cfg.get_max_download_connections(), 30);
    }

    #[test]
    fn test_load_from_db() {
        let conn = open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO config_items (key, value) VALUES ('usenet.max-download-connections', '42')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO config_items (key, value) VALUES ('api.import-strategy', 'copy')",
            [],
        )
        .unwrap();

        let cfg = ConfigManager::new();
        cfg.load_from_db(&conn).unwrap();

        assert_eq!(cfg.get_max_download_connections(), 42);
        assert_eq!(cfg.get_import_strategy(), "copy");
    }

    #[test]
    fn test_get_categories_parsing() {
        let conn = open_in_memory().unwrap();
        let cfg = ConfigManager::new();

        cfg.set(&conn, "api.categories", "tv,movies, music")
            .unwrap();
        let cats = cfg.get_categories();
        assert_eq!(cats, vec!["tv", "movies", "music"]);
    }
}
