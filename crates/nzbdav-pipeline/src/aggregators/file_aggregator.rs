use nzbdav_core::models::{DavItem, DavNzbFile, ItemSubType, ItemType};
use uuid::Uuid;

use crate::types::ProcessedFile;

use super::dav_names::dav_leaf_name;

/// Create `DavItem` + `DavNzbFile` entries for plain (non-RAR) files.
///
/// Each processed file gets a single `DavNzbFile` with the segment IDs from
/// its first (and only) `FilePart`.
pub fn aggregate_plain_files(
    processed_files: &[ProcessedFile],
    parent_id: Uuid,
    parent_path: &str,
) -> Vec<(DavItem, DavNzbFile)> {
    processed_files
        .iter()
        .map(|pf| {
            let segment_ids = pf
                .file_parts
                .first()
                .map(|fp| fp.segment_ids.clone())
                .unwrap_or_default();

            let dav_name = dav_leaf_name(&pf.filename);
            let dav_item = DavItem {
                id: Uuid::new_v4(),
                id_prefix: String::new(),
                created_at: chrono::Utc::now().naive_utc(),
                parent_id: Some(parent_id),
                name: dav_name.clone(),
                file_size: Some(pf.file_size as i64),
                item_type: ItemType::UsenetFile,
                sub_type: ItemSubType::NzbFile,
                path: format!("{parent_path}{dav_name}"),
                release_date: None,
                last_health_check: None,
                next_health_check: None,
                history_item_id: None,
                file_blob_id: None,
                nzb_blob_id: None,
            };

            let dav_nzb = DavNzbFile { segment_ids };

            (dav_item, dav_nzb)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nzbdav_core::models::{FilePart, LongRange};

    #[test]
    fn plain_paths_are_flattened_to_leaf_names() {
        let parent_id = Uuid::new_v4();
        let files = vec![ProcessedFile {
            filename: "folder\\movie.mkv".to_string(),
            file_size: 1024,
            is_directory: false,
            source_file_index: 0,
            volume_number: None,
            file_parts: vec![FilePart {
                segment_ids: vec!["message-id".to_string()],
                segment_id_byte_range: LongRange::new(0, 1024),
                file_part_byte_range: LongRange::new(0, 1024),
            }],
            is_encrypted: false,
            encryption: None,
        }];

        let aggregated = aggregate_plain_files(&files, parent_id, "/content/download/");

        assert_eq!(aggregated[0].0.name, "movie.mkv");
        assert_eq!(aggregated[0].0.path, "/content/download/movie.mkv");
    }
}
