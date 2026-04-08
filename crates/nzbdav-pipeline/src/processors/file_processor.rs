use nzbdav_core::models::{FilePart, LongRange};

use crate::deobfuscation::NzbFileInfo;
use crate::types::ProcessedFile;

/// Process plain (non-RAR, non-PAR2) files.
///
/// Each file gets a single `FilePart` spanning the entire file, with segment
/// IDs copied directly from the NZB file info.
pub fn process_plain_files(files: &[NzbFileInfo]) -> Vec<ProcessedFile> {
    files
        .iter()
        .filter(|f| !f.is_rar && !f.is_par2)
        .map(|f| {
            let file_part = FilePart {
                segment_ids: f.segment_ids.clone(),
                segment_id_byte_range: LongRange::new(0, f.file_size as i64),
                file_part_byte_range: LongRange::new(0, f.file_size as i64),
            };

            ProcessedFile {
                filename: f.resolved_name.clone(),
                file_size: f.file_size,
                is_directory: false,
                source_file_index: f.file_index,
                volume_number: None,
                file_parts: vec![file_part],
                is_encrypted: false,
                encryption: None,
            }
        })
        .collect()
}
