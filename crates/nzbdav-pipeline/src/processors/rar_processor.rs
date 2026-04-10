use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use nzbdav_core::models::{FilePart, LongRange};
use nzbdav_stream::provider::UsenetArticleProvider;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::deobfuscation::NzbFileInfo;
use crate::error::{PipelineError, Result};
use crate::types::ProcessedFile;

/// Process RAR files: parse headers to discover contained files.
///
/// Probes the first RAR file to detect structural errors (unsupported
/// compression, encrypted headers, wrong password) before processing the
/// rest in parallel. All volumes in a multi-volume set share the same
/// format, so a structural failure on the first volume means they all fail.
///
/// Concurrency is bounded by the NNTP connection pool capacity so we never
/// try to acquire more connections than the servers can provide.
///
/// If `password` is provided, it will be used to decrypt RAR5 archives with
/// encrypted headers.
pub async fn process_rar_files(
    provider: &Arc<UsenetArticleProvider>,
    rar_files: &[NzbFileInfo],
    _lookahead: usize,
    password: Option<&str>,
) -> Result<Vec<ProcessedFile>> {
    let rar_only: Vec<_> = rar_files.iter().filter(|f| f.is_rar).collect();
    let total_rar = rar_only.len();
    if total_rar == 0 {
        return Ok(Vec::new());
    }
    info!(rar_files = total_rar, "processing RAR headers");

    // ── Probe first RAR to detect structural errors early ───────────
    let first = rar_only[0];
    let probe_result = fetch_and_parse_rar(
        provider,
        first.file_index,
        &first.resolved_name,
        &first.segment_ids,
        password,
    )
    .await;

    let first_headers = match probe_result {
        Ok(headers) => headers,
        Err(e) => {
            warn!(
                name = %first.resolved_name,
                error = %e,
                remaining_skipped = total_rar - 1,
                "first RAR failed — skipping remaining volumes"
            );
            return Ok(Vec::new());
        }
    };

    let mut results = collect_file_headers(&first_headers, first);
    info!(
        progress = 1,
        total = total_rar,
        files_found = results.len(),
        "parsing RAR headers (probe OK)"
    );

    if total_rar == 1 {
        return Ok(results);
    }

    // ── Process remaining RAR files in parallel ─────────────────────
    let total_conns = provider.total_connections().max(1);
    let semaphore = Arc::new(Semaphore::new(total_conns));
    let completed = Arc::new(AtomicUsize::new(1)); // 1 = probe already done
    let mut join_set = JoinSet::new();

    for rar_file in &rar_only[1..] {
        let provider = Arc::clone(provider);
        let sem = Arc::clone(&semaphore);
        let completed = Arc::clone(&completed);
        let pw = password.map(String::from);
        let file_index = rar_file.file_index;
        let resolved_name = rar_file.resolved_name.clone();
        let segment_ids = rar_file.segment_ids.clone();

        let permit = sem
            .acquire_owned()
            .await
            .map_err(|e| PipelineError::Other(e.to_string()))?;

        join_set.spawn(async move {
            let result = fetch_and_parse_rar(
                &provider,
                file_index,
                &resolved_name,
                &segment_ids,
                pw.as_deref(),
            )
            .await;
            drop(permit);
            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
            if done.is_multiple_of(10) || done == total_rar {
                info!(progress = done, total = total_rar, "parsing RAR headers");
            }
            (file_index, resolved_name, segment_ids, result)
        });
    }

    while let Some(join_result) = join_set.join_next().await {
        let (file_index, resolved_name, segment_ids, parse_result) = match join_result {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "RAR header task panicked");
                continue;
            }
        };

        let file_headers = match parse_result {
            Ok(headers) => headers,
            Err(e) => {
                warn!(
                    file_index,
                    name = %resolved_name,
                    error = %e,
                    "failed to parse RAR headers, skipping"
                );
                continue;
            }
        };

        for fh in &file_headers {
            if fh.is_directory {
                continue;
            }

            let file_part = FilePart {
                segment_ids: segment_ids.clone(),
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
                source_file_index: file_index,
                volume_number: fh.volume_number,
                file_parts: vec![file_part],
                is_encrypted: fh.is_encrypted,
                encryption: fh.encryption.clone(),
            });
        }
    }

    // Sort by source file index for deterministic output order.
    results.sort_by_key(|r| (r.source_file_index, r.volume_number));

    info!(
        total = total_rar,
        files_found = results.len(),
        "RAR header processing complete"
    );
    Ok(results)
}

/// Extract `ProcessedFile` entries from parsed file headers.
fn collect_file_headers(
    file_headers: &[nzbdav_rar::FileHeader],
    rar_file: &NzbFileInfo,
) -> Vec<ProcessedFile> {
    file_headers
        .iter()
        .filter(|fh| !fh.is_directory)
        .map(|fh| {
            let file_part = FilePart {
                segment_ids: rar_file.segment_ids.clone(),
                segment_id_byte_range: LongRange::new(
                    fh.data_start_position as i64,
                    (fh.data_start_position + fh.data_size) as i64,
                ),
                file_part_byte_range: LongRange::new(0, fh.data_size as i64),
            };

            ProcessedFile {
                filename: fh.filename.clone(),
                file_size: fh.uncompressed_size,
                is_directory: false,
                source_file_index: rar_file.file_index,
                volume_number: fh.volume_number,
                file_parts: vec![file_part],
                is_encrypted: fh.is_encrypted,
                encryption: fh.encryption.clone(),
            }
        })
        .collect()
}

/// Fetch the first few segments of a RAR file and parse its headers.
async fn fetch_and_parse_rar(
    provider: &UsenetArticleProvider,
    file_index: usize,
    resolved_name: &str,
    segment_ids: &[String],
    password: Option<&str>,
) -> std::result::Result<Vec<nzbdav_rar::FileHeader>, PipelineError> {
    debug!(
        file_index,
        name = %resolved_name,
        segments = segment_ids.len(),
        "fetching RAR segments for header parsing"
    );

    // Fetch only the first few segments — enough for RAR headers.
    // RAR headers are typically in the first 1-2 KB. We fetch up to 3
    // segments to be safe (covers headers + extra areas).
    let max_header_segments = 3.min(segment_ids.len());
    let mut data = Vec::new();
    for segment_id in &segment_ids[..max_header_segments] {
        let segment_data = provider.fetch_decoded(segment_id).await.map_err(|e| {
            PipelineError::Other(format!(
                "failed to fetch segment {segment_id} for RAR file {resolved_name}: {e}",
            ))
        })?;
        data.extend_from_slice(&segment_data);
    }

    debug!(file_index, bytes = data.len(), "parsing RAR headers");

    // Parse RAR headers (sync I/O, run on blocking thread).
    let pw = password.map(String::from);
    let file_headers = tokio::task::spawn_blocking(move || {
        let mut cursor = std::io::Cursor::new(data);
        nzbdav_rar::parse_headers(&mut cursor, pw.as_deref())
    })
    .await
    .map_err(|e| PipelineError::Other(format!("spawn_blocking join error: {e}")))?
    .map_err(|e| PipelineError::Other(format!("RAR parse error: {e}")))?;

    Ok(file_headers)
}
