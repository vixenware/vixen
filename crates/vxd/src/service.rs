//! Minimal `VxdService` shell during the evaluator purge.
//!
//! The old build.vx/picante/runtime path has been removed on purpose. Keep only
//! the daemon RPC shell, store access needed by `vx clean --cache`, session
//! wiring, and small bootstrap-side helper types that `main.rs` still uses.

use crate::{BuildExecutorBackendKind, SpawnTracker};
use camino::Utf8PathBuf;
use moire::sync::SyncMutex;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use vx_io::boot::GracefulShutdown;
use vx_io::spawn::ServiceHandle;
use vx_runner_proto::RunnerClient;
use vx_store_proto::{ServiceVersion, StoreClient};
use vxd_proto::{
    BuildRequest, BuildResult, ProgressListenerClient, PsEntry, PsResult, UnpublishResult,
    VXD_PROTOCOL_VERSION, Vxd as VxdRpc,
};

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
    pub(crate) build_vx_executors: BuildVxExecutors,
    pub(crate) store_handle: Arc<ServiceHandle>,
    pub(crate) activity_registry: ActivityRegistry,
    pub(crate) build_vx_config_path: Option<Utf8PathBuf>,
}

type BuildExecutorFuture =
    Pin<Box<dyn Future<Output = std::result::Result<BuildVxExecutor, String>> + Send>>;
type LocalExecutorFactory =
    Arc<dyn Fn(Option<Arc<ProgressListenerClient>>) -> BuildExecutorFuture + Send + Sync>;
type SshBootstrapFactory = Arc<
    dyn Fn(String, String, Option<Arc<ProgressListenerClient>>) -> BuildExecutorFuture
        + Send
        + Sync,
>;

#[derive(Clone)]
pub(crate) struct BuildVxExecutor {
    pub(crate) id: String,
    pub(crate) backend: BuildExecutorBackendKind,
    pub(crate) runner: Arc<RunnerClient>,
    pub(crate) store: Option<Arc<StoreClient>>,
    pub(crate) store_supports_readthrough: bool,
    pub(crate) runner_handle: Option<Arc<ServiceHandle>>,
    pub(crate) store_handle: Option<Arc<ServiceHandle>>,
}

impl BuildVxExecutor {
    pub(crate) fn local(
        runner: Arc<RunnerClient>,
        runner_handle: Option<Arc<ServiceHandle>>,
    ) -> Self {
        Self {
            id: "local".to_string(),
            backend: BuildExecutorBackendKind::Local,
            runner,
            store: None,
            store_supports_readthrough: false,
            runner_handle,
            store_handle: None,
        }
    }

    pub(crate) fn daemon(
        id: String,
        runner: Arc<RunnerClient>,
        runner_handle: Option<Arc<ServiceHandle>>,
    ) -> Self {
        Self {
            id,
            backend: BuildExecutorBackendKind::Daemon,
            runner,
            store: None,
            store_supports_readthrough: false,
            runner_handle,
            store_handle: None,
        }
    }

    pub(crate) fn attach_store(&mut self, store: Arc<StoreClient>) {
        self.store = Some(store);
        self.store_supports_readthrough = true;
    }
}

#[derive(Clone)]
pub(crate) struct SshBootstrapExecutor {
    pub(crate) id: String,
    pub(crate) backend: BuildExecutorBackendKind,
    pub(crate) runner: Arc<RunnerClient>,
    pub(crate) store: Option<Arc<StoreClient>>,
    pub(crate) store_supports_readthrough: bool,
    pub(crate) runner_handle: Option<Arc<ServiceHandle>>,
    pub(crate) store_handle: Option<Arc<ServiceHandle>>,
}

