//! Toolchain Manager (Inflight Deduplication)

use futures_util::{StreamExt, future::join};
use jiff::Timestamp;
use std::collections::HashMap;
use std::sync::Arc;
use vx_store_proto::{
    Blake3Hash, BlobHash, EnsureRustToolchainComponentResult, EnsureStatus,
    EnsureStatus as ToolchainEnsureStatus, EnsureToolchainResult, RustChannel, RustComponent,
    RustToolchainSpec, StoreProgress, TOOLCHAIN_MANIFEST_SCHEMA_VERSION, ToolchainComponentTree,
    ToolchainKind, ToolchainManifest, ToolchainSpecKey, ZigToolchainSpec,
};
use vx_tarball::Compression;

use crate::types::StoreService;

#[derive(Debug, Clone)]
pub(crate) struct ExtractedRustComponent {
    pub(crate) tree_manifest: Blake3Hash,
    pub(crate) target: String,
    pub(crate) total_size_bytes: u64,
    pub(crate) file_count: u32,
    pub(crate) unique_blobs: u32,
}

// =============================================================================
// HTTP Download Functions (Rust Toolchains)
// =============================================================================

/// Fetch a channel manifest from static.rust-lang.org
async fn fetch_channel_manifest(
    channel: &vx_toolchain::Channel,
) -> Result<vx_toolchain::ChannelManifest, vx_toolchain::ToolchainError> {
    let url = vx_toolchain::rust_channel_manifest_url(channel);

    tracing::debug!(url = %url, "fetching channel manifest");

    let body =
        vx_http::get_text(&url)
            .await
            .map_err(|e| vx_toolchain::ToolchainError::FetchError {
                url: url.clone(),
                source: Box::new(e),
            })?;

    vx_toolchain::ChannelManifest::from_toml(&body)
}

/// Download a component with streaming checksum verification and progress reporting.
///
/// Reads Content-Length from the HTTP response and streams progress updates
/// through the provided channel as bytes arrive.
async fn download_component(
    url: &str,
    expected_hash: &str,
    component_name: &str,
    progress: &roam::Tx<StoreProgress>,
) -> Result<Vec<u8>, vx_toolchain::ToolchainError> {
    use http_body_util::BodyExt;
    use tokio::io::AsyncReadExt;

    tracing::info!(url = %url, "downloading component");

    let response =
        vx_http::get(url)
            .await
            .map_err(|e| vx_toolchain::ToolchainError::FetchError {
                url: url.to_string(),
                source: Box::new(e),
            })?;

    let status = response.status();
    if !status.is_success() {
        return Err(vx_toolchain::ToolchainError::FetchError {
            url: url.to_string(),
            source: Box::new(std::io::Error::other(format!("HTTP {}", status))),
        });
    }

    // Extract Content-Length for progress tracking
    let content_length = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    let _ = progress
        .send(&StoreProgress {
            message: format!("downloading {component_name}"),
            current: Some(0),
            total: content_length,
        })
        .await;

    // Wrap the response body in a hash-verifying reader
    let body_reader = tokio_util::io::StreamReader::new(
        response
            .into_body()
            .into_data_stream()
            .map(|result| result.map_err(std::io::Error::other)),
    );

    let mut verifying_reader =
        crate::hash_reader::Sha256VerifyingReader::new(body_reader, expected_hash.to_string());

    // Read in chunks, reporting progress at most every 250ms
    let mut bytes = Vec::with_capacity(content_length.unwrap_or(0) as usize);
    let mut buf = vec![0u8; 256 * 1024]; // 256KB chunks
    let mut bytes_read: u64 = 0;
    let mut last_progress = std::time::Instant::now();

    loop {
        match verifying_reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                bytes.extend_from_slice(&buf[..n]);
                bytes_read += n as u64;
                let now = std::time::Instant::now();
                if now.duration_since(last_progress) >= std::time::Duration::from_millis(250) {
                    last_progress = now;
                    let _ = progress
                        .send(&StoreProgress {
                            message: format!("downloading {component_name}"),
                            current: Some(bytes_read),
                            total: content_length,
                        })
                        .await;
                }
            }
            Err(e) => {
                return Err(vx_toolchain::ToolchainError::ChecksumMismatch {
                    url: url.to_string(),
                    expected: expected_hash.to_string(),
                    actual: e.to_string(),
                });
            }
        }
    }

    // Always send a final progress update at 100%
    let _ = progress
        .send(&StoreProgress {
            message: format!("downloading {component_name}"),
            current: Some(bytes_read),
            total: content_length,
        })
        .await;

    tracing::debug!(
        url = %url,
        size = bytes.len(),
        hash = %expected_hash,
        "component downloaded and verified"
    );

    Ok(bytes)
}

