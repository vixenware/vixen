//! Shared HTTP client for vixen, built on hyper + rustls.
//!
//! When the `VX_HTTP_CACHE_DIR` environment variable is set, responses are cached
//! to disk.

use bytes::Bytes;
use futures_util::TryStreamExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use moire::sync::{Notify, SyncMutex};
use pin_project_lite::pin_project;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;
use tokio_util::io::ReaderStream;

type HttpsConnector =
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>;
type HttpBody = BoxBody<Bytes, std::io::Error>;

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

pin_project! {
    struct CachingBody {
        #[pin]
        inner: Incoming,
        writer: Option<std::fs::File>,
        temp_path: PathBuf,
        final_path: PathBuf,
        inflight_key: String,
        notify: Arc<Notify>,
        completed: bool,
        bytes_written: u64,
    }

    impl PinnedDrop for CachingBody {
        fn drop(this: Pin<&mut Self>) {
            let this = this.project();
            if !*this.completed {
                this.writer.take();
                let _ = std::fs::remove_file(&*this.temp_path);
                remove_inflight(this.inflight_key, this.notify);
                this.notify.notify_waiters();
            }
        }
    }
}

impl CachingBody {
    fn finish(&mut self) {
        if self.completed {
            return;
        }

        self.completed = true;
        self.writer.take();
        remove_inflight(&self.inflight_key, &self.notify);
        self.notify.notify_waiters();
    }

    fn fail(&mut self) {
        if self.completed {
            return;
        }

        self.completed = true;
        self.writer.take();
        let _ = std::fs::remove_file(&self.temp_path);
        remove_inflight(&self.inflight_key, &self.notify);
        self.notify.notify_waiters();
    }
}

