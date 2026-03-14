//! Toolchain Manager (Inflight Deduplication)

use futures_util::{StreamExt, future::join};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vx_store_proto::chunking::prepare_blob;
use vx_store_proto::{
    Blake3Hash, BlobRef, CDC_MAX_CHUNK, CasNode, ChunkHash, ContentHash, DirEntry, DirEntryKind,
    DirectoryNode, EnsureRustToolchainComponentResult, EnsureStatus,
    EnsureStatus as ToolchainEnsureStatus, EnsureToolchainResult, NodeHash, RustChannel,
    RustComponent, RustToolchainSpec, StoreProgress, TOOLCHAIN_MANIFEST_SCHEMA_VERSION,
    ToolchainComponentTree, ToolchainKind, ToolchainManifest, ToolchainSpecKey, ZigToolchainSpec,
};
use vx_tarball::Compression;

use crate::types::StoreService;

#[derive(Debug, Clone)]
pub(crate) struct ExtractedRustComponent {
    pub(crate) tree_root: NodeHash,
    pub(crate) target: String,
    pub(crate) total_size_bytes: u64,
    pub(crate) file_count: u32,
    pub(crate) unique_chunks: u32,
}

/// Info tracked per file during extraction — enough to build DirEntryKind later.
struct ExtractedFileInfo {
    content_hash: ContentHash,
    blob_node: Option<NodeHash>,
    size: u64,
    executable: bool,
}

type RustComponentKey = (ToolchainSpecKey, RustComponent);
type InflightComponentFuture = Arc<moire::sync::OnceCell<EnsureRustToolchainComponentResult>>;

const PROGRESS_SEND_TIMEOUT: Duration = Duration::from_millis(25);
const PROGRESS_BIND_GRACE_PERIOD: Duration = Duration::from_secs(1);
const PROGRESS_BIND_RETRY_INTERVAL: Duration = Duration::from_millis(10);

