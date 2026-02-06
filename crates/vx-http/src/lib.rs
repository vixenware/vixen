//! Shared HTTP client for vixen, built on hyper + rustls.
//!
//! When the `VX_HTTP_CACHE_DIR` environment variable is set, responses are cached
//! to disk with file-lock-based deduplication. Multiple processes contending for
//! the same URL will block on the lock; one fetches, the rest read from cache.

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use thiserror::Error;

type HttpsConnector =
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>;

/// Maximum number of redirects to follow
const MAX_REDIRECTS: u8 = 10;

#[derive(Debug, Error)]
pub enum HttpError {
    #[error("invalid URI: {0}")]
    InvalidUri(#[from] hyper::http::uri::InvalidUri),

    #[error("HTTP request failed: {0}")]
    Hyper(#[from] hyper::Error),

    #[error("HTTP error {0}")]
    HttpError(#[from] hyper_util::client::legacy::Error),

    #[error("HTTP builder error: {0}")]
    HttpBuilder(#[from] hyper::http::Error),

    #[error("invalid UTF-8 in response body")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),

    #[error("HTTP {status}")]
    Status { status: StatusCode },

    #[error("too many redirects")]
    TooManyRedirects,

    #[error("redirect without Location header")]
    MissingLocation,

    #[error("cache I/O error: {0}")]
    CacheIo(#[from] std::io::Error),
}

/// Returns the cache directory if `VX_HTTP_CACHE_DIR` is set.
fn cache_dir() -> Option<&'static PathBuf> {
    static CACHE_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHE_DIR
        .get_or_init(|| std::env::var("VX_HTTP_CACHE_DIR").ok().map(PathBuf::from))
        .as_ref()
}

/// Get a shared HTTPS client instance
fn client() -> &'static Client<HttpsConnector, String> {
    static CLIENT: OnceLock<Client<HttpsConnector, String>> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .expect("failed to load native roots")
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();

        Client::builder(TokioExecutor::new()).build(https)
    })
}

/// Perform a simple GET request and return the response, following redirects.
///
/// Note: the streaming `get` cannot use the disk cache (the cache stores complete
/// responses). If you need caching, use `get_bytes` or `get_text` instead.
pub async fn get(url: &str) -> Result<Response<Incoming>, HttpError> {
    get_network(url).await
}

/// The actual network fetch, following redirects.
async fn get_network(url: &str) -> Result<Response<Incoming>, HttpError> {
    let mut current_url = url.to_string();

    for _ in 0..MAX_REDIRECTS {
        let req = Request::builder().uri(&current_url).body(String::new())?;

        let response = client().request(req).await?;
        let status = response.status();

        // Follow redirects (301, 302, 303, 307, 308)
        if status.is_redirection() {
            let location = response
                .headers()
                .get(hyper::header::LOCATION)
                .ok_or(HttpError::MissingLocation)?
                .to_str()
                .map_err(|_| HttpError::MissingLocation)?;

            // Handle relative URLs by resolving against current URL
            current_url = if location.starts_with('/') {
                // Relative path - extract scheme + host from current URL
                let uri: hyper::Uri = current_url.parse()?;
                format!(
                    "{}://{}{}",
                    uri.scheme_str().unwrap_or("https"),
                    uri.host().unwrap_or(""),
                    location
                )
            } else {
                location.to_string()
            };

            tracing::debug!(from = %url, to = %current_url, "following redirect");
            continue;
        }

        return Ok(response);
    }

    Err(HttpError::TooManyRedirects)
}

/// GET request and collect the full response body as bytes.
///
/// When `VX_HTTP_CACHE_DIR` is set, results are cached to disk keyed by blake3
/// hash of the URL.
pub async fn get_bytes(url: &str) -> Result<Vec<u8>, HttpError> {
    if let Some(dir) = cache_dir() {
        return cache_get_bytes(dir, url).await;
    }
    get_bytes_network(url).await
}

/// Fetch bytes from network (no caching).
async fn get_bytes_network(url: &str) -> Result<Vec<u8>, HttpError> {
    use http_body_util::BodyExt;

    let response = get_network(url).await?;
    let status = response.status();

    if !status.is_success() {
        return Err(HttpError::Status { status });
    }

    let body = response.into_body().collect().await?.to_bytes();
    Ok(body.to_vec())
}

/// GET request and collect the response as a UTF-8 string.
///
/// When `VX_HTTP_CACHE_DIR` is set, results are cached to disk.
pub async fn get_text(url: &str) -> Result<String, HttpError> {
    let bytes = get_bytes(url).await?;
    let text = String::from_utf8(bytes)?;
    Ok(text)
}

// =============================================================================
// File-lock disk cache
// =============================================================================

/// Deterministic cache path for a URL: `{cache_dir}/{blake3_hex}.data`
fn cache_data_path(cache_dir: &Path, url: &str) -> PathBuf {
    let hash = blake3::hash(url.as_bytes());
    cache_dir.join(format!("{}.data", hash.to_hex()))
}

/// Lock file path for a URL: `{cache_dir}/{blake3_hex}.lock`
fn cache_lock_path(cache_dir: &Path, url: &str) -> PathBuf {
    let hash = blake3::hash(url.as_bytes());
    cache_dir.join(format!("{}.lock", hash.to_hex()))
}

/// Try to serve from cache, falling back to network with file-lock deduplication.
async fn cache_get_bytes(cache_dir: &Path, url: &str) -> Result<Vec<u8>, HttpError> {
    use std::time::Instant;

    let data_path = cache_data_path(cache_dir, url);
    let t0 = Instant::now();

    // Fast path: cache hit without locking
    if let Ok(data) = std::fs::read(&data_path) {
        let elapsed = t0.elapsed();
        tracing::info!(
            url = %url,
            size = data.len(),
            elapsed_us = elapsed.as_micros() as u64,
            "HTTP cache hit"
        );
        return Ok(data);
    }

    // Slow path: acquire lock, double-check, fetch if needed
    std::fs::create_dir_all(cache_dir)?;

    let lock_path = cache_lock_path(cache_dir, url);
    let url_owned = url.to_string();
    let data_path_clone = data_path.clone();

    // Do the blocking flock in a spawn_blocking so we don't stall the runtime
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>, HttpError> {
        use std::fs::{File, OpenOptions};
        use std::io::Write;

        let lock_t0 = Instant::now();

        // Open/create the lock file and acquire exclusive lock
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)?;

        flock_exclusive(&lock_file)?;
        let lock_elapsed = lock_t0.elapsed();

        // Double-check: another process may have populated the cache while we waited
        if let Ok(data) = std::fs::read(&data_path_clone) {
            let total_elapsed = t0.elapsed();
            tracing::info!(
                url = %url_owned,
                size = data.len(),
                lock_wait_us = lock_elapsed.as_micros() as u64,
                elapsed_us = total_elapsed.as_micros() as u64,
                "HTTP cache hit (after lock wait)"
            );
            // Lock released on drop
            return Ok(data);
        }

        // We won the race — fetch from network.
        // We need a tokio runtime for the async HTTP call. Since we're in
        // spawn_blocking, use a new current-thread runtime.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| std::io::Error::other(format!("failed to build runtime: {e}")))?;

        let fetch_t0 = Instant::now();
        let bytes = rt.block_on(get_bytes_network(&url_owned))?;
        let fetch_elapsed = fetch_t0.elapsed();

        // Write to .tmp then rename for atomicity
        let tmp_path = data_path_clone.with_extension("tmp");
        let mut tmp_file = File::create(&tmp_path)?;
        tmp_file.write_all(&bytes)?;
        tmp_file.sync_all()?;
        std::fs::rename(&tmp_path, &data_path_clone)?;

        let total_elapsed = t0.elapsed();
        tracing::info!(
            url = %url_owned,
            size = bytes.len(),
            lock_wait_us = lock_elapsed.as_micros() as u64,
            fetch_ms = fetch_elapsed.as_millis() as u64,
            elapsed_ms = total_elapsed.as_millis() as u64,
            "HTTP cache miss — fetched and cached"
        );

        // Lock released on drop
        Ok(bytes)
    })
    .await
    .expect("cache task panicked")
}

/// Acquire an exclusive flock on a file descriptor.
#[cfg(unix)]
fn flock_exclusive(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn flock_exclusive(_file: &std::fs::File) -> std::io::Result<()> {
    // On non-Unix, skip locking (cache still works, just no deduplication)
    Ok(())
}
