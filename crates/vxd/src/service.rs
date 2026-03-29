//! Minimal `VxdService` shell during the evaluator purge.
//!
//! The old build.vx/picante/runtime path has been removed on purpose. Keep only
//! the daemon RPC shell, store access needed by `vx clean --cache`, session
//! wiring, and small bootstrap-side helper types that `main.rs` still uses.

use crate::SpawnTracker;
use camino::Utf8PathBuf;
use moire::sync::SyncMutex;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use vx_io::boot::GracefulShutdown;
use vx_io::spawn::ServiceHandle;
use vx_services::{
    BuildRequest, BuildResult, ProgressListenerClient, PsEntry, PsResult, UnpublishResult,
    VXD_PROTOCOL_VERSION, Vxd as VxdRpc,
};
use vx_services::{ServiceVersion, StoreClient};

#[derive(Clone)]
pub struct VxdService {
    store: Arc<StoreClient>,
    _store_handle: Arc<ServiceHandle>,
    _exec_host_triple: String,
    _vx_home: Utf8PathBuf,
    spawn_tracker: Arc<SyncMutex<SpawnTracker>>,
    activity_registry: ActivityRegistry,
    progress_listener: Arc<SyncMutex<Option<Arc<ProgressListenerClient>>>>,
    session_id: Arc<SyncMutex<String>>,
    active_build_cancel: Arc<SyncMutex<Option<CancellationToken>>>,
}

pub(crate) struct VxdServiceDeps {
    pub(crate) spawn_tracker: Arc<SyncMutex<SpawnTracker>>,
    pub(crate) store: Arc<StoreClient>,
    pub(crate) store_handle: Arc<ServiceHandle>,
    pub(crate) activity_registry: ActivityRegistry,
}

#[derive(Clone, Default)]
pub(crate) struct ActivityRegistry;

impl ActivityRegistry {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn snapshot(&self) -> Vec<PsEntry> {
        Vec::new()
    }
}

impl VxdService {
    pub(crate) async fn new(
        vx_home: Utf8PathBuf,
        exec_host_triple: String,
        deps: VxdServiceDeps,
    ) -> Self {
        let VxdServiceDeps {
            spawn_tracker,
            store,
            store_handle,
            activity_registry,
        } = deps;

        Self {
            store,
            _store_handle: store_handle,
            _exec_host_triple: exec_host_triple,
            _vx_home: vx_home,
            spawn_tracker,
            activity_registry,
            progress_listener: Arc::new(SyncMutex::new("vxd/progress_listener", None)),
            session_id: Arc::new(SyncMutex::new("vxd/session_id", String::new())),
            active_build_cancel: Arc::new(SyncMutex::new("vxd/active_build_cancel", None)),
        }
    }

    pub fn new_session(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
            _store_handle: Arc::clone(&self._store_handle),
            _exec_host_triple: self._exec_host_triple.clone(),
            _vx_home: self._vx_home.clone(),
            spawn_tracker: Arc::clone(&self.spawn_tracker),
            activity_registry: self.activity_registry.clone(),
            progress_listener: Arc::new(SyncMutex::new("vxd/progress_listener", None)),
            session_id: Arc::new(SyncMutex::new("vxd/session_id", String::new())),
            active_build_cancel: Arc::new(SyncMutex::new("vxd/active_build_cancel", None)),
        }
    }

    pub async fn set_progress_listener(&self, listener: Option<Arc<ProgressListenerClient>>) {
        *self.progress_listener.lock() = listener;
    }

    pub async fn cancel_active_build(&self) {
        if let Some(token) = self.active_build_cancel.lock().as_ref() {
            token.cancel();
        }
    }

    pub async fn kill_spawned_services(&self) {
        self.spawn_tracker.lock().kill_all();
    }

    fn unavailable_build_result(&self, request: &BuildRequest) -> BuildResult {
        let target = request
            .build_file_path
            .as_ref()
            .cloned()
            .unwrap_or_else(|| request.project_path.join("build.vx"));
        let message = format!(
            "legacy vxd build execution has been removed during the evaluator purge; {} is temporarily unavailable",
            target
        );
        BuildResult {
            success: false,
            message: "Build failed".to_string(),
            cached: false,
            duration_ms: 0,
            output_path: None,
            error: Some(message),
            failure: None,
            total_actions: 0,
            cache_hits: 0,
            rebuilt: 0,
            cancelled: 0,
        }
    }
}

impl GracefulShutdown for VxdService {
    async fn graceful_shutdown(&self) {}
}

impl VxdRpc for VxdService {
    async fn version(&self) -> ServiceVersion {
        ServiceVersion {
            service: "vxd".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: VXD_PROTOCOL_VERSION,
        }
    }

    async fn build(&self, request: BuildRequest) -> BuildResult {
        self.unavailable_build_result(&request)
    }

    async fn set_session(&self, session_id: String) {
        *self.session_id.lock() = session_id;
    }

    async fn unpublish_cache_keys(
        &self,
        cache_keys: Vec<vx_services::CacheKey>,
    ) -> UnpublishResult {
        let mut removed = 0u32;
        let mut not_found = 0u32;

        for key in cache_keys {
            if self.store.unpublish(key).await.unwrap_or(false) {
                removed += 1;
            } else {
                not_found += 1;
            }
        }

        UnpublishResult { removed, not_found }
    }

    async fn ps(&self) -> PsResult {
        PsResult {
            entries: self.activity_registry.snapshot(),
        }
    }
}