/// Download and extract a component in a single streaming pipeline.
///
/// Instead of buffering the entire tarball in memory, this pipes the HTTP response
/// directly through decompression and tar extraction:
///
/// ```text
/// HTTP body → StreamReader → Sha256VerifyingReader → CountingReader → extract_to_tree → CAS
/// ```
///
/// Progress is reported by polling an atomic byte counter on a 250ms interval.
/// SHA256 verification is triggered by draining the reader to EOF after extraction
/// (the tar reader stops at the end-of-archive marker, leaving compressed stream
/// tail bytes unread).
async fn download_and_extract_component(
    url: &str,
    expected_hash: &str,
    component_name: &str,
    compression: Compression,
    strip_components: u32,
    progress: &roam::Tx<StoreProgress>,
    store_blob: impl AsyncFn(Vec<u8>) -> Result<BlobHash, String>,
) -> Result<vx_store_proto::TreeManifest, vx_toolchain::ToolchainError> {
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    tracing::info!(url = %url, "streaming download+extract for {component_name}");

    let response =
        vx_http::get(url)
            .await
            .map_err(|e| vx_toolchain::ToolchainError::FetchError {
                url: url.to_string(),
                source: Box::new(e),
            })?;

    let status = response.status();
    if !status.is_success() {
        return Err(vx_toolchain::ToolchainError::FetchError {
            url: url.to_string(),
            source: Box::new(std::io::Error::other(format!("HTTP {status}"))),
        });
    }

    let content_length = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    // Build reader chain: HTTP body → StreamReader → Sha256VerifyingReader → CountingReader
    let body_reader = tokio_util::io::StreamReader::new(
        response
            .into_body()
            .into_data_stream()
            .map(|result| result.map_err(std::io::Error::other)),
    );
    let verifying_reader =
        crate::hash_reader::Sha256VerifyingReader::new(body_reader, expected_hash.to_string());
    let counter = Arc::new(AtomicU64::new(0));
    let counting_reader =
        crate::hash_reader::CountingReader::new(verifying_reader, counter.clone());

    // Box::pin so &mut reader satisfies AsyncRead + Unpin
    let mut reader = Box::pin(counting_reader);

    // Run extraction with periodic progress reporting via tokio::select!
    let tree = {
        let extract_fut =
            vx_tarball::extract_to_tree(&mut reader, compression, strip_components, store_blob);
        tokio::pin!(extract_fut);
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        loop {
            tokio::select! {
                biased;
                result = &mut extract_fut => break result,
                _ = interval.tick() => {
                    let current = counter.load(Ordering::Relaxed);
                    let _ = progress
                        .send(&StoreProgress {
                            message: format!("downloading+extracting {component_name}"),
                            current: Some(current),
                            total: content_length,
                        })
                        .await;
                }
            }
        }
    };

    let tree = tree.map_err(|e| {
        let msg = e.to_string();
        if msg.contains("SHA256 mismatch") {
            vx_toolchain::ToolchainError::ChecksumMismatch {
                url: url.to_string(),
                expected: expected_hash.to_string(),
                actual: msg,
            }
        } else {
            vx_toolchain::ToolchainError::FetchError {
                url: url.to_string(),
                source: Box::new(e),
            }
        }
    })?;

    // Drain remaining compressed bytes to trigger SHA256 verification on EOF.
    // The tar reader stops at the end-of-archive marker; there may be trailing
    // bytes in the compressed stream that haven't been read yet.
    if let Err(e) = tokio::io::copy(&mut reader, &mut tokio::io::sink()).await {
        let msg = e.to_string();
        if msg.contains("SHA256 mismatch") {
            return Err(vx_toolchain::ToolchainError::ChecksumMismatch {
                url: url.to_string(),
                expected: expected_hash.to_string(),
                actual: msg,
            });
        }
        return Err(vx_toolchain::ToolchainError::FetchError {
            url: url.to_string(),
            source: Box::new(e),
        });
    }

    // Final progress update
    let _ = progress
        .send(&StoreProgress {
            message: format!("downloading+extracting {component_name}"),
            current: Some(counter.load(Ordering::Relaxed)),
            total: content_length,
        })
        .await;

    tracing::debug!(
        url = %url,
        bytes = counter.load(Ordering::Relaxed),
        hash = %expected_hash,
        files = tree.entries.len(),
        "component streamed, extracted, and verified"
    );

    Ok(tree)
}

fn rust_component_name(component: &RustComponent) -> Option<&'static str> {
    match component {
        RustComponent::Rustc => Some("rustc"),
        RustComponent::RustStd => Some("rust-std"),
        RustComponent::Cargo => None,
    }
}

type InflightFuture = Arc<tokio::sync::OnceCell<EnsureToolchainResult>>;

/// Manages in-flight toolchain acquisitions with deduplication.
///
/// Inflight entries are never removed. This is intentional:
/// - Memory cost is negligible (one OnceCell per unique ToolchainSpecKey)
/// - Avoids race between store lookup miss and inflight insert
/// - Once the store has the mapping, lookup_fn fast-paths and OnceCell is never awaited
pub(crate) struct ToolchainManager {
    inflight: tokio::sync::Mutex<HashMap<ToolchainSpecKey, InflightFuture>>,
}

impl ToolchainManager {
    pub(crate) fn new() -> Self {
        Self {
            inflight: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Ensure a toolchain, deduplicating concurrent requests.
    ///
    /// `lookup_fn` is async because the store is remote in production.
    pub(crate) async fn ensure(
        &self,
        spec_key: ToolchainSpecKey,
        lookup_fn: impl AsyncFn() -> Option<Blake3Hash>,
        acquire_fn: impl AsyncFn() -> EnsureToolchainResult,
    ) -> EnsureToolchainResult {
        // Fast path: check if already in CAS (async RPC)
        if let Some(manifest_hash) = lookup_fn().await {
            return EnsureToolchainResult {
                spec_key: Some(spec_key),
                toolchain_id: None, // Caller should read manifest for ID
                manifest_hash: Some(manifest_hash),
                status: ToolchainEnsureStatus::Hit,
                error: None,
            };
        }

        // Get or create inflight entry
        let cell = {
            let mut inflight = self.inflight.lock().await;
            inflight
                .entry(spec_key)
                .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new()))
                .clone()
        };

        // Initialize if we're first, otherwise wait
        #[allow(clippy::redundant_closure)] // it's not
        cell.get_or_init(|| acquire_fn()).await.clone()
    }
}

