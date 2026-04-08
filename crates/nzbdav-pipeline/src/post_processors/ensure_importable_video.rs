use nzbdav_core::models::DavItem;
use nzbdav_core::util::is_video_file;

use crate::error::{PipelineError, Result};

/// Verify that at least one video file exists in the items.
///
/// Returns `Ok(())` if a video is found, or
/// `Err(PipelineError::NoImportableVideo)` otherwise. This is used to
/// short-circuit processing when a media manager (Sonarr/Radarr) won't be
/// able to import anything.
pub fn ensure_importable_video(items: &[DavItem]) -> Result<()> {
    if items.iter().any(|item| is_video_file(&item.name)) {
        Ok(())
    } else {
        Err(PipelineError::NoImportableVideo)
    }
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
    fn passes_with_video_file() {
        let items = vec![make_item("movie.mkv"), make_item("subs.srt")];
        assert!(ensure_importable_video(&items).is_ok());
    }

    #[test]
    fn fails_without_video_file() {
        let items = vec![make_item("subs.srt"), make_item("info.nfo")];
        assert!(ensure_importable_video(&items).is_err());
    }

    #[test]
    fn fails_on_empty_list() {
        assert!(ensure_importable_video(&[]).is_err());
    }

    #[test]
    fn recognises_various_video_extensions() {
        for ext in &["mkv", "avi", "mp4", "wmv", "ts", "m4v", "mov", "webm"] {
            let name = format!("file.{ext}");
            let items = vec![make_item(&name)];
            assert!(
                ensure_importable_video(&items).is_ok(),
                "should accept .{ext}"
            );
        }
    }
}
