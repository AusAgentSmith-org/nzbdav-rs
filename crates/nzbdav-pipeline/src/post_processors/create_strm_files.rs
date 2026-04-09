use nzbdav_core::models::{DavItem, ItemSubType, ItemType};
use nzbdav_core::util::is_video_file;
use uuid::Uuid;

/// For each video file item, create a corresponding `.strm` file item.
///
/// STRM files contain a URL pointing to the WebDAV endpoint for the video.
/// Media managers (Sonarr/Radarr/Plex) can read `.strm` files to locate the
/// actual video stream.
pub fn create_strm_items(
    items: &[DavItem],
    parent_id: Uuid,
    parent_path: &str,
    webdav_base_url: &str,
) -> Vec<DavItem> {
    items
        .iter()
        .filter(|item| is_video_file(&item.name))
        .map(|item| {
            let stem = file_stem(&item.name);
            let strm_name = format!("{stem}.strm");
            let strm_path = format!("{parent_path}{strm_name}");
            let url = format!("{webdav_base_url}{}", item.path);

            DavItem {
                id: Uuid::new_v4(),
                id_prefix: strm_name
                    .chars()
                    .next()
                    .unwrap_or('s')
                    .to_ascii_lowercase()
                    .to_string(),
                created_at: chrono::Utc::now().naive_utc(),
                parent_id: Some(parent_id),
                name: strm_name,
                file_size: Some(url.len() as i64),
                item_type: ItemType::UsenetFile,
                sub_type: ItemSubType::NzbFile,
                path: strm_path,
                release_date: None,
                last_health_check: None,
                next_health_check: None,
                history_item_id: None,
                file_blob_id: None,
                nzb_blob_id: None,
            }
        })
        .collect()
}

/// Extract the file stem (filename without extension).
fn file_stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(pos) if pos > 0 => &name[..pos],
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_video_item(name: &str, path: &str) -> DavItem {
        DavItem {
            id: Uuid::new_v4(),
            id_prefix: name.chars().next().unwrap_or('x').to_string(),
            created_at: chrono::Utc::now().naive_utc(),
            parent_id: None,
            name: name.to_string(),
            file_size: Some(1_000_000),
            item_type: ItemType::UsenetFile,
            sub_type: ItemSubType::MultipartFile,
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
    fn creates_strm_for_video() {
        let parent_id = Uuid::new_v4();
        let items = vec![make_video_item(
            "movie.mkv",
            "/content/movies/job/movie.mkv",
        )];

        let strms = create_strm_items(
            &items,
            parent_id,
            "/content/movies/job/",
            "http://localhost:8080",
        );
        assert_eq!(strms.len(), 1);
        assert_eq!(strms[0].name, "movie.strm");
        assert_eq!(strms[0].path, "/content/movies/job/movie.strm");
        assert_eq!(strms[0].parent_id, Some(parent_id));
        assert_eq!(strms[0].item_type, ItemType::UsenetFile);
        assert_eq!(strms[0].sub_type, ItemSubType::NzbFile);

        let expected_url = "http://localhost:8080/content/movies/job/movie.mkv";
        assert_eq!(strms[0].file_size, Some(expected_url.len() as i64));
    }

    #[test]
    fn skips_non_video_files() {
        let parent_id = Uuid::new_v4();
        let items = vec![
            make_video_item("subs.srt", "/content/job/subs.srt"),
            make_video_item("movie.mkv", "/content/job/movie.mkv"),
            make_video_item("info.nfo", "/content/job/info.nfo"),
        ];

        let strms = create_strm_items(&items, parent_id, "/content/job/", "http://localhost:8080");
        assert_eq!(strms.len(), 1);
        assert_eq!(strms[0].name, "movie.strm");
    }

    #[test]
    fn empty_input_returns_empty() {
        let strms = create_strm_items(
            &[],
            Uuid::new_v4(),
            "/content/job/",
            "http://localhost:8080",
        );
        assert!(strms.is_empty());
    }
}
