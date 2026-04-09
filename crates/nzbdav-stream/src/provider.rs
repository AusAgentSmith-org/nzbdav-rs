//! `UsenetArticleProvider` — wraps nzb-nntp connection pools for on-demand
//! article fetching with multi-server failover and yEnc decoding.

use std::sync::Arc;

use arc_swap::ArcSwap;
use nzb_decode::decode_yenc;
use nzb_nntp::{ConnectionPool, NntpError};
use tracing::{debug, warn};

use crate::error::{Result, StreamError};

/// Decoded yEnc part header information, used for seek interpolation.
pub struct YencHeaders {
    /// Byte offset where this part begins in the reassembled file.
    pub part_begin: u64,
    /// Byte offset where this part ends in the reassembled file.
    pub part_end: u64,
    /// Total size of the reassembled file.
    pub total_size: u64,
}

/// Fetches and decodes Usenet articles with multi-server failover.
///
/// Pools are tried in priority order. On `ArticleNotFound`, the connection is
/// released cleanly and the next pool is attempted. On any other NNTP error
/// the connection is discarded (it may be in a bad state) and the next pool
/// is attempted.
///
/// The pool list can be atomically swapped at runtime via [`replace_pools`]
/// when server configuration changes.
pub struct UsenetArticleProvider {
    pools: ArcSwap<Vec<Arc<ConnectionPool>>>,
}

impl UsenetArticleProvider {
    /// Create a new provider with pools listed in priority order.
    pub fn new(pools: Vec<Arc<ConnectionPool>>) -> Self {
        Self {
            pools: ArcSwap::from_pointee(pools),
        }
    }

    /// Atomically replace the connection pools (e.g. after server config change).
    pub fn replace_pools(&self, pools: Vec<Arc<ConnectionPool>>) {
        self.pools.store(Arc::new(pools));
    }

    fn load_pools(&self) -> arc_swap::Guard<Arc<Vec<Arc<ConnectionPool>>>> {
        self.pools.load()
    }

    /// Total number of connections across all pools.
    /// Useful for limiting concurrency to what the pools can actually serve.
    pub fn total_connections(&self) -> usize {
        let pools = self.load_pools();
        pools.iter().map(|p| p.config().connections as usize).sum()
    }

    /// Fetch a single article by message-id, decode yEnc, and return the
    /// decoded payload. Tries each server pool in order with failover.
    pub async fn fetch_decoded(&self, message_id: &str) -> Result<Vec<u8>> {
        let pools = self.load_pools();
        let mut last_err: Option<StreamError> = None;

        for (i, pool) in pools.iter().enumerate() {
            let mut pooled = match pool.acquire().await {
                Ok(p) => p,
                Err(e) => {
                    warn!(pool = i, error = %e, "failed to acquire connection");
                    last_err = Some(StreamError::NntpError(e.to_string()));
                    continue;
                }
            };

            match pooled.conn.fetch_article(message_id).await {
                Ok(resp) => {
                    pool.release(pooled);
                    let raw = resp.data.ok_or_else(|| {
                        StreamError::NntpError(format!("empty response for article {message_id}"))
                    })?;
                    let decoded =
                        decode_yenc(&raw).map_err(|e| StreamError::YencDecode(e.to_string()))?;
                    debug!(message_id, bytes = decoded.data.len(), "decoded article");
                    return Ok(decoded.data);
                }
                Err(NntpError::ArticleNotFound(_)) => {
                    debug!(pool = i, message_id, "article not found, trying next");
                    pool.release(pooled);
                    last_err = Some(StreamError::ArticleNotFound(message_id.to_string()));
                }
                Err(e) => {
                    warn!(pool = i, message_id, error = %e, "nntp error, discarding connection");
                    pool.discard(pooled);
                    last_err = Some(StreamError::NntpError(e.to_string()));
                }
            }
        }

        Err(last_err.unwrap_or_else(|| StreamError::AllServersExhausted(message_id.to_string())))
    }