impl From<SshBootstrapExecutor> for BuildVxExecutor {
    fn from(value: SshBootstrapExecutor) -> Self {
        Self {
            id: value.id,
            backend: value.backend,
            runner: value.runner,
            store: value.store,
            store_supports_readthrough: value.store_supports_readthrough,
            runner_handle: value.runner_handle,
            store_handle: value.store_handle,
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct BuildVxExecutors {
    local_factory: Option<LocalExecutorFactory>,
    ssh_bootstrap_factory: Option<SshBootstrapFactory>,
    class_aliases: BTreeMap<String, String>,
    class_executors: BTreeMap<String, BuildVxExecutor>,
    unavailable: BTreeMap<String, BuildExecutorUnavailable>,
}

#[derive(Clone)]
struct BuildExecutorUnavailable {
    id: String,
    backend: BuildExecutorBackendKind,
    reason: String,
}

impl BuildVxExecutors {
    pub(crate) fn local_lazy<F>(factory: F) -> Self
    where
        F: Fn(Option<Arc<ProgressListenerClient>>) -> BuildExecutorFuture + Send + Sync + 'static,
    {
        Self {
            local_factory: Some(Arc::new(factory)),
            ..Self::default()
        }
    }

    pub(crate) fn with_ssh_bootstrap_factory<F>(mut self, factory: F) -> Self
    where
        F: Fn(String, String, Option<Arc<ProgressListenerClient>>) -> BuildExecutorFuture
            + Send
            + Sync
            + 'static,
    {
        self.ssh_bootstrap_factory = Some(Arc::new(factory));
        self
    }

    pub(crate) fn register_class_alias(
        &mut self,
        class_label: String,
        target: &str,
    ) -> std::result::Result<(), String> {
        if class_label == target {
            return Err(format!(
                "build executor class {class_label:?} aliases itself"
            ));
        }
        self.class_aliases.insert(class_label, target.to_string());
        Ok(())
    }

    pub(crate) fn register_class_executor(
        &mut self,
        class_label: String,
        executor: BuildVxExecutor,
    ) {
        self.class_executors.insert(class_label, executor);
    }

    pub(crate) fn register_class_unavailable(
        &mut self,
        class_label: String,
        id: String,
        backend: BuildExecutorBackendKind,
        reason: String,
    ) {
        self.unavailable.insert(
            class_label,
            BuildExecutorUnavailable {
                id,
                backend,
                reason,
            },
        );
    }

    #[allow(dead_code)]
    pub(crate) fn local_factory(&self) -> Option<&LocalExecutorFactory> {
        self.local_factory.as_ref()
    }

    #[allow(dead_code)]
    pub(crate) fn ssh_bootstrap_factory(&self) -> Option<&SshBootstrapFactory> {
        self.ssh_bootstrap_factory.as_ref()
    }

    #[allow(dead_code)]
    pub(crate) fn class_executor(&self, class_label: &str) -> Option<&BuildVxExecutor> {
        self.class_executors.get(class_label)
    }

    #[allow(dead_code)]
    pub(crate) fn class_alias(&self, class_label: &str) -> Option<&str> {
        self.class_aliases.get(class_label).map(String::as_str)
    }

    #[allow(dead_code)]
    pub(crate) fn unavailable(&self, class_label: &str) -> Option<&str> {
        self.unavailable
            .get(class_label)
            .map(|entry| entry.reason.as_str())
    }
}

#[derive(Clone, Default)]
pub(crate) struct ActivityRegistry {
    next_id: Arc<AtomicU64>,
    entries: Arc<SyncMutex<BTreeMap<u64, Arc<SyncMutex<ActivityState>>>>>,
}

#[derive(Clone)]
pub(crate) struct ActivityHandle {
    inner: Arc<SyncMutex<ActivityState>>,
}

struct ActivityState {
    id: u64,
    kind: String,
    label: String,
    executor: Option<String>,
    started_at: Instant,
    last_update_at: Instant,
    last_message: Option<String>,
    remote_last_update_at: Option<Instant>,
    last_remote_message: Option<String>,
}

impl ActivityRegistry {
    pub(crate) fn new() -> Self {
        Self {
            next_id: Arc::new(AtomicU64::new(1)),
            entries: Arc::new(SyncMutex::new("vxd/activity_entries", BTreeMap::new())),
        }
    }

    pub(crate) fn start(
        &self,
        kind: impl Into<String>,
        label: impl Into<String>,
        executor: Option<String>,
    ) -> ActivityHandle {
        let now = Instant::now();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let state = Arc::new(SyncMutex::new(
            "vxd/activity_state",
            ActivityState {
                id,
                kind: kind.into(),
                label: label.into(),
                executor,
                started_at: now,
                last_update_at: now,
                last_message: None,
                remote_last_update_at: None,
                last_remote_message: None,
            },
        ));
        self.entries.lock().insert(id, Arc::clone(&state));
        ActivityHandle { inner: state }
    }

    pub(crate) fn snapshot(&self) -> Vec<PsEntry> {
        self.entries
            .lock()
            .values()
            .map(|entry| {
                let state = entry.lock();
                let age_ms = state.started_at.elapsed().as_millis() as u64;
                let idle_ms = state.last_update_at.elapsed().as_millis() as u64;
                let remote_idle_ms = state
                    .remote_last_update_at
                    .map(|instant| instant.elapsed().as_millis() as u64);
                PsEntry {
                    id: state.id,
                    kind: state.kind.clone(),
                    label: state.label.clone(),
                    executor: state.executor.clone(),
                    age_ms,
                    idle_ms,
                    last_message: state.last_message.clone(),
                    remote_idle_ms,
                    last_remote_message: state.last_remote_message.clone(),
                }
            })
            .collect()
    }
}

impl ActivityHandle {
    pub(crate) fn update(&self, message: impl Into<String>) {
        let mut state = self.inner.lock();
        state.last_update_at = Instant::now();
        state.last_message = Some(message.into());
    }

    pub(crate) fn update_remote(&self, message: impl Into<String>) {
        let mut state = self.inner.lock();
        state.remote_last_update_at = Some(Instant::now());
        state.last_remote_message = Some(message.into());
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
            build_vx_executors: _,
            store_handle,
            activity_registry,
            build_vx_config_path: _,
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
        cache_keys: Vec<vx_store_proto::CacheKey>,
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
