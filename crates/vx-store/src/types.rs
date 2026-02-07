use std::sync::Arc;

use camino::Utf8PathBuf;
use tokio::sync::Semaphore;

use crate::{RegistryManager, toolchain::ToolchainManager};

/// CAS service implementation
#[derive(Clone)]
pub(crate) struct StoreService {
    /// Inner state
    pub(crate) inner: Arc<StoreServiceInner>,
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

    /// VX home directory (typically .vx) - parent of root, used for toolchain spec mappings
    #[allow(dead_code)]
    pub(crate) vx_home: Utf8PathBuf,

    /// Toolchain acquisition manager (handles inflight deduplication)
    pub(crate) toolchain_manager: ToolchainManager,

    /// Registry crate acquisition manager (handles inflight deduplication)
    pub(crate) registry_manager: RegistryManager,

    /// Semaphore to limit concurrent download/extract operations
    pub(crate) download_semaphore: Arc<Semaphore>,
}
