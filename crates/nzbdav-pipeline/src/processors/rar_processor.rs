use std::sync::Arc;

use nzbdav_core::models::{FilePart, LongRange};
use nzbdav_stream::provider::UsenetArticleProvider;
use tracing::{debug, info, warn};

use crate::deobfuscation::NzbFileInfo;
use crate::error::{PipelineError, Result};
use crate::types::ProcessedFile;

/// Process RAR files: parse headers to discover contained files.
///
/// Fetches all segments for each RAR file, parses RAR headers, and creates
/// `ProcessedFile` entries with file parts pointing to data regions.
///
/// If `password` is provided, it will be used to decrypt RAR5 archives with
/// encrypted headers.
pub async fn process_rar_files(
    provider: &Arc<UsenetArticleProvider>,
    rar_files: &[NzbFileInfo],
    _lookahead: usize,
    password: Option<&str>,
) -> Result<Vec<ProcessedFile>> {
    let mut results = Vec::new();
    let total_rar = rar_files.iter().filter(|f| f.is_rar).count();
    if total_rar > 0 {
        info!(rar_files = total_rar, "processing RAR headers");
    }

    let mut rar_done = 0usize;
    for rar_file in rar_files {
        if !rar_file.is_rar {
            continue;
        }

        debug!(
            file_index = rar_file.file_index,
            name = %rar_file.resolved_name,
            segments = rar_file.segment_ids.len(),
            "fetching RAR segments for header parsing"
        );

        // Fetch only the first few segments — enough for RAR headers.
        // RAR headers are typically in the first 1-2 KB. We fetch up to 3
        // segments to be safe (covers headers + extra areas).
        let max_header_segments = 3.min(rar_file.segment_ids.len());
        let mut data = Vec::new();
        for segment_id in &rar_file.segment_ids[..max_header_segments] {
            let segment_data = provider.fetch_decoded(segment_id).await.map_err(|e| {
                PipelineError::Other(format!(
                    "failed to fetch segment {segment_id} for RAR file {}: {e}",
                    rar_file.resolved_name
                ))
            })?;
            data.extend_from_slice(&segment_data);
        }

        debug!(
            file_index = rar_file.file_index,
            bytes = data.len(),
            "parsing RAR headers"
        );

        // Parse RAR headers (sync I/O, run on blocking thread).
        let pw = password.map(String::from);
        let file_headers = tokio::task::spawn_blocking(move || {
            let mut cursor = std::io::Cursor::new(data);
            nzbdav_rar::parse_headers(&mut cursor, pw.as_deref())
        })
        .await
        .map_err(|e| PipelineError::Other(format!("spawn_blocking join error: {e}")))?;

        let file_headers = match file_headers {
            Ok(headers) => headers,
            Err(e) => {
                warn!(
                    file_index = rar_file.file_index,
                    name = %rar_file.resolved_name,
                    error = %e,
                    "failed to parse RAR headers, skipping"
                );
                continue; // Skip this RAR file, try others
            }
        };

        for fh in &file_headers {
            if fh.is_directory {
                continue;
            }

            let file_part = FilePart {
                segment_ids: rar_file.segment_ids.clone(),
                segment_id_byte_range: LongRange::new(
                    fh.data_start_position as i64,
                    (fh.data_start_position + fh.data_size) as i64,
                ),
                file_part_byte_range: LongRange::new(0, fh.data_size as i64),
            };

            results.push(ProcessedFile {
                filename: fh.filename.clone(),
                file_size: fh.uncompressed_size,
                is_directory: false,
                source_file_index: rar_file.file_index,
                volume_number: fh.volume_number,
                file_parts: vec![file_part],
                is_encrypted: fh.is_encrypted,
                encryption: fh.encryption.clone(),
            });
        }

        rar_done += 1;
        if rar_done % 10 == 0 || rar_done == total_rar {
            info!(
                progress = rar_done,
                total = total_rar,
                files_found = results.len(),
                "parsing RAR headers"
            );
        }
    }

    Ok(results)
}
