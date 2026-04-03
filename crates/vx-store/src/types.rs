use std::sync::Arc;

use moire::sync::SyncMutex;
use vx_services::{ChunkHash, NodeHash, StoreClient};

/// CAS service implementation
#[derive(Clone)]
pub struct StoreService {
    /// Inner state
    pub(crate) inner: Arc<StoreServiceInner>,
    /// Optional trusted upstream store for read-through on misses.
    pub(crate) upstream: Arc<SyncMutex<Option<Arc<StoreClient>>>>,
}

impl std::ops::Deref for StoreService {
    type Target = StoreServiceInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// CAS service inner state
pub struct StoreServiceInner {
    /// Storage backend
    pub(crate) storage: crate::storage::CasStore,

    /// Positive CAS presence cache for chunks known to exist in this store.
    pub(crate) present_chunks: SyncMutex<std::collections::HashSet<ChunkHash>>,

    /// Positive CAS presence cache for nodes known to exist in this store.
    pub(crate) present_nodes: SyncMutex<std::collections::HashSet<NodeHash>>,
}

impl StoreService {
    pub(crate) fn set_upstream(&self, upstream: Arc<StoreClient>) {
        *self.upstream.lock() = Some(upstream);
    }

    pub(crate) fn upstream(&self) -> Option<Arc<StoreClient>> {
        self.upstream.lock().clone()
    }
}
