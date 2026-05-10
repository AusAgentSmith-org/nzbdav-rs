use std::collections::HashMap;

use nzbdav_core::models::{
    AesParams, DavItem, DavMultipartFile, FilePart, ItemSubType, ItemType, LongRange,
};
use nzbdav_rar::header::RarEncryption;
use uuid::Uuid;

use crate::error::Result;
use crate::types::ProcessedFile;

use super::dav_names::dav_leaf_name;

/// Aggregate RAR-extracted files: group by filename across volumes, compute
/// virtual file byte ranges, and create `DavMultipartFile` entries.
///
/// Files split across multiple RAR volumes share the same filename and are
/// reassembled by sorting on `volume_number`, then laying out each part's
/// `file_part_byte_range` contiguously in a virtual file.
///
/// If a file has per-file encryption (RAR5 file-level), the `password` is used
/// to derive AES params and attach them to the `DavMultipartFile` so the
/// streaming layer can decrypt on playback.
pub fn aggregate_rar_files(
    processed_files: &[ProcessedFile],
    parent_id: Uuid,
    parent_path: &str,
    password: Option<&str>,
) -> Result<Vec<(DavItem, DavMultipartFile)>> {
    // Group by filename.
    let mut groups: HashMap<&str, Vec<&ProcessedFile>> = HashMap::new();
    for pf in processed_files {
        groups.entry(&pf.filename).or_default().push(pf);
    }

    let mut results = Vec::new();

    for (filename, mut parts) in groups {
        // Sort by volume number (None sorts before Some).
        parts.sort_by_key(|p| p.volume_number);

        // Build contiguous file parts with adjusted byte ranges.
        let mut file_parts = Vec::new();
        let mut offset: i64 = 0;

        for pf in &parts {
            for part in &pf.file_parts {
                let part_size = part.segment_id_byte_range.size();
                file_parts.push(FilePart {
                    segment_ids: part.segment_ids.clone(),
                    segment_id_byte_range: part.segment_id_byte_range,
                    file_part_byte_range: LongRange::new(offset, offset + part_size),
                });
                offset += part_size;
            }
        }

        let total_size = offset;

        let dav_name = dav_leaf_name(filename);
        let dav_item = DavItem {
            id: Uuid::new_v4(),
            id_prefix: String::new(),
            created_at: chrono::Utc::now().naive_utc(),
            parent_id: Some(parent_id),
            name: dav_name.clone(),
            file_size: Some(total_size),
            item_type: ItemType::UsenetFile,
            sub_type: ItemSubType::MultipartFile,
            path: format!("{parent_path}{dav_name}"),
            release_date: None,
            last_health_check: None,
            next_health_check: None,
            history_item_id: None,
            file_blob_id: None,
            nzb_blob_id: None,
        };

        // Derive AES params for encrypted files if a password is available.
        let aes_params = if let Some(pw) = password {
            // Check if any part in this group is encrypted — use the first
            // encryption record found (all volumes share the same password).
            parts.iter().find_map(|pf| {
                if !pf.is_encrypted {
                    return None;
                }
                match &pf.encryption {
                    Some(RarEncryption::Rar5 {
                        lg2_count,
                        salt,
                        iv,
                        use_psw_check,
                        psw_check,
                    }) => {
                        let header = nzbdav_rar::crypto::Rar5EncryptionHeader {
                            lg2_count: *lg2_count,
                            salt: salt.clone(),
                            has_psw_check: *use_psw_check,
                            psw_check: psw_check.clone(),
                            psw_check_sum: Vec::new(),
                        };
                        match nzbdav_rar::crypto::derive_rar5_key(pw, &header) {
                            Ok(derived) => Some(AesParams {
                                key: derived.key.to_vec(),
                                iv: iv.clone(),
                                decoded_size: total_size,
                            }),
                            Err(e) => {
                                tracing::warn!(
                                    filename = filename,
                                    error = %e,
                                    "failed to derive AES key for encrypted file"
                                );
                                None
                            }
                        }
                    }
                    _ => None,
                }
            })
        } else {
            None
        };

        let dav_multipart = DavMultipartFile {
            aes_params,
            file_parts,
        };

        results.push((dav_item, dav_multipart));
    }

    // Sort output by filename for deterministic ordering.
    results.sort_by(|a, b| a.0.name.cmp(&b.0.name));

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn processed_file(filename: &str) -> ProcessedFile {
        ProcessedFile {
            filename: filename.to_string(),
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
        }
    }

    #[test]
    fn archive_paths_are_flattened_to_leaf_names() {
        let parent_id = Uuid::new_v4();
        let files = vec![processed_file(
            r"1sAmXP9m2rQ8eEDY2Ko4m4g\ELiFENiC-462x.yaRulB.p0801.4991.noitpmedeR.knahswahS.ehT\The.Shawshank.Redemption.1994.1080p.BluRay.x264-CiNEFiLE .mkv",
        )];

        let aggregated =
            aggregate_rar_files(&files, parent_id, "/content/download/", None).unwrap();

        assert_eq!(aggregated.len(), 1);
        assert_eq!(
            aggregated[0].0.name,
            "The.Shawshank.Redemption.1994.1080p.BluRay.x264-CiNEFiLE .mkv"
        );
        assert_eq!(
            aggregated[0].0.path,
            "/content/download/The.Shawshank.Redemption.1994.1080p.BluRay.x264-CiNEFiLE .mkv"
        );
    }
}
