//! Step 2: Find the smallest PAR2 index file and parse its file descriptors.
//!
//! The PAR2 index file (the one without recovery blocks) is always the smallest
//! `.par2` file in the set. It contains file descriptor packets with real
//! filenames and 16KB hashes that we use for deobfuscation matching.

use std::io::Cursor;
use std::sync::Arc;

use nzbdav_stream::provider::UsenetArticleProvider;
use rust_par2::Par2File;
use tracing::{debug, info, warn};

use super::NzbFileInfo;
use crate::error::Result;

/// Find the smallest PAR2 index file among the NZB files, fetch all of its
/// segments, parse the PAR2 packets, and return the file descriptors.
///
/// Returns an empty vec if no PAR2 files are found (deobfuscation will fall
/// back to subject names).
pub async fn get_par2_file_descriptors(
    provider: &Arc<UsenetArticleProvider>,
    file_infos: &[NzbFileInfo],
) -> Result<Vec<Par2File>> {
    // Find PAR2 files and pick the smallest (the index file).
    let par2_file = file_infos
        .iter()
        .filter(|f| f.is_par2)
        .min_by_key(|f| f.file_size);

    let par2_file = match par2_file {
        Some(f) => f,
        None => {
            debug!("no PAR2 files found, skipping PAR2 descriptor parsing");
            return Ok(Vec::new());
        }
    };

    info!(
        subject_name = %par2_file.subject_name,
        file_size = par2_file.file_size,
        segments = par2_file.segment_ids.len(),
        "fetching PAR2 index file"
    );

    // Fetch all segments and concatenate in order.
    let par2_data = fetch_all_segments(provider, &par2_file.segment_ids).await?;

    debug!(
        total_bytes = par2_data.len(),
        "PAR2 index file fetched, parsing packets"
    );

    // Parse PAR2 packets from the reassembled data.
    // If parsing fails, log a warning and return empty — deobfuscation will
    // fall back to subject/yEnc filenames.
    let mut cursor = Cursor::new(&par2_data);
    let descriptors: Vec<Par2File> =
        match rust_par2::parse_par2_reader(&mut cursor, par2_data.len() as u64) {
            Ok(file_set) => file_set.files.into_values().collect(),
            Err(e) => {
                warn!("failed to parse PAR2 index file (falling back to subject names): {e}");
                return Ok(Vec::new());
            }
        };

    info!(
        descriptor_count = descriptors.len(),
        "parsed PAR2 file descriptors"
    );

    for desc in &descriptors {
        debug!(
            filename = %desc.filename,
            size = desc.size,
            hash_16k = %hex::encode(desc.hash_16k),
            "PAR2 file descriptor"
        );
    }

    Ok(descriptors)
}

/// Fetch all segments of a file sequentially and concatenate the decoded data.
async fn fetch_all_segments(
    provider: &Arc<UsenetArticleProvider>,
    segment_ids: &[String],
) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    let total = segment_ids.len();

    for (i, message_id) in segment_ids.iter().enumerate() {
        let decoded = provider.fetch_decoded(message_id).await.map_err(|e| {
            warn!(
                segment = i,
                message_id = %message_id,
                error = %e,
                "failed to fetch PAR2 segment"
            );
            e
        })?;

        data.extend_from_slice(&decoded);

        let done = i + 1;
        if total > 5 && (done % 5 == 0 || done == total) {
            info!(
                progress = done,
                total,
                bytes = data.len(),
                "fetching PAR2 segments"
            );
        }
    }

    Ok(data)
}