// =============================================================================
// Toolchain methods (Inherent methods, NOT part of CAS RPC trait)
// =============================================================================

impl StoreService {
    /// Ensure a Rust toolchain exists in CAS (internal helper, not an RPC method).
    #[tracing::instrument(skip(self, progress), fields(spec_key = tracing::field::Empty))]
    pub async fn ensure_rust_toolchain(
        &self,
        spec: RustToolchainSpec,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureToolchainResult {
        // Validate and compute spec_key first
        let spec_key = match spec.spec_key() {
            Ok(k) => {
                tracing::Span::current().record("spec_key", k.to_string());
                k
            }
            Err(e) => {
                return EnsureToolchainResult {
                    spec_key: None,
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(format!("invalid spec: {}", e)),
                };
            }
        };

        let this = self.clone();
        let this2 = self.clone();

        self.toolchain_manager
            .ensure(
                spec_key,
                async move || {
                    this.storage
                        .get_toolchain_spec(&spec_key)
                        .await
                        .ok()
                        .flatten()
                },
                async move || {
                    this2
                        .acquire_rust_toolchain_impl(spec_key, &spec, &progress)
                        .await
                },
            )
            .await
    }

    async fn acquire_rust_toolchain_impl(
        &self,
        spec_key: ToolchainSpecKey,
        spec: &RustToolchainSpec,
        progress: &roam::Tx<StoreProgress>,
    ) -> EnsureToolchainResult {
        // Convert proto spec to vx_toolchain types
        let channel = match &spec.channel {
            RustChannel::Stable => vx_toolchain::Channel::Stable,
            RustChannel::Beta => vx_toolchain::Channel::Beta,
            RustChannel::Nightly { date } => vx_toolchain::Channel::Nightly { date: date.clone() },
        };

        // Fetch channel manifest to get download URLs
        let manifest = match fetch_channel_manifest(&channel).await {
            Ok(m) => m,
            Err(e) => {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(format!("failed to fetch channel manifest: {}", e)),
                };
            }
        };

        let rustc_target = match manifest.rustc_for_target(&spec.host) {
            Ok(t) => t,
            Err(e) => {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(format!("{}", e)),
                };
            }
        };

