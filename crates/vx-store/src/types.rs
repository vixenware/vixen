use std::sync::Arc;

use moire::sync::SyncMutex;
use tokio::sync::Semaphore;
use vx_store_proto::{
    BlobRef, ChunkHash, EnsureRustToolchainComponentResult, NodeHash, RustComponent, StoreClient,
    ToolchainSpecKey,
};

use crate::{
    RegistryManager,
    toolchain::{ExtractedRustComponent, ToolchainManager},
};

type RustComponentFlightKey = (ToolchainSpecKey, RustComponent);
type RustComponentFlight = Arc<moire::sync::OnceCell<EnsureRustToolchainComponentResult>>;
type RustComponentFlightMap =
    std::collections::HashMap<RustComponentFlightKey, RustComponentFlight>;

/// CAS service implementation
#[derive(Clone)]
pub(crate) struct StoreService {
    /// Inner state
    pub(crate) inner: Arc<StoreServiceInner>,
    /// Optional trusted upstream store for read-through on misses.
    pub(crate) upstream: Arc<SyncMutex<Option<Arc<StoreClient>>>>,
}

impl vx_io::boot::GracefulShutdown for StoreService {
    async fn graceful_shutdown(&self) {}
}

impl std::ops::Deref for StoreService {
    type Target = StoreServiceInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// CAS service implementation
pub(crate) struct StoreServiceInner {
    /// Storage backend (filesystem, optionally layered with read-only base)
    pub(crate) storage: crate::storage::CasStore,

    /// Toolchain acquisition manager (handles inflight deduplication)
    pub(crate) toolchain_manager: ToolchainManager,

    /// Registry crate acquisition manager (handles inflight deduplication)
    pub(crate) registry_manager: RegistryManager,

    /// Semaphore to limit concurrent download/extract operations
    pub(crate) download_semaphore: Arc<Semaphore>,

    /// Downloaded Rust toolchain component tarballs by (spec_key, component)
    pub(crate) rust_component_tarballs:
        SyncMutex<std::collections::HashMap<(ToolchainSpecKey, RustComponent), BlobRef>>,

    /// In-flight Rust component tarball downloads by (spec_key, component)
    pub(crate) rust_component_tarball_flights: SyncMutex<RustComponentFlightMap>,

    /// Extracted Rust toolchain components by (spec_key, component)
    pub(crate) rust_component_trees: SyncMutex<
        std::collections::HashMap<(ToolchainSpecKey, RustComponent), ExtractedRustComponent>,
    >,

    /// In-flight Rust component extractions by (spec_key, component)
    pub(crate) rust_component_tree_flights: SyncMutex<RustComponentFlightMap>,

    /// Positive CAS presence cache for chunks known to exist in this store.
    pub(crate) present_chunks: SyncMutex<std::collections::HashSet<ChunkHash>>,

    /// Positive CAS presence cache for nodes known to exist in this store.
    pub(crate) present_nodes: SyncMutex<std::collections::HashSet<NodeHash>>,
}

impl StoreService {
    pub(crate) fn with_upstream_slot(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            upstream: Arc::new(SyncMutex::new("store/upstream", None)),
        }
    }

    pub(crate) fn set_upstream(&self, upstream: Arc<StoreClient>) {
        *self.upstream.lock() = Some(upstream);
    }

    pub(crate) fn upstream(&self) -> Option<Arc<StoreClient>> {
        self.upstream.lock().clone()
    }
}
