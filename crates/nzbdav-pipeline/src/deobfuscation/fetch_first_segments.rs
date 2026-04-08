//! Step 1: Fetch the first segment of each NZB file to detect file types
//! and compute 16KB MD5 hashes for PAR2 matching.

use std::sync::Arc;

use md5::{Digest, Md5};
use nzb_core::models::NzbJob;
use nzbdav_core::util::is_par2_file;
use nzbdav_stream::provider::UsenetArticleProvider;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use super::NzbFileInfo;
use crate::error::{PipelineError, Result};

/// Size of the first-N-bytes window used for 16KB MD5 matching with PAR2.
const FIRST_16K: usize = 16 * 1024;

/// Fetch the first segment of each NZB file to detect file types and get yEnc
/// filenames. Returns one [`NzbFileInfo`] per file in the job.
///
/// Concurrency is limited to the total connection pool capacity so we never
/// try to acquire more connections than the servers can provide.
pub async fn fetch_first_segments(
    provider: &Arc<UsenetArticleProvider>,
    job: &NzbJob,
) -> Result<Vec<NzbFileInfo>> {
    let total_conns = provider.total_connections().max(1);
    info!(
        files = job.files.len(),
        concurrency = total_conns,
        "fetching first segments"
    );

    // When concurrency is very low, fetch sequentially to avoid timeouts
    if total_conns <= 2 {
        return fetch_sequential(provider, job).await;
    }

    let semaphore = Arc::new(Semaphore::new(total_conns));
    let mut join_set = JoinSet::new();

    for (index, nzb_file) in job.files.iter().enumerate() {
        let provider = Arc::clone(provider);
        let sem = Arc::clone(&semaphore);

        let first_message_id = match nzb_file.articles.first() {
            Some(article) => article.message_id.clone(),
            None => {
                warn!(file = %nzb_file.filename, "no articles in NZB file, skipping");
                continue;
            }
        };

        let segment_ids: Vec<String> = nzb_file
            .articles
            .iter()
            .map(|a| a.message_id.clone())
            .collect();

        let subject_name = nzb_file.filename.clone();
        let file_size = nzb_file.bytes;
        let is_par2 = nzb_file.is_par2 || is_par2_file(&subject_name);

        // Acquire the semaphore BEFORE spawning so we don't flood the pool
        let permit = sem
            .acquire_owned()
            .await
            .map_err(|e| PipelineError::Other(e.to_string()))?;

        join_set.spawn(async move {
            let result = fetch_single_first_segment(
                &provider,
                index,
                &first_message_id,
                &subject_name,
                file_size,
                segment_ids,
                is_par2,
            )
            .await;
            drop(permit);
            result
        });
    }

    collect_results(&mut join_set, job.files.len()).await
}

/// Sequential fetch path — used when pool capacity is very small.
/// Avoids all concurrency overhead and timeout issues.
async fn fetch_sequential(
    provider: &Arc<UsenetArticleProvider>,
    job: &NzbJob,
) -> Result<Vec<NzbFileInfo>> {
    let mut file_infos = Vec::with_capacity(job.files.len());

    for (index, nzb_file) in job.files.iter().enumerate() {
        let first_message_id = match nzb_file.articles.first() {
            Some(article) => &article.message_id,
            None => {
                warn!(file = %nzb_file.filename, "no articles in NZB file, skipping");
                continue;
            }
        };

        let segment_ids: Vec<String> = nzb_file
            .articles
            .iter()
            .map(|a| a.message_id.clone())
            .collect();

        let is_par2 = nzb_file.is_par2 || is_par2_file(&nzb_file.filename);

        match fetch_single_first_segment(
            provider,
            index,
            first_message_id,
            &nzb_file.filename,
            nzb_file.bytes,
            segment_ids,
            is_par2,
        )
        .await
        {
            Ok(info) => file_infos.push(info),
            Err(e) => {
                warn!(
                    file = %nzb_file.filename,
                    error = %e,
                    "failed to fetch first segment, file will use subject name"
                );
            }
        }
    }

    debug!(
        total = job.files.len(),
        fetched = file_infos.len(),
        "first-segment fetch complete (sequential)"
    );
    Ok(file_infos)
}

async fn collect_results(
    join_set: &mut JoinSet<Result<NzbFileInfo>>,
    total: usize,
) -> Result<Vec<NzbFileInfo>> {
    let mut file_infos = Vec::with_capacity(total);
    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(info)) => file_infos.push(info),
            Ok(Err(e)) => {
                warn!(error = %e, "failed to fetch first segment, file will use subject name");
            }
            Err(e) => {
                warn!(error = %e, "task panicked while fetching first segment");
            }
        }
    }
    file_infos.sort_by_key(|info| info.file_index);

    debug!(
        total,
        fetched = file_infos.len(),
        "first-segment fetch complete"
    );
    Ok(file_infos)
}

async fn fetch_single_first_segment(
    provider: &UsenetArticleProvider,
    file_index: usize,
    message_id: &str,
    subject_name: &str,
    file_size: u64,
    segment_ids: Vec<String>,
    is_par2: bool,
) -> Result<NzbFileInfo> {
    let decoded = provider.fetch_decoded(message_id).await?;

    let first_16k_len = decoded.len().min(FIRST_16K);
    let first_16k_data = decoded[..first_16k_len].to_vec();
    let hash_16k: [u8; 16] = Md5::digest(&first_16k_data).into();
    let is_rar = nzbdav_rar::detect_version(&first_16k_data).is_some();

    debug!(
        file_index,
        subject_name,
        is_rar,
        is_par2,
        first_16k_bytes = first_16k_data.len(),
        "analysed first segment"
    );

    Ok(NzbFileInfo {
        file_index,
        subject_name: subject_name.to_owned(),
        yenc_name: None,
        resolved_name: subject_name.to_owned(),
        file_size,
        segment_ids,
        is_rar,
        is_par2,
        first_16k: Some(first_16k_data),
        hash_16k: Some(hash_16k),
    })
}