        let rust_std_target = match manifest.rust_std_for_target(&spec.target) {
            Ok(t) => t,
            Err(e) => {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(format!("{}", e)),
                };
            }
        };

        // Compute toolchain ID from manifest SHA256s
        let toolchain_id = vx_toolchain::RustToolchainId::from_manifest_sha256s(
            &spec.host,
            &spec.target,
            &rustc_target.hash,
            &rust_std_target.hash,
        );

        // Download AND extract both components in parallel (streaming pipeline).
        // Each component streams: HTTP → SHA256 verify → decompress → tar → CAS.
        let rustc_url = rustc_target.url.clone();
        let rustc_hash = rustc_target.hash.clone();
        let rust_std_url = rust_std_target.url.clone();
        let rust_std_hash = rust_std_target.hash.clone();
        let sem = self.download_semaphore.clone();

        let this1 = self.clone();
        let this2 = self.clone();

        let (rustc_result, rust_std_result) = join(
            async {
                let _permit = sem.acquire().await.unwrap();
                download_and_extract_component(
                    &rustc_url,
                    &rustc_hash,
                    "rustc",
                    Compression::Xz,
                    2,
                    progress,
                    async move |data: Vec<u8>| {
                        let hash = BlobHash::from_bytes(&data);
                        if let Err(e) = this1.storage.put_blob(&hash, &data).await {
                            tracing::warn!("failed to write blob: {}", e);
                        }
                        Ok(hash)
                    },
                )
                .await
            },
            async {
                let _permit = sem.acquire().await.unwrap();
                download_and_extract_component(
                    &rust_std_url,
                    &rust_std_hash,
                    "rust-std",
                    Compression::Xz,
                    2,
                    progress,
                    async move |data: Vec<u8>| {
                        let hash = BlobHash::from_bytes(&data);
                        if let Err(e) = this2.storage.put_blob(&hash, &data).await {
                            tracing::warn!("failed to write blob: {}", e);
                        }
                        Ok(hash)
                    },
                )
                .await
            },
        )
        .await;

        let rustc_tree = match rustc_result {
            Ok(tree) => tree,
            Err(e) => {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(format!("failed to download/extract rustc: {}", e)),
                };
            }
        };

        let rust_std_tree = match rust_std_result {
            Ok(tree) => tree,
            Err(e) => {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(format!("failed to download/extract rust-std: {}", e)),
                };
            }
        };

        // Store rustc tree manifest
        let rustc_tree_json =
            facet_json::to_string(&rustc_tree).expect("rustc tree serialization must succeed");
        let rustc_tree_hash = BlobHash::from_bytes(rustc_tree_json.as_bytes());
        if let Err(e) = self
            .storage
            .put_blob(&rustc_tree_hash, rustc_tree_json.as_bytes())
            .await
        {
            tracing::warn!("failed to write rustc tree blob: {}", e);
        }

        tracing::info!(
            rustc_tree = %rustc_tree_hash,
            files = rustc_tree.entries.len(),
            unique_blobs = rustc_tree.unique_blobs,
            total_bytes = rustc_tree.total_size_bytes,
            "stored rustc tree"
        );

        // Store rust-std tree manifest
        let rust_std_tree_json = facet_json::to_string(&rust_std_tree)
            .expect("rust-std tree serialization must succeed");
        let rust_std_tree_hash = BlobHash::from_bytes(rust_std_tree_json.as_bytes());
        if let Err(e) = self
            .storage
            .put_blob(&rust_std_tree_hash, rust_std_tree_json.as_bytes())
            .await
        {
            tracing::warn!("failed to write rust-std tree blob: {}", e);
        }

        tracing::info!(
            rust_std_tree = %rust_std_tree_hash,
            files = rust_std_tree.entries.len(),
            unique_blobs = rust_std_tree.unique_blobs,
            total_bytes = rust_std_tree.total_size_bytes,
            "stored rust-std tree"
        );

        // Build toolchain manifest
        let toolchain_manifest = ToolchainManifest {
            schema_version: TOOLCHAIN_MANIFEST_SCHEMA_VERSION,
            kind: ToolchainKind::Rust,
            spec_key,
            toolchain_id: toolchain_id.0,
            created_at: Timestamp::now().in_tz("UTC").unwrap().to_string(),
            rust_manifest_date: Some(manifest.date.clone()),
            rust_version: Some(manifest.rustc.version.clone()),
            zig_version: None,
            components: vec![
                ToolchainComponentTree {
                    name: "rustc".to_string(),
                    target: Some(spec.host.clone()),
                    tree_manifest: rustc_tree_hash,
                    sha256: rustc_target.hash.clone(),
                    total_size_bytes: rustc_tree.total_size_bytes,
                    file_count: rustc_tree.entries.len() as u32,
                    unique_blobs: rustc_tree.unique_blobs,
                },
                ToolchainComponentTree {
                    name: "rust-std".to_string(),
                    target: Some(spec.target.clone()),
                    tree_manifest: rust_std_tree_hash,
                    sha256: rust_std_target.hash.clone(),
                    total_size_bytes: rust_std_tree.total_size_bytes,
                    file_count: rust_std_tree.entries.len() as u32,
                    unique_blobs: rust_std_tree.unique_blobs,
                },
            ],
        };

        // Store manifest
        let manifest_json = facet_json::to_string(&toolchain_manifest)
            .expect("toolchain manifest serialization must succeed");
        let manifest_hash = Blake3Hash::from_bytes(manifest_json.as_bytes());
        if let Err(e) = self
            .storage
            .put_manifest(&manifest_hash, manifest_json.as_bytes())
            .await
        {
            tracing::warn!("failed to write toolchain manifest: {}", e);
        }

        // Publish spec → manifest_hash mapping
        if let Err(e) = self
            .storage
            .put_toolchain_spec(&spec_key, &manifest_hash)
            .await
        {
            tracing::warn!("failed to publish spec mapping for {}: {}", spec_key, e);
        }

        tracing::info!(
            spec_key = %spec_key,
            toolchain_id = %toolchain_id,
            manifest_hash = %manifest_hash,
            "stored Rust toolchain in CAS"
        );

        EnsureToolchainResult {
            spec_key: Some(spec_key),
            toolchain_id: Some(toolchain_id.0),
            manifest_hash: Some(manifest_hash),
            status: ToolchainEnsureStatus::Downloaded,
            error: None,
        }
    }

    /// Download one Rust toolchain component tarball and store it as a blob.
    #[tracing::instrument(skip(self, progress), fields(spec_key = tracing::field::Empty, component = ?component))]
    pub async fn download_rust_toolchain_component(
        &self,
        spec: RustToolchainSpec,
        component: RustComponent,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureRustToolchainComponentResult {
        let component_name = match rust_component_name(&component) {
            Some(name) => name,
            None => {
                return EnsureRustToolchainComponentResult {
                    spec_key: None,
                    component,
                    tarball_blob: None,
                    tree_manifest: None,
                    status: EnsureStatus::Failed,
                    error: Some("unsupported component for Rust toolchain acquisition".to_string()),
                };
            }
        };

        let spec_key = match spec.spec_key() {
            Ok(k) => {
                tracing::Span::current().record("spec_key", k.to_string());
                k
            }
            Err(e) => {
                return EnsureRustToolchainComponentResult {
                    spec_key: None,
                    component,
                    tarball_blob: None,
                    tree_manifest: None,
                    status: EnsureStatus::Failed,
                    error: Some(format!("invalid spec: {}", e)),
                };
            }
        };

        if self
            .storage
            .get_toolchain_spec(&spec_key)
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return EnsureRustToolchainComponentResult {
                spec_key: Some(spec_key),
                component,
                tarball_blob: None,
                tree_manifest: None,
                status: EnsureStatus::Hit,
                error: None,
            };
        }

        let key = (spec_key, component.clone());
        if let Some(blob) = self.rust_component_tarballs.lock().await.get(&key).copied() {
            return EnsureRustToolchainComponentResult {
                spec_key: Some(spec_key),
                component,
                tarball_blob: Some(blob),
                tree_manifest: None,
                status: EnsureStatus::Hit,
                error: None,
            };
        }

        let channel = match &spec.channel {
            RustChannel::Stable => vx_toolchain::Channel::Stable,
            RustChannel::Beta => vx_toolchain::Channel::Beta,
            RustChannel::Nightly { date } => vx_toolchain::Channel::Nightly { date: date.clone() },
        };

        let manifest = match fetch_channel_manifest(&channel).await {
            Ok(m) => m,
            Err(e) => {
                return EnsureRustToolchainComponentResult {
                    spec_key: Some(spec_key),
                    component,
                    tarball_blob: None,
                    tree_manifest: None,
                    status: EnsureStatus::Failed,
                    error: Some(format!("failed to fetch channel manifest: {}", e)),
                };
            }
        };

        let target_manifest = match component {
            RustComponent::Rustc => manifest.rustc_for_target(&spec.host),
            RustComponent::RustStd => manifest.rust_std_for_target(&spec.target),
            RustComponent::Cargo => unreachable!("cargo is filtered above"),
        };

        let target_manifest = match target_manifest {
            Ok(t) => t,
            Err(e) => {
                return EnsureRustToolchainComponentResult {
                    spec_key: Some(spec_key),
                    component,
                    tarball_blob: None,
                    tree_manifest: None,
                    status: EnsureStatus::Failed,
                    error: Some(e.to_string()),
                };
            }
        };

        let tarball = match download_component(
            &target_manifest.url,
            &target_manifest.hash,
            component_name,
            &progress,
        )
        .await
        {
            Ok(bytes) => bytes,
            Err(e) => {
                return EnsureRustToolchainComponentResult {
                    spec_key: Some(spec_key),
                    component,
                    tarball_blob: None,
                    tree_manifest: None,
                    status: EnsureStatus::Failed,
                    error: Some(format!("failed to download {component_name}: {e}")),
                };
            }
        };

        let tarball_blob = BlobHash::from_bytes(&tarball);
        if let Err(e) = self.storage.put_blob(&tarball_blob, &tarball).await {
            tracing::warn!(blob_hash = %tarball_blob, error = %e, "failed to write tarball blob");
        }

        self.rust_component_tarballs
            .lock()
            .await
            .insert((spec_key, component.clone()), tarball_blob);

        EnsureRustToolchainComponentResult {
            spec_key: Some(spec_key),
            component,
            tarball_blob: Some(tarball_blob),
            tree_manifest: None,
            status: EnsureStatus::Downloaded,
            error: None,
        }
    }

    /// Extract one Rust toolchain component tarball into a tree manifest.
    #[tracing::instrument(skip(self, progress), fields(spec_key = tracing::field::Empty, component = ?component))]
    pub async fn extract_rust_toolchain_component(
        &self,
        spec: RustToolchainSpec,
        component: RustComponent,
        tarball_blob: Blake3Hash,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureRustToolchainComponentResult {
        let component_name = match rust_component_name(&component) {
            Some(name) => name,
            None => {
                return EnsureRustToolchainComponentResult {
                    spec_key: None,
                    component,
                    tarball_blob: None,
                    tree_manifest: None,
                    status: EnsureStatus::Failed,
                    error: Some("unsupported component for Rust toolchain extraction".to_string()),
                };
            }
        };

        let spec_key = match spec.spec_key() {
            Ok(k) => {
                tracing::Span::current().record("spec_key", k.to_string());
                k
            }
            Err(e) => {
                return EnsureRustToolchainComponentResult {
                    spec_key: None,
                    component,
                    tarball_blob: None,
                    tree_manifest: None,
                    status: EnsureStatus::Failed,
                    error: Some(format!("invalid spec: {}", e)),
                };
            }
        };

        if self
            .storage
            .get_toolchain_spec(&spec_key)
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return EnsureRustToolchainComponentResult {
                spec_key: Some(spec_key),
                component,
                tarball_blob: Some(tarball_blob),
                tree_manifest: None,
                status: EnsureStatus::Hit,
                error: None,
            };
        }

        let extracted_key = (spec_key, component.clone());
        if let Some(existing) = self
            .rust_component_trees
            .lock()
            .await
            .get(&extracted_key)
            .cloned()
        {
            return EnsureRustToolchainComponentResult {
                spec_key: Some(spec_key),
                component,
                tarball_blob: Some(tarball_blob),
                tree_manifest: Some(existing.tree_manifest),
                status: EnsureStatus::Hit,
                error: None,
            };
        }

        let tarball = match self.storage.get_blob(&tarball_blob).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                return EnsureRustToolchainComponentResult {
                    spec_key: Some(spec_key),
                    component,
                    tarball_blob: Some(tarball_blob),
                    tree_manifest: None,
                    status: EnsureStatus::Failed,
                    error: Some(format!("tarball blob {} not found", tarball_blob)),
                };
            }
            Err(e) => {
                return EnsureRustToolchainComponentResult {
                    spec_key: Some(spec_key),
                    component,
                    tarball_blob: Some(tarball_blob),
                    tree_manifest: None,
                    status: EnsureStatus::Failed,
                    error: Some(format!(
                        "failed reading tarball blob {}: {}",
                        tarball_blob, e
                    )),
                };
            }
        };

        let _ = progress
            .send(&StoreProgress {
                message: format!("extracting {component_name}"),
                current: None,
                total: None,
            })
            .await;

        let strip_components = match component {
            RustComponent::Rustc => 2,
            RustComponent::RustStd => 2,
            RustComponent::Cargo => unreachable!("cargo is filtered above"),
        };

        let this = self.clone();
        let tree = match vx_tarball::extract_to_tree(
            std::io::Cursor::new(tarball),
            Compression::Xz,
            strip_components,
            async move |data: Vec<u8>| {
                let hash = BlobHash::from_bytes(&data);
                if let Err(e) = this.storage.put_blob(&hash, &data).await {
                    tracing::warn!("failed to write blob: {}", e);
                }
                Ok(hash)
            },
        )
        .await
        {
            Ok(tree) => tree,
            Err(e) => {
                return EnsureRustToolchainComponentResult {
                    spec_key: Some(spec_key),
                    component,
                    tarball_blob: Some(tarball_blob),
                    tree_manifest: None,
                    status: EnsureStatus::Failed,
                    error: Some(format!("failed to extract {component_name}: {}", e)),
                };
            }
        };

        let tree_json = facet_json::to_string(&tree).expect("tree serialization must succeed");
        let tree_hash = BlobHash::from_bytes(tree_json.as_bytes());
        if let Err(e) = self
            .storage
            .put_blob(&tree_hash, tree_json.as_bytes())
            .await
        {
            tracing::warn!("failed to write {} tree blob: {}", component_name, e);
        }

        let target = match component {
            RustComponent::Rustc => spec.host.clone(),
            RustComponent::RustStd => spec.target.clone(),
            RustComponent::Cargo => unreachable!("cargo is filtered above"),
        };

        self.rust_component_trees.lock().await.insert(
            extracted_key,
            ExtractedRustComponent {
                tree_manifest: tree_hash,
                target,
                total_size_bytes: tree.total_size_bytes,
                file_count: tree.entries.len() as u32,
                unique_blobs: tree.unique_blobs,
            },
        );

        EnsureRustToolchainComponentResult {
            spec_key: Some(spec_key),
            component,
            tarball_blob: Some(tarball_blob),
            tree_manifest: Some(tree_hash),
            status: EnsureStatus::Downloaded,
            error: None,
        }
    }

    /// Build and publish the Rust toolchain manifest from extracted components.
    #[tracing::instrument(skip(self), fields(spec_key = tracing::field::Empty))]
    pub async fn finalize_rust_toolchain(&self, spec: RustToolchainSpec) -> EnsureToolchainResult {
        let spec_key = match spec.spec_key() {
            Ok(k) => {
                tracing::Span::current().record("spec_key", k.to_string());
                k
            }
            Err(e) => {
                return EnsureToolchainResult {
                    spec_key: None,
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(format!("invalid spec: {}", e)),
                };
            }
        };

        if let Some(manifest_hash) = self
            .storage
            .get_toolchain_spec(&spec_key)
            .await
            .ok()
            .flatten()
        {
            return EnsureToolchainResult {
                spec_key: Some(spec_key),
                toolchain_id: None,
                manifest_hash: Some(manifest_hash),
                status: ToolchainEnsureStatus::Hit,
                error: None,
            };
        }

        let rustc_component = {
            let lock = self.rust_component_trees.lock().await;
            lock.get(&(spec_key, RustComponent::Rustc)).cloned()
        };
        let rust_std_component = {
            let lock = self.rust_component_trees.lock().await;
            lock.get(&(spec_key, RustComponent::RustStd)).cloned()
        };

        let rustc_component = match rustc_component {
            Some(c) => c,
            None => {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some("rustc component has not been extracted".to_string()),
                };
            }
        };
        let rust_std_component = match rust_std_component {
            Some(c) => c,
            None => {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some("rust-std component has not been extracted".to_string()),
                };
            }
        };

        let channel = match &spec.channel {
            RustChannel::Stable => vx_toolchain::Channel::Stable,
            RustChannel::Beta => vx_toolchain::Channel::Beta,
            RustChannel::Nightly { date } => vx_toolchain::Channel::Nightly { date: date.clone() },
        };

        let manifest = match fetch_channel_manifest(&channel).await {
            Ok(m) => m,
            Err(e) => {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(format!("failed to fetch channel manifest: {}", e)),
                };
            }
        };

        let rustc_target = match manifest.rustc_for_target(&spec.host) {
            Ok(t) => t,
            Err(e) => {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(e.to_string()),
                };
            }
        };
        let rust_std_target = match manifest.rust_std_for_target(&spec.target) {
            Ok(t) => t,
            Err(e) => {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(e.to_string()),
                };
            }
        };

        let toolchain_id = vx_toolchain::RustToolchainId::from_manifest_sha256s(
            &spec.host,
            &spec.target,
            &rustc_target.hash,
            &rust_std_target.hash,
        );

        let toolchain_manifest = ToolchainManifest {
            schema_version: TOOLCHAIN_MANIFEST_SCHEMA_VERSION,
            kind: ToolchainKind::Rust,
            spec_key,
            toolchain_id: toolchain_id.0,
            created_at: Timestamp::now().in_tz("UTC").unwrap().to_string(),
            rust_manifest_date: Some(manifest.date.clone()),
            rust_version: Some(manifest.rustc.version.clone()),
            zig_version: None,
            components: vec![
                ToolchainComponentTree {
                    name: "rustc".to_string(),
                    target: Some(rustc_component.target),
                    tree_manifest: rustc_component.tree_manifest,
                    sha256: rustc_target.hash.clone(),
                    total_size_bytes: rustc_component.total_size_bytes,
                    file_count: rustc_component.file_count,
                    unique_blobs: rustc_component.unique_blobs,
                },
                ToolchainComponentTree {
                    name: "rust-std".to_string(),
                    target: Some(rust_std_component.target),
                    tree_manifest: rust_std_component.tree_manifest,
                    sha256: rust_std_target.hash.clone(),
                    total_size_bytes: rust_std_component.total_size_bytes,
                    file_count: rust_std_component.file_count,
                    unique_blobs: rust_std_component.unique_blobs,
                },
            ],
        };

        let manifest_json = facet_json::to_string(&toolchain_manifest)
            .expect("toolchain manifest serialization must succeed");
        let manifest_hash = Blake3Hash::from_bytes(manifest_json.as_bytes());
        if let Err(e) = self
            .storage
            .put_manifest(&manifest_hash, manifest_json.as_bytes())
            .await
        {
            tracing::warn!(manifest_hash = %manifest_hash, error = %e, "failed to write toolchain manifest");
        }

        if let Err(e) = self
            .storage
            .put_toolchain_spec(&spec_key, &manifest_hash)
            .await
        {
            tracing::warn!("failed to publish spec mapping for {}: {}", spec_key, e);
        }

        EnsureToolchainResult {
            spec_key: Some(spec_key),
            toolchain_id: Some(toolchain_id.0),
            manifest_hash: Some(manifest_hash),
            status: ToolchainEnsureStatus::Downloaded,
            error: None,
        }
    }

    /// Ensure a Zig toolchain exists in CAS (internal helper, not an RPC method).
    #[tracing::instrument(skip(self, progress), fields(spec_key = tracing::field::Empty))]
    pub async fn ensure_zig_toolchain(
        &self,
        spec: ZigToolchainSpec,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureToolchainResult {
        // Compute spec_key
        let spec_key = match spec.spec_key() {
            Ok(k) => {
                tracing::Span::current().record("spec_key", k.to_string());
                k
            }
            Err(e) => {
                return EnsureToolchainResult {
                    spec_key: None,
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(format!("invalid spec: {}", e)),
                };
            }
        };

        let this = self.clone();
        let this2 = self.clone();

        self.toolchain_manager
            .ensure(
                spec_key,
                async move || {
                    this.storage
                        .get_toolchain_spec(&spec_key)
                        .await
                        .ok()
                        .flatten()
                },
                async move || {
                    this2
                        .acquire_zig_toolchain_impl(spec_key, &spec, &progress)
                        .await
                },
            )
            .await
    }

    async fn acquire_zig_toolchain_impl(
        &self,
        spec_key: ToolchainSpecKey,
        spec: &ZigToolchainSpec,
        progress: &roam::Tx<StoreProgress>,
    ) -> EnsureToolchainResult {
        // Convert spec to vx_toolchain types
        let version = vx_toolchain::zig::ZigVersion::new(&spec.version);
        let platform = vx_toolchain::zig::HostPlatform {
            os: if spec.host_platform.contains("macos") {
                "macos"
            } else if spec.host_platform.contains("linux") {
                "linux"
            } else if spec.host_platform.contains("windows") {
                "windows"
            } else {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(format!("unsupported platform: {}", spec.host_platform)),
                };
            }
            .to_string(),
            arch: if spec.host_platform.contains("x86_64") {
                "x86_64"
            } else if spec.host_platform.contains("aarch64") {
                "aarch64"
            } else {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(format!("unsupported platform: {}", spec.host_platform)),
                };
            }
            .to_string(),
        };

        let _ = progress
            .send(&StoreProgress {
                message: format!("downloading zig {}", spec.version),
                current: None,
                total: None,
            })
            .await;

        // Download and extract Zig toolchain
        let acquired =
            match vx_toolchain::zig::acquire_zig_toolchain(version.clone(), platform.clone()).await
            {
                Ok(a) => a,
                Err(e) => {
                    return EnsureToolchainResult {
                        spec_key: Some(spec_key),
                        toolchain_id: None,
                        manifest_hash: None,
                        status: ToolchainEnsureStatus::Failed,
                        error: Some(format!("failed to acquire Zig toolchain: {}", e)),
                    };
                }
            };

        let _ = progress
            .send(&StoreProgress {
                message: "storing zig toolchain".to_string(),
                current: None,
                total: None,
            })
            .await;

        // Store zig executable as tree manifest (single-file tree)
        let zig_exe_blob_hash = BlobHash::from_bytes(&acquired.zig_exe_contents);
        if let Err(e) = self
            .storage
            .put_blob(&zig_exe_blob_hash, &acquired.zig_exe_contents)
            .await
        {
            tracing::warn!("failed to write zig exe blob: {}", e);
        }
        let mut zig_exe_tree = vx_store_proto::TreeManifest::new();
        zig_exe_tree.add_file(
            "zig".to_string(),
            zig_exe_blob_hash,
            acquired.zig_exe_contents.len() as u64,
            true, // executable
        );
        zig_exe_tree.finalize();
        let zig_exe_tree_json = facet_json::to_string(&zig_exe_tree)
            .expect("zig executable tree serialization must succeed");
        let zig_exe_hash = BlobHash::from_bytes(zig_exe_tree_json.as_bytes());
        if let Err(e) = self
            .storage
            .put_blob(&zig_exe_hash, zig_exe_tree_json.as_bytes())
            .await
        {
            tracing::warn!("failed to write zig exe tree blob: {}", e);
        }

        // Extract lib tarball to tree manually
        // First collect all entries synchronously (tar::Archive is not Send)
        // Note: Paths in the tarball are relative (e.g. "std/std.zig"), but Zig expects them under "lib/"
        let mut lib_files: Vec<(String, Vec<u8>)> = Vec::new();
        {
            let mut archive = tar::Archive::new(std::io::Cursor::new(&acquired.lib_tarball));

            for entry_result in archive
                .entries()
                .map_err(|e| format!("tar entries: {}", e))
                .unwrap()
            {
                let mut entry = entry_result
                    .map_err(|e| format!("tar entry: {}", e))
                    .unwrap();
                let path = entry
                    .path()
                    .map_err(|e| format!("entry path: {}", e))
                    .unwrap();
                let path_str = path.to_string_lossy().to_string();

                // Prepend "lib/" to all paths since Zig expects lib directory
                let full_path = format!("lib/{}", path_str);

                // Read entry data
                let mut data = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut data)
                    .map_err(|e| format!("read entry: {}", e))
                    .unwrap();

                if !data.is_empty() {
                    lib_files.push((full_path, data));
                }
            }
        } // archive dropped here

        // Now upload blobs asynchronously and build tree
        let mut lib_tree = vx_store_proto::TreeManifest::new();
        for (path_str, data) in lib_files {
            let blob_hash = BlobHash::from_bytes(&data);
            if let Err(e) = self.storage.put_blob(&blob_hash, &data).await {
                tracing::warn!("failed to write zig lib blob: {}", e);
            }
            lib_tree.add_file(
                path_str,
                blob_hash,
                data.len() as u64,
                false, // lib files are not executable
            );
        }

        lib_tree.finalize();

        // Store lib tree manifest
        let lib_tree_json =
            facet_json::to_string(&lib_tree).expect("zig lib tree serialization must succeed");
        let lib_hash = BlobHash::from_bytes(lib_tree_json.as_bytes());
        if let Err(e) = self
            .storage
            .put_blob(&lib_hash, lib_tree_json.as_bytes())
            .await
        {
            tracing::warn!("failed to write zig lib tree blob: {}", e);
        }

        // Cleanup extraction directory
        if let Err(e) = acquired.cleanup() {
            tracing::warn!("failed to cleanup Zig extraction directory: {}", e);
        }

        let toolchain_id = acquired.id.0;

        tracing::info!(
            zig_exe = %zig_exe_hash,
            lib = %lib_hash,
            toolchain_id = %toolchain_id,
            "stored Zig toolchain blobs in CAS"
        );

        // Create toolchain manifest
        let toolchain_manifest = ToolchainManifest {
            schema_version: TOOLCHAIN_MANIFEST_SCHEMA_VERSION,
            kind: ToolchainKind::Zig,
            spec_key,
            toolchain_id,
            created_at: Timestamp::now().in_tz("UTC").unwrap().to_string(),
            rust_manifest_date: None,
            rust_version: None,
            zig_version: Some(version.version.clone()),
            components: vec![
                ToolchainComponentTree {
                    name: "zig-exe".to_string(),
                    target: Some(platform.to_string()),
                    tree_manifest: zig_exe_hash,
                    sha256: String::new(), // Zig doesn't provide checksums
                    total_size_bytes: acquired.zig_exe_contents.len() as u64,
                    file_count: 1,
                    unique_blobs: 1,
                },
                ToolchainComponentTree {
                    name: "zig-lib".to_string(),
                    target: Some(platform.to_string()),
                    tree_manifest: lib_hash,
                    sha256: String::new(),
                    total_size_bytes: acquired.lib_tarball.len() as u64,
                    file_count: 1, // It's a tarball, many files inside
                    unique_blobs: 1,
                },
            ],
        };

        // Store manifest
        let manifest_json = facet_json::to_string(&toolchain_manifest)
            .expect("toolchain manifest serialization must succeed");
        let manifest_hash = Blake3Hash::from_bytes(manifest_json.as_bytes());
        if let Err(e) = self
            .storage
            .put_manifest(&manifest_hash, manifest_json.as_bytes())
            .await
        {
            tracing::warn!("failed to write toolchain manifest: {}", e);
        }

        // Publish spec → manifest_hash mapping
        if let Err(e) = self
            .storage
            .put_toolchain_spec(&spec_key, &manifest_hash)
            .await
        {
            tracing::warn!("failed to publish spec mapping for {}: {}", spec_key, e);
        }

        tracing::info!(
            spec_key = %spec_key,
            toolchain_id = %toolchain_id,
            manifest_hash = %manifest_hash,
            "stored Zig toolchain in CAS"
        );

        EnsureToolchainResult {
            spec_key: Some(spec_key),
            toolchain_id: Some(toolchain_id),
            manifest_hash: Some(manifest_hash),
            status: ToolchainEnsureStatus::Downloaded,
            error: None,
        }
    }
}
