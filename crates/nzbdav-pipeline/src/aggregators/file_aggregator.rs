use nzbdav_core::models::{DavItem, DavNzbFile, ItemSubType, ItemType};
use uuid::Uuid;

use crate::types::ProcessedFile;

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

            let dav_item = DavItem {
                id: Uuid::new_v4(),
                id_prefix: String::new(),
                created_at: chrono::Utc::now().naive_utc(),
                parent_id: Some(parent_id),
                name: pf.filename.clone(),
                file_size: Some(pf.file_size as i64),
                item_type: ItemType::UsenetFile,
                sub_type: ItemSubType::NzbFile,
                path: format!("{parent_path}{}", pf.filename),
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
