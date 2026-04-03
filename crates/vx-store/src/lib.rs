//! vx-store - Content-Addressed Storage service library.
//!
//! Call [`open`] to initialize a [`StoreService`] from a storage root directory.

pub(crate) mod service;
pub(crate) mod storage;
pub(crate) mod types;

use camino::Utf8PathBuf;
use eyre::Result;

use crate::storage::{CasStore, FsStorage};
use crate::types::StoreServiceInner;

pub use crate::types::StoreService;

/// Open (or create) a store rooted at `root`, initializing all required
/// subdirectories, and return a ready-to-use [`StoreService`].
pub async fn open(root: Utf8PathBuf) -> Result<StoreService> {
    let storage = CasStore::new(FsStorage::new(root));
    storage.init().await?;
    Ok(StoreService::new(storage))
}

impl StoreService {
    pub(crate) fn new(storage: CasStore) -> Self {
        use moire::sync::SyncMutex;
        use std::sync::Arc;

        Self {
            inner: Arc::new(StoreServiceInner {
                storage,
                present_chunks: SyncMutex::new(
                    "store/present_chunks",
                    std::collections::HashSet::new(),
                ),
                present_nodes: SyncMutex::new(
                    "store/present_nodes",
                    std::collections::HashSet::new(),
                ),
            }),
            upstream: Arc::new(SyncMutex::new("store/upstream", None)),
        }
    }
}
