//! Step 1: Fetch the first segment of each NZB file to detect file types
//! and compute 16KB MD5 hashes for PAR2 matching.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use md5::{Digest, Md5};
use nzb_core::models::NzbJob;
use nzbdav_core::util::{is_par2_file, is_rar_file};
use nzbdav_stream::provider::UsenetArticleProvider;
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

    let mut join_set = JoinSet::new();
    let total_files = job.files.len();
    let completed = Arc::new(AtomicUsize::new(0));

    for (index, nzb_file) in job.files.iter().enumerate() {
        let provider = Arc::clone(provider);
        let completed = Arc::clone(&completed);

        // Sort articles by segment number to ensure correct byte order.
        let mut sorted_articles: Vec<_> = nzb_file.articles.iter().collect();
        sorted_articles.sort_by_key(|a| a.segment_number);

        let first_message_id = match sorted_articles.first() {
            Some(article) => article.message_id.clone(),
            None => {
                warn!(file = %nzb_file.filename, "no articles in NZB file, skipping");
                continue;
            }
        };

        let segment_ids: Vec<String> = sorted_articles
            .iter()
            .map(|a| a.message_id.clone())
            .collect();

        let subject_name = nzb_file.filename.clone();
        let file_size = nzb_file.bytes;
        let is_par2 = nzb_file.is_par2 || is_par2_file(&subject_name);

        join_set.spawn(async move {
            let result = fetch_single_first_segment(
                &provider,
                index,
                &first_message_id,
                &subject_name,
                file_size,
                segment_ids.clone(),
                is_par2,
            )
            .await;
            let info = match result {
                Ok(info) => info,
                Err(e) => {
                    warn!(
                        file = %subject_name,
                        error = %e,
                        "failed to fetch first segment, file will use subject name"
                    );
                    fallback_file_info(
                        index,
                        &subject_name,
                        file_size,
                        segment_ids,
                        is_par2,
                        Some(&e),
                    )
                }
            };
            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
            if done.is_multiple_of(10) || done == total_files {
                info!(
                    progress = done,
                    total = total_files,
                    "fetching first segments"
                );
            }
            info
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
    let total = job.files.len();

    for (index, nzb_file) in job.files.iter().enumerate() {
        let mut sorted_articles: Vec<_> = nzb_file.articles.iter().collect();
        sorted_articles.sort_by_key(|a| a.segment_number);

        let first_message_id = match sorted_articles.first() {
            Some(article) => &article.message_id,
            None => {
                warn!(file = %nzb_file.filename, "no articles in NZB file, skipping");
                continue;
            }
        };

        let segment_ids: Vec<String> = sorted_articles
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
            segment_ids.clone(),
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
                file_infos.push(fallback_file_info(
                    index,
                    &nzb_file.filename,
                    nzb_file.bytes,
                    segment_ids,
                    is_par2,
                    Some(&e),
                ));
            }
        }

        let done = index + 1;
        if done.is_multiple_of(10) || done == total {
            info!(
                progress = done,
                total, "fetching first segments (sequential)"
            );
        }
    }

    info!(
        total,
        fetched = file_infos.len(),
        "first-segment fetch complete (sequential)"
    );
    Ok(file_infos)
}

async fn collect_results(
    join_set: &mut JoinSet<NzbFileInfo>,
    total: usize,
) -> Result<Vec<NzbFileInfo>> {
    let mut file_infos = Vec::with_capacity(total);
    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(info) => file_infos.push(info),
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

fn fallback_file_info(
    file_index: usize,
    subject_name: &str,
    file_size: u64,
    segment_ids: Vec<String>,
    is_par2: bool,
    error: Option<&PipelineError>,
) -> NzbFileInfo {
    NzbFileInfo {
        file_index,
        subject_name: subject_name.to_owned(),
        yenc_name: None,
        resolved_name: subject_name.to_owned(),
        file_size,
        segment_ids,
        is_rar: is_rar_file(subject_name),
        is_par2,
        first_16k: None,
        hash_16k: None,
        first_segment_error: error.map(ToString::to_string),
        first_segment_missing_article: error
            .and_then(PipelineError::missing_article_id)
            .map(str::to_owned),
    }
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
    let decoded = provider.fetch_decoded_low(message_id).await?;

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
        first_segment_error: None,
        first_segment_missing_article: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use nzb_core::models::{Article, JobStatus, NzbFile, NzbJob, Priority};
    use nzbdav_stream::provider::UsenetArticleProvider;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn article(message_id: &str, segment_number: u32) -> Article {
        Article {
            message_id: message_id.to_owned(),
            segment_number,
            bytes: 123,
            downloaded: false,
            data_begin: None,
            data_size: None,
            crc32: None,
            tried_servers: Vec::new(),
            tries: 0,
        }
    }

    fn job_with_file(filename: &str, articles: Vec<Article>) -> NzbJob {
        NzbJob {
            id: "job-1".to_owned(),
            name: "job".to_owned(),
            category: String::new(),
            status: JobStatus::Queued,
            priority: Priority::Normal,
            total_bytes: 123,
            downloaded_bytes: 0,
            file_count: 1,
            files_completed: 0,
            article_count: articles.len(),
            articles_downloaded: 0,
            articles_failed: 0,
            added_at: Utc::now(),
            completed_at: None,
            work_dir: PathBuf::new(),
            output_dir: PathBuf::new(),
            password: None,
            error_message: None,
            speed_bps: 0,
            server_stats: Vec::new(),
            files: vec![NzbFile {
                id: "file-1".to_owned(),
                filename: filename.to_owned(),
                bytes: 123,
                bytes_downloaded: 0,
                is_par2: is_par2_file(filename),
                par2_setname: None,
                par2_vol: None,
                par2_blocks: None,
                assembled: false,
                groups: Vec::new(),
                articles,
            }],
        }
    }

    #[test]
    fn fallback_marks_rar_by_subject_name() {
        let info = fallback_file_info(
            7,
            "movie.part001.rar",
            42,
            vec!["seg-1@example.test".to_owned()],
            false,
            None,
        );

        assert_eq!(info.file_index, 7);
        assert_eq!(info.resolved_name, "movie.part001.rar");
        assert!(info.is_rar);
        assert!(!info.is_par2);
        assert!(info.first_16k.is_none());
        assert!(info.hash_16k.is_none());
        assert!(info.first_segment_error.is_none());
        assert!(info.first_segment_missing_article.is_none());
    }

    #[tokio::test]
    async fn fetch_failure_keeps_subject_name_fallback() {
        let provider = Arc::new(UsenetArticleProvider::new(Vec::new()));
        let job = job_with_file("movie.mkv", vec![article("missing@example.test", 1)]);

        let infos = fetch_first_segments(&provider, &job).await.unwrap();

        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].resolved_name, "movie.mkv");
        assert_eq!(infos[0].segment_ids, vec!["missing@example.test"]);
        assert!(!infos[0].is_rar);
        assert!(!infos[0].is_par2);
        assert!(infos[0].first_16k.is_none());
        assert!(infos[0].hash_16k.is_none());
        assert_eq!(
            infos[0].first_segment_missing_article.as_deref(),
            Some("missing@example.test")
        );
    }
}
