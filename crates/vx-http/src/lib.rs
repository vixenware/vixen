//! Shared HTTP client for vixen, built on hyper + rustls.
//!
//! When the `VX_HTTP_CACHE_DIR` environment variable is set, responses are cached
//! to disk.

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use moire::sync::{Notify, SyncMutex};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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

    #[error("HTTP {status}{detail}")]
    Status { status: StatusCode, detail: String },

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

fn inflight_cache() -> &'static SyncMutex<HashMap<String, Arc<Notify>>> {
    static INFLIGHT: OnceLock<SyncMutex<HashMap<String, Arc<Notify>>>> = OnceLock::new();
    INFLIGHT.get_or_init(|| SyncMutex::new("vx-http/inflight_cache", HashMap::new()))
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

/// GET request and collect the full response body as bytes, caching into the
/// provided directory.
pub async fn get_bytes_cached(cache_dir: &Path, url: &str) -> Result<Vec<u8>, HttpError> {
    cache_get_bytes(cache_dir, url).await
}

/// Fetch bytes from network (no caching).
async fn get_bytes_network(url: &str) -> Result<Vec<u8>, HttpError> {
    use http_body_util::BodyExt;

    let response = get_network(url).await?;
    let status = response.status();

    if !status.is_success() {
        return Err(http_status_error(response).await?);
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

/// GET request and collect the response as a UTF-8 string, caching into the
/// provided directory.
pub async fn get_text_cached(cache_dir: &Path, url: &str) -> Result<String, HttpError> {
    let bytes = get_bytes_cached(cache_dir, url).await?;
    let text = String::from_utf8(bytes)?;
    Ok(text)
}

// =============================================================================
// Disk cache
// =============================================================================

/// Deterministic cache path for a URL: `{cache_dir}/{blake3_hex}.data`
fn cache_data_path(cache_dir: &Path, url: &str) -> PathBuf {
    let hash = blake3::hash(url.as_bytes());
    cache_dir.join(format!("{}.data", hash.to_hex()))
}

async fn http_status_error(response: Response<Incoming>) -> Result<HttpError, HttpError> {
    use http_body_util::BodyExt;

    const BODY_SNIPPET_LIMIT: usize = 512;

    let status = response.status();
    let headers = response.headers();
    let retry_after = header_value(headers, hyper::header::RETRY_AFTER);
    let content_type = header_value(headers, hyper::header::CONTENT_TYPE);
    let rate_limit_remaining = header_value_text(headers, "x-ratelimit-remaining");
    let rate_limit_reset = header_value_text(headers, "x-ratelimit-reset");
    let body = response.into_body().collect().await?.to_bytes();

    let mut detail_parts = Vec::new();
    if let Some(retry_after) = retry_after {
        detail_parts.push(format!("retry-after={retry_after}"));
    }
    if let Some(rate_limit_remaining) = rate_limit_remaining {
        detail_parts.push(format!("x-ratelimit-remaining={rate_limit_remaining}"));
    }
    if let Some(rate_limit_reset) = rate_limit_reset {
        detail_parts.push(format!("x-ratelimit-reset={rate_limit_reset}"));
    }
    if let Some(content_type) = content_type {
        detail_parts.push(format!("content-type={content_type}"));
    }
    if !body.is_empty() {
        let snippet = String::from_utf8_lossy(&body[..body.len().min(BODY_SNIPPET_LIMIT)])
            .trim()
            .replace('\n', "\\n");
        if !snippet.is_empty() {
            let truncated = body.len() > BODY_SNIPPET_LIMIT;
            detail_parts.push(if truncated {
                format!("body={snippet}...")
            } else {
                format!("body={snippet}")
            });
        }
    }

    let detail = if detail_parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", detail_parts.join(", "))
    };
    Ok(HttpError::Status { status, detail })
}

fn header_value(headers: &hyper::HeaderMap, name: hyper::header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn header_value_text(headers: &hyper::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Try to serve from cache, falling back to network and atomic write.
async fn cache_get_bytes(cache_dir: &Path, url: &str) -> Result<Vec<u8>, HttpError> {
    use std::time::Instant;

    let data_path = cache_data_path(cache_dir, url);
    let key = data_path.to_string_lossy().to_string();
    let t0 = Instant::now();

    loop {
        if let Ok(data) = moire::fs::read(&data_path).await {
            let elapsed = t0.elapsed();
            tracing::debug!(
                url = %url,
                size = data.len(),
                elapsed_us = elapsed.as_micros() as u64,
                "HTTP cache hit"
            );
            return Ok(data);
        }

        let maybe_waiter = {
            let mut inflight = inflight_cache().lock();
            if let Some(notify) = inflight.get(&key) {
                Some(notify.clone())
            } else {
                inflight.insert(
                    key.clone(),
                    Arc::new(Notify::new("vx-http/cache_single_flight")),
                );
                None
            }
        };

        if let Some(notify) = maybe_waiter {
            notify.notified().await;
            continue;
        }

        // We are the single-flight owner for this cache key.
        let result = async {
            moire::fs::create_dir_all(cache_dir).await?;
            let fetch_t0 = Instant::now();
            let bytes = get_bytes_network(url).await?;
            let fetch_elapsed = fetch_t0.elapsed();

            vx_io::atomic_write(
                &camino::Utf8PathBuf::from_path_buf(data_path.clone())
                    .map_err(|_| std::io::Error::other("cache path is not valid UTF-8"))?,
                &bytes,
                false,
            )
            .await?;

            let total_elapsed = t0.elapsed();
            tracing::debug!(
                url = %url,
                size = bytes.len(),
                fetch_ms = fetch_elapsed.as_millis() as u64,
                elapsed_ms = total_elapsed.as_millis() as u64,
                "HTTP cache miss — fetched and cached"
            );
            Ok::<Vec<u8>, HttpError>(bytes)
        }
        .await;

        let notify = {
            let mut inflight = inflight_cache().lock();
            inflight
                .remove(&key)
                .expect("in-flight key must exist for owner")
        };
        notify.notify_waiters();

        return result;
    }
}