    /// Check article existence via the STAT command. Returns `true` if any
    /// server has the article.
    pub async fn stat(&self, message_id: &str) -> Result<bool> {
        let pools = self.load_pools();
        let mut last_err: Option<StreamError> = None;

        for (i, pool) in pools.iter().enumerate() {
            let mut pooled = match pool.acquire().await {
                Ok(p) => p,
                Err(e) => {
                    warn!(pool = i, error = %e, "failed to acquire connection");
                    last_err = Some(StreamError::NntpError(e.to_string()));
                    continue;
                }
            };

            match pooled.conn.stat_article(message_id).await {
                Ok(resp) => {
                    pool.release(pooled);
                    // Code 223 means the article exists.
                    return Ok(resp.code == 223);
                }
                Err(NntpError::ArticleNotFound(_)) => {
                    debug!(
                        pool = i,
                        message_id, "article not found via STAT, trying next"
                    );
                    pool.release(pooled);
                    last_err = Some(StreamError::ArticleNotFound(message_id.to_string()));
                }
                Err(e) => {
                    warn!(pool = i, message_id, error = %e, "nntp STAT error, discarding connection");
                    pool.discard(pooled);
                    last_err = Some(StreamError::NntpError(e.to_string()));
                }
            }
        }

        // If every server said ArticleNotFound, the article doesn't exist.
        if matches!(last_err, Some(StreamError::ArticleNotFound(_))) {
            return Ok(false);
        }

        Err(last_err.unwrap_or_else(|| StreamError::AllServersExhausted(message_id.to_string())))
    }

    /// Fetch an article and return its yEnc part headers (begin, end, total).
    /// Useful for seek interpolation to determine which segment contains a
    /// given byte offset.
    pub async fn yenc_headers(&self, message_id: &str) -> Result<YencHeaders> {
        let pools = self.load_pools();
        let mut last_err: Option<StreamError> = None;

        for (i, pool) in pools.iter().enumerate() {
            let mut pooled = match pool.acquire().await {
                Ok(p) => p,
                Err(e) => {
                    warn!(pool = i, error = %e, "failed to acquire connection");
                    last_err = Some(StreamError::NntpError(e.to_string()));
                    continue;
                }
            };

            match pooled.conn.fetch_article(message_id).await {
                Ok(resp) => {
                    pool.release(pooled);
                    let raw = resp.data.ok_or_else(|| {
                        StreamError::NntpError(format!("empty response for article {message_id}"))
                    })?;
                    let decoded =
                        decode_yenc(&raw).map_err(|e| StreamError::YencDecode(e.to_string()))?;
                    return Ok(YencHeaders {
                        part_begin: decoded.part_begin.unwrap_or(0),
                        part_end: decoded.part_end.unwrap_or(0),
                        total_size: decoded.file_size.unwrap_or(0),
                    });
                }
                Err(NntpError::ArticleNotFound(_)) => {
                    debug!(pool = i, message_id, "article not found, trying next");
                    pool.release(pooled);
                    last_err = Some(StreamError::ArticleNotFound(message_id.to_string()));
                }
                Err(e) => {
                    warn!(pool = i, message_id, error = %e, "nntp error, discarding connection");
                    pool.discard(pooled);
                    last_err = Some(StreamError::NntpError(e.to_string()));
                }
            }
        }

        Err(last_err.unwrap_or_else(|| StreamError::AllServersExhausted(message_id.to_string())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_empty_provider_fetch_decoded() {
        let provider = UsenetArticleProvider::new(vec![]);
        let result = provider.fetch_decoded("test@example.com").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, StreamError::AllServersExhausted(_)),
            "expected AllServersExhausted, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_empty_provider_stat() {
        let provider = UsenetArticleProvider::new(vec![]);
        let result = provider.stat("test@example.com").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, StreamError::AllServersExhausted(_)),
            "expected AllServersExhausted, got: {err}"
        );
    }

    #[test]
    fn test_replace_pools() {
        let provider = UsenetArticleProvider::new(vec![]);
        assert_eq!(provider.total_connections(), 0);

        // We can't easily create real ConnectionPool instances without a server,
        // but we can verify replace_pools with another empty vec and that
        // total_connections stays consistent.
        provider.replace_pools(vec![]);
        assert_eq!(provider.total_connections(), 0);
    }
}
