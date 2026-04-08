use nzbdav_core::models::DavItem;
use nzbdav_core::util::matches_blocklist;

/// Remove items whose filenames match any blocklist pattern.
///
/// Returns the filtered list containing only items that do **not** match any
/// pattern. Uses glob-style matching via [`matches_blocklist`].
pub fn filter_blocklisted(items: Vec<DavItem>, blocklist: &[String]) -> Vec<DavItem> {
    if blocklist.is_empty() {
        return items;
    }

    items
        .into_iter()
        .filter(|item| !matches_blocklist(&item.name, blocklist))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nzbdav_core::models::{ItemSubType, ItemType};
    use uuid::Uuid;

    fn make_item(name: &str) -> DavItem {
        DavItem {
            id: Uuid::new_v4(),
            id_prefix: name.chars().next().unwrap_or('x').to_string(),
            created_at: chrono::Utc::now().naive_utc(),
            parent_id: None,
            name: name.to_string(),
            file_size: Some(1024),
            item_type: ItemType::UsenetFile,
            sub_type: ItemSubType::NzbFile,
            path: format!("/content/job/{name}"),
            release_date: None,
            last_health_check: None,
            next_health_check: None,
            history_item_id: None,
            file_blob_id: None,
            nzb_blob_id: None,
        }
    }

    #[test]
    fn empty_blocklist_keeps_all() {
        let items = vec![make_item("movie.mkv"), make_item("subs.srt")];
        let result = filter_blocklisted(items, &[]);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filters_matching_pattern() {
        let items = vec![
            make_item("movie.mkv"),
            make_item("sample.mkv"),
            make_item("subs.srt"),
        ];
        let blocklist = vec!["sample.*".to_string()];
        let result = filter_blocklisted(items, &blocklist);
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|i| i.name != "sample.mkv"));
    }

    #[test]
    fn filters_wildcard_extension() {
        let items = vec![
            make_item("movie.mkv"),
            make_item("info.nfo"),
            make_item("readme.nfo"),
        ];
        let blocklist = vec!["*.nfo".to_string()];
        let result = filter_blocklisted(items, &blocklist);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "movie.mkv");
    }
}
