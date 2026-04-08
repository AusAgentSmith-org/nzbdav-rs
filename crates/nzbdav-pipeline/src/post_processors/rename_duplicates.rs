use std::collections::HashMap;

use nzbdav_core::models::DavItem;

/// Rename duplicate filenames by appending (2), (3), etc.
///
/// Operates on a mutable slice of [`DavItem`]s, modifying `name` and `path`
/// in-place when a collision is detected.
pub fn rename_duplicates(items: &mut [DavItem]) {
    let mut seen: HashMap<String, u32> = HashMap::new();

    for item in items.iter_mut() {
        let lower_name = item.name.to_ascii_lowercase();
        let count = seen.entry(lower_name).or_insert(0);
        *count += 1;

        if *count > 1 {
            let (stem, ext) = split_name_ext(&item.name);
            let new_name = format!("{stem} ({count}){ext}");

            // Update path: replace the trailing filename portion.
            if let Some(parent_path) = item.path.strip_suffix(&item.name) {
                item.path = format!("{parent_path}{new_name}");
            }
            item.name = new_name;
        }
    }
}

/// Split a filename into stem and extension (including the dot).
/// Returns `("movie", ".mkv")` for `"movie.mkv"`, or `("readme", "")` if no
/// extension.
fn split_name_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(pos) if pos > 0 => (&name[..pos], &name[pos..]),
        _ => (name, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nzbdav_core::models::{DavItem, ItemSubType, ItemType};
    use uuid::Uuid;

    fn make_item(name: &str, path: &str) -> DavItem {
        DavItem {
            id: Uuid::new_v4(),
            id_prefix: name.chars().next().unwrap_or('x').to_string(),
            created_at: chrono::Utc::now().naive_utc(),
            parent_id: None,
            name: name.to_string(),
            file_size: Some(1024),
            item_type: ItemType::UsenetFile,
            sub_type: ItemSubType::NzbFile,
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
    fn no_duplicates_unchanged() {
        let mut items = vec![
            make_item("movie.mkv", "/content/job/movie.mkv"),
            make_item("subs.srt", "/content/job/subs.srt"),
        ];
        rename_duplicates(&mut items);
        assert_eq!(items[0].name, "movie.mkv");
        assert_eq!(items[1].name, "subs.srt");
    }

    #[test]
    fn duplicates_get_numbered() {
        let mut items = vec![
            make_item("movie.mkv", "/content/job/movie.mkv"),
            make_item("movie.mkv", "/content/job/movie.mkv"),
            make_item("movie.mkv", "/content/job/movie.mkv"),
        ];
        rename_duplicates(&mut items);
        assert_eq!(items[0].name, "movie.mkv");
        assert_eq!(items[1].name, "movie (2).mkv");
        assert_eq!(items[1].path, "/content/job/movie (2).mkv");
        assert_eq!(items[2].name, "movie (3).mkv");
        assert_eq!(items[2].path, "/content/job/movie (3).mkv");
    }

    #[test]
    fn case_insensitive_matching() {
        let mut items = vec![
            make_item("Movie.MKV", "/content/job/Movie.MKV"),
            make_item("movie.mkv", "/content/job/movie.mkv"),
        ];
        rename_duplicates(&mut items);
        assert_eq!(items[0].name, "Movie.MKV");
        assert_eq!(items[1].name, "movie (2).mkv");
    }

    #[test]
    fn no_extension() {
        let mut items = vec![
            make_item("README", "/content/job/README"),
            make_item("README", "/content/job/README"),
        ];
        rename_duplicates(&mut items);
        assert_eq!(items[0].name, "README");
        assert_eq!(items[1].name, "README (2)");
    }
}
