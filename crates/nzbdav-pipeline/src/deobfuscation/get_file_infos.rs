//! Step 3: Match NZB files to PAR2 file descriptors and resolve final filenames.
//!
//! Each PAR2 file descriptor contains the real filename and an MD5 hash of the
//! first 16KB. We match NZB files to descriptors by comparing their 16KB hashes.

use rust_par2::Par2File;
use tracing::debug;

use super::NzbFileInfo;

/// Match NZB files to PAR2 file descriptors by 16KB MD5 hash and update
/// `resolved_name` to the real filename from the PAR2 set.
///
/// Files that don't match any PAR2 descriptor keep their current
/// `resolved_name` (which defaults to `subject_name`).
pub fn get_file_infos(file_infos: &mut [NzbFileInfo], par2_files: &[Par2File]) {
    if par2_files.is_empty() {
        debug!("no PAR2 descriptors available, all files keep subject names");
        return;
    }

    let mut matched = 0usize;
    let mut unmatched = 0usize;

    for info in file_infos.iter_mut() {
        let hash_16k = match info.hash_16k {
            Some(h) => h,
            None => {
                // No hash available (fetch failed), keep subject name.
                unmatched += 1;
                continue;
            }
        };

        if let Some(par2_file) = par2_files.iter().find(|p| p.hash_16k == hash_16k) {
            debug!(
                file_index = info.file_index,
                subject_name = %info.subject_name,
                par2_name = %par2_file.filename,
                "matched NZB file to PAR2 descriptor"
            );
            info.resolved_name = par2_file.filename.clone();
            matched += 1;
        } else {
            // No matching PAR2 descriptor — keep resolved_name as-is.
            // This is normal for PAR2 files themselves, sample files, etc.
            unmatched += 1;
        }
    }

    debug!(matched, unmatched, "file info resolution complete");
}