async fn send_progress_best_effort(progress: &roam::Tx<StoreProgress>, update: StoreProgress) {
    let deadline = Instant::now() + PROGRESS_BIND_GRACE_PERIOD;
    loop {
        match tokio::time::timeout(PROGRESS_SEND_TIMEOUT, progress.send(update.clone())).await {
            Ok(Ok(())) => return,
            Ok(Err(roam::TxError::Unbound)) if Instant::now() < deadline => {
                tokio::time::sleep(PROGRESS_BIND_RETRY_INTERVAL).await;
            }
            _ => return,
        }
    }
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
async fn download_component(
    url: &str,
    expected_hash: &str,
    component_name: &str,
    progress: &roam::Tx<StoreProgress>,
) -> Result<Vec<u8>, vx_toolchain::ToolchainError> {
    tracing::info!(url = %url, "downloading component");

    send_progress_best_effort(
        progress,
        StoreProgress {
            message: format!("downloading {component_name}"),
            current: Some(0),
            total: None,
        },
    )
    .await;

    let bytes =
        vx_http::get_bytes(url)
            .await
            .map_err(|e| vx_toolchain::ToolchainError::FetchError {
                url: url.to_string(),
                source: Box::new(e),
            })?;

    let actual_hash = hex::encode(Sha256::digest(&bytes));
    if actual_hash != expected_hash {
        return Err(vx_toolchain::ToolchainError::ChecksumMismatch {
            url: url.to_string(),
            expected: expected_hash.to_string(),
            actual: actual_hash,
        });
    }

    let bytes_read = bytes.len() as u64;

    send_progress_best_effort(
        progress,
        StoreProgress {
            message: format!("downloading {component_name}"),
            current: Some(bytes_read),
            total: Some(bytes_read),
        },
    )
    .await;

    tracing::debug!(
        url = %url,
        size = bytes.len(),
        hash = %expected_hash,
        "component downloaded, verified, and cached if configured"
    );

    Ok(bytes)
}

/// Download and extract a component in a single streaming pipeline.
///
/// HTTP body → StreamReader → Sha256VerifyingReader → CountingReader → extract_to_tree → CAS
///
/// The store_blob callback now chunks data and stores chunks/BlobNodes.
/// It returns a Blake3Hash (content hash) for the TreeManifest, but also
/// records per-file info via the side channel.
async fn download_and_extract_component(
    url: &str,
    expected_hash: &str,
    component_name: &str,
    compression: Compression,
    strip_components: u32,
    progress: &roam::Tx<StoreProgress>,
    store_blob: impl AsyncFn(Vec<u8>) -> Result<Blake3Hash, String>,
) -> Result<vx_store_proto::TreeManifest, vx_toolchain::ToolchainError> {
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
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

    let mut reader = Box::pin(counting_reader);

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
                    send_progress_best_effort(
                        progress,
                        StoreProgress {
                            message: format!("downloading+extracting {component_name}"),
                            current: Some(current),
                            total: content_length,
                        },
                    )
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

    send_progress_best_effort(
        progress,
        StoreProgress {
            message: format!("downloading+extracting {component_name}"),
            current: Some(counter.load(std::sync::atomic::Ordering::Relaxed)),
            total: content_length,
        },
    )
    .await;

    tracing::debug!(
        url = %url,
        bytes = counter.load(std::sync::atomic::Ordering::Relaxed),
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

type InflightFuture = Arc<moire::sync::OnceCell<EnsureToolchainResult>>;

async fn singleflight_component(
    flights: &moire::sync::SyncMutex<HashMap<RustComponentKey, InflightComponentFuture>>,
    key: RustComponentKey,
    cell_name: &'static str,
    acquire_fn: impl AsyncFn() -> EnsureRustToolchainComponentResult,
) -> EnsureRustToolchainComponentResult {
    let cell = {
        let mut inflight = flights.lock();
        inflight
            .entry(key.clone())
            .or_insert_with(|| Arc::new(moire::sync::OnceCell::new(cell_name)))
            .clone()
    };

    #[allow(clippy::redundant_closure)]
    let result = cell.get_or_init(|| acquire_fn()).await.clone();

    let mut inflight = flights.lock();
    if let Some(existing) = inflight.get(&key)
        && Arc::ptr_eq(existing, &cell)
    {
        inflight.remove(&key);
    }

    result
}

/// Manages in-flight toolchain acquisitions with deduplication.
pub(crate) struct ToolchainManager {
    inflight: moire::sync::SyncMutex<HashMap<ToolchainSpecKey, InflightFuture>>,
}

impl ToolchainManager {
    pub(crate) fn new() -> Self {
        Self {
            inflight: moire::sync::SyncMutex::new("store/toolchain_inflight", HashMap::new()),
        }
    }

    pub(crate) async fn ensure(
        &self,
        spec_key: ToolchainSpecKey,
        lookup_fn: impl AsyncFn() -> Option<Blake3Hash>,
        acquire_fn: impl AsyncFn() -> EnsureToolchainResult,
    ) -> EnsureToolchainResult {
        if let Some(manifest_hash) = lookup_fn().await {
            return EnsureToolchainResult {
                spec_key: Some(spec_key),
                toolchain_id: None,
                manifest_hash: Some(NodeHash(manifest_hash)),
                status: ToolchainEnsureStatus::Hit,
                error: None,
            };
        }

        let cell = {
            let mut inflight = self.inflight.lock();
            inflight
                .entry(spec_key)
                .or_insert_with(|| Arc::new(moire::sync::OnceCell::new("toolchain_inflight")))
                .clone()
        };

        #[allow(clippy::redundant_closure)]
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
// Internal helpers for chunk-based storage
// =============================================================================

/// Store blob data as chunks, return a BlobRef.
async fn store_blob_chunked(service: &StoreService, data: &[u8]) -> Result<BlobRef, String> {
    let prepared = prepare_blob(data);

    // Store all chunks
    for chunk_info in &prepared.chunks {
        let chunk_data = &data[chunk_info.offset..chunk_info.offset + chunk_info.size as usize];
        service
            .storage
            .put_chunk(&chunk_info.hash, chunk_data)
            .await
            .map_err(|e| format!("failed to store chunk: {e}"))?;
    }

    if let Some(blob_node) = &prepared.blob_node {
        service
            .storage
            .put_node(&blob_node.node_hash, &blob_node.serialized_bytes)
            .await
            .map_err(|e| format!("failed to store BlobNode: {e}"))?;

        Ok(BlobRef::Large {
            blob_node: blob_node.node_hash,
            content_hash: prepared.content_hash,
            size: data.len() as u64,
        })
    } else {
        assert_eq!(prepared.chunks.len(), 1);
        Ok(BlobRef::Small {
            chunk: prepared.chunks[0].hash,
            size: data.len() as u64,
        })
    }
}

/// Build a DirectoryNode Merkle tree from a flat list of (path, info) pairs.
/// Returns the root NodeHash after storing all directory nodes.
async fn build_merkle_tree(
    service: &StoreService,
    files: &[(String, ExtractedFileInfo)],
    symlinks: &[(String, String)],
) -> Result<(NodeHash, u64), String> {
    // Group entries by their parent directory path.
    // We process deepest directories first (bottom-up) so child dir hashes
    // are available when building parent directories.
    use std::collections::BTreeMap;

    // Collect all directory paths that need nodes
    let mut dir_entries: BTreeMap<String, Vec<DirEntry>> = BTreeMap::new();
    let mut dir_sizes: BTreeMap<String, u64> = BTreeMap::new();

    // Add files
    for (path, info) in files {
        let (parent, name) = split_path(path);
        let kind = if info.size <= CDC_MAX_CHUNK as u64 {
            DirEntryKind::SmallFile {
                content_hash: info.content_hash,
                size: info.size,
                executable: info.executable,
            }
        } else {
            let blob_node = match info.blob_node {
                Some(hash) => hash,
                None => {
                    return Err(format!(
                        "missing BlobNode for large file '{}' (size={})",
                        path, info.size
                    ));
                }
            };
            DirEntryKind::LargeFile {
                content_hash: info.content_hash,
                blob_node,
                size: info.size,
                executable: info.executable,
            }
        };
        dir_entries
            .entry(parent.to_string())
            .or_default()
            .push(DirEntry {
                name: name.to_string(),
                kind,
            });
        *dir_sizes.entry(parent.to_string()).or_default() += info.size;
    }

    // Add symlinks
    for (path, target) in symlinks {
        let (parent, name) = split_path(path);
        dir_entries
            .entry(parent.to_string())
            .or_default()
            .push(DirEntry {
                name: name.to_string(),
                kind: DirEntryKind::Symlink {
                    target: target.clone(),
                },
            });
    }

    // Ensure intermediate directories exist in our map
    let all_dirs: Vec<String> = dir_entries.keys().cloned().collect();
    for dir in &all_dirs {
        ensure_parent_dirs(&mut dir_entries, dir);
    }

    // Process directories bottom-up (longest paths first)
    let mut dir_list: Vec<String> = dir_entries.keys().cloned().collect();
    dir_list.sort_by(|a, b| {
        // Sort by depth (deepest first), then lexicographically
        let a_depth = if a.is_empty() {
            0
        } else {
            a.matches('/').count() + 1
        };
        let b_depth = if b.is_empty() {
            0
        } else {
            b.matches('/').count() + 1
        };
        b_depth.cmp(&a_depth).then_with(|| a.cmp(b))
    });

    let mut dir_hashes: HashMap<String, (NodeHash, u64)> = HashMap::new();

    for dir_path in &dir_list {
        let entries = match dir_entries.get_mut(dir_path) {
            Some(entries) => entries,
            None => {
                return Err(format!(
                    "internal error: missing dir entries for '{dir_path}'"
                ));
            }
        };

        // Sort entries by name (raw byte comparison)
        entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));

        // Compute total_size for this directory
        let mut total_size: u64 = 0;
        for entry in entries.iter() {
            match &entry.kind {
                DirEntryKind::SmallFile { size, .. } => total_size += size,
                DirEntryKind::LargeFile { size, .. } => total_size += size,
                DirEntryKind::Dir { total_size: ts, .. } => total_size += ts,
                DirEntryKind::Symlink { .. } => {}
            }
        }

        let dir_node = CasNode::Directory(DirectoryNode {
            entries: entries.clone(),
            total_size,
        });

        let (node_bytes, node_hash) = dir_node.to_bytes_and_hash();
        service
            .storage
            .put_node(&node_hash, &node_bytes)
            .await
            .map_err(|e| format!("failed to store DirectoryNode for '{}': {e}", dir_path))?;

        dir_hashes.insert(dir_path.clone(), (node_hash, total_size));

        // Add this directory as a Dir entry in the parent
        if !dir_path.is_empty() {
            let (parent, name) = split_path(dir_path);
            dir_entries
                .entry(parent.to_string())
                .or_default()
                .push(DirEntry {
                    name: name.to_string(),
                    kind: DirEntryKind::Dir {
                        hash: node_hash,
                        total_size,
                    },
                });
        }
    }

    dir_hashes
        .get("")
        .copied()
        .ok_or_else(|| "no root directory".to_string())
}

async fn validate_blob_node_closure(
    service: &StoreService,
    blob_node_hash: NodeHash,
) -> Result<(), String> {
    let node_data = service
        .storage
        .get_node(&blob_node_hash)
        .await
        .map_err(|e| format!("failed to read BlobNode {}: {e}", blob_node_hash.to_hex()))?
        .ok_or_else(|| format!("BlobNode {} not found", blob_node_hash.to_hex()))?;

    let node = CasNode::from_bytes(&node_data).map_err(|e| {
        format!(
            "failed to deserialize BlobNode {}: {e}",
            blob_node_hash.to_hex()
        )
    })?;
    let blob = match node {
        CasNode::Blob(blob) => blob,
        _ => {
            return Err(format!(
                "node {} is not a BlobNode",
                blob_node_hash.to_hex()
            ));
        }
    };

    for chunk in blob.chunks {
        let exists = service
            .storage
            .has_chunk(&chunk.hash)
            .await
            .map_err(|e| format!("failed to check chunk {}: {e}", chunk.hash.to_hex()))?;
        if !exists {
            return Err(format!(
                "BlobNode {} references missing chunk {}",
                blob_node_hash.to_hex(),
                chunk.hash.to_hex()
            ));
        }
    }

    Ok(())
}

async fn validate_directory_closure(service: &StoreService, root: NodeHash) -> Result<(), String> {
    let mut stack = vec![root];
    let mut visited = HashSet::new();

    while let Some(dir_hash) = stack.pop() {
        if !visited.insert(dir_hash) {
            continue;
        }

        let node_data = service
            .storage
            .get_node(&dir_hash)
            .await
            .map_err(|e| format!("failed to read DirectoryNode {}: {e}", dir_hash.to_hex()))?
            .ok_or_else(|| format!("DirectoryNode {} not found", dir_hash.to_hex()))?;

        let node = CasNode::from_bytes(&node_data).map_err(|e| {
            format!(
                "failed to deserialize DirectoryNode {}: {e}",
                dir_hash.to_hex()
            )
        })?;
        let dir = match node {
            CasNode::Directory(dir) => dir,
            _ => return Err(format!("node {} is not a DirectoryNode", dir_hash.to_hex())),
        };

        for entry in dir.entries {
            match entry.kind {
                DirEntryKind::SmallFile { content_hash, .. } => {
                    let chunk = ChunkHash(content_hash.0);
                    let exists =
                        service.storage.has_chunk(&chunk).await.map_err(|e| {
                            format!("failed to check chunk {}: {e}", chunk.to_hex())
                        })?;
                    if !exists {
                        return Err(format!(
                            "DirectoryNode {} references missing chunk {}",
                            dir_hash.to_hex(),
                            chunk.to_hex()
                        ));
                    }
                }
                DirEntryKind::LargeFile { blob_node, .. } => {
                    validate_blob_node_closure(service, blob_node).await?;
                }
                DirEntryKind::Dir { hash, .. } => stack.push(hash),
                DirEntryKind::Symlink { .. } => {}
            }
        }
    }

    Ok(())
}

/// Split "foo/bar/baz" into ("foo/bar", "baz"). Root entries: ("", "name").
fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(pos) => (&path[..pos], &path[pos + 1..]),
        None => ("", path),
    }
}

/// Ensure all parent directories of `dir` exist in the map.
fn ensure_parent_dirs(
    dir_entries: &mut std::collections::BTreeMap<String, Vec<DirEntry>>,
    dir: &str,
) {
    let mut current = dir.to_string();
    while let Some(pos) = current.rfind('/') {
        current = current[..pos].to_string();
        dir_entries.entry(current.clone()).or_default();
    }
    if !current.is_empty() {
        dir_entries.entry(String::new()).or_default();
    }
}

/// Read blob data from a BlobRef.
async fn read_blob_from_ref(service: &StoreService, blob_ref: &BlobRef) -> Result<Vec<u8>, String> {
    match blob_ref {
        BlobRef::Small { chunk, size: _ } => {
            let data = service
                .storage
                .get_chunk(chunk)
                .await
                .map_err(|e| format!("failed to read chunk: {e}"))?
                .ok_or_else(|| format!("chunk {} not found", chunk.to_hex()))?;
            Ok(data)
        }
        BlobRef::Large {
            blob_node,
            content_hash: _,
            size: _,
        } => {
            let node_data = service
                .storage
                .get_node(blob_node)
                .await
                .map_err(|e| format!("failed to read BlobNode: {e}"))?
                .ok_or_else(|| format!("BlobNode {} not found", blob_node.to_hex()))?;

            let node = CasNode::from_bytes(&node_data)
                .map_err(|e| format!("failed to deserialize BlobNode: {e}"))?;

            let blob = match node {
                CasNode::Blob(b) => b,
                _ => return Err(format!("node {} is not a BlobNode", blob_node.to_hex())),
            };

            let mut result = Vec::with_capacity(blob.total_size as usize);
            for chunk_entry in &blob.chunks {
                let chunk_data = service
                    .storage
                    .get_chunk(&chunk_entry.hash)
                    .await
                    .map_err(|e| format!("failed to read chunk: {e}"))?
                    .ok_or_else(|| format!("chunk {} not found", chunk_entry.hash.to_hex()))?;
                result.extend_from_slice(&chunk_data);
            }

            Ok(result)
        }
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

        let toolchain_id = vx_toolchain::RustToolchainId::from_manifest_sha256s(
            &spec.host,
            &spec.target,
            &rustc_target.hash,
            &rust_std_target.hash,
        );

        let rustc_url = rustc_target.url.clone();
        let rustc_hash = rustc_target.hash.clone();
        let rust_std_url = rust_std_target.url.clone();
        let rust_std_hash = rust_std_target.hash.clone();
        let sem = self.download_semaphore.clone();

        // Track per-file info: content_hash -> (blob_node, size)
        // This lets us build DirectoryNode entries after extraction.
        let rustc_file_info: Arc<moire::sync::SyncMutex<HashMap<Blake3Hash, ExtractedFileInfo>>> =
            Arc::new(moire::sync::SyncMutex::new(
                "store/rustc_file_info",
                HashMap::new(),
            ));
        let std_file_info: Arc<moire::sync::SyncMutex<HashMap<Blake3Hash, ExtractedFileInfo>>> =
            Arc::new(moire::sync::SyncMutex::new(
                "store/std_file_info",
                HashMap::new(),
            ));

        let this1 = self.clone();
        let this2 = self.clone();
        let rustc_info = rustc_file_info.clone();
        let std_info = std_file_info.clone();

        let (rustc_result, rust_std_result) =
            join(
                async {
                    let _permit = sem.acquire().await.map_err(|e| {
                        vx_toolchain::ToolchainError::FetchError {
                            url: rustc_url.clone(),
                            source: Box::new(std::io::Error::other(format!(
                                "download semaphore closed: {e}"
                            ))),
                        }
                    })?;
                    download_and_extract_component(
                        &rustc_url,
                        &rustc_hash,
                        "rustc",
                        Compression::Xz,
                        2,
                        progress,
                        async move |data: Vec<u8>| {
                            let content_hash = ContentHash(Blake3Hash::from_bytes(&data));
                            let prepared = prepare_blob(&data);
                            // Store chunks
                            for ci in &prepared.chunks {
                                let chunk_data = &data[ci.offset..ci.offset + ci.size as usize];
                                this1
                                    .storage
                                    .put_chunk(&ci.hash, chunk_data)
                                    .await
                                    .map_err(|e| format!("failed to write chunk: {e}"))?;
                            }
                            // Store blob node if multi-chunk
                            let blob_node = if let Some(bn) = &prepared.blob_node {
                                this1
                                    .storage
                                    .put_node(&bn.node_hash, &bn.serialized_bytes)
                                    .await
                                    .map_err(|e| format!("failed to write BlobNode: {e}"))?;
                                Some(bn.node_hash)
                            } else {
                                None
                            };
                            // Track info
                            rustc_info.lock().insert(
                                content_hash.0,
                                ExtractedFileInfo {
                                    content_hash,
                                    blob_node,
                                    size: data.len() as u64,
                                    executable: false, // will be set from tree manifest
                                },
                            );
                            Ok(content_hash.0)
                        },
                    )
                    .await
                },
                async {
                    let _permit = sem.acquire().await.map_err(|e| {
                        vx_toolchain::ToolchainError::FetchError {
                            url: rust_std_url.clone(),
                            source: Box::new(std::io::Error::other(format!(
                                "download semaphore closed: {e}"
                            ))),
                        }
                    })?;
                    download_and_extract_component(
                        &rust_std_url,
                        &rust_std_hash,
                        "rust-std",
                        Compression::Xz,
                        2,
                        progress,
                        async move |data: Vec<u8>| {
                            let content_hash = ContentHash(Blake3Hash::from_bytes(&data));
                            let prepared = prepare_blob(&data);
                            for ci in &prepared.chunks {
                                let chunk_data = &data[ci.offset..ci.offset + ci.size as usize];
                                this2
                                    .storage
                                    .put_chunk(&ci.hash, chunk_data)
                                    .await
                                    .map_err(|e| format!("failed to write chunk: {e}"))?;
                            }
                            let blob_node = if let Some(bn) = &prepared.blob_node {
                                this2
                                    .storage
                                    .put_node(&bn.node_hash, &bn.serialized_bytes)
                                    .await
                                    .map_err(|e| format!("failed to write BlobNode: {e}"))?;
                                Some(bn.node_hash)
                            } else {
                                None
                            };
                            std_info.lock().insert(
                                content_hash.0,
                                ExtractedFileInfo {
                                    content_hash,
                                    blob_node,
                                    size: data.len() as u64,
                                    executable: false,
                                },
                            );
                            Ok(content_hash.0)
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

        // Build DirectoryNode trees from the flat TreeManifests
        let rustc_root = match tree_manifest_to_merkle(self, &rustc_tree, &rustc_file_info).await {
            Ok(r) => r,
            Err(e) => {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(format!("failed to build rustc merkle tree: {e}")),
                };
            }
        };

        let rust_std_root =
            match tree_manifest_to_merkle(self, &rust_std_tree, &std_file_info).await {
                Ok(r) => r,
                Err(e) => {
                    return EnsureToolchainResult {
                        spec_key: Some(spec_key),
                        toolchain_id: None,
                        manifest_hash: None,
                        status: ToolchainEnsureStatus::Failed,
                        error: Some(format!("failed to build rust-std merkle tree: {e}")),
                    };
                }
            };

        tracing::info!(
            rustc_root = %rustc_root.0,
            rustc_files = rustc_tree.entries.len(),
            rustc_bytes = rustc_root.1,
            "stored rustc tree"
        );

        tracing::info!(
            rust_std_root = %rust_std_root.0,
            rust_std_files = rust_std_tree.entries.len(),
            rust_std_bytes = rust_std_root.1,
            "stored rust-std tree"
        );

        // Count unique chunks
        let rustc_unique_chunks = {
            let info = rustc_file_info.lock();
            info.len() as u32
        };
        let std_unique_chunks = {
            let info = std_file_info.lock();
            info.len() as u32
        };

        // Build toolchain manifest as CasNode::Toolchain
        let toolchain_manifest = ToolchainManifest {
            schema_version: TOOLCHAIN_MANIFEST_SCHEMA_VERSION,
            kind: ToolchainKind::Rust,
            spec_key,
            toolchain_id: toolchain_id.0,
            rust_manifest_date: Some(manifest.date.clone()),
            rust_version: Some(manifest.rustc.version.clone()),
            zig_version: None,
            components: vec![
                ToolchainComponentTree {
                    name: "rustc".to_string(),
                    target: Some(spec.host.clone()),
                    tree_root: rustc_root.0,
                    sha256: rustc_target.hash.clone(),
                    total_size_bytes: rustc_root.1,
                    file_count: rustc_tree.entries.len() as u32,
                    unique_chunks: rustc_unique_chunks,
                },
                ToolchainComponentTree {
                    name: "rust-std".to_string(),
                    target: Some(spec.target.clone()),
                    tree_root: rust_std_root.0,
                    sha256: rust_std_target.hash.clone(),
                    total_size_bytes: rust_std_root.1,
                    file_count: rust_std_tree.entries.len() as u32,
                    unique_chunks: std_unique_chunks,
                },
            ],
        };

        // Store manifest as CasNode::Toolchain
        let node = CasNode::Toolchain(toolchain_manifest);
        let (node_bytes, manifest_hash) = node.to_bytes_and_hash();
        if let Err(e) = self.storage.put_node(&manifest_hash, &node_bytes).await {
            return EnsureToolchainResult {
                spec_key: Some(spec_key),
                toolchain_id: Some(toolchain_id.0),
                manifest_hash: None,
                status: ToolchainEnsureStatus::Failed,
                error: Some(format!("failed to write toolchain manifest: {e}")),
            };
        }

        if let Err(e) = validate_directory_closure(self, rustc_root.0).await {
            return EnsureToolchainResult {
                spec_key: Some(spec_key),
                toolchain_id: Some(toolchain_id.0),
                manifest_hash: None,
                status: ToolchainEnsureStatus::Failed,
                error: Some(format!("toolchain closure validation failed (rustc): {e}")),
            };
        }
        if let Err(e) = validate_directory_closure(self, rust_std_root.0).await {
            return EnsureToolchainResult {
                spec_key: Some(spec_key),
                toolchain_id: Some(toolchain_id.0),
                manifest_hash: None,
                status: ToolchainEnsureStatus::Failed,
                error: Some(format!(
                    "toolchain closure validation failed (rust-std): {e}"
                )),
            };
        }

        // Publish spec → manifest_hash mapping and return canonical mapping winner.
        match self
            .storage
            .put_toolchain_spec(&spec_key, &manifest_hash.0)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                let winner_hash = match self.storage.get_toolchain_spec(&spec_key).await {
                    Ok(Some(hash)) => hash,
                    Ok(None) => {
                        return EnsureToolchainResult {
                            spec_key: Some(spec_key),
                            toolchain_id: Some(toolchain_id.0),
                            manifest_hash: None,
                            status: ToolchainEnsureStatus::Failed,
                            error: Some(format!(
                                "spec mapping for {spec_key} already exists but could not be read"
                            )),
                        };
                    }
                    Err(e) => {
                        return EnsureToolchainResult {
                            spec_key: Some(spec_key),
                            toolchain_id: Some(toolchain_id.0),
                            manifest_hash: None,
                            status: ToolchainEnsureStatus::Failed,
                            error: Some(format!(
                                "failed to read winner mapping for {spec_key}: {e}"
                            )),
                        };
                    }
                };
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: Some(toolchain_id.0),
                    manifest_hash: Some(NodeHash(winner_hash)),
                    status: ToolchainEnsureStatus::Hit,
                    error: None,
                };
            }
            Err(e) => {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: Some(toolchain_id.0),
                    manifest_hash: Some(manifest_hash),
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(format!(
                        "failed to publish spec mapping for {spec_key}: {e}"
                    )),
                };
            }
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

    /// Download one Rust toolchain component tarball and store it as chunks.
    #[tracing::instrument(skip(self, progress), fields(spec_key = tracing::field::Empty, component = ?component))]
    pub async fn download_rust_toolchain_component(
        &self,
        spec: RustToolchainSpec,
        component: RustComponent,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureRustToolchainComponentResult {
        if rust_component_name(&component).is_none() {
            return EnsureRustToolchainComponentResult {
                spec_key: None,
                component,
                tarball: None,
                tree_root: None,
                status: EnsureStatus::Failed,
                error: Some("unsupported component for Rust toolchain acquisition".to_string()),
            };
        }

        let spec_key = match spec.spec_key() {
            Ok(k) => {
                tracing::Span::current().record("spec_key", k.to_string());
                k
            }
            Err(e) => {
                return EnsureRustToolchainComponentResult {
                    spec_key: None,
                    component,
                    tarball: None,
                    tree_root: None,
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
                tarball: None,
                tree_root: None,
                status: EnsureStatus::Hit,
                error: None,
            };
        }

        let key = (spec_key, component.clone());
        if let Some(blob_ref) = self.rust_component_tarballs.lock().get(&key).cloned() {
            return EnsureRustToolchainComponentResult {
                spec_key: Some(spec_key),
                component,
                tarball: Some(blob_ref),
                tree_root: None,
                status: EnsureStatus::Hit,
                error: None,
            };
        }

        let this = self.clone();
        let spec_for_flight = spec.clone();
        let component_for_flight = component.clone();
        let progress = Arc::new(progress);
        singleflight_component(
            &self.rust_component_tarball_flights,
            key,
            "rust_component_tarball_flight",
            async move || {
                let this = this.clone();
                let spec_for_flight = spec_for_flight.clone();
                let component_for_flight = component_for_flight.clone();
                let progress = Arc::clone(&progress);
                this.download_rust_toolchain_component_impl(
                    spec_key,
                    spec_for_flight,
                    component_for_flight,
                    progress.as_ref(),
                )
                .await
            },
        )
        .await
    }

    async fn download_rust_toolchain_component_impl(
        &self,
        spec_key: ToolchainSpecKey,
        spec: RustToolchainSpec,
        component: RustComponent,
        progress: &roam::Tx<StoreProgress>,
    ) -> EnsureRustToolchainComponentResult {
        let component_name = match rust_component_name(&component) {
            Some(name) => name,
            None => {
                return EnsureRustToolchainComponentResult {
                    spec_key: Some(spec_key),
                    component,
                    tarball: None,
                    tree_root: None,
                    status: EnsureStatus::Failed,
                    error: Some("unsupported component for Rust toolchain acquisition".to_string()),
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
                return EnsureRustToolchainComponentResult {
                    spec_key: Some(spec_key),
                    component,
                    tarball: None,
                    tree_root: None,
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
                    tarball: None,
                    tree_root: None,
                    status: EnsureStatus::Failed,
                    error: Some(e.to_string()),
                };
            }
        };

        let tarball = match download_component(
            &target_manifest.url,
            &target_manifest.hash,
            component_name,
            progress,
        )
        .await
        {
            Ok(bytes) => bytes,
            Err(e) => {
                return EnsureRustToolchainComponentResult {
                    spec_key: Some(spec_key),
                    component,
                    tarball: None,
                    tree_root: None,
                    status: EnsureStatus::Failed,
                    error: Some(format!("failed to download {component_name}: {e}")),
                };
            }
        };

        let tarball_ref = match store_blob_chunked(self, &tarball).await {
            Ok(r) => r,
            Err(e) => {
                return EnsureRustToolchainComponentResult {
                    spec_key: Some(spec_key),
                    component,
                    tarball: None,
                    tree_root: None,
                    status: EnsureStatus::Failed,
                    error: Some(format!("failed to store tarball: {e}")),
                };
            }
        };

        self.rust_component_tarballs
            .lock()
            .insert((spec_key, component.clone()), tarball_ref.clone());

        EnsureRustToolchainComponentResult {
            spec_key: Some(spec_key),
            component,
            tarball: Some(tarball_ref),
            tree_root: None,
            status: EnsureStatus::Downloaded,
            error: None,
        }
    }

    /// Extract one Rust toolchain component tarball into a DirectoryNode tree.
    #[tracing::instrument(skip(self, progress), fields(spec_key = tracing::field::Empty, component = ?component))]
    pub async fn extract_rust_toolchain_component(
        &self,
        spec: RustToolchainSpec,
        component: RustComponent,
        tarball_ref: BlobRef,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureRustToolchainComponentResult {
        if rust_component_name(&component).is_none() {
            return EnsureRustToolchainComponentResult {
                spec_key: None,
                component,
                tarball: None,
                tree_root: None,
                status: EnsureStatus::Failed,
                error: Some("unsupported component for Rust toolchain extraction".to_string()),
            };
        }

        let spec_key = match spec.spec_key() {
            Ok(k) => {
                tracing::Span::current().record("spec_key", k.to_string());
                k
            }
            Err(e) => {
                return EnsureRustToolchainComponentResult {
                    spec_key: None,
                    component,
                    tarball: None,
                    tree_root: None,
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
                tarball: Some(tarball_ref),
                tree_root: None,
                status: EnsureStatus::Hit,
                error: None,
            };
        }

        let extracted_key = (spec_key, component.clone());
        if let Some(existing) = self
            .rust_component_trees
            .lock()
            .get(&extracted_key)
            .cloned()
        {
            return EnsureRustToolchainComponentResult {
                spec_key: Some(spec_key),
                component,
                tarball: Some(tarball_ref),
                tree_root: Some(existing.tree_root),
                status: EnsureStatus::Hit,
                error: None,
            };
        }

        let this = self.clone();
        let spec_for_flight = spec.clone();
        let component_for_flight = component.clone();
        let tarball_for_flight = tarball_ref.clone();
        let progress = Arc::new(progress);
        singleflight_component(
            &self.rust_component_tree_flights,
            extracted_key,
            "rust_component_tree_flight",
            async move || {
                let this = this.clone();
                let spec_for_flight = spec_for_flight.clone();
                let component_for_flight = component_for_flight.clone();
                let tarball_for_flight = tarball_for_flight.clone();
                let progress = Arc::clone(&progress);
                this.extract_rust_toolchain_component_impl(
                    spec_key,
                    spec_for_flight,
                    component_for_flight,
                    tarball_for_flight,
                    progress.as_ref(),
                )
                .await
            },
        )
        .await
    }

    async fn extract_rust_toolchain_component_impl(
        &self,
        spec_key: ToolchainSpecKey,
        spec: RustToolchainSpec,
        component: RustComponent,
        tarball_ref: BlobRef,
        progress: &roam::Tx<StoreProgress>,
    ) -> EnsureRustToolchainComponentResult {
        let component_name = match rust_component_name(&component) {
            Some(name) => name,
            None => {
                return EnsureRustToolchainComponentResult {
                    spec_key: Some(spec_key),
                    component,
                    tarball: Some(tarball_ref),
                    tree_root: None,
                    status: EnsureStatus::Failed,
                    error: Some("unsupported component for Rust toolchain extraction".to_string()),
                };
            }
        };

        // Read tarball from CAS
        let tarball = match read_blob_from_ref(self, &tarball_ref).await {
            Ok(bytes) => bytes,
            Err(e) => {
                return EnsureRustToolchainComponentResult {
                    spec_key: Some(spec_key),
                    component,
                    tarball: Some(tarball_ref),
                    tree_root: None,
                    status: EnsureStatus::Failed,
                    error: Some(format!("failed to read tarball: {e}")),
                };
            }
        };

        send_progress_best_effort(
            progress,
            StoreProgress {
                message: format!("extracting {component_name}"),
                current: None,
                total: None,
            },
        )
        .await;

        let strip_components = match component {
            RustComponent::Rustc => 2,
            RustComponent::RustStd => 2,
            RustComponent::Cargo => unreachable!("cargo is filtered above"),
        };

        // Track per-file info during extraction
        let file_info: Arc<moire::sync::SyncMutex<HashMap<Blake3Hash, ExtractedFileInfo>>> =
            Arc::new(moire::sync::SyncMutex::new(
                "store/file_info",
                HashMap::new(),
            ));
        let file_info_clone = file_info.clone();
        let this = self.clone();

        let tree = match vx_tarball::extract_to_tree(
            std::io::Cursor::new(tarball),
            Compression::Xz,
            strip_components,
            async move |data: Vec<u8>| {
                let content_hash = ContentHash(Blake3Hash::from_bytes(&data));
                let prepared = prepare_blob(&data);
                for ci in &prepared.chunks {
                    let chunk_data = &data[ci.offset..ci.offset + ci.size as usize];
                    this.storage
                        .put_chunk(&ci.hash, chunk_data)
                        .await
                        .map_err(|e| format!("failed to write chunk: {e}"))?;
                }
                let blob_node = if let Some(bn) = &prepared.blob_node {
                    this.storage
                        .put_node(&bn.node_hash, &bn.serialized_bytes)
                        .await
                        .map_err(|e| format!("failed to write BlobNode: {e}"))?;
                    Some(bn.node_hash)
                } else {
                    None
                };
                file_info_clone.lock().insert(
                    content_hash.0,
                    ExtractedFileInfo {
                        content_hash,
                        blob_node,
                        size: data.len() as u64,
                        executable: false,
                    },
                );
                Ok(content_hash.0)
            },
        )
        .await
        {
            Ok(tree) => tree,
            Err(e) => {
                return EnsureRustToolchainComponentResult {
                    spec_key: Some(spec_key),
                    component,
                    tarball: Some(tarball_ref),
                    tree_root: None,
                    status: EnsureStatus::Failed,
                    error: Some(format!("failed to extract {component_name}: {}", e)),
                };
            }
        };

        // Build DirectoryNode tree from the flat TreeManifest
        let (tree_root, total_size) = match tree_manifest_to_merkle(self, &tree, &file_info).await {
            Ok(r) => r,
            Err(e) => {
                return EnsureRustToolchainComponentResult {
                    spec_key: Some(spec_key),
                    component,
                    tarball: Some(tarball_ref),
                    tree_root: None,
                    status: EnsureStatus::Failed,
                    error: Some(format!("failed to build merkle tree: {e}")),
                };
            }
        };

        let target = match component {
            RustComponent::Rustc => spec.host.clone(),
            RustComponent::RustStd => spec.target.clone(),
            RustComponent::Cargo => unreachable!("cargo is filtered above"),
        };

        let unique_chunks = file_info.lock().len() as u32;
        let extracted_key = (spec_key, component.clone());

        self.rust_component_trees.lock().insert(
            extracted_key,
            ExtractedRustComponent {
                tree_root,
                target,
                total_size_bytes: total_size,
                file_count: tree.entries.len() as u32,
                unique_chunks,
            },
        );

        EnsureRustToolchainComponentResult {
            spec_key: Some(spec_key),
            component,
            tarball: Some(tarball_ref),
            tree_root: Some(tree_root),
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
                manifest_hash: Some(NodeHash(manifest_hash)),
                status: ToolchainEnsureStatus::Hit,
                error: None,
            };
        }

        let rustc_component = {
            let lock = self.rust_component_trees.lock();
            lock.get(&(spec_key, RustComponent::Rustc)).cloned()
        };
        let rust_std_component = {
            let lock = self.rust_component_trees.lock();
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
            rust_manifest_date: Some(manifest.date.clone()),
            rust_version: Some(manifest.rustc.version.clone()),
            zig_version: None,
            components: vec![
                ToolchainComponentTree {
                    name: "rustc".to_string(),
                    target: Some(rustc_component.target),
                    tree_root: rustc_component.tree_root,
                    sha256: rustc_target.hash.clone(),
                    total_size_bytes: rustc_component.total_size_bytes,
                    file_count: rustc_component.file_count,
                    unique_chunks: rustc_component.unique_chunks,
                },
                ToolchainComponentTree {
                    name: "rust-std".to_string(),
                    target: Some(rust_std_component.target),
                    tree_root: rust_std_component.tree_root,
                    sha256: rust_std_target.hash.clone(),
                    total_size_bytes: rust_std_component.total_size_bytes,
                    file_count: rust_std_component.file_count,
                    unique_chunks: rust_std_component.unique_chunks,
                },
            ],
        };

        let node = CasNode::Toolchain(toolchain_manifest);
        let (node_bytes, manifest_hash) = node.to_bytes_and_hash();
        if let Err(e) = self.storage.put_node(&manifest_hash, &node_bytes).await {
            return EnsureToolchainResult {
                spec_key: Some(spec_key),
                toolchain_id: Some(toolchain_id.0),
                manifest_hash: None,
                status: ToolchainEnsureStatus::Failed,
                error: Some(format!("failed to write toolchain manifest: {e}")),
            };
        }

        if let Err(e) = validate_directory_closure(self, rustc_component.tree_root).await {
            return EnsureToolchainResult {
                spec_key: Some(spec_key),
                toolchain_id: Some(toolchain_id.0),
                manifest_hash: None,
                status: ToolchainEnsureStatus::Failed,
                error: Some(format!("toolchain closure validation failed (rustc): {e}")),
            };
        }
        if let Err(e) = validate_directory_closure(self, rust_std_component.tree_root).await {
            return EnsureToolchainResult {
                spec_key: Some(spec_key),
                toolchain_id: Some(toolchain_id.0),
                manifest_hash: None,
                status: ToolchainEnsureStatus::Failed,
                error: Some(format!(
                    "toolchain closure validation failed (rust-std): {e}"
                )),
            };
        }

        match self
            .storage
            .put_toolchain_spec(&spec_key, &manifest_hash.0)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                let winner_hash = match self.storage.get_toolchain_spec(&spec_key).await {
                    Ok(Some(hash)) => hash,
                    Ok(None) => {
                        return EnsureToolchainResult {
                            spec_key: Some(spec_key),
                            toolchain_id: Some(toolchain_id.0),
                            manifest_hash: None,
                            status: ToolchainEnsureStatus::Failed,
                            error: Some(format!(
                                "spec mapping for {spec_key} already exists but could not be read"
                            )),
                        };
                    }
                    Err(e) => {
                        return EnsureToolchainResult {
                            spec_key: Some(spec_key),
                            toolchain_id: Some(toolchain_id.0),
                            manifest_hash: None,
                            status: ToolchainEnsureStatus::Failed,
                            error: Some(format!(
                                "failed to read winner mapping for {spec_key}: {e}"
                            )),
                        };
                    }
                };
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: Some(toolchain_id.0),
                    manifest_hash: Some(NodeHash(winner_hash)),
                    status: ToolchainEnsureStatus::Hit,
                    error: None,
                };
            }
            Err(e) => {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: Some(toolchain_id.0),
                    manifest_hash: Some(manifest_hash),
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(format!(
                        "failed to publish spec mapping for {spec_key}: {e}"
                    )),
                };
            }
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

        send_progress_best_effort(
            progress,
            StoreProgress {
                message: format!("downloading zig {}", spec.version),
                current: None,
                total: None,
            },
        )
        .await;

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

        send_progress_best_effort(
            progress,
            StoreProgress {
                message: "storing zig toolchain".to_string(),
                current: None,
                total: None,
            },
        )
        .await;

        // Store zig executable as a single-file DirectoryNode
        let zig_exe_ref = match store_blob_chunked(self, &acquired.zig_exe_contents).await {
            Ok(r) => r,
            Err(e) => {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(format!("failed to store zig executable: {e}")),
                };
            }
        };

        let zig_exe_entry = match &zig_exe_ref {
            BlobRef::Small { chunk, size } => DirEntry {
                name: "zig".to_string(),
                kind: DirEntryKind::SmallFile {
                    content_hash: ContentHash(chunk.0),
                    size: *size,
                    executable: true,
                },
            },
            BlobRef::Large {
                blob_node,
                content_hash,
                size,
            } => DirEntry {
                name: "zig".to_string(),
                kind: DirEntryKind::LargeFile {
                    content_hash: *content_hash,
                    blob_node: *blob_node,
                    size: *size,
                    executable: true,
                },
            },
        };

        let zig_exe_dir = CasNode::Directory(DirectoryNode {
            entries: vec![zig_exe_entry],
            total_size: acquired.zig_exe_contents.len() as u64,
        });
        let (zig_exe_bytes, zig_exe_hash) = zig_exe_dir.to_bytes_and_hash();
        if let Err(e) = self.storage.put_node(&zig_exe_hash, &zig_exe_bytes).await {
            return EnsureToolchainResult {
                spec_key: Some(spec_key),
                toolchain_id: None,
                manifest_hash: None,
                status: ToolchainEnsureStatus::Failed,
                error: Some(format!("failed to write zig exe DirectoryNode: {e}")),
            };
        }

        // Extract lib tarball — first pass: read all entries synchronously (tar
        // iterator is !Send), then store blobs asynchronously.
        let lib_raw_entries: Vec<(String, Vec<u8>)> = {
            let mut entries = Vec::new();
            let mut archive = tar::Archive::new(std::io::Cursor::new(&acquired.lib_tarball));
            let iter = match archive.entries() {
                Ok(iter) => iter,
                Err(e) => {
                    return EnsureToolchainResult {
                        spec_key: Some(spec_key),
                        toolchain_id: None,
                        manifest_hash: None,
                        status: ToolchainEnsureStatus::Failed,
                        error: Some(format!("failed to read zig lib tar entries: {e}")),
                    };
                }
            };
            for entry_result in iter {
                let mut entry = match entry_result {
                    Ok(entry) => entry,
                    Err(e) => {
                        return EnsureToolchainResult {
                            spec_key: Some(spec_key),
                            toolchain_id: None,
                            manifest_hash: None,
                            status: ToolchainEnsureStatus::Failed,
                            error: Some(format!("failed to read zig lib tar entry: {e}")),
                        };
                    }
                };
                let path = match entry.path() {
                    Ok(path) => path,
                    Err(e) => {
                        return EnsureToolchainResult {
                            spec_key: Some(spec_key),
                            toolchain_id: None,
                            manifest_hash: None,
                            status: ToolchainEnsureStatus::Failed,
                            error: Some(format!("failed to read zig lib entry path: {e}")),
                        };
                    }
                };
                let path_str = path.to_string_lossy().to_string();
                let full_path = format!("lib/{path_str}");

                let mut data = Vec::new();
                if let Err(e) = std::io::Read::read_to_end(&mut entry, &mut data) {
                    return EnsureToolchainResult {
                        spec_key: Some(spec_key),
                        toolchain_id: None,
                        manifest_hash: None,
                        status: ToolchainEnsureStatus::Failed,
                        error: Some(format!("failed to read zig lib entry contents: {e}")),
                    };
                }

                if !data.is_empty() {
                    entries.push((full_path, data));
                }
            }
            entries
        };

        let mut lib_files: Vec<(String, ExtractedFileInfo)> = Vec::new();
        for (full_path, data) in &lib_raw_entries {
            let blob_ref = match store_blob_chunked(self, data).await {
                Ok(r) => r,
                Err(e) => {
                    return EnsureToolchainResult {
                        spec_key: Some(spec_key),
                        toolchain_id: None,
                        manifest_hash: None,
                        status: ToolchainEnsureStatus::Failed,
                        error: Some(format!("failed to store zig lib blob for {full_path}: {e}")),
                    };
                }
            };

            let (content_hash, blob_node, size) = match &blob_ref {
                BlobRef::Small { chunk, size } => (ContentHash(chunk.0), None, *size),
                BlobRef::Large {
                    blob_node,
                    content_hash,
                    size,
                } => (*content_hash, Some(*blob_node), *size),
            };

            lib_files.push((
                full_path.clone(),
                ExtractedFileInfo {
                    content_hash,
                    blob_node,
                    size,
                    executable: false,
                },
            ));
        }

        let (lib_hash, lib_total_size) = match build_merkle_tree(self, &lib_files, &[]).await {
            Ok(r) => r,
            Err(e) => {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: None,
                    manifest_hash: None,
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(format!("failed to build zig lib merkle tree: {e}")),
                };
            }
        };

        if let Err(e) = acquired.cleanup().await {
            tracing::warn!("failed to cleanup Zig extraction directory: {}", e);
        }

        let toolchain_id = acquired.id.0;

        tracing::info!(
            zig_exe = %zig_exe_hash,
            lib = %lib_hash,
            toolchain_id = %toolchain_id,
            "stored Zig toolchain in CAS"
        );

        let toolchain_manifest = ToolchainManifest {
            schema_version: TOOLCHAIN_MANIFEST_SCHEMA_VERSION,
            kind: ToolchainKind::Zig,
            spec_key,
            toolchain_id,
            rust_manifest_date: None,
            rust_version: None,
            zig_version: Some(version.version.clone()),
            components: vec![
                ToolchainComponentTree {
                    name: "zig-exe".to_string(),
                    target: Some(platform.to_string()),
                    tree_root: zig_exe_hash,
                    sha256: String::new(),
                    total_size_bytes: acquired.zig_exe_contents.len() as u64,
                    file_count: 1,
                    unique_chunks: 1,
                },
                ToolchainComponentTree {
                    name: "zig-lib".to_string(),
                    target: Some(platform.to_string()),
                    tree_root: lib_hash,
                    sha256: String::new(),
                    total_size_bytes: lib_total_size,
                    file_count: lib_files.len() as u32,
                    unique_chunks: lib_files.len() as u32,
                },
            ],
        };

        let node = CasNode::Toolchain(toolchain_manifest);
        let (node_bytes, manifest_hash) = node.to_bytes_and_hash();
        if let Err(e) = self.storage.put_node(&manifest_hash, &node_bytes).await {
            return EnsureToolchainResult {
                spec_key: Some(spec_key),
                toolchain_id: Some(toolchain_id),
                manifest_hash: None,
                status: ToolchainEnsureStatus::Failed,
                error: Some(format!("failed to write toolchain manifest: {e}")),
            };
        }

        if let Err(e) = validate_directory_closure(self, zig_exe_hash).await {
            return EnsureToolchainResult {
                spec_key: Some(spec_key),
                toolchain_id: Some(toolchain_id),
                manifest_hash: None,
                status: ToolchainEnsureStatus::Failed,
                error: Some(format!(
                    "toolchain closure validation failed (zig-exe): {e}"
                )),
            };
        }
        if let Err(e) = validate_directory_closure(self, lib_hash).await {
            return EnsureToolchainResult {
                spec_key: Some(spec_key),
                toolchain_id: Some(toolchain_id),
                manifest_hash: None,
                status: ToolchainEnsureStatus::Failed,
                error: Some(format!(
                    "toolchain closure validation failed (zig-lib): {e}"
                )),
            };
        }

        match self
            .storage
            .put_toolchain_spec(&spec_key, &manifest_hash.0)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                let winner_hash = match self.storage.get_toolchain_spec(&spec_key).await {
                    Ok(Some(hash)) => hash,
                    Ok(None) => {
                        return EnsureToolchainResult {
                            spec_key: Some(spec_key),
                            toolchain_id: Some(toolchain_id),
                            manifest_hash: None,
                            status: ToolchainEnsureStatus::Failed,
                            error: Some(format!(
                                "spec mapping for {spec_key} already exists but could not be read"
                            )),
                        };
                    }
                    Err(e) => {
                        return EnsureToolchainResult {
                            spec_key: Some(spec_key),
                            toolchain_id: Some(toolchain_id),
                            manifest_hash: None,
                            status: ToolchainEnsureStatus::Failed,
                            error: Some(format!(
                                "failed to read winner mapping for {spec_key}: {e}"
                            )),
                        };
                    }
                };
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: Some(toolchain_id),
                    manifest_hash: Some(NodeHash(winner_hash)),
                    status: ToolchainEnsureStatus::Hit,
                    error: None,
                };
            }
            Err(e) => {
                return EnsureToolchainResult {
                    spec_key: Some(spec_key),
                    toolchain_id: Some(toolchain_id),
                    manifest_hash: Some(manifest_hash),
                    status: ToolchainEnsureStatus::Failed,
                    error: Some(format!(
                        "failed to publish spec mapping for {spec_key}: {e}"
                    )),
                };
            }
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

/// Convert a flat TreeManifest + per-file info into a DirectoryNode Merkle tree.
/// This is the bridge between the old tarball extraction API and the new CAS.
async fn tree_manifest_to_merkle(
    service: &StoreService,
    tree: &vx_store_proto::TreeManifest,
    file_info: &Arc<moire::sync::SyncMutex<HashMap<Blake3Hash, ExtractedFileInfo>>>,
) -> Result<(NodeHash, u64), String> {
    use vx_store_proto::TreeEntryKind;

    let (files, symlinks) = {
        let info = file_info.lock();

        let mut files: Vec<(String, ExtractedFileInfo)> = Vec::new();
        let mut symlinks: Vec<(String, String)> = Vec::new();

        for entry in &tree.entries {
            match &entry.kind {
                TreeEntryKind::File {
                    blob,
                    size,
                    executable,
                } => {
                    let extracted = info
                        .get(blob)
                        .ok_or_else(|| format!("file info not found for blob {}", blob.to_hex()))?;
                    files.push((
                        entry.path.clone(),
                        ExtractedFileInfo {
                            content_hash: extracted.content_hash,
                            blob_node: extracted.blob_node,
                            size: *size,
                            executable: *executable,
                        },
                    ));
                }
                TreeEntryKind::Symlink { target } => {
                    symlinks.push((entry.path.clone(), target.clone()));
                }
                TreeEntryKind::Directory => {
                    // Empty directories — these become empty DirectoryNode children
                    // if they have no files. The build_merkle_tree function handles
                    // this naturally by creating empty entry lists.
                }
            }
        }

        (files, symlinks)
    };

    build_merkle_tree(service, &files, &symlinks).await
}