impl hyper::body::Body for CachingBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.as_mut().project();

        match this.inner.poll_frame(cx) {
            std::task::Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref()
                    && let Some(writer) = this.writer.as_mut()
                {
                    if let Err(error) = std::io::Write::write_all(writer, data) {
                        self.get_mut().fail();
                        return std::task::Poll::Ready(Some(Err(error)));
                    }
                    *this.bytes_written += data.len() as u64;
                }
                std::task::Poll::Ready(Some(Ok(frame)))
            }
            std::task::Poll::Ready(Some(Err(error))) => {
                self.get_mut().fail();
                std::task::Poll::Ready(Some(Err(std::io::Error::other(error))))
            }
            std::task::Poll::Ready(None) => {
                if let Some(writer) = this.writer.as_mut() {
                    if let Err(error) = std::io::Write::flush(writer) {
                        self.get_mut().fail();
                        return std::task::Poll::Ready(Some(Err(error)));
                    }
                }
                this.writer.take();

                if let Err(error) = std::fs::rename(&this.temp_path, &this.final_path) {
                    self.get_mut().fail();
                    return std::task::Poll::Ready(Some(Err(error)));
                }

                tracing::debug!(
                    path = %this.final_path.display(),
                    size = *this.bytes_written,
                    "HTTP cache miss - streamed and cached"
                );

                self.get_mut().finish();
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
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

fn next_temp_id() -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn remove_inflight(key: &str, notify: &Arc<Notify>) {
    let mut inflight = inflight_cache().lock();
    if let Some(existing) = inflight.get(key)
        && Arc::ptr_eq(existing, notify)
    {
        inflight.remove(key);
    }
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
/// When `VX_HTTP_CACHE_DIR` is set, successful responses are cached to disk and
/// reused on subsequent requests.
pub async fn get(url: &str) -> Result<Response<HttpBody>, HttpError> {
    let Some(cache_dir) = cache_dir() else {
        return Ok(normalize_network_response(get_network(url).await?));
    };

    cache_get_response(cache_dir, url).await
}

/// The actual network fetch, following redirects.
async fn get_network(url: &str) -> Result<Response<Incoming>, HttpError> {
    let mut current_url = url.to_string();

    for _ in 0..MAX_REDIRECTS {
        let req = Request::builder().uri(&current_url).body(String::new())?;

        let response = client().request(req).await?;
        let status = response.status();

        if status.is_redirection() {
            let location = response
                .headers()
                .get(hyper::header::LOCATION)
                .ok_or(HttpError::MissingLocation)?
                .to_str()
                .map_err(|_| HttpError::MissingLocation)?;

            current_url = if location.starts_with('/') {
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

fn normalize_network_response(response: Response<Incoming>) -> Response<HttpBody> {
    response.map(|body| body.map_err(std::io::Error::other).boxed())
}

async fn cached_file_response(data_path: &Path) -> Result<Response<HttpBody>, HttpError> {
    let file = tokio::fs::File::open(data_path).await?;
    let size = file.metadata().await?.len();
    let stream = ReaderStream::new(file).map_ok(Frame::data);
    let body = StreamBody::new(stream).boxed();

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(hyper::header::CONTENT_LENGTH, size.to_string())
        .body(body)?)
}

fn cache_data_path(cache_dir: &Path, url: &str) -> PathBuf {
    let hash = blake3::hash(url.as_bytes());
    cache_dir.join(format!("{}.data", hash.to_hex()))
}

fn cache_temp_path(data_path: &Path) -> PathBuf {
    data_path.with_extension(format!("data.tmp.{}", next_temp_id()))
}

async fn cache_get_response(cache_dir: &Path, url: &str) -> Result<Response<HttpBody>, HttpError> {
    let data_path = cache_data_path(cache_dir, url);
    let inflight_key = data_path.to_string_lossy().to_string();

    loop {
        if tokio::fs::try_exists(&data_path).await? {
            tracing::debug!(url = %url, path = %data_path.display(), "HTTP cache hit");
            return cached_file_response(&data_path).await;
        }

        let maybe_waiter = {
            let mut inflight = inflight_cache().lock();
            if let Some(notify) = inflight.get(&inflight_key) {
                Some(notify.clone())
            } else {
                inflight.insert(
                    inflight_key.clone(),
                    Arc::new(Notify::new("vx-http/cache_single_flight")),
                );
                None
            }
        };

        if let Some(notify) = maybe_waiter {
            notify.notified().await;
            continue;
        }

        moire::fs::create_dir_all(cache_dir).await?;
        let response = match get_network(url).await {
            Ok(response) => response,
            Err(error) => {
                let notify = inflight_cache()
                    .lock()
                    .remove(&inflight_key)
                    .expect("in-flight key must exist for owner");
                notify.notify_waiters();
                return Err(error);
            }
        };

        if !response.status().is_success() {
            let notify = inflight_cache()
                .lock()
                .remove(&inflight_key)
                .expect("in-flight key must exist for owner");
            notify.notify_waiters();
            return Ok(normalize_network_response(response));
        }

        let temp_path = cache_temp_path(&data_path);
        let temp_file = match std::fs::File::create(&temp_path) {
            Ok(file) => file,
            Err(error) => {
                let notify = inflight_cache()
                    .lock()
                    .remove(&inflight_key)
                    .expect("in-flight key must exist for owner");
                notify.notify_waiters();
                return Err(HttpError::CacheIo(error));
            }
        };
        let notify = inflight_cache()
            .lock()
            .get(&inflight_key)
            .cloned()
            .expect("in-flight key must exist for owner");

        let (parts, body) = response.into_parts();
        let caching_body = CachingBody {
            inner: body,
            writer: Some(temp_file),
            temp_path,
            final_path: data_path,
            inflight_key,
            notify,
            completed: false,
            bytes_written: 0,
        };
        return Ok(Response::from_parts(parts, caching_body.boxed()));
    }
}

/// GET request and collect the full response body as bytes.
pub async fn get_bytes(url: &str) -> Result<Vec<u8>, HttpError> {
    let response = get(url).await?;
    let status = response.status();
    if !status.is_success() {
        return Err(HttpError::Status { status });
    }

    let body = response.into_body().collect().await?.to_bytes();
    Ok(body.to_vec())
}

/// GET request and collect the response as a UTF-8 string.
pub async fn get_text(url: &str) -> Result<String, HttpError> {
    let bytes = get_bytes(url).await?;
    let text = String::from_utf8(bytes)?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::get;
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn get_reuses_disk_cache() {
        let cache_dir = TempDir::new().expect("cache dir");
        unsafe {
            std::env::set_var("VX_HTTP_CACHE_DIR", cache_dir.path());
        }

        let body = b"streamed and cached".to_vec();
        let hit_count = Arc::new(AtomicUsize::new(0));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let hits = hit_count.clone();
        let body_for_server = body.clone();

        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.expect("accept");
                hits.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await.expect("read request");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body_for_server.len()
                );
                socket
                    .write_all(response.as_bytes())
                    .await
                    .expect("headers");
                socket.write_all(&body_for_server).await.expect("body");
            }
        });

        let url = format!("http://{addr}/cached");
        let first = get(&url)
            .await
            .expect("first response")
            .into_body()
            .collect()
            .await
            .expect("collect first")
            .to_bytes();
        let second = get(&url)
            .await
            .expect("second response")
            .into_body()
            .collect()
            .await
            .expect("collect second")
            .to_bytes();

        assert_eq!(first.as_ref(), body.as_slice());
        assert_eq!(second.as_ref(), body.as_slice());
        assert_eq!(hit_count.load(Ordering::SeqCst), 1);

        server.abort();
    }

    #[tokio::test]
    async fn dropped_stream_does_not_publish_partial_cache_entry() {
        let cache_dir = TempDir::new().expect("cache dir");
        unsafe {
            std::env::set_var("VX_HTTP_CACHE_DIR", cache_dir.path());
        }

        let body = vec![b'x'; 64 * 1024];
        let hit_count = Arc::new(AtomicUsize::new(0));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let hits = hit_count.clone();
        let body_for_server = body.clone();

        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.expect("accept");
                hits.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await.expect("read request");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body_for_server.len()
                );
                socket
                    .write_all(response.as_bytes())
                    .await
                    .expect("headers");
                socket
                    .write_all(&body_for_server[..1024])
                    .await
                    .expect("first chunk");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                socket
                    .write_all(&body_for_server[1024..])
                    .await
                    .expect("rest");
            }
        });

        let url = format!("http://{addr}/partial");
        let mut response = get(&url).await.expect("response");
        let first_frame = response
            .body_mut()
            .frame()
            .await
            .expect("frame result")
            .expect("frame value");
        assert!(first_frame.data_ref().is_some());
        drop(response);

        let second = get(&url)
            .await
            .expect("retry response")
            .into_body()
            .collect()
            .await
            .expect("collect second")
            .to_bytes();

        assert_eq!(second.len(), body.len());
        assert_eq!(hit_count.load(Ordering::SeqCst), 2);

        server.abort();
    }
}
