//! Registry crate acquisition for the store.
//!
//! Downloads .crate tarballs from crates.io, validates checksums and structure,
//! and stores them in the store.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use thiserror::Error;
use vx_services::Blake3Hash;
use vx_services::{EnsureRegistryCrateResult, EnsureStatus, NodeHash, RegistrySpecKey};

/// Maximum retry attempts for transient failures
const MAX_RETRIES: u32 = 3;

/// Initial backoff duration
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);

/// Maximum backoff duration
const MAX_BACKOFF: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
pub(crate) enum DownloadError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] vx_http::HttpError),

    #[error("rate limited (429)")]
    RateLimited,

    #[error("server error: {0}")]
    ServerError(u16),

    #[error("HTTP error: {0}")]
    HttpError(u16),

    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
}

// =============================================================================
// Registry Manager (Inflight Deduplication)
// =============================================================================

type InflightFuture = Arc<moire::sync::OnceCell<EnsureRegistryCrateResult>>;

/// Manages in-flight registry crate acquisitions with deduplication.
///
/// Same design as ToolchainManager: inflight entries are never removed.
pub struct RegistryManager {
    inflight: moire::sync::SyncMutex<HashMap<RegistrySpecKey, InflightFuture>>,
}

impl RegistryManager {
    pub fn new() -> Self {
        Self {
            inflight: moire::sync::SyncMutex::new("store/registry_inflight", HashMap::new()),
        }
    }

    /// Ensure a registry crate, deduplicating concurrent requests.
    pub async fn ensure(
        &self,
        spec_key: RegistrySpecKey,
        lookup_fn: impl AsyncFn() -> Option<Blake3Hash> + 'static,
        acquire_fn: impl AsyncFn() -> EnsureRegistryCrateResult + 'static,
    ) -> EnsureRegistryCrateResult {
        // Fast path: check if already in CAS
        if let Some(manifest_hash) = lookup_fn().await {
            return EnsureRegistryCrateResult {
                spec_key: Some(spec_key),
                manifest_hash: Some(NodeHash(manifest_hash)),
                status: EnsureStatus::Hit,
                error: None,
            };
        }

        // Get or create inflight entry
        let cell = {
            let mut inflight = self.inflight.lock();
            inflight
                .entry(spec_key)
                .or_insert_with(|| Arc::new(moire::sync::OnceCell::new("registry_inflight")))
                .clone()
        };

        // Initialize if we're first, otherwise wait
        let result = cell.get_or_init(|| acquire_fn()).await.clone();

        let mut inflight = self.inflight.lock();
        if let Some(existing) = inflight.get(&spec_key)
            && Arc::ptr_eq(existing, &cell)
        {
            inflight.remove(&spec_key);
        }

        result
    }
}

// =============================================================================
// Download and Validation
// =============================================================================

/// Download a crate tarball from crates.io with retries.
pub(crate) async fn download_crate(
    name: &str,
    version: &str,
    expected_checksum: &str,
) -> Result<Vec<u8>, DownloadError> {
    let url = format!(
        "https://crates.io/api/v1/crates/{}/{}/download",
        name, version
    );

    let mut last_error: Option<DownloadError> = None;
    let mut backoff = INITIAL_BACKOFF;

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            tracing::info!(
                attempt = attempt + 1,
                backoff_ms = backoff.as_millis(),
                "retrying crate download"
            );
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }

        match download_crate_attempt(&url, expected_checksum).await {
            Ok(bytes) => return Ok(bytes),
            Err(e) => {
                // Check if error is retryable
                if !is_retryable_error(&e) {
                    return Err(e);
                }
                tracing::warn!(error = %e, "transient download error");
                last_error = Some(e);
            }
        }
    }

    Err(last_error.unwrap_or(DownloadError::HttpError(0)))
}

/// Single download attempt with streaming verification
async fn download_crate_attempt(
    url: &str,
    expected_checksum: &str,
) -> Result<Vec<u8>, DownloadError> {
    use http_body_util::BodyExt;
    use tokio::io::AsyncReadExt;

    tracing::info!(url = %url, "downloading crate");

    let response = vx_http::get(url).await?;
    let status = response.status();

    // Check for rate limiting
    if status.as_u16() == 429 {
        return Err(DownloadError::RateLimited);
    }

    // Check for server errors (retryable)
    if status.is_server_error() {
        return Err(DownloadError::ServerError(status.as_u16()));
    }

    // Check for client errors (not retryable except 429)
    if !status.is_success() {
        return Err(DownloadError::HttpError(status.as_u16()));
    }

    // Wrap the response body in a hash-verifying reader
    let body_reader = tokio_util::io::StreamReader::new(
        response
            .into_body()
            .into_data_stream()
            .map(|result| result.map_err(std::io::Error::other)),
    );

    let mut verifying_reader =
        crate::hash_reader::Sha256VerifyingReader::new(body_reader, expected_checksum.to_string());

    // Read through the verifying reader (hash verification happens on EOF)
    let mut bytes = Vec::new();
    verifying_reader
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| DownloadError::ChecksumMismatch {
            expected: expected_checksum.to_string(),
            actual: e.to_string(),
        })?;

    tracing::debug!(
        url = %url,
        size = bytes.len(),
        checksum = %expected_checksum,
        "crate downloaded and verified"
    );

    Ok(bytes)
}

fn is_retryable_error(error: &DownloadError) -> bool {
    matches!(
        error,
        DownloadError::RateLimited | DownloadError::ServerError(_) | DownloadError::Http(_)
    )
}
