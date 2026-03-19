//! VxdService implementation.
//!
//! Orchestrates builds by:
//! 1. Computing cache keys via picante
//! 2. Checking CAS for cache hits
//! 3. Sending compile requests to execd for cache misses
//! 4. Materializing outputs locally

use crate::SpawnTracker;
use crate::activity::{ActivityHandle, ActivityRegistry};
use crate::build_failure;
use crate::db::Database;
use crate::error::{Result, VxdError};
use ::moire::sync::SyncMutex;
use camino::{Utf8Path, Utf8PathBuf};
use facet::trace;
use facet_postcard::{from_slice, to_vec};
use facet_value::value;
use futures_util::stream;
use futures_util::{StreamExt, TryStreamExt};
use http_body_util::BodyExt;
use moire::task::FutureExt;
use picante::HasRuntime;
use picante::wal::WalWriter;
use pin_project_lite::pin_project;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::future::Future;
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Instant;
use tokio::io::{AsyncRead, ReadBuf};
use tokio::runtime::Handle;
use tokio::task::block_in_place;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use vx_eval::{
    AcquireRequest as EvalAcquireRequest, BlobValue, BoxFuture, BuildRuntime, DemandChain,
    DiscoverRequest as EvalDiscoverRequest, DiscoveredValue, Engine, EventSink, ExecArgValue,
    ExecMount, ExecMountMode, ExecPathValue, ExecRequest, ExecutorBackendKind, ExecutorSelector,
    ImportRequest as EvalImportRequest, MapValue, Module as EvalModule, PathValue, PoisonError,
    PoisonKind, RenderRequest as EvalRenderRequest, ShippedFunction,
    StabilizeRequest as EvalStabilizeRequest, StoreHydrationRequest as EvalStoreHydrationRequest,
    StructValue, TransportBlob, TransportBuiltinValue, TransportTree, TransportValue, TreeValue,
    Value, current_trace_track_id, lower_source_file_with_source_id,
};
use vx_io::spawn::ServiceHandle;
use vx_parser::SourceFile;
#[cfg(test)]
use vx_report::{GraphTopology, RulePlan, RulePlanNodeKind};
#[cfg(test)]
use vx_rs::CrateGraph;
use vx_runner_proto::{
    DiscoverRequest as RunnerDiscoverRequest, RunArg, RunEvalPayload, RunFailureKind, RunFileMount,
    RunMountMode, RunRequest, RunTreeMount, RunnerClient,
};
use vx_store_proto::chunking::{
    prepare_blob, put_node_if_missing, store_blob_stream, store_prepared_blob,
};
use vx_store_proto::{
    Blake3Hash, BlobRef, CasNode, ChunkHash, ContentHash, DirEntry, DirEntryKind, DirectoryNode,
    EnsureStatus, NodeHash, NodeId, NodeManifest, OutputBlobRef, OutputEntry, RemoteTracePhase,
    RemoteTraceSpan, RustChannel, RustComponent, RustToolchainSpec, ServiceVersion, StoreClient,
    StoreProgress, TreeEntryKind, TreeManifest,
};
use vxd_proto::{
    ActionType, BuildFailure, BuildRequest, BuildResult, ProgressListenerClient, PsResult,
    UnpublishResult, VXD_PROTOCOL_VERSION, Vxd as VxdRpc,
};

mod build_vx_render;
pub(crate) use build_vx_render::run_build_vx_render_via_store;

pin_project! {
    struct AcquireDigestVerifyingReader<R> {
        #[pin]
        inner: R,
        digest: AcquireDigest,
        sha256: Sha256,
        blake3: blake3::Hasher,
        verified: bool,
    }
}

impl<R: AsyncRead> AcquireDigestVerifyingReader<R> {
    fn new(inner: R, digest: AcquireDigest) -> Self {
        Self {
            inner,
            digest,
            sha256: Sha256::new(),
            blake3: blake3::Hasher::new(),
            verified: false,
        }
    }
}

impl<R: AsyncRead> AsyncRead for AcquireDigestVerifyingReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.project();
        let before = buf.filled().len();

        match this.inner.poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let after = buf.filled().len();
                let new_data = &buf.filled()[before..after];
                this.sha256.update(new_data);
                this.blake3.update(new_data);

                if new_data.is_empty() && !*this.verified {
                    let verified = match this.digest {
                        AcquireDigest::Sha256(expected) => {
                            let actual = this.sha256.finalize_reset();
                            if actual.as_slice() == expected.as_slice() {
                                Ok(())
                            } else {
                                Err(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    format!(
                                        "acquire digest mismatch: expected sha256:{}, got sha256:{}",
                                        hex::encode(expected),
                                        hex::encode(actual),
                                    ),
                                ))
                            }
                        }
                        AcquireDigest::Blake3(expected) => {
                            let actual = *this.blake3.finalize().as_bytes();
                            if actual == *expected {
                                Ok(())
                            } else {
                                Err(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    format!(
                                        "acquire digest mismatch: expected blake3:{}, got blake3:{}",
                                        hex::encode(expected),
                                        hex::encode(actual),
                                    ),
                                ))
                            }
                        }
                    };

                    if let Err(error) = verified {
                        return Poll::Ready(Err(error));
                    }
                    *this.verified = true;
                }

                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Toolchain information (manifest-only, no materialization)
pub struct ToolchainInfo {
    /// Hash of the ToolchainManifest node in CAS
    pub manifest_hash: NodeHash,
    /// Content-derived toolchain ID (from manifest)
    pub toolchain_id: Blake3Hash,
    /// Rustc version string (for reports)
    pub version: Option<String>,
    /// Manifest date (for reports)
    pub manifest_date: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct PreparedExecMounts {
    tree_mounts: Vec<RunTreeMount>,
    file_mounts: Vec<RunFileMount>,
}

type ExecMountPrepResult = std::result::Result<PreparedExecMounts, String>;
type ExecMountPrepFuture = Arc<moire::sync::OnceCell<ExecMountPrepResult>>;
type BuildVxFsTreeSnapshotResult = std::result::Result<(NodeHash, u64), String>;
type BuildVxFsTreeSnapshotFuture = Arc<moire::sync::OnceCell<BuildVxFsTreeSnapshotResult>>;
type BuildVxExecutorEnsureResult = std::result::Result<(), String>;
type BuildVxExecutorEnsureFuture = Arc<moire::sync::OnceCell<BuildVxExecutorEnsureResult>>;
type SnapshotTreeFuture<'a> =
    Pin<Box<dyn Future<Output = std::result::Result<(NodeHash, u64), String>> + Send + 'a>>;
type SnapshotTreeEntryFuture<'a> =
    Pin<Box<dyn Future<Output = std::result::Result<(DirEntryKind, u64), String>> + Send + 'a>>;

pub(crate) const BUILD_VX_CONFIG_IMPORT_PATH: &str = "vixen:config";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostRelation {
    SameHost,
    CrossHost,
    Unknown,
}

impl HostRelation {
    fn as_str(self) -> &'static str {
        match self {
            Self::SameHost => "same_host",
            Self::CrossHost => "cross_host",
            Self::Unknown => "unknown",
        }
    }
}

/// Acquired toolchains (manifest references only)
pub struct AcquiredToolchains {
    /// Rust toolchain (if acquired)
    pub rust: Option<ToolchainInfo>,
}

#[derive(Clone)]
pub(crate) struct BuildVxExecutor {
    id: String,
    backend: ExecutorBackendKind,
    host_relation: HostRelation,
    runner: Option<Arc<RunnerClient>>,
    store: Option<Arc<StoreClient>>,
    store_supports_readthrough: bool,
    unavailable_reason: Option<String>,
    _runner_handle: Option<Arc<ServiceHandle>>,
    _store_handle: Option<Arc<ServiceHandle>>,
}

impl BuildVxExecutor {
    pub(crate) fn local(
        runner: Arc<RunnerClient>,
        runner_handle: Option<Arc<ServiceHandle>>,
    ) -> Self {
        Self {
            id: "local".to_string(),
            backend: ExecutorBackendKind::Local,
            host_relation: HostRelation::SameHost,
            runner: Some(runner),
            store: None,
            store_supports_readthrough: false,
            unavailable_reason: None,
            _runner_handle: runner_handle,
            _store_handle: None,
        }
    }

    pub(crate) fn daemon(
        id: impl Into<String>,
        runner: Arc<RunnerClient>,
        runner_handle: Option<Arc<ServiceHandle>>,
    ) -> Self {
        Self {
            id: id.into(),
            backend: ExecutorBackendKind::Daemon,
            host_relation: HostRelation::Unknown,
            runner: Some(runner),
            store: None,
            store_supports_readthrough: false,
            unavailable_reason: None,
            _runner_handle: runner_handle,
            _store_handle: None,
        }
    }

    pub(crate) fn attach_store(&mut self, store: Arc<StoreClient>) {
        self.store = Some(store);
    }
}

#[derive(Clone)]
pub(crate) struct BuildVxExecutors {
    state: Arc<std::sync::Mutex<BuildVxExecutorsState>>,
    ssh_bootstrap_factory: Option<SshBootstrapFactory>,
    local_factory: Option<LocalExecutorFactory>,
    ensure_inflight: Arc<SyncMutex<HashMap<String, BuildVxExecutorEnsureFuture>>>,
}

struct BuildVxExecutorsState {
    executors: HashMap<String, BuildVxExecutor>,
    class_aliases: HashMap<String, String>,
}

pub(crate) struct SshBootstrapExecutor {
    pub(crate) id: String,
    pub(crate) backend: ExecutorBackendKind,
    pub(crate) runner: Arc<RunnerClient>,
    pub(crate) store: Option<Arc<StoreClient>>,
    pub(crate) store_supports_readthrough: bool,
    pub(crate) runner_handle: Option<Arc<ServiceHandle>>,
    pub(crate) store_handle: Option<Arc<ServiceHandle>>,
}

#[derive(Clone)]
struct ResolvedBuildVxExecutor {
    runner: Arc<RunnerClient>,
    store: Option<Arc<StoreClient>>,
    store_supports_readthrough: bool,
    host_relation: HostRelation,
}

impl SshBootstrapExecutor {
    fn into_build_vx_executor(self) -> BuildVxExecutor {
        BuildVxExecutor {
            id: self.id,
            backend: self.backend,
            host_relation: HostRelation::CrossHost,
            runner: Some(self.runner),
            store: self.store,
            store_supports_readthrough: self.store_supports_readthrough,
            unavailable_reason: None,
            _runner_handle: self.runner_handle,
            _store_handle: self.store_handle,
        }
    }
}

impl From<SshBootstrapExecutor> for BuildVxExecutor {
    fn from(value: SshBootstrapExecutor) -> Self {
        value.into_build_vx_executor()
    }
}

type SshBootstrapFactory = Arc<
    dyn Fn(
            String,
            String,
            Option<Arc<ProgressListenerClient>>,
        ) -> Pin<
            Box<dyn Future<Output = std::result::Result<SshBootstrapExecutor, String>> + Send>,
        > + Send
        + Sync,
>;

type LocalExecutorFactory = Arc<
    dyn Fn(
            Option<Arc<ProgressListenerClient>>,
        )
            -> Pin<Box<dyn Future<Output = std::result::Result<BuildVxExecutor, String>> + Send>>
        + Send
        + Sync,
>;

fn implicit_ssh_class_target(class_label: &str) -> Option<&str> {
    class_label
        .strip_prefix("ssh:")
        .filter(|target| !target.is_empty())
}

impl BuildVxExecutors {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn local_default(store: Arc<StoreClient>, runner: Arc<RunnerClient>) -> Self {
        let executor_id = "local".to_string();
        let executor = BuildVxExecutor {
            id: executor_id.clone(),
            backend: ExecutorBackendKind::Local,
            host_relation: HostRelation::SameHost,
            runner: Some(runner),
            store: Some(store),
            store_supports_readthrough: false,
            unavailable_reason: None,
            _runner_handle: None,
            _store_handle: None,
        };
        Self {
            state: Arc::new(std::sync::Mutex::new(BuildVxExecutorsState {
                executors: HashMap::from([(executor_id.clone(), executor)]),
                class_aliases: HashMap::from([("default".to_string(), executor_id.clone())]),
            })),
            ssh_bootstrap_factory: None,
            local_factory: None,
            ensure_inflight: Arc::new(SyncMutex::new(
                "vxd/build_vx_executor_ensure_inflight",
                HashMap::new(),
            )),
        }
    }

    pub(crate) fn local_lazy(
        factory: impl Fn(
            Option<Arc<ProgressListenerClient>>,
        ) -> Pin<
            Box<dyn Future<Output = std::result::Result<BuildVxExecutor, String>> + Send>,
        > + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            state: Arc::new(std::sync::Mutex::new(BuildVxExecutorsState {
                executors: HashMap::new(),
                class_aliases: HashMap::from([("default".to_string(), "local".to_string())]),
            })),
            ssh_bootstrap_factory: None,
            local_factory: Some(Arc::new(factory)),
            ensure_inflight: Arc::new(SyncMutex::new(
                "vxd/build_vx_executor_ensure_inflight",
                HashMap::new(),
            )),
        }
    }

    pub(crate) fn with_ssh_bootstrap_factory(
        mut self,
        factory: impl Fn(
            String,
            String,
            Option<Arc<ProgressListenerClient>>,
        ) -> Pin<
            Box<dyn Future<Output = std::result::Result<SshBootstrapExecutor, String>> + Send>,
        > + Send
        + Sync
        + 'static,
    ) -> Self {
        self.ssh_bootstrap_factory = Some(Arc::new(factory));
        self
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn register_class_alias(
        &mut self,
        class_label: impl Into<String>,
        executor_id: &str,
    ) -> std::result::Result<(), String> {
        let mut state = self.state.lock().expect("build_vx_executors poisoned");
        if !(state.executors.contains_key(executor_id)
            || executor_id == "local" && self.local_factory.is_some())
        {
            return Err(format!(
                "cannot alias build.vx executor class to unknown executor {executor_id}"
            ));
        }
        state
            .class_aliases
            .insert(class_label.into(), executor_id.to_string());
        Ok(())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn register_class_runner(
        &mut self,
        class_label: impl Into<String>,
        executor_id: impl Into<String>,
        backend: ExecutorBackendKind,
        runner: Arc<RunnerClient>,
    ) {
        self.register_class_runner_with_store(class_label, executor_id, backend, runner, None);
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn register_class_runner_with_store(
        &mut self,
        class_label: impl Into<String>,
        executor_id: impl Into<String>,
        backend: ExecutorBackendKind,
        runner: Arc<RunnerClient>,
        store: Option<Arc<StoreClient>>,
    ) {
        self.register_class_executor(
            class_label,
            BuildVxExecutor {
                id: executor_id.into(),
                backend,
                host_relation: HostRelation::Unknown,
                runner: Some(runner),
                store,
                store_supports_readthrough: false,
                unavailable_reason: None,
                _runner_handle: None,
                _store_handle: None,
            },
        );
    }

    pub(crate) fn register_class_executor(
        &mut self,
        class_label: impl Into<String>,
        executor: BuildVxExecutor,
    ) {
        let executor_id = executor.id.clone();
        let mut state = self.state.lock().expect("build_vx_executors poisoned");
        state.executors.insert(executor_id.clone(), executor);
        state.class_aliases.insert(class_label.into(), executor_id);
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn register_class_unavailable(
        &mut self,
        class_label: impl Into<String>,
        executor_id: impl Into<String>,
        backend: ExecutorBackendKind,
        unavailable_reason: impl Into<String>,
    ) {
        let executor_id = executor_id.into();
        let mut state = self.state.lock().expect("build_vx_executors poisoned");
        state.executors.insert(
            executor_id.clone(),
            BuildVxExecutor {
                id: executor_id.clone(),
                backend,
                host_relation: HostRelation::Unknown,
                runner: None,
                store: None,
                store_supports_readthrough: false,
                unavailable_reason: Some(unavailable_reason.into()),
                _runner_handle: None,
                _store_handle: None,
            },
        );
        state.class_aliases.insert(class_label.into(), executor_id);
    }

    fn resolve_executor_locked<'a>(
        &self,
        state: &'a BuildVxExecutorsState,
        selector: &ExecutorSelector,
    ) -> std::result::Result<&'a BuildVxExecutor, String> {
        match selector.backend {
            ExecutorBackendKind::Class => state
                .class_aliases
                .get(&selector.id)
                .and_then(|executor_id| state.executors.get(executor_id))
                .ok_or_else(|| {
                    format!(
                        "build.vx executor {}:{} is not configured",
                        selector.backend.as_tag(),
                        selector.id
                    )
                }),
            _ => state
                .executors
                .get(&selector.id)
                .filter(|executor| executor.backend == selector.backend)
                .ok_or_else(|| {
                    format!(
                        "build.vx executor {}:{} is not configured",
                        selector.backend.as_tag(),
                        selector.id
                    )
                }),
        }
    }

    async fn ensure_executor(
        &self,
        selector: &ExecutorSelector,
        progress_listener: Option<Arc<ProgressListenerClient>>,
    ) -> std::result::Result<(), String> {
        #[derive(Clone)]
        enum PendingExecutorBootstrap {
            Local,
            ImplicitSsh { class_label: String, target: String },
        }

        {
            let state = self.state.lock().expect("build_vx_executors poisoned");
            if self.resolve_executor_locked(&state, selector).is_ok() {
                return Ok(());
            }
        }

        let is_pending_local_class = if selector.backend == ExecutorBackendKind::Class {
            let state = self.state.lock().expect("build_vx_executors poisoned");
            state
                .class_aliases
                .get(&selector.id)
                .is_some_and(|executor_id| executor_id == "local")
        } else {
            false
        };

        let pending = if (selector.backend == ExecutorBackendKind::Local && selector.id == "local")
            || is_pending_local_class
        {
            if self.local_factory.is_none() {
                return Err(format!(
                    "build.vx executor {}:{} is not configured",
                    selector.backend.as_tag(),
                    selector.id
                ));
            }
            PendingExecutorBootstrap::Local
        } else {
            if selector.backend != ExecutorBackendKind::Class {
                return Err(format!(
                    "build.vx executor {}:{} is not configured",
                    selector.backend.as_tag(),
                    selector.id
                ));
            }

            let Some(target) = implicit_ssh_class_target(&selector.id) else {
                return Err(format!(
                    "build.vx executor {}:{} is not configured",
                    selector.backend.as_tag(),
                    selector.id
                ));
            };
            if self.ssh_bootstrap_factory.is_none() {
                return Err(format!(
                    "build.vx executor class:{} is not available on this host",
                    selector.id
                ));
            }
            PendingExecutorBootstrap::ImplicitSsh {
                class_label: selector.id.clone(),
                target: target.to_string(),
            }
        };

        let key = match &pending {
            PendingExecutorBootstrap::Local => "local".to_string(),
            PendingExecutorBootstrap::ImplicitSsh { class_label, .. } => {
                format!("class:{class_label}")
            }
        };

        let (cell, inserted) = {
            let mut inflight = self.ensure_inflight.lock();
            match inflight.entry(key.clone()) {
                std::collections::hash_map::Entry::Occupied(entry) => (entry.get().clone(), false),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let cell = Arc::new(moire::sync::OnceCell::new("build_vx_executor_ensure"));
                    entry.insert(cell.clone());
                    (cell, true)
                }
            }
        };

        let state = Arc::clone(&self.state);
        let local_factory = self.local_factory.clone();
        let ssh_bootstrap_factory = self.ssh_bootstrap_factory.clone();
        let selector = selector.clone();
        let result = cell
            .get_or_init(|| async move {
                match pending {
                    PendingExecutorBootstrap::Local => {
                        let factory = local_factory.ok_or_else(|| {
                            format!(
                                "build.vx executor {}:{} is not configured",
                                selector.backend.as_tag(),
                                selector.id
                            )
                        })?;
                        let synthesized = factory(progress_listener).await?;
                        let mut state = state.lock().expect("build_vx_executors poisoned");
                        state
                            .executors
                            .entry("local".to_string())
                            .or_insert_with(|| synthesized);
                        Ok(())
                    }
                    PendingExecutorBootstrap::ImplicitSsh {
                        class_label,
                        target,
                    } => {
                        let factory = ssh_bootstrap_factory.ok_or_else(|| {
                            format!(
                                "build.vx executor class:{} is not available on this host",
                                class_label
                            )
                        })?;
                        let synthesized =
                            factory(class_label.clone(), target, progress_listener).await?;
                        let mut state = state.lock().expect("build_vx_executors poisoned");
                        if state
                            .class_aliases
                            .get(&class_label)
                            .and_then(|executor_id| state.executors.get(executor_id))
                            .is_none()
                        {
                            let executor_id = synthesized.id.clone();
                            state
                                .executors
                                .insert(executor_id.clone(), synthesized.into_build_vx_executor());
                            state.class_aliases.insert(class_label, executor_id);
                        }
                        Ok(())
                    }
                }
            })
            .await
            .clone();

        if result.is_err() || inserted {
            let mut inflight = self.ensure_inflight.lock();
            if result.is_err()
                || inflight
                    .get(&key)
                    .is_some_and(|existing| Arc::ptr_eq(existing, &cell))
            {
                inflight.remove(&key);
            }
        }

        result
    }

    pub(crate) async fn runner_for(
        &self,
        selector: &ExecutorSelector,
        progress_listener: Option<Arc<ProgressListenerClient>>,
    ) -> std::result::Result<Arc<RunnerClient>, String> {
        self.ensure_executor(selector, progress_listener).await?;
        let state = self.state.lock().expect("build_vx_executors poisoned");
        let executor = self.resolve_executor_locked(&state, selector)?;
        if let Some(runner) = &executor.runner {
            return Ok(Arc::clone(runner));
        }
        Err(executor.unavailable_reason.clone().unwrap_or_else(|| {
            format!(
                "build.vx executor {} is configured without a runner connection",
                executor.id
            )
        }))
    }

    pub(crate) async fn store_for(
        &self,
        selector: &ExecutorSelector,
        progress_listener: Option<Arc<ProgressListenerClient>>,
    ) -> std::result::Result<Option<Arc<StoreClient>>, String> {
        self.ensure_executor(selector, progress_listener).await?;
        let state = self.state.lock().expect("build_vx_executors poisoned");
        let executor = self.resolve_executor_locked(&state, selector)?;
        Ok(executor.store.clone())
    }

    pub(crate) async fn store_supports_readthrough(
        &self,
        selector: &ExecutorSelector,
        progress_listener: Option<Arc<ProgressListenerClient>>,
    ) -> std::result::Result<bool, String> {
        self.ensure_executor(selector, progress_listener).await?;
        let state = self.state.lock().expect("build_vx_executors poisoned");
        let executor = self.resolve_executor_locked(&state, selector)?;
        Ok(executor.store_supports_readthrough)
    }

    async fn resolved_executor_for(
        &self,
        selector: &ExecutorSelector,
        progress_listener: Option<Arc<ProgressListenerClient>>,
    ) -> std::result::Result<ResolvedBuildVxExecutor, String> {
        self.ensure_executor(selector, progress_listener).await?;
        let state = self.state.lock().expect("build_vx_executors poisoned");
        let executor = self.resolve_executor_locked(&state, selector)?;
        let Some(runner) = &executor.runner else {
            return Err(executor.unavailable_reason.clone().unwrap_or_else(|| {
                format!(
                    "build.vx executor {} is configured without a runner connection",
                    executor.id
                )
            }));
        };
        Ok(ResolvedBuildVxExecutor {
            runner: Arc::clone(runner),
            store: executor.store.clone(),
            store_supports_readthrough: executor.store_supports_readthrough,
            host_relation: executor.host_relation,
        })
    }
}

async fn mirror_chunk(
    source: &StoreClient,
    dest: &StoreClient,
    hash: ChunkHash,
) -> std::result::Result<(), String> {
    if dest
        .has_chunk(hash)
        .await
        .map_err(|error| format!("{error:?}"))?
    {
        return Ok(());
    }
    let data = source
        .get_chunk(hash)
        .await
        .map_err(|error| format!("{error:?}"))?
        .ok_or_else(|| format!("chunk {} not found in source store", hash.to_hex()))?;
    dest.put_chunk(hash, data)
        .await
        .map_err(|error| format!("{error:?}"))?;
    Ok(())
}

async fn mirror_blob_ref(
    source: &StoreClient,
    dest: &StoreClient,
    blob_ref: &BlobRef,
) -> std::result::Result<(), String> {
    match blob_ref {
        BlobRef::Small { chunk, .. } => mirror_chunk(source, dest, *chunk).await,
        BlobRef::Large { blob_node, .. } => mirror_node(source, dest, *blob_node).await,
    }
}

async fn mirror_output_blob_ref(
    source: &StoreClient,
    dest: &StoreClient,
    blob_ref: &OutputBlobRef,
) -> std::result::Result<(), String> {
    match blob_ref {
        OutputBlobRef::Small { chunk, .. } => mirror_chunk(source, dest, *chunk).await,
        OutputBlobRef::Large { blob_node, .. } => mirror_node(source, dest, *blob_node).await,
    }
}

fn mirror_directory_entry_kind<'a>(
    source: &'a StoreClient,
    dest: &'a StoreClient,
    kind: &'a DirEntryKind,
) -> Pin<Box<dyn Future<Output = std::result::Result<(), String>> + Send + 'a>> {
    Box::pin(async move {
        match kind {
            DirEntryKind::SmallFile { content_hash, .. } => {
                mirror_chunk(source, dest, ChunkHash(content_hash.0)).await
            }
            DirEntryKind::LargeFile { blob_node, .. } => {
                mirror_node(source, dest, *blob_node).await
            }
            DirEntryKind::Dir { hash, .. } => mirror_node(source, dest, *hash).await,
            DirEntryKind::Symlink { .. } => Ok(()),
        }
    })
}

fn mirror_node<'a>(
    source: &'a StoreClient,
    dest: &'a StoreClient,
    hash: NodeHash,
) -> Pin<Box<dyn Future<Output = std::result::Result<(), String>> + Send + 'a>> {
    Box::pin(async move {
        if dest
            .has_node(hash)
            .await
            .map_err(|error| format!("{error:?}"))?
        {
            return Ok(());
        }
        let bytes = source
            .get_node(hash)
            .await
            .map_err(|error| format!("{error:?}"))?
            .ok_or_else(|| format!("node {} not found in source store", hash.to_hex()))?;
        let node = CasNode::from_bytes(&bytes).map_err(|error| error.to_string())?;

        match &node {
            CasNode::Blob(blob) => {
                for chunk in &blob.chunks {
                    mirror_chunk(source, dest, chunk.hash).await?;
                }
            }
            CasNode::Directory(directory) => {
                for entry in &directory.entries {
                    mirror_directory_entry_kind(source, dest, &entry.kind).await?;
                }
            }
            CasNode::Manifest(manifest) => {
                for output in &manifest.outputs {
                    mirror_output_blob_ref(source, dest, &output.blob).await?;
                }
            }
            _ => {}
        }

        dest.put_node(hash, bytes)
            .await
            .map_err(|error| format!("{error:?}"))?;
        Ok(())
    })
}

fn mirror_transport_value_store_refs<'a>(
    source: &'a StoreClient,
    dest: &'a StoreClient,
    value: TransportValue,
) -> Pin<Box<dyn Future<Output = std::result::Result<TransportValue, String>> + Send + 'a>> {
    Box::pin(async move {
        match value {
            TransportValue::Blob(blob) => match blob {
                TransportBlob::Inline { .. } => Ok(TransportValue::Blob(blob)),
                TransportBlob::Store {
                    blob_ref,
                    executable,
                } => {
                    mirror_blob_ref(source, dest, &blob_ref).await?;
                    Ok(TransportValue::Blob(TransportBlob::Store {
                        blob_ref,
                        executable,
                    }))
                }
            },
            TransportValue::Tree(tree) => match tree {
                TransportTree::Inline(entries) => {
                    let mut mirrored = Vec::with_capacity(entries.len());
                    for (name, value) in entries {
                        mirrored.push((
                            name,
                            mirror_transport_value_store_refs(source, dest, value).await?,
                        ));
                    }
                    Ok(TransportValue::Tree(TransportTree::Inline(mirrored)))
                }
                TransportTree::Store(root) => {
                    mirror_node(source, dest, root).await?;
                    Ok(TransportValue::Tree(TransportTree::Store(root)))
                }
            },
            TransportValue::Discovered { value, obligations } => {
                let value = mirror_transport_value_store_refs(source, dest, *value).await?;
                Ok(TransportValue::Discovered {
                    value: Box::new(value),
                    obligations,
                })
            }
            TransportValue::Tuple(values) => {
                let mut mirrored = Vec::with_capacity(values.len());
                for value in values {
                    mirrored.push(mirror_transport_value_store_refs(source, dest, value).await?);
                }
                Ok(TransportValue::Tuple(mirrored))
            }
            TransportValue::Array(values) => {
                let mut mirrored = Vec::with_capacity(values.len());
                for value in values {
                    mirrored.push(mirror_transport_value_store_refs(source, dest, value).await?);
                }
                Ok(TransportValue::Array(mirrored))
            }
            TransportValue::Map(entries) => {
                let mut mirrored = Vec::with_capacity(entries.len());
                for (key, value) in entries {
                    mirrored.push((
                        mirror_transport_value_store_refs(source, dest, key).await?,
                        mirror_transport_value_store_refs(source, dest, value).await?,
                    ));
                }
                Ok(TransportValue::Map(mirrored))
            }
            TransportValue::Struct(struct_value) => {
                let mut fields = Vec::with_capacity(struct_value.fields.len());
                for (name, value) in struct_value.fields {
                    fields.push((
                        name,
                        mirror_transport_value_store_refs(source, dest, value).await?,
                    ));
                }
                Ok(TransportValue::Struct(vx_eval::TransportStruct {
                    type_name: struct_value.type_name,
                    fields,
                }))
            }
            TransportValue::Function(function) => Ok(TransportValue::Function(
                mirror_shipped_function_store_refs(source, dest, function).await?,
            )),
            TransportValue::Builtin(builtin) => Ok(TransportValue::Builtin(
                mirror_transport_builtin_value_store_refs(source, dest, builtin).await?,
            )),
            other => Ok(other),
        }
    })
}

fn mirror_transport_builtin_value_store_refs<'a>(
    source: &'a StoreClient,
    dest: &'a StoreClient,
    builtin: TransportBuiltinValue,
) -> Pin<Box<dyn Future<Output = std::result::Result<TransportBuiltinValue, String>> + Send + 'a>> {
    Box::pin(async move {
        match builtin {
            TransportBuiltinValue::BlobToString { receiver } => {
                Ok(TransportBuiltinValue::BlobToString {
                    receiver: Box::new(
                        mirror_transport_value_store_refs(source, dest, *receiver).await?,
                    ),
                })
            }
            TransportBuiltinValue::BlobExecutable { receiver } => {
                Ok(TransportBuiltinValue::BlobExecutable {
                    receiver: Box::new(
                        mirror_transport_value_store_refs(source, dest, *receiver).await?,
                    ),
                })
            }
            TransportBuiltinValue::BlobUtf8 { receiver } => Ok(TransportBuiltinValue::BlobUtf8 {
                receiver: Box::new(
                    mirror_transport_value_store_refs(source, dest, *receiver).await?,
                ),
            }),
            TransportBuiltinValue::BlobUtf8Lossy { receiver } => {
                Ok(TransportBuiltinValue::BlobUtf8Lossy {
                    receiver: Box::new(
                        mirror_transport_value_store_refs(source, dest, *receiver).await?,
                    ),
                })
            }
            TransportBuiltinValue::BytesUtf8 { receiver } => Ok(TransportBuiltinValue::BytesUtf8 {
                receiver: Box::new(
                    mirror_transport_value_store_refs(source, dest, *receiver).await?,
                ),
            }),
            TransportBuiltinValue::BytesUtf8Lossy { receiver } => {
                Ok(TransportBuiltinValue::BytesUtf8Lossy {
                    receiver: Box::new(
                        mirror_transport_value_store_refs(source, dest, *receiver).await?,
                    ),
                })
            }
            TransportBuiltinValue::BytesBlob { receiver } => Ok(TransportBuiltinValue::BytesBlob {
                receiver: Box::new(
                    mirror_transport_value_store_refs(source, dest, *receiver).await?,
                ),
            }),
            TransportBuiltinValue::StreamCollect { receiver } => {
                Ok(TransportBuiltinValue::StreamCollect {
                    receiver: Box::new(
                        mirror_transport_value_store_refs(source, dest, *receiver).await?,
                    ),
                })
            }
            TransportBuiltinValue::TreeFile { receiver } => Ok(TransportBuiltinValue::TreeFile {
                receiver: Box::new(
                    mirror_transport_value_store_refs(source, dest, *receiver).await?,
                ),
            }),
            TransportBuiltinValue::TreeMerge { receiver } => Ok(TransportBuiltinValue::TreeMerge {
                receiver: Box::new(
                    mirror_transport_value_store_refs(source, dest, *receiver).await?,
                ),
            }),
            TransportBuiltinValue::TreeSubtree { receiver } => {
                Ok(TransportBuiltinValue::TreeSubtree {
                    receiver: Box::new(
                        mirror_transport_value_store_refs(source, dest, *receiver).await?,
                    ),
                })
            }
            TransportBuiltinValue::FallibleCall { receiver } => {
                Ok(TransportBuiltinValue::FallibleCall {
                    receiver: Box::new(
                        mirror_transport_value_store_refs(source, dest, *receiver).await?,
                    ),
                })
            }
            other => Ok(other),
        }
    })
}

fn mirror_shipped_function_store_refs<'a>(
    source: &'a StoreClient,
    dest: &'a StoreClient,
    function: ShippedFunction,
) -> Pin<Box<dyn Future<Output = std::result::Result<ShippedFunction, String>> + Send + 'a>> {
    Box::pin(async move {
        let mut captures = Vec::with_capacity(function.captures.len());
        for (name, value) in function.captures {
            captures.push((
                name,
                mirror_transport_value_store_refs(source, dest, value).await?,
            ));
        }
        Ok(ShippedFunction {
            module_id: function.module_id,
            code_identity: function.code_identity,
            def: function.def,
            captures,
        })
    })
}

/// The daemon service implementation
// TODO: group some of those locks together?
#[derive(Clone)]
pub struct VxdService {
    store: Arc<StoreClient>,
    build_vx_executors: Arc<BuildVxExecutors>,
    _store_handle: Arc<ServiceHandle>,

    /// The host triple of the execd machine (used for toolchain selection + cache keys).
    exec_host_triple: String,

    /// VX_HOME directory
    vx_home: Utf8PathBuf,

    /// The picante incremental database
    db: Arc<Database>,

    /// Path to the picante cache file
    picante_cache: Utf8PathBuf,

    /// Path to the picante WAL file
    picante_wal: Utf8PathBuf,

    /// WAL writer for incremental persistence (kept open across builds)
    wal_writer: Arc<SyncMutex<Option<WalWriter>>>,

    /// Acquired toolchains (manifest references only)
    toolchains: Arc<SyncMutex<AcquiredToolchains>>,

    /// Spawn tracker for child services
    spawn_tracker: Arc<SyncMutex<SpawnTracker>>,

    /// Shared daemon-side activity registry used by `vx ps`
    activity_registry: ActivityRegistry,

    /// Optional virtual build.vx config module exposed as `import("vixen:config")`.
    build_vx_config_path: Option<Utf8PathBuf>,

    /// In-flight deduplication for identical exec root mount preparation.
    exec_mount_prep_inflight: Arc<SyncMutex<HashMap<String, ExecMountPrepFuture>>>,

    /// Progress listener client (for reporting progress to vx CLI)
    progress_listener: Arc<SyncMutex<Option<Arc<ProgressListenerClient>>>>,

    /// Session ID for cache key namespacing. Empty = session zero (shared).
    /// Per-connection state — each connection handler gets its own instance.
    session_id: Arc<SyncMutex<String>>,

    /// Cancellation token for the build active on this session (if any).
    /// Per-connection: when the CLI disconnects, only this session's build is cancelled.
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

pub(crate) fn exec_mounts_singleflight_key(mounts: &[ExecMount]) -> Option<String> {
    let mut hasher = blake3::Hasher::new();
    // Sort mounts by path for deterministic key.
    let mut sorted: Vec<_> = mounts.iter().collect();
    sorted.sort_by(|a, b| a.path.as_str().as_bytes().cmp(b.path.as_str().as_bytes()));
    for mount in sorted {
        hasher.update(mount.path.as_str().as_bytes());
        hasher.update(b"\x00");
        let mode = match mount.mode {
            ExecMountMode::Tracked => b"tracked".as_slice(),
            ExecMountMode::Opaque => b"opaque".as_slice(),
        };
        hasher.update(mode);
        hasher.update(b"\x00");
        hash_mount_value(&mut hasher, &mount.value)?;
    }
    Some(hasher.finalize().to_hex().to_string())
}

fn hash_mount_value(hasher: &mut blake3::Hasher, value: &Value) -> Option<()> {
    match value {
        Value::Discovered(discovered) => hash_mount_value(hasher, discovered.value.as_ref()),
        Value::ExecPath(path) => hash_exec_path_value(hasher, path),
        Value::Blob(blob) => hash_blob_value(hasher, blob),
        Value::Tree(tree) => hash_tree_value(hasher, tree),
        other => {
            hasher.update(b"v:");
            hasher.update(other.key()?.as_bytes());
            Some(())
        }
    }
}

fn hash_exec_path_value(hasher: &mut blake3::Hasher, path: &ExecPathValue) -> Option<()> {
    match path {
        ExecPathValue::InTree { tree, suffix } => {
            hasher.update(b"execpath:tree:");
            hash_tree_value(hasher, tree)?;
            hasher.update(suffix.as_str().as_bytes());
            Some(())
        }
        ExecPathValue::InBlob { blob, leaf } => {
            hasher.update(b"execpath:blob:");
            hash_blob_value(hasher, blob)?;
            hasher.update(leaf.as_str().as_bytes());
            Some(())
        }
    }
}

fn hash_blob_value(hasher: &mut blake3::Hasher, blob: &BlobValue) -> Option<()> {
    match blob {
        BlobValue::Inline { bytes, executable } => {
            hasher.update(b"blob:inline:");
            hasher.update(if *executable { b"x:" } else { b"-:" });
            hasher.update(blake3::hash(bytes).as_bytes());
            Some(())
        }
        BlobValue::Store {
            blob_ref,
            executable,
        } => {
            hasher.update(b"blob:store:");
            hasher.update(if *executable { b"x:" } else { b"-:" });
            match blob_ref {
                BlobRef::Small { chunk, size } => {
                    hasher.update(&chunk.0.0);
                    hasher.update(&size.to_le_bytes());
                }
                BlobRef::Large {
                    blob_node,
                    content_hash,
                    size,
                } => {
                    hasher.update(&blob_node.0.0);
                    hasher.update(&content_hash.0.0);
                    hasher.update(&size.to_le_bytes());
                }
            }
            Some(())
        }
        BlobValue::FsFile { path, executable } => {
            hasher.update(b"blob:fs:");
            hasher.update(if *executable { b"x:" } else { b"-:" });
            hasher.update(path.as_str().as_bytes());
            Some(())
        }
    }
}

fn hash_tree_value(hasher: &mut blake3::Hasher, tree: &TreeValue) -> Option<()> {
    // Fast path: if the merkle root is known, just feed it into the hasher.
    if let Some(hash) = tree.node_hash() {
        hasher.update(b"tree:");
        hasher.update(&hash.0.0);
        return Some(());
    }
    match tree {
        TreeValue::Inline { entries, .. } => {
            hasher.update(b"tree:inline:");
            let mut sorted: Vec<_> = entries.iter().collect();
            sorted.sort_by(|(a, _), (b, _)| a.as_bytes().cmp(b.as_bytes()));
            for (name, value) in sorted {
                hasher.update(name.as_bytes());
                hasher.update(b"\x00");
                hash_mount_value(hasher, value)?;
            }
            Some(())
        }
        TreeValue::FsDir(path) => {
            hasher.update(b"tree:fsdir:");
            hasher.update(path.as_str().as_bytes());
            Some(())
        }
        TreeValue::Store(root) => {
            hasher.update(b"tree:store:");
            hasher.update(&root.0.0);
            Some(())
        }
    }
}

#[cfg(test)]
fn parity_focus_node_id(node_id: &str) -> bool {
    node_id.starts_with("compile-") || node_id.starts_with("acquire-registry:")
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct RulePlanParityDiff {
    pub legacy_focus_count: usize,
    pub rule_plan_focus_count: usize,
    pub missing_in_rule_plan: Vec<String>,
    pub extra_in_rule_plan: Vec<String>,
}

#[cfg(test)]
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct RulePlanParityEdgeDiff {
    pub missing_in_rule_plan: Vec<String>,
    pub extra_in_rule_plan: Vec<String>,
}

#[cfg(test)]
#[cfg(test)]
pub(crate) fn compare_rule_plan_parity(
    legacy_topology: &GraphTopology,
    rule_plan_topology: &GraphTopology,
) -> RulePlanParityDiff {
    let legacy_ids = legacy_topology
        .nodes
        .iter()
        .filter(|node| parity_focus_node_id(&node.node_id))
        .map(|node| node.node_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let rule_plan_ids = rule_plan_topology
        .nodes
        .iter()
        .filter(|node| parity_focus_node_id(&node.node_id))
        .map(|node| node.node_id.clone())
        .collect::<std::collections::BTreeSet<_>>();

    let missing_in_rule_plan = legacy_ids
        .difference(&rule_plan_ids)
        .cloned()
        .collect::<Vec<_>>();
    let extra_in_rule_plan = rule_plan_ids
        .difference(&legacy_ids)
        .cloned()
        .collect::<Vec<_>>();

    RulePlanParityDiff {
        legacy_focus_count: legacy_ids.len(),
        rule_plan_focus_count: rule_plan_ids.len(),
        missing_in_rule_plan,
        extra_in_rule_plan,
    }
}

#[cfg(test)]
pub(crate) fn compare_rule_plan_edge_parity(
    legacy_topology: &GraphTopology,
    rule_plan_topology: &GraphTopology,
) -> RulePlanParityEdgeDiff {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    let legacy_nodes_by_index = legacy_topology
        .nodes
        .iter()
        .map(|node| (node.index, node.node_id.clone()))
        .collect::<BTreeMap<_, _>>();
    let legacy_focus_by_id = legacy_topology
        .nodes
        .iter()
        .filter(|node| parity_focus_node_id(&node.node_id))
        .map(|node| (node.node_id.clone(), node.index))
        .collect::<BTreeMap<_, _>>();

    let rule_nodes_by_index = rule_plan_topology
        .nodes
        .iter()
        .map(|node| (node.index, node.node_id.clone()))
        .collect::<BTreeMap<_, _>>();
    let rule_focus_by_id = rule_plan_topology
        .nodes
        .iter()
        .filter(|node| parity_focus_node_id(&node.node_id))
        .map(|node| (node.node_id.clone(), node.index))
        .collect::<BTreeMap<_, _>>();

    let mut legacy_focus_deps = BTreeMap::<String, BTreeSet<String>>::new();
    for (node_id, node_index) in &legacy_focus_by_id {
        let mut deps = BTreeSet::new();
        for edge in &legacy_topology.edges {
            if edge.from == *node_index
                && let Some(dep_id) = legacy_nodes_by_index.get(&edge.to)
                && parity_focus_node_id(dep_id)
            {
                deps.insert(dep_id.clone());
            }
        }
        legacy_focus_deps.insert(node_id.clone(), deps);
    }

    // In the RulePlan bridge, crate deps are expressed as: dep_compile -> dep_artifact -> compile.
    // We treat focus nodes as terminals and allow a shallow walk through non-focus nodes.
    let mut rule_adj = BTreeMap::<u32, Vec<u32>>::new();
    for edge in &rule_plan_topology.edges {
        rule_adj.entry(edge.from).or_default().push(edge.to);
    }

    let mut rule_focus_deps = BTreeMap::<String, BTreeSet<String>>::new();
    let max_depth = 2u32;
    for (node_id, start_index) in &rule_focus_by_id {
        let mut deps = BTreeSet::new();
        let mut visited = BTreeSet::new();
        let mut queue = VecDeque::new();

        visited.insert(*start_index);
        queue.push_back((*start_index, 0u32));

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let next_depth = depth + 1;
            for &next in rule_adj.get(&current).into_iter().flatten() {
                if !visited.insert(next) {
                    continue;
                }
                let Some(next_id) = rule_nodes_by_index.get(&next) else {
                    continue;
                };

                if parity_focus_node_id(next_id) {
                    // Treat focus nodes as terminals: compare immediate focus deps only.
                    if next_id != node_id {
                        deps.insert(next_id.clone());
                    }
                    continue;
                }

                queue.push_back((next, next_depth));
            }
        }

        rule_focus_deps.insert(node_id.clone(), deps);
    }

    let mut missing_in_rule_plan = BTreeSet::new();
    let mut extra_in_rule_plan = BTreeSet::new();

    for (node_id, legacy_deps) in legacy_focus_deps {
        let Some(rule_deps) = rule_focus_deps.get(&node_id) else {
            // Node parity check will already report this.
            continue;
        };

        for dep in legacy_deps.difference(rule_deps) {
            missing_in_rule_plan.insert(format!("{node_id} -> {dep}"));
        }
        for dep in rule_deps.difference(&legacy_deps) {
            extra_in_rule_plan.insert(format!("{node_id} -> {dep}"));
        }
    }

    RulePlanParityEdgeDiff {
        missing_in_rule_plan: missing_in_rule_plan.into_iter().collect(),
        extra_in_rule_plan: extra_in_rule_plan.into_iter().collect(),
    }
}

#[cfg(test)]
pub(crate) fn selected_crates_from_rule_plan(
    plan: &RulePlan,
    graph: &CrateGraph,
) -> std::result::Result<std::collections::HashSet<vx_rs::crate_graph::CrateId>, VxdError> {
    use vx_rs::crate_graph::CrateId;

    let mut selected = std::collections::HashSet::new();
    for node in &plan.nodes {
        if node.kind != RulePlanNodeKind::Action {
            continue;
        }

        let (Some(crate_name), Some(crate_type)) = (
            node.provenance.metadata.get("crate_name"),
            node.provenance.metadata.get("crate_type"),
        ) else {
            continue;
        };

        let mut matches = graph
            .nodes
            .values()
            .filter(|crate_node| {
                crate_node.crate_name == *crate_name
                    && crate_node.crate_type.as_str() == *crate_type
            })
            .map(|crate_node| crate_node.id)
            .collect::<Vec<CrateId>>();

        match matches.as_slice() {
            [] => {
                return Err(VxdError::InvalidManifest(format!(
                    "RulePlan selected crate '{crate_name}' type '{crate_type}' but it is not present in the crate graph",
                )));
            }
            [only] => {
                selected.insert(*only);
            }
            _ => {
                matches.sort_by_key(|id: &CrateId| id.short_hex());
                return Err(VxdError::InvalidManifest(format!(
                    "RulePlan selected crate '{crate_name}' type '{crate_type}' but it matched multiple crate IDs: {}",
                    matches
                        .into_iter()
                        .map(|id: CrateId| id.short_hex())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        }
    }

    if selected.is_empty() {
        return Err(VxdError::InvalidManifest(
            "RulePlan execution is enabled but no action nodes with provenance metadata {crate_name, crate_type} were found in the plan".to_string(),
        ));
    }

    Ok(selected)
}

#[cfg(test)]
pub(crate) fn transitive_closure_crates(
    graph: &CrateGraph,
    selected: &std::collections::HashSet<vx_rs::crate_graph::CrateId>,
) -> std::collections::HashSet<vx_rs::crate_graph::CrateId> {
    use vx_rs::crate_graph::CrateId;

    let mut closure = std::collections::HashSet::new();
    let mut stack = selected.iter().copied().collect::<Vec<CrateId>>();

    while let Some(id) = stack.pop() {
        if !closure.insert(id) {
            continue;
        }
        let Some(node) = graph.nodes.get(&id) else {
            continue;
        };
        for dep in &node.deps {
            stack.push(dep.crate_id);
        }
    }

    closure
}

#[cfg(test)]
pub(crate) fn sanitize_id_component(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn legacy_compile_node_id(crate_name: &str, crate_type: &str) -> String {
    format!("compile-{crate_type}:{crate_name}")
}

pub(crate) fn materialize_store_tree_into_dir(
    store: &StoreClient,
    root: NodeHash,
    destination: &Utf8Path,
) -> Result<()> {
    block_in_place(|| {
        Handle::current().block_on(materialize_store_tree_into_dir_async(
            store,
            root,
            destination,
        ))
    })
}

pub(crate) async fn materialize_store_tree_into_dir_async(
    store: &StoreClient,
    root: NodeHash,
    destination: &Utf8Path,
) -> Result<()> {
    fs::create_dir_all(destination).map_err(|error| VxdError::CreateDir {
        path: destination.to_path_buf(),
        message: error.to_string(),
    })?;

    let mut pending = vec![(root, destination.to_path_buf())];
    while let Some((node_hash, dir_path)) = pending.pop() {
        fs::create_dir_all(&dir_path).map_err(|error| VxdError::CreateDir {
            path: dir_path.clone(),
            message: error.to_string(),
        })?;

        let directory = load_store_directory(store, node_hash).await?;
        for entry in directory.entries {
            let entry_path = dir_path.join(&entry.name);
            match entry.kind {
                DirEntryKind::SmallFile {
                    content_hash,
                    size,
                    executable,
                } => {
                    let bytes = fetch_store_blob_bytes(
                        store,
                        &BlobRef::Small {
                            chunk: ChunkHash(content_hash.0),
                            size,
                        },
                    )
                    .await?;
                    write_materialized_blob(&entry_path, bytes, executable)?;
                }
                DirEntryKind::LargeFile {
                    blob_node,
                    content_hash,
                    size,
                    executable,
                } => {
                    let bytes = fetch_store_blob_bytes(
                        store,
                        &BlobRef::Large {
                            blob_node,
                            content_hash,
                            size,
                        },
                    )
                    .await?;
                    write_materialized_blob(&entry_path, bytes, executable)?;
                }
                DirEntryKind::Dir { hash, .. } => pending.push((hash, entry_path)),
                DirEntryKind::Symlink { target } => {
                    return Err(VxdError::WriteOutput {
                        path: entry_path,
                        message: format!(
                            "build.vx materialization does not support store-backed symlink outputs yet (target: {target})"
                        ),
                    });
                }
            }
        }
    }

    Ok(())
}

fn materialize_store_blob_to_path(
    store: &StoreClient,
    path: &Utf8Path,
    blob_ref: &BlobRef,
    executable: bool,
) -> Result<()> {
    let bytes =
        block_in_place(|| Handle::current().block_on(fetch_store_blob_bytes(store, blob_ref)))?;
    write_materialized_blob(path, bytes, executable)
}

fn write_materialized_blob(path: &Utf8Path, bytes: Vec<u8>, executable: bool) -> Result<()> {
    let parent = path.parent().ok_or_else(|| VxdError::WriteOutput {
        path: path.to_path_buf(),
        message: "output path has no parent directory".to_string(),
    })?;
    fs::create_dir_all(parent).map_err(|error| VxdError::CreateDir {
        path: parent.to_path_buf(),
        message: error.to_string(),
    })?;

    fs::write(path, bytes).map_err(|error| VxdError::WriteOutput {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    set_executable_if_needed(path, executable)?;
    Ok(())
}

async fn fetch_store_blob_bytes(store: &StoreClient, blob_ref: &BlobRef) -> Result<Vec<u8>> {
    match blob_ref {
        BlobRef::Small { chunk, .. } => store
            .get_chunk(*chunk)
            .await
            .map_err(|error| VxdError::CasRpc(format!("{error:?}")))?
            .ok_or_else(|| VxdError::BlobNotFound(format!("chunk {}", chunk.to_hex()))),
        BlobRef::Large {
            blob_node,
            content_hash: _,
            size: _,
        } => {
            let node_bytes = store
                .get_node(*blob_node)
                .await
                .map_err(|error| VxdError::CasRpc(format!("{error:?}")))?
                .ok_or_else(|| VxdError::BlobNotFound(format!("blob node {blob_node}")))?;
            let blob_node_data = match CasNode::from_bytes(&node_bytes) {
                Ok(CasNode::Blob(blob)) => blob,
                Ok(_) => {
                    return Err(VxdError::BlobNotFound(format!(
                        "node {blob_node} is not a BlobNode"
                    )));
                }
                Err(error) => {
                    return Err(VxdError::CasRpc(format!(
                        "failed to deserialize blob node {blob_node}: {error}"
                    )));
                }
            };

            let mut result = Vec::with_capacity(blob_node_data.total_size as usize);
            for chunk_entry in &blob_node_data.chunks {
                let chunk_data = store
                    .get_chunk(chunk_entry.hash)
                    .await
                    .map_err(|error| VxdError::CasRpc(format!("{error:?}")))?
                    .ok_or_else(|| {
                        VxdError::BlobNotFound(format!("chunk {}", chunk_entry.hash.to_hex()))
                    })?;
                result.extend_from_slice(&chunk_data);
            }
            Ok(result)
        }
    }
}

async fn load_store_directory(store: &StoreClient, root: NodeHash) -> Result<DirectoryNode> {
    let node_bytes = store
        .get_node(root)
        .await
        .map_err(|error| VxdError::CasRpc(format!("{error:?}")))?
        .ok_or_else(|| VxdError::BlobNotFound(format!("directory node {root}")))?;
    match CasNode::from_bytes(&node_bytes) {
        Ok(CasNode::Directory(directory)) => Ok(directory),
        Ok(_) => Err(VxdError::BlobNotFound(format!(
            "node {root} is not a DirectoryNode"
        ))),
        Err(error) => Err(VxdError::CasRpc(format!(
            "failed to deserialize directory node {root}: {error}"
        ))),
    }
}

#[cfg(unix)]
fn set_executable_if_needed(path: &Utf8Path, executable: bool) -> Result<()> {
    if !executable {
        return Ok(());
    }
    let metadata = fs::metadata(path).map_err(|error| VxdError::FileRead {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(path, permissions).map_err(|error| VxdError::WriteOutput {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

#[cfg(not(unix))]
fn set_executable_if_needed(_path: &Utf8Path, _executable: bool) -> Result<()> {
    Ok(())
}

fn exec_arg_to_run_arg(arg: &ExecArgValue) -> RunArg {
    match arg {
        ExecArgValue::Literal(value) => RunArg::literal(value.clone()),
        ExecArgValue::Path(path) => RunArg::path(path.as_str()),
    }
}

#[cfg(test)]
fn join_exec_mount_path(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

fn exec_blob_kind_from_blob_ref(blob_ref: &BlobRef, executable: bool) -> DirEntryKind {
    match blob_ref {
        BlobRef::Small { chunk, size } => DirEntryKind::SmallFile {
            content_hash: ContentHash(chunk.0),
            size: *size,
            executable,
        },
        BlobRef::Large {
            blob_node,
            content_hash,
            size,
        } => DirEntryKind::LargeFile {
            blob_node: *blob_node,
            content_hash: *content_hash,
            size: *size,
            executable,
        },
    }
}

fn blob_ref_size(blob_ref: &BlobRef) -> u64 {
    match blob_ref {
        BlobRef::Small { size, .. } | BlobRef::Large { size, .. } => *size,
    }
}

fn validate_tree_entry_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(format!("invalid tree entry name '{name}'"));
    }
    Ok(())
}

fn mount_prep_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(8)
        .saturating_mul(2)
        .clamp(4, 64)
}

async fn store_directory_node(
    store: &StoreClient,
    entries: Vec<DirEntry>,
    total_size: u64,
) -> std::result::Result<(NodeHash, u64), String> {
    let directory = CasNode::Directory(DirectoryNode {
        entries,
        total_size,
    });
    let (bytes, hash) = directory.to_bytes_and_hash();
    put_node_if_missing(store, hash, bytes).await?;
    Ok((hash, total_size))
}

async fn store_exec_blob_ref(
    store: &StoreClient,
    blob: &BlobValue,
) -> std::result::Result<(BlobRef, bool), String> {
    match blob {
        BlobValue::Inline { bytes, executable } => {
            let prepared = prepare_blob(bytes.as_ref());
            let blob_ref = store_prepared_blob(store, &prepared, bytes.as_ref()).await?;
            Ok((blob_ref, *executable))
        }
        BlobValue::Store {
            blob_ref,
            executable,
        } => Ok((blob_ref.clone(), *executable)),
        BlobValue::FsFile { path, executable } => {
            let bytes = fs::read(path.as_str())
                .map_err(|error| format!("failed to read {}: {error}", path.as_str()))?;
            let prepared = prepare_blob(&bytes);
            let blob_ref = store_prepared_blob(store, &prepared, &bytes).await?;
            Ok((blob_ref, *executable))
        }
    }
}

#[cfg(test)]
async fn store_exec_blob(
    store: &StoreClient,
    blob: &BlobValue,
) -> std::result::Result<DirEntryKind, String> {
    let (blob_ref, executable) = store_exec_blob_ref(store, blob).await?;
    Ok(exec_blob_kind_from_blob_ref(&blob_ref, executable))
}

#[cfg(unix)]
fn host_path_is_executable(path: &PathValue) -> std::result::Result<bool, String> {
    let metadata = fs::metadata(path.as_str())
        .map_err(|error| format!("failed to stat {}: {error}", path.as_str()))?;
    Ok(metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn host_path_is_executable(_path: &PathValue) -> std::result::Result<bool, String> {
    Ok(false)
}

async fn snapshot_exec_fs_tree_cached(
    store: &StoreClient,
    cache: &Arc<SyncMutex<HashMap<String, BuildVxFsTreeSnapshotFuture>>>,
    path: &PathValue,
) -> BuildVxFsTreeSnapshotResult {
    let key = path.as_str().to_string();
    let (cell, inserted) = {
        let mut cache = cache.lock();
        match cache.entry(key.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => (entry.get().clone(), false),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let cell = Arc::new(moire::sync::OnceCell::new("build_vx_fs_tree_snapshot"));
                entry.insert(cell.clone());
                (cell, true)
            }
        }
    };

    let result = cell
        .get_or_init(|| snapshot_exec_fs_tree_uncached(store, path.clone()))
        .await
        .clone();

    if result.is_err() || !inserted {
        let mut cache = cache.lock();
        if result.is_err() {
            cache.remove(&key);
        }
    }

    result
}

fn snapshot_exec_inline_tree<'a>(
    store: &'a StoreClient,
    cache: &'a Arc<SyncMutex<HashMap<String, BuildVxFsTreeSnapshotFuture>>>,
    entries: &'a Arc<[(Arc<str>, Value)]>,
) -> SnapshotTreeFuture<'a> {
    Box::pin(async move {
        let mut sorted = entries.iter().cloned().collect::<Vec<_>>();
        sorted.sort_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
        let mut seen_names = HashSet::with_capacity(sorted.len());
        for (name, _) in &sorted {
            validate_tree_entry_name(name.as_ref())?;
            if !seen_names.insert(name.to_string()) {
                return Err(format!("duplicate tree entry '{name}'"));
            }
        }
        let parallelism = mount_prep_parallelism();
        let mut prepared_entries: Vec<(String, DirEntryKind, u64)> = stream::iter(sorted)
            .map(|(name, value)| async move {
                let (kind, size) = snapshot_exec_tree_entry(store, cache, value).await?;
                Ok::<_, String>((name.to_string(), kind, size))
            })
            .buffer_unordered(parallelism)
            .try_collect()
            .await?;
        prepared_entries
            .sort_by(|(left, _, _), (right, _, _)| left.as_bytes().cmp(right.as_bytes()));

        let total_size = prepared_entries.iter().map(|(_, _, size)| *size).sum();
        let dir_entries = prepared_entries
            .into_iter()
            .map(|(name, kind, _)| DirEntry { name, kind })
            .collect();
        store_directory_node(store, dir_entries, total_size).await
    })
}

fn snapshot_exec_fs_tree_uncached<'a>(
    store: &'a StoreClient,
    root: PathValue,
) -> SnapshotTreeFuture<'a> {
    Box::pin(async move {
        let mut read_dir = fs::read_dir(root.as_str())
            .map_err(|error| format!("failed to read {}: {error}", root.as_str()))?;
        let mut names = Vec::new();
        while let Some(entry) = read_dir
            .next()
            .transpose()
            .map_err(|error| format!("failed to read {}: {error}", root.as_str()))?
        {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        let parallelism = mount_prep_parallelism();
        let mut prepared_entries: Vec<(String, DirEntryKind, u64)> = stream::iter(names)
            .map(|name| {
                let root = root.clone();
                async move {
                    validate_tree_entry_name(&name)?;
                    let child = root.join(&name);
                    let metadata = fs::metadata(child.as_str())
                        .map_err(|error| format!("failed to stat {}: {error}", child.as_str()))?;
                    let (kind, size) = if metadata.is_dir() {
                        let (hash, total_size) =
                            snapshot_exec_fs_tree_uncached(store, child).await?;
                        (DirEntryKind::Dir { hash }, total_size)
                    } else {
                        let executable = host_path_is_executable(&child)?;
                        let (blob_ref, executable) = store_exec_blob_ref(
                            store,
                            &BlobValue::FsFile {
                                path: child,
                                executable,
                            },
                        )
                        .await?;
                        let size = blob_ref_size(&blob_ref);
                        (exec_blob_kind_from_blob_ref(&blob_ref, executable), size)
                    };
                    Ok::<_, String>((name, kind, size))
                }
            })
            .buffer_unordered(parallelism)
            .try_collect()
            .await?;
        prepared_entries
            .sort_by(|(left, _, _), (right, _, _)| left.as_bytes().cmp(right.as_bytes()));

        let total_size = prepared_entries.iter().map(|(_, _, size)| *size).sum();
        let dir_entries = prepared_entries
            .into_iter()
            .map(|(name, kind, _)| DirEntry { name, kind })
            .collect();
        store_directory_node(store, dir_entries, total_size).await
    })
}

fn snapshot_exec_tree_entry<'a>(
    store: &'a StoreClient,
    cache: &'a Arc<SyncMutex<HashMap<String, BuildVxFsTreeSnapshotFuture>>>,
    value: Value,
) -> SnapshotTreeEntryFuture<'a> {
    Box::pin(async move {
        match value {
            Value::Discovered(discovered) => {
                snapshot_exec_tree_entry(store, cache, *discovered.value).await
            }
            Value::Blob(blob) => {
                let (blob_ref, executable) = store_exec_blob_ref(store, &blob).await?;
                let size = blob_ref_size(&blob_ref);
                Ok((exec_blob_kind_from_blob_ref(&blob_ref, executable), size))
            }
            Value::Tree(tree) => {
                let (hash, total_size) = snapshot_exec_tree(store, cache, &tree).await?;
                Ok((DirEntryKind::Dir { hash }, total_size))
            }
            other => Err(format!(
                "tree entries must be Blob or Tree, got {}",
                other.type_name()
            )),
        }
    })
}

async fn snapshot_exec_tree(
    store: &StoreClient,
    cache: &Arc<SyncMutex<HashMap<String, BuildVxFsTreeSnapshotFuture>>>,
    tree: &TreeValue,
) -> std::result::Result<(NodeHash, u64), String> {
    match tree {
        TreeValue::Inline { entries, .. } => snapshot_exec_inline_tree(store, cache, entries).await,
        TreeValue::FsDir(path) => snapshot_exec_fs_tree_cached(store, cache, path).await,
        TreeValue::Store(root) => {
            let directory = load_store_directory(store, *root)
                .await
                .map_err(|error| error.to_string())?;
            Ok((*root, directory.total_size))
        }
    }
}

async fn snapshot_exec_blob_mount(
    store: &StoreClient,
    path: &str,
    blob: &BlobValue,
    mode: RunMountMode,
) -> std::result::Result<RunFileMount, String> {
    let (blob_ref, executable) = store_exec_blob_ref(store, blob).await?;
    let filename = path
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("input")
        .to_string();
    let manifest = NodeManifest {
        node_id: NodeId(format!("exec-input:{}", path.replace(['\\', '/'], "_"))),
        cache_key: Blake3Hash([0; 32]),
        produced_at: String::new(),
        outputs: vec![OutputEntry {
            logical: "blob".to_string(),
            filename,
            blob: output_blob_ref_from_blob_ref(&blob_ref),
            executable,
        }],
    };
    let (node_bytes, manifest_hash) = CasNode::Manifest(manifest).to_bytes_and_hash();
    put_node_if_missing(store, manifest_hash, node_bytes).await?;
    Ok(RunFileMount {
        path: path.to_string(),
        manifest: manifest_hash.0,
        logical: "blob".to_string(),
        mode,
    })
}

async fn prepare_exec_mounts_with_progress(
    store: &StoreClient,
    mounts: &[ExecMount],
    fs_tree_snapshot_cache: &Arc<SyncMutex<HashMap<String, BuildVxFsTreeSnapshotFuture>>>,
    progress_listener: &Option<Arc<ProgressListenerClient>>,
    activity: Option<&ActivityHandle>,
    executor_summary: Option<&str>,
    host_relation: HostRelation,
) -> ExecMountPrepResult {
    if let (Some(activity), Some(executor_summary)) = (activity, executor_summary) {
        emit_build_vx_progress_activity(
            progress_listener,
            activity,
            format!(
                "build.vx executor {}: preparing {} structural exec mounts ({})",
                executor_summary,
                mounts.len(),
                host_relation.as_str()
            ),
        )
        .await;
    }

    enum PreparedMount {
        Tree(RunTreeMount),
        File(RunFileMount),
    }

    let parallelism = mount_prep_parallelism();
    let mount_inputs: Vec<_> = mounts.iter().cloned().enumerate().collect();
    let prepared_mounts: Vec<(usize, PreparedMount)> = stream::iter(mount_inputs)
        .map(|(index, mount)| async move {
            let mode = match mount.mode {
                ExecMountMode::Tracked => RunMountMode::Tracked,
                ExecMountMode::Opaque => RunMountMode::Opaque,
            };
            let value = match &mount.value {
                Value::Discovered(discovered) => discovered.value.as_ref(),
                other => other,
            };
            let prepared = match value {
                Value::Tree(tree) => {
                    let (root, _) = snapshot_exec_tree(store, fs_tree_snapshot_cache, tree).await?;
                    PreparedMount::Tree(RunTreeMount {
                        mount: mount.path.as_str().to_string(),
                        manifest: root.0,
                        mode,
                    })
                }
                Value::Blob(blob) => PreparedMount::File(
                    snapshot_exec_blob_mount(store, mount.path.as_str(), blob, mode).await?,
                ),
                other => {
                    return Err(format!(
                        "exec mount values must be Blob or Tree, got {}",
                        other.type_name()
                    ));
                }
            };
            Ok::<_, String>((index, prepared))
        })
        .buffer_unordered(parallelism)
        .try_collect()
        .await?;

    let mut prepared = PreparedExecMounts::default();
    let mut prepared_mounts = prepared_mounts;
    prepared_mounts.sort_by_key(|(index, _)| *index);
    for (_, mount) in prepared_mounts {
        match mount {
            PreparedMount::Tree(tree_mount) => prepared.tree_mounts.push(tree_mount),
            PreparedMount::File(file_mount) => prepared.file_mounts.push(file_mount),
        }
    }

    if let (Some(activity), Some(executor_summary)) = (activity, executor_summary) {
        emit_build_vx_progress_activity(
            progress_listener,
            activity,
            format!(
                "build.vx executor {}: prepared {} tree mounts and {} file mounts ({})",
                executor_summary,
                prepared.tree_mounts.len(),
                prepared.file_mounts.len(),
                host_relation.as_str()
            ),
        )
        .await;
    }

    Ok(prepared)
}

#[cfg(test)]
pub(crate) async fn store_exec_mounts(
    store: &StoreClient,
    mounts: &[ExecMount],
) -> std::result::Result<Option<NodeHash>, String> {
    let no_listener = None;
    store_exec_mounts_with_progress(store, mounts, &no_listener, None, None).await
}

#[cfg(test)]
async fn store_exec_mounts_with_progress(
    store: &StoreClient,
    mounts: &[ExecMount],
    progress_listener: &Option<Arc<ProgressListenerClient>>,
    activity: Option<&ActivityHandle>,
    executor_summary: Option<&str>,
) -> std::result::Result<Option<NodeHash>, String> {
    enum PendingMount {
        Value(String, Value),
        HostPath(String, PathValue),
        StoreDir(String, NodeHash),
    }

    let mut pending = mounts
        .iter()
        .map(|mount| PendingMount::Value(mount.path.as_str().to_string(), mount.value.clone()))
        .collect::<Vec<_>>();
    let mut files = Vec::new();
    let mut processed_items = 0usize;
    let mut last_progress_emit = 0usize;

    if let (Some(activity), Some(executor_summary)) = (activity, executor_summary) {
        emit_build_vx_progress_activity(
            progress_listener,
            activity,
            format!(
                "build.vx executor {}: exec root mount expanding {} top-level mounts",
                executor_summary,
                mounts.len()
            ),
        )
        .await;
    }

    while let Some(item) = pending.pop() {
        processed_items += 1;
        match item {
            PendingMount::Value(path, value) => match value {
                Value::Discovered(discovered) => {
                    pending.push(PendingMount::Value(path, *discovered.value));
                }
                Value::Blob(blob) => files.push(crate::cas_tree::TreeFile {
                    path,
                    kind: store_exec_blob(store, &blob).await?,
                }),
                Value::Tree(TreeValue::Inline { entries, .. }) => {
                    let mut entries = entries.iter().cloned().collect::<Vec<_>>();
                    entries.sort_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
                    for (name, value) in entries.into_iter().rev() {
                        pending.push(PendingMount::Value(
                            join_exec_mount_path(&path, name.as_ref()),
                            value,
                        ));
                    }
                }
                Value::Tree(TreeValue::Store(root)) => {
                    pending.push(PendingMount::StoreDir(path, root))
                }
                Value::Tree(TreeValue::FsDir(dir)) => {
                    pending.push(PendingMount::HostPath(path, dir))
                }
                other => {
                    return Err(format!(
                        "exec mount values must be Blob or Tree, got {}",
                        other.type_name()
                    ));
                }
            },
            PendingMount::HostPath(path, host_path) => {
                let metadata = fs::metadata(host_path.as_str())
                    .map_err(|error| format!("failed to stat {}: {error}", host_path.as_str()))?;
                if metadata.is_dir() {
                    let mut names = fs::read_dir(host_path.as_str())
                        .map_err(|error| format!("failed to read {}: {error}", host_path.as_str()))?
                        .map(|entry| {
                            entry
                                .map(|entry| entry.file_name().to_string_lossy().to_string())
                                .map_err(|error| {
                                    format!("failed to read {}: {error}", host_path.as_str())
                                })
                        })
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    names.sort();
                    for name in names.into_iter().rev() {
                        pending.push(PendingMount::HostPath(
                            join_exec_mount_path(&path, &name),
                            host_path.join(&name),
                        ));
                    }
                } else {
                    let bytes = fs::read(host_path.as_str()).map_err(|error| {
                        format!("failed to read {}: {error}", host_path.as_str())
                    })?;
                    let prepared = prepare_blob(&bytes);
                    let blob_ref = store_prepared_blob(store, &prepared, &bytes).await?;
                    files.push(crate::cas_tree::TreeFile {
                        path,
                        kind: exec_blob_kind_from_blob_ref(
                            &blob_ref,
                            host_path_is_executable(&host_path)?,
                        ),
                    });
                }
            }
            PendingMount::StoreDir(path, root) => {
                let directory = load_store_directory(store, root)
                    .await
                    .map_err(|error| error.to_string())?;
                for entry in directory.entries.into_iter().rev() {
                    let entry_path = join_exec_mount_path(&path, &entry.name);
                    match entry.kind {
                        DirEntryKind::SmallFile {
                            content_hash,
                            size,
                            executable,
                        } => files.push(crate::cas_tree::TreeFile {
                            path: entry_path,
                            kind: DirEntryKind::SmallFile {
                                content_hash,
                                size,
                                executable,
                            },
                        }),
                        DirEntryKind::LargeFile {
                            blob_node,
                            content_hash,
                            size,
                            executable,
                        } => files.push(crate::cas_tree::TreeFile {
                            path: entry_path,
                            kind: DirEntryKind::LargeFile {
                                blob_node,
                                content_hash,
                                size,
                                executable,
                            },
                        }),
                        DirEntryKind::Dir { hash, .. } => {
                            pending.push(PendingMount::StoreDir(entry_path, hash));
                        }
                        DirEntryKind::Symlink { target } => files.push(crate::cas_tree::TreeFile {
                            path: entry_path,
                            kind: DirEntryKind::Symlink { target },
                        }),
                    }
                }
            }
        }

        if processed_items.saturating_sub(last_progress_emit) >= 128 {
            if let (Some(activity), Some(executor_summary)) = (activity, executor_summary) {
                emit_build_vx_progress_activity(
                    progress_listener,
                    activity,
                    format!(
                        "build.vx executor {}: exec root mount expanded {} items into {} files",
                        executor_summary,
                        processed_items,
                        files.len()
                    ),
                )
                .await;
            }
            last_progress_emit = processed_items;
        }
    }

    if files.is_empty() {
        return Ok(None);
    }

    if let (Some(activity), Some(executor_summary)) = (activity, executor_summary) {
        emit_build_vx_progress_activity(
            progress_listener,
            activity,
            format!(
                "build.vx executor {}: building synthetic exec root tree from {} files",
                executor_summary,
                files.len()
            ),
        )
        .await;
    }

    crate::cas_tree::build_merkle_tree(store, files)
        .await
        .map(|(root, _)| Some(root))
}

async fn store_exec_mounts_deduped(
    store: &StoreClient,
    mounts: &[ExecMount],
    fs_tree_snapshot_cache: &Arc<SyncMutex<HashMap<String, BuildVxFsTreeSnapshotFuture>>>,
    progress_listener: &Option<Arc<ProgressListenerClient>>,
    activity: Option<&ActivityHandle>,
    executor_summary: Option<&str>,
    host_relation: HostRelation,
    inflight: &Arc<SyncMutex<HashMap<String, ExecMountPrepFuture>>>,
    event_recorder: Option<&vx_eval::EventRecorder>,
) -> ExecMountPrepResult {
    let Some(key) = exec_mounts_singleflight_key(mounts) else {
        return prepare_exec_mounts_with_progress(
            store,
            mounts,
            fs_tree_snapshot_cache,
            progress_listener,
            activity,
            executor_summary,
            host_relation,
        )
        .await;
    };

    let (cell, inserted) = {
        let mut inflight = inflight.lock();
        match inflight.entry(key.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => (entry.get().clone(), false),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let cell = Arc::new(moire::sync::OnceCell::new("build_vx_exec_mount_prep"));
                entry.insert(cell.clone());
                (cell, true)
            }
        }
    };

    let track_id = current_trace_track_id();
    let result = if inserted {
        cell.get_or_init(|| {
            let compute_span = event_recorder.map(|recorder| {
                recorder.begin_detached_span(track_id, "exec.mounts.prepare.compute")
            });
            async move {
                let result = prepare_exec_mounts_with_progress(
                    store,
                    mounts,
                    fs_tree_snapshot_cache,
                    progress_listener,
                    activity,
                    executor_summary,
                    host_relation,
                )
                .await;
                drop(compute_span);
                result
            }
        })
        .await
        .clone()
    } else {
        let wait_span = event_recorder
            .map(|recorder| recorder.begin_detached_span(track_id, "exec.mounts.prepare.wait"));
        let result = cell
            .get_or_init(|| {
                prepare_exec_mounts_with_progress(
                    store,
                    mounts,
                    fs_tree_snapshot_cache,
                    progress_listener,
                    activity,
                    executor_summary,
                    host_relation,
                )
            })
            .await
            .clone();
        drop(wait_span);
        result
    };

    if result.is_err() || inserted {
        let mut inflight = inflight.lock();
        if result.is_err()
            || inflight
                .get(&key)
                .is_some_and(|existing| Arc::ptr_eq(existing, &cell))
        {
            inflight.remove(&key);
        }
    }

    result
}

#[derive(Clone, Copy)]
enum AcquireDigest {
    Sha256([u8; 32]),
    Blake3([u8; 32]),
}

impl AcquireDigest {
    fn parse(value: &str) -> std::result::Result<Self, String> {
        let (scheme, hex_value) = value
            .split_once(':')
            .ok_or_else(|| "acquire digest must be written as scheme:hex".to_string())?;

        match scheme.trim().to_ascii_lowercase().as_str() {
            "sha256" => {
                let mut bytes = [0; 32];
                hex::decode_to_slice(hex_value, &mut bytes)
                    .map_err(|error| format!("invalid sha256 acquire digest: {error}"))?;
                Ok(Self::Sha256(bytes))
            }
            "blake3" => {
                let mut bytes = [0; 32];
                hex::decode_to_slice(hex_value, &mut bytes)
                    .map_err(|error| format!("invalid blake3 acquire digest: {error}"))?;
                Ok(Self::Blake3(bytes))
            }
            other => Err(format!("unsupported acquire digest scheme '{other}'")),
        }
    }

    fn normalized_string(self) -> String {
        match self {
            Self::Sha256(bytes) => format!("sha256:{}", hex::encode(bytes)),
            Self::Blake3(bytes) => format!("blake3:{}", hex::encode(bytes)),
        }
    }
}

fn acquire_trace_args(request: &EvalAcquireRequest, display_name: &str) -> facet_value::Value {
    value!({
        "display_name": ((display_name.to_string())),
        "url": ((request.url.clone())),
        "digest": ((request.digest.clone())),
        "extract": ((request.extract.clone().unwrap_or_default()))
    })
}

#[derive(Clone, Copy)]
enum AcquireArchiveFormat {
    TarGz,
    TarXz,
}

impl AcquireArchiveFormat {
    fn parse(value: &str) -> std::result::Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "tar.gz" => Ok(Self::TarGz),
            "tar.xz" => Ok(Self::TarXz),
            other => Err(format!(
                "unsupported acquire extract format '{other}' (expected tar.gz or tar.xz)"
            )),
        }
    }

    fn cache_key_tag(self) -> &'static str {
        match self {
            Self::TarGz => "tar.gz",
            Self::TarXz => "tar.xz",
        }
    }

    fn compression(self) -> vx_tarball::Compression {
        match self {
            Self::TarGz => vx_tarball::Compression::Gzip,
            Self::TarXz => vx_tarball::Compression::Xz,
        }
    }
}

fn acquire_cache_key(url: &str, digest: &str, extract: Option<&str>) -> Blake3Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"acquire-cache-v1\n");
    hasher.update(b"url:");
    hasher.update(url.as_bytes());
    hasher.update(b"\n");
    hasher.update(b"digest:");
    hasher.update(digest.as_bytes());
    hasher.update(b"\n");
    hasher.update(b"extract:");
    hasher.update(extract.unwrap_or("").as_bytes());
    hasher.update(b"\n");
    Blake3Hash(*hasher.finalize().as_bytes())
}

fn output_blob_ref_from_blob_ref(blob_ref: &BlobRef) -> OutputBlobRef {
    match blob_ref {
        BlobRef::Small { chunk, size } => OutputBlobRef::Small {
            chunk: *chunk,
            size: *size,
        },
        BlobRef::Large {
            blob_node,
            content_hash,
            size,
        } => OutputBlobRef::Large {
            blob_node: *blob_node,
            content_hash: *content_hash,
            size: *size,
        },
    }
}

async fn mirror_prepared_exec_mounts(
    source: &StoreClient,
    dest: &StoreClient,
    prepared: &PreparedExecMounts,
) -> std::result::Result<PreparedExecMounts, String> {
    for mount in &prepared.tree_mounts {
        mirror_node(source, dest, NodeHash(mount.manifest)).await?;
    }
    for mount in &prepared.file_mounts {
        mirror_node(source, dest, NodeHash(mount.manifest)).await?;
    }
    Ok(prepared.clone())
}

fn blob_ref_from_output_blob_ref(blob_ref: &OutputBlobRef) -> BlobRef {
    match blob_ref {
        OutputBlobRef::Small { chunk, size } => BlobRef::Small {
            chunk: *chunk,
            size: *size,
        },
        OutputBlobRef::Large {
            blob_node,
            content_hash,
            size,
        } => BlobRef::Large {
            blob_node: *blob_node,
            content_hash: *content_hash,
            size: *size,
        },
    }
}

fn split_tree_entry_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(index) => (&path[..index], &path[index + 1..]),
        None => ("", path),
    }
}

fn ensure_tree_parent_dirs(dir_entries: &mut BTreeMap<String, Vec<DirEntry>>, dir: &str) {
    let mut current = dir.to_string();
    while let Some(index) = current.rfind('/') {
        current = current[..index].to_string();
        dir_entries.entry(current.clone()).or_default();
    }
    if !current.is_empty() {
        dir_entries.entry(String::new()).or_default();
    }
}

fn acquire_archive_root_prefix(tree: &TreeManifest) -> Option<String> {
    let mut root: Option<&str> = None;
    let mut saw_nested_entries = false;

    for entry in &tree.entries {
        let path = entry.path.as_str();
        if path.is_empty() {
            return None;
        }
        let (first, rest) = match path.split_once('/') {
            Some((first, rest)) => (first, Some(rest)),
            None => (path, None),
        };

        match root {
            Some(existing) if existing != first => return None,
            None => root = Some(first),
            _ => {}
        }

        match entry.kind {
            TreeEntryKind::File { .. } | TreeEntryKind::Symlink { .. } if rest.is_none() => {
                return None;
            }
            TreeEntryKind::File { .. } | TreeEntryKind::Symlink { .. } => saw_nested_entries = true,
            TreeEntryKind::Directory => {
                if rest.is_some() {
                    saw_nested_entries = true;
                }
            }
        }
    }

    if saw_nested_entries {
        root.map(str::to_string)
    } else {
        None
    }
}

fn normalize_acquired_tree_path(path: &str, root_prefix: Option<&str>) -> Option<String> {
    let Some(root_prefix) = root_prefix else {
        return Some(path.to_string());
    };

    if path == root_prefix {
        return None;
    }

    path.strip_prefix(root_prefix)
        .and_then(|rest| rest.strip_prefix('/'))
        .map(str::to_string)
}

async fn store_acquired_tree_manifest(
    store: &StoreClient,
    tree: &TreeManifest,
    files: &HashMap<Blake3Hash, BlobRef>,
    event_recorder: Option<&vx_eval::EventRecorder>,
    store_node_track_id: Option<u64>,
) -> std::result::Result<(NodeHash, u64), String> {
    let root_prefix = acquire_archive_root_prefix(tree);
    let mut dir_entries: BTreeMap<String, Vec<DirEntry>> = BTreeMap::new();

    for entry in &tree.entries {
        let Some(path) = normalize_acquired_tree_path(&entry.path, root_prefix.as_deref()) else {
            continue;
        };

        match &entry.kind {
            TreeEntryKind::File {
                blob, executable, ..
            } => {
                let blob_ref = files.get(blob).ok_or_else(|| {
                    format!("acquire tree file info not found for {}", blob.to_hex())
                })?;
                let (parent, name) = split_tree_entry_path(&path);
                dir_entries
                    .entry(parent.to_string())
                    .or_default()
                    .push(DirEntry {
                        name: name.to_string(),
                        kind: exec_blob_kind_from_blob_ref(blob_ref, *executable),
                    });
            }
            TreeEntryKind::Symlink { target } => {
                let (parent, name) = split_tree_entry_path(&path);
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
            TreeEntryKind::Directory => {
                dir_entries.entry(path).or_default();
            }
        }
    }

    if dir_entries.is_empty() {
        dir_entries.insert(String::new(), Vec::new());
    }

    let all_dirs = dir_entries.keys().cloned().collect::<Vec<_>>();
    for dir in &all_dirs {
        ensure_tree_parent_dirs(&mut dir_entries, dir);
    }

    let mut dir_list = dir_entries.keys().cloned().collect::<Vec<_>>();
    dir_list.sort_by(|left, right| {
        let left_depth = if left.is_empty() {
            0
        } else {
            left.matches('/').count() + 1
        };
        let right_depth = if right.is_empty() {
            0
        } else {
            right.matches('/').count() + 1
        };
        right_depth.cmp(&left_depth).then_with(|| left.cmp(right))
    });

    let mut dir_hashes = HashMap::new();
    for dir_path in &dir_list {
        let entries = dir_entries
            .get_mut(dir_path)
            .ok_or_else(|| format!("missing directory entry list for '{dir_path}'"))?;
        entries.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));

        let total_size = entries.iter().fold(0u64, |acc, entry| {
            acc + match &entry.kind {
                DirEntryKind::SmallFile { size, .. } | DirEntryKind::LargeFile { size, .. } => {
                    *size
                }
                DirEntryKind::Dir { .. } => {
                    let child_path = if dir_path.is_empty() {
                        entry.name.clone()
                    } else {
                        format!("{dir_path}/{}", entry.name)
                    };
                    dir_hashes.get(&child_path).map(|&(_, s)| s).unwrap_or(0)
                }
                DirEntryKind::Symlink { .. } => 0,
            }
        });

        let node = CasNode::Directory(DirectoryNode {
            entries: entries.clone(),
            total_size,
        });
        let (node_bytes, node_hash) = node.to_bytes_and_hash();
        let dir_display = if dir_path.is_empty() {
            ".".to_string()
        } else {
            dir_path.clone()
        };
        let _store_node_span = match (event_recorder, store_node_track_id) {
            (Some(recorder), Some(track_id)) => Some(recorder.begin_detached_span_with_args(
                track_id,
                "acquire.store_node",
                value!({
                    "display_name": ((format!("store_node({dir_display})")))
                }),
            )),
            _ => None,
        };
        store
            .put_node(node_hash, node_bytes)
            .await
            .map_err(|error| {
                format!("failed to store acquire directory node '{dir_path}': {error:?}")
            })?;

        dir_hashes.insert(dir_path.clone(), (node_hash, total_size));

        if !dir_path.is_empty() {
            let (parent, name) = split_tree_entry_path(dir_path);
            dir_entries
                .entry(parent.to_string())
                .or_default()
                .push(DirEntry {
                    name: name.to_string(),
                    kind: DirEntryKind::Dir { hash: node_hash },
                });
        }
    }

    dir_hashes
        .get("")
        .copied()
        .ok_or_else(|| "acquire tree has no root directory".to_string())
}

async fn load_acquired_blob_from_manifest(
    store: &StoreClient,
    manifest_hash: NodeHash,
) -> std::result::Result<Value, String> {
    let node_bytes = store
        .get_node(manifest_hash)
        .await
        .map_err(|error| format!("failed to load acquire manifest: {error:?}"))?
        .ok_or_else(|| format!("acquire manifest {manifest_hash} not found"))?;
    let node = CasNode::from_bytes(&node_bytes)
        .map_err(|error| format!("failed to decode acquire manifest {manifest_hash}: {error}"))?;
    let CasNode::Manifest(manifest) = node else {
        return Err(format!(
            "acquire cache entry {manifest_hash} is not a blob manifest"
        ));
    };
    let [output] = manifest.outputs.as_slice() else {
        return Err(format!(
            "acquire blob manifest {manifest_hash} must contain exactly one output"
        ));
    };
    Ok(Value::Blob(BlobValue::Store {
        blob_ref: blob_ref_from_output_blob_ref(&output.blob),
        executable: output.executable,
    }))
}

async fn publish_acquire_cache_entry(
    store: &StoreClient,
    cache_key: Blake3Hash,
    node_hash: NodeHash,
) -> std::result::Result<(), String> {
    let publish = store
        .publish(cache_key, node_hash)
        .await
        .map_err(|error| format!("failed to publish acquire cache entry: {error:?}"))?;
    if publish.success {
        Ok(())
    } else {
        Err(publish
            .error
            .unwrap_or_else(|| "failed to publish acquire cache entry".to_string()))
    }
}

pub(crate) async fn run_build_vx_acquire_via_store(
    store: &Arc<StoreClient>,
    request: EvalAcquireRequest,
    event_recorder: Option<&vx_eval::EventRecorder>,
) -> std::result::Result<Value, PoisonError> {
    let demand_chain = request.demand_chain.clone();
    let digest = AcquireDigest::parse(&request.digest)
        .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?;
    let extract_format = request
        .extract
        .as_deref()
        .map(AcquireArchiveFormat::parse)
        .transpose()
        .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?;
    let normalized_digest = digest.normalized_string();
    let cache_key = acquire_cache_key(
        &request.url,
        &normalized_digest,
        extract_format.map(AcquireArchiveFormat::cache_key_tag),
    );

    let cached = {
        let _lookup_span = event_recorder.map(|recorder| {
            EventSink::begin_span_with_args(
                recorder,
                "acquire.lookup",
                acquire_trace_args(&request, "acquire lookup"),
            )
        });
        store.lookup(cache_key).await.map_err(|error| {
            exec_bridge_poison(PoisonKind::Internal, format!("{error:?}"), &demand_chain)
        })?
    };

    let acquired = if let Some(cached) = cached {
        let _cached_span = event_recorder.map(|recorder| {
            EventSink::begin_span_with_args(
                recorder,
                "acquire.load_cached",
                acquire_trace_args(&request, "load cached acquire"),
            )
        });
        match extract_format {
            Some(_) => Value::Tree(TreeValue::Store(cached)),
            None => load_acquired_blob_from_manifest(store, cached)
                .await
                .map_err(|message| {
                    exec_bridge_poison(PoisonKind::Internal, message, &demand_chain)
                })?,
        }
    } else {
        let response = {
            let _download_span = event_recorder.map(|recorder| {
                EventSink::begin_span_with_args(
                    recorder,
                    "acquire.open_http",
                    acquire_trace_args(&request, "open acquire response"),
                )
            });
            vx_http::get(&request.url).await.map_err(|error| {
                exec_bridge_poison(
                    PoisonKind::UserFail,
                    format!("failed to acquire {}: {error}", request.url),
                    &demand_chain,
                )
            })?
        };
        let status = response.status();
        if !status.is_success() {
            return Err(exec_bridge_poison(
                PoisonKind::UserFail,
                format!("failed to acquire {}: HTTP {status}", request.url),
                &demand_chain,
            ));
        }

        let body_reader = tokio_util::io::StreamReader::new(
            response
                .into_body()
                .into_data_stream()
                .map(|result| result.map_err(std::io::Error::other)),
        );
        let mut reader = Box::pin(AcquireDigestVerifyingReader::new(body_reader, digest));
        let acquire_track_id = current_trace_track_id();
        let event_recorder_owned = event_recorder.cloned();
        let acquire_blob_tracks = event_recorder_owned.as_ref().map(|recorder| {
            (0..4)
                .map(|index| {
                    recorder.acquire_detached_track_with_descriptor(
                        format!("acquire.store_blob[{index}]"),
                        (acquire_track_id as i64) * 10 + index as i64 + 1,
                        Some(acquire_track_id),
                        None,
                        Some(index + 1),
                        Some(
                            vx_eval::perfetto_proto_generated::track_descriptor::SiblingMergeBehavior::None
                                as i32,
                        ),
                    )
                })
                .collect::<Vec<_>>()
        });
        let acquire_blob_track_ids = acquire_blob_tracks
            .as_ref()
            .map(|tracks| Arc::new(tracks.iter().map(|track| track.id()).collect::<Vec<_>>()));
        let next_acquire_blob_track = Arc::new(AtomicUsize::new(0));
        let acquire_store_node_track = event_recorder_owned.as_ref().map(|recorder| {
            recorder.acquire_detached_track_with_descriptor(
                "acquire.store_node".to_string(),
                (acquire_track_id as i64) * 10 + 10,
                Some(acquire_track_id),
                None,
                Some(10),
                Some(
                    vx_eval::perfetto_proto_generated::track_descriptor::SiblingMergeBehavior::None
                        as i32,
                ),
            )
        });

        match extract_format {
            Some(format) => {
                let store_for_extract = Arc::clone(store);
                let event_recorder_for_extract = event_recorder_owned.clone();
                let acquire_blob_track_ids = acquire_blob_track_ids.clone();
                let next_acquire_blob_track = Arc::clone(&next_acquire_blob_track);
                let (tree, extracted_files) = {
                    let _extract_span = event_recorder.map(|recorder| {
                        EventSink::begin_span_with_args(
                            recorder,
                            "acquire.extract",
                            acquire_trace_args(&request, "extract acquire archive"),
                        )
                    });
                    vx_tarball::extract_to_tree(
                        &mut reader,
                        format.compression(),
                        0,
                        move |path, mut entry_reader, _executable: bool| {
                            let store_for_extract = Arc::clone(&store_for_extract);
                            let event_recorder_for_extract = event_recorder_for_extract.clone();
                            let acquire_blob_track_ids = acquire_blob_track_ids.clone();
                            let next_acquire_blob_track = Arc::clone(&next_acquire_blob_track);
                            Box::pin(async move {
                                let _store_blob_span = match (
                                    event_recorder_for_extract.as_ref(),
                                    acquire_blob_track_ids.as_ref(),
                                ) {
                                    (Some(recorder), Some(track_ids)) if !track_ids.is_empty() => {
                                        let slot = next_acquire_blob_track
                                            .fetch_add(1, Ordering::Relaxed)
                                            % track_ids.len();
                                        Some(recorder.begin_detached_span_with_args(
                                            track_ids[slot],
                                            "acquire.store_blob",
                                            value!({
                                                "display_name": ((format!("store_blob({path})")))
                                            }),
                                        ))
                                    }
                                    _ => None,
                                };
                                let streamed =
                                    store_blob_stream(&store_for_extract, entry_reader.as_mut())
                                        .await?;
                                Ok(vx_tarball::ExtractedFile {
                                    content_hash: streamed.content_hash,
                                    blob_ref: streamed.blob_ref,
                                    size: streamed.size,
                                })
                            })
                        },
                    )
                    .await
                    .map_err(|error| {
                        exec_bridge_poison(
                            PoisonKind::UserFail,
                            format!("failed to extract {}: {error}", request.url),
                            &demand_chain,
                        )
                    })?
                };
                tokio::io::copy(&mut reader, &mut tokio::io::sink())
                    .await
                    .map_err(|error| {
                        exec_bridge_poison(
                            PoisonKind::UserFail,
                            format!("failed to verify {}: {error}", request.url),
                            &demand_chain,
                        )
                    })?;
                let files = extracted_files
                    .into_iter()
                    .map(|(_, extracted, _)| (extracted.content_hash.0, extracted.blob_ref))
                    .collect::<HashMap<_, _>>();
                let (root, _) = {
                    let _manifest_span = event_recorder.map(|recorder| {
                        EventSink::begin_span_with_args(
                            recorder,
                            "acquire.store_tree",
                            acquire_trace_args(&request, "store acquire tree"),
                        )
                    });
                    store_acquired_tree_manifest(
                        store,
                        &tree,
                        &files,
                        event_recorder_owned.as_ref(),
                        acquire_store_node_track.as_ref().map(|track| track.id()),
                    )
                    .await
                    .map_err(|message| {
                        exec_bridge_poison(PoisonKind::Internal, message, &demand_chain)
                    })?
                };
                {
                    let _publish_span = event_recorder.map(|recorder| {
                        EventSink::begin_span_with_args(
                            recorder,
                            "acquire.publish_cache",
                            acquire_trace_args(&request, "publish acquire cache"),
                        )
                    });
                    publish_acquire_cache_entry(store, cache_key, root)
                        .await
                        .map_err(|message| {
                            exec_bridge_poison(PoisonKind::Internal, message, &demand_chain)
                        })?;
                }
                Value::Tree(TreeValue::Store(root))
            }
            None => {
                let blob_ref = {
                    let _store_blob_span = event_recorder.map(|recorder| {
                        EventSink::begin_span_with_args(
                            recorder,
                            "acquire.store_blob",
                            acquire_trace_args(&request, "store acquire blob"),
                        )
                    });
                    store_blob_stream(store, &mut reader)
                        .await
                        .map(|streamed| streamed.blob_ref)
                        .map_err(|message| {
                            exec_bridge_poison(PoisonKind::Internal, message, &demand_chain)
                        })?
                };

                let manifest = NodeManifest {
                    node_id: NodeId("acquire:blob".to_string()),
                    cache_key,
                    produced_at: String::new(),
                    outputs: vec![OutputEntry {
                        logical: "blob".to_string(),
                        filename: "acquired".to_string(),
                        blob: output_blob_ref_from_blob_ref(&blob_ref),
                        executable: false,
                    }],
                };
                let (node_bytes, manifest_hash) = CasNode::Manifest(manifest).to_bytes_and_hash();
                {
                    let _manifest_span = event_recorder.map(|recorder| {
                        EventSink::begin_span_with_args(
                            recorder,
                            "acquire.store_manifest",
                            acquire_trace_args(&request, "store acquire manifest"),
                        )
                    });
                    store
                        .put_node(manifest_hash, node_bytes)
                        .await
                        .map_err(|error| {
                            exec_bridge_poison(
                                PoisonKind::Internal,
                                format!("{error:?}"),
                                &demand_chain,
                            )
                        })?;
                }
                {
                    let _publish_span = event_recorder.map(|recorder| {
                        EventSink::begin_span_with_args(
                            recorder,
                            "acquire.publish_cache",
                            acquire_trace_args(&request, "publish acquire cache"),
                        )
                    });
                    publish_acquire_cache_entry(store, cache_key, manifest_hash)
                        .await
                        .map_err(|message| {
                            exec_bridge_poison(PoisonKind::Internal, message, &demand_chain)
                        })?;
                }
                Value::Blob(BlobValue::Store {
                    blob_ref,
                    executable: false,
                })
            }
        }
    };

    Ok(acquired)
}

fn remote_trace_span_name(span: RemoteTraceSpan) -> &'static str {
    match span {
        RemoteTraceSpan::RunnerPrepareVfs => "exec.runner.prepare_vfs",
        RemoteTraceSpan::RunnerCommand => "exec.runner.command",
        RemoteTraceSpan::RunnerObserver => "exec.runner.observer",
        RemoteTraceSpan::RunnerObserverInvoke => "exec.runner.observer.invoke",
        RemoteTraceSpan::RunnerObserverCommit => "exec.runner.observer.commit",
        RemoteTraceSpan::RunnerObserverCommitTree => "exec.runner.observer.commit_tree",
        RemoteTraceSpan::RunnerObserverCommitStoreBlob => "exec.runner.observer.store_blob",
        RemoteTraceSpan::RunnerObserverCommitStoreNode => "exec.runner.observer.store_node",
        RemoteTraceSpan::RunnerFlushOutputs => "exec.runner.flush_outputs",
        RemoteTraceSpan::RunnerStoreManifest => "exec.runner.store_manifest",
        RemoteTraceSpan::RunnerCleanup => "exec.runner.cleanup",
    }
}

fn exec_argv_strings(args: &[ExecArgValue]) -> Vec<String> {
    args.iter().map(|arg| arg.as_str().to_string()).collect()
}

fn format_exec_command(argv: &[String]) -> String {
    argv.join(" ")
}

async fn run_build_vx_exec_via_runner(
    store: &Arc<StoreClient>,
    executors: &BuildVxExecutors,
    activity_registry: &ActivityRegistry,
    exec_mount_prep_inflight: &Arc<SyncMutex<HashMap<String, ExecMountPrepFuture>>>,
    exec_fs_tree_snapshot_cache: &Arc<SyncMutex<HashMap<String, BuildVxFsTreeSnapshotFuture>>>,
    progress_listener: Option<Arc<ProgressListenerClient>>,
    event_recorder: Option<vx_eval::EventRecorder>,
    request: ExecRequest,
) -> std::result::Result<Value, PoisonError> {
    let demand_chain = request.demand_chain.clone();
    let executor_summary = describe_executor_selector(&request.executor);
    let command_argv = exec_argv_strings(&request.args);
    let command = format_exec_command(&command_argv);
    let command_preview = summarize_items(&command_argv, 12);
    let _exec_span = event_recorder.as_ref().map(|recorder| {
        EventSink::begin_span_with_args(
            recorder,
            "exec.runtime",
            value!({
                "display_name": ((format!("exec {command_preview}"))),
                "executor": ((executor_summary.clone())),
                "command": ((command.clone())),
                "command_preview": ((command_preview.clone())),
                "mount_count": ((request.mounts.len() as u64)),
                "writable_root_count": ((request.writable_roots.len() as u64)),
                "create_dir_count": ((request.create_dirs.len() as u64))
            }),
        )
    });
    let activity = start_build_vx_activity(
        activity_registry,
        "exec",
        &request.executor,
        summarize_build_vx_exec_request(&request),
    );
    let exec_started = Instant::now();
    emit_build_vx_progress_activity(
        &progress_listener,
        &activity,
        format!("resolving build.vx executor {}", executor_summary),
    )
    .await;
    let (exec, exec_store, store_supports_readthrough, host_relation) = {
        let _resolve_executor_span = event_recorder.as_ref().map(|recorder| {
            EventSink::begin_span_with_args(
                recorder,
                "exec.resolve_executor",
                value!({
                    "executor": ((executor_summary.clone()))
                }),
            )
        });
        let resolved = executors
            .resolved_executor_for(&request.executor, progress_listener.clone())
            .await
            .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?;
        (
            resolved.runner,
            resolved.store,
            resolved.store_supports_readthrough,
            resolved.host_relation,
        )
    };
    emit_build_vx_progress_activity(
        &progress_listener,
        &activity,
        format!("build.vx executor {} ready", executor_summary),
    )
    .await;
    let target_store = exec_store.as_deref().unwrap_or(store);
    let mut args = request.args.into_iter();
    let program = args.next().ok_or_else(|| {
        exec_bridge_poison(
            PoisonKind::UserFail,
            "exec requires at least one program argument",
            &demand_chain,
        )
    })?;
    let mount_prep_started = Instant::now();
    emit_build_vx_progress_activity(
        &progress_listener,
        &activity,
        format!(
            "build.vx executor {}: preparing exec mounts",
            executor_summary
        ),
    )
    .await;
    let prepared_mounts = {
        let _mount_prep_span = event_recorder.as_ref().map(|recorder| {
            EventSink::begin_span_with_args(
                recorder,
                "exec.mounts.prepare",
                value!({
                    "executor": ((executor_summary.clone())),
                    "mount_count": ((request.mounts.len() as u64)),
                    "host_relation": ((host_relation.as_str()))
                }),
            )
        });
        store_exec_mounts_deduped(
            store,
            &request.mounts,
            exec_fs_tree_snapshot_cache,
            &progress_listener,
            Some(&activity),
            Some(&executor_summary),
            host_relation,
            exec_mount_prep_inflight,
            event_recorder.as_ref(),
        )
        .await
        .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?
    };
    tracing::info!(
        executor = %describe_executor_selector(&request.executor),
        tree_mount_count = prepared_mounts.tree_mounts.len(),
        file_mount_count = prepared_mounts.file_mounts.len(),
        cross_store = !std::ptr::eq(target_store, &**store),
        host_relation = %host_relation.as_str(),
        store_supports_readthrough,
        "build.vx exec prepared mounts"
    );
    emit_build_vx_progress_activity(
        &progress_listener,
        &activity,
        format!(
            "build.vx executor {}: exec mounts ready ({})",
            executor_summary,
            format_progress_elapsed(mount_prep_started.elapsed())
        ),
    )
    .await;
    let prepared_mounts = if !std::ptr::eq(target_store, &**store) && !store_supports_readthrough {
        let mirror_started = Instant::now();
        emit_build_vx_progress_activity(
            &progress_listener,
            &activity,
            format!(
                "build.vx executor {}: mirroring exec mounts",
                executor_summary
            ),
        )
        .await;
        let mirrored_mounts = {
            let _mirror_span = event_recorder.as_ref().map(|recorder| {
                EventSink::begin_span_with_args(
                    recorder,
                    "exec.mounts.mirror",
                    value!({
                        "executor": ((executor_summary.clone())),
                        "tree_mount_count": ((prepared_mounts.tree_mounts.len() as u64)),
                        "file_mount_count": ((prepared_mounts.file_mounts.len() as u64))
                    }),
                )
            });
            mirror_prepared_exec_mounts(store, target_store, &prepared_mounts)
                .await
                .map_err(|message| {
                    exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain)
                })?
        };
        emit_build_vx_progress_activity(
            &progress_listener,
            &activity,
            format!(
                "build.vx executor {}: mirrored exec mounts ({})",
                executor_summary,
                format_progress_elapsed(mirror_started.elapsed())
            ),
        )
        .await;
        mirrored_mounts
    } else {
        prepared_mounts
    };
    emit_build_vx_progress_activity(
        &progress_listener,
        &activity,
        format!(
            "build.vx executor {}: prepared exec inputs ({})",
            executor_summary,
            format_progress_elapsed(exec_started.elapsed())
        ),
    )
    .await;
    let (progress, mut progress_rx) = roam::channel::<StoreProgress>();
    let progress_listener_for_exec = progress_listener.clone();
    let executor_summary_for_progress = executor_summary.clone();
    let activity_for_progress = activity.clone();
    let observer = if std::ptr::eq(target_store, &**store) || store_supports_readthrough {
        request.observer
    } else {
        mirror_shipped_function_store_refs(store, target_store, request.observer)
            .await
            .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?
    };

    emit_build_vx_progress_activity(
        &progress_listener,
        &activity,
        format!("build.vx executor {}: calling runner.run", executor_summary),
    )
    .await;
    let result = {
        let _runner_span = event_recorder.as_ref().map(|recorder| {
            EventSink::begin_span_with_args(
                recorder,
                "exec.runner.run",
                value!({
                    "display_name": ((format!("run {command_preview}"))),
                    "executor": ((executor_summary.clone())),
                    "command": ((command.clone())),
                    "command_preview": ((command_preview.clone())),
                    "tree_mount_count": ((prepared_mounts.tree_mounts.len() as u64)),
                    "file_mount_count": ((prepared_mounts.file_mounts.len() as u64)),
                    "cross_store": ((!std::ptr::eq(target_store, &**store))),
                    "store_supports_readthrough": ((store_supports_readthrough))
                }),
            )
        });
        let runner_track_id = current_trace_track_id();
        let event_recorder_for_progress = event_recorder.clone();
        let progress_handle = moire::task::spawn(
            async move {
                let store_blob_track = event_recorder_for_progress.as_ref().map(|recorder| {
                    recorder.acquire_detached_track_with_descriptor(
                        "exec.runner.observer.store_blob".to_string(),
                        (runner_track_id as i64) * 10 + 1,
                        Some(runner_track_id),
                        None,
                        Some(1),
                        Some(
                            vx_eval::perfetto_proto_generated::track_descriptor::SiblingMergeBehavior::None
                                as i32,
                        ),
                    )
                });
                let store_node_track = event_recorder_for_progress.as_ref().map(|recorder| {
                    recorder.acquire_detached_track_with_descriptor(
                        "exec.runner.observer.store_node".to_string(),
                        (runner_track_id as i64) * 10 + 2,
                        Some(runner_track_id),
                        None,
                        Some(2),
                        Some(
                            vx_eval::perfetto_proto_generated::track_descriptor::SiblingMergeBehavior::None
                                as i32,
                        ),
                    )
                });
                let mut remote_trace_spans =
                    HashMap::<(RemoteTraceSpan, u64), vx_eval::events::DetachedRecorderSpan>::new();
                while let Ok(Some(update)) = progress_rx.recv().await {
                    if let Some(trace_event) = update.trace_event.as_ref()
                        && let Some(recorder) = event_recorder_for_progress.as_ref()
                    {
                        let trace_track_id = match trace_event.span {
                            RemoteTraceSpan::RunnerObserverCommitStoreBlob => store_blob_track
                                .as_ref()
                                .map_or(runner_track_id, |track| track.id()),
                            RemoteTraceSpan::RunnerObserverCommitStoreNode => store_node_track
                                .as_ref()
                                .map_or(runner_track_id, |track| track.id()),
                            _ => runner_track_id,
                        };
                        match trace_event.phase {
                            RemoteTracePhase::Begin => {
                                let span_name = remote_trace_span_name(trace_event.span);
                                let span = if trace_event.label.is_empty() {
                                    recorder.begin_detached_span(trace_track_id, span_name)
                                } else {
                                    recorder.begin_detached_span_with_args(
                                        trace_track_id,
                                        span_name,
                                        value!({
                                            "display_name": ((trace_event.label.clone()))
                                        }),
                                    )
                                };
                                remote_trace_spans
                                    .insert((trace_event.span, trace_event.instance_id), span);
                            }
                            RemoteTracePhase::End => {
                                remote_trace_spans
                                    .remove(&(trace_event.span, trace_event.instance_id));
                            }
                            RemoteTracePhase::Instant => {
                                recorder.instant_on_track(
                                    trace_track_id,
                                    remote_trace_span_name(trace_event.span),
                                );
                            }
                        }
                    }

                    if !update.message.is_empty() {
                        let message = format!(
                            "build.vx executor {}: {}",
                            executor_summary_for_progress, update.message
                        );
                        emit_build_vx_remote_progress_activity(
                            &progress_listener_for_exec,
                            &activity_for_progress,
                            message,
                        )
                        .await;
                    }
                }
                drop(store_blob_track);
                drop(store_node_track);
                drop(remote_trace_spans);
            }
            .named("build_vx_exec_progress_fwd"),
        );
        let run_result = exec
            .run(
                RunRequest {
                    executor: request.executor.clone(),
                    program: exec_arg_to_run_arg(&program),
                    args: args.map(|arg| exec_arg_to_run_arg(&arg)).collect(),
                    tree_mounts: prepared_mounts.tree_mounts,
                    file_mounts: prepared_mounts.file_mounts,
                    writable_roots: request
                        .writable_roots
                        .iter()
                        .map(|path| path.as_str().to_string())
                        .collect(),
                    create_dirs: request
                        .create_dirs
                        .iter()
                        .map(|path| path.as_str().to_string())
                        .collect(),
                    cwd: None,
                    env: request
                        .env
                        .iter()
                        .map(|(key, value)| (key.clone(), exec_arg_to_run_arg(value)))
                        .collect(),
                    clear_env: false,
                    eval: Some(RunEvalPayload {
                        module: request.module,
                        observer: Some(observer),
                        verification_obligations: request.verification_obligations,
                    }),
                    macos_commandlinetools_version: None,
                },
                progress,
            )
            .await;
        let _ = progress_handle.await;
        run_result.map_err(|error| {
            exec_bridge_poison(PoisonKind::UserFail, format!("{error:?}"), &demand_chain)
        })?
    };
    emit_build_vx_progress_activity(
        &progress_listener,
        &activity,
        format!(
            "build.vx executor {}: exec returned ({})",
            executor_summary,
            format_progress_elapsed(exec_started.elapsed())
        ),
    )
    .await;
    tracing::info!(
        executor = %executor_summary,
        success = result.success,
        has_observer_value = result.observer_value.is_some(),
        "build.vx exec returned from runner"
    );

    if !result.success {
        if let Some(poison) = result.poison {
            let mut error = PoisonError::from(poison);
            if !result.stderr.trim().is_empty() {
                error = error.with_context(format!("process stderr:\n{}", result.stderr.trim()));
            }
            if !result.stdout.trim().is_empty() {
                error = error.with_context(format!("process stdout:\n{}", result.stdout.trim()));
            }
            return Err(error);
        }
        let kind = match result.error_kind {
            Some(RunFailureKind::VerificationFailure) => PoisonKind::VerificationFailure,
            _ => PoisonKind::UserFail,
        };
        return Err(exec_bridge_poison(
            kind,
            result.error.unwrap_or(result.stderr),
            &demand_chain,
        ));
    }

    let observer_value = result.observer_value.ok_or_else(|| {
        exec_bridge_poison(
            PoisonKind::UserFail,
            "runner did not return an observer value",
            &demand_chain,
        )
    })?;
    let observer_bridge_started = Instant::now();
    emit_build_vx_progress_activity(
        &progress_listener,
        &activity,
        format!(
            "build.vx executor {}: bridging observer value",
            executor_summary
        ),
    )
    .await;
    let observer_value = {
        let _observer_bridge_span = event_recorder.as_ref().map(|recorder| {
            EventSink::begin_span_with_args(
                recorder,
                "exec.observer.bridge",
                value!({
                    "executor": ((executor_summary.clone())),
                    "cross_store": ((!std::ptr::eq(target_store, &**store)))
                }),
            )
        });
        tracing::info!(executor = %executor_summary, "build.vx exec starting observer-value bridge");
        if std::ptr::eq(target_store, &**store) {
            observer_value
        } else {
            mirror_transport_value_store_refs(target_store, store, observer_value)
                .await
                .map_err(|message| {
                    exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain)
                })?
        }
    };
    tracing::info!(executor = %executor_summary, "build.vx exec finished observer-value bridge");
    emit_build_vx_progress_activity(
        &progress_listener,
        &activity,
        format!(
            "build.vx executor {}: observer value ready ({})",
            executor_summary,
            format_progress_elapsed(observer_bridge_started.elapsed())
        ),
    )
    .await;
    let value = Value::try_from(observer_value).map_err(|error| {
        exec_bridge_poison(PoisonKind::UserFail, error.to_string(), &demand_chain)
    })?;
    Ok(value)
}

async fn run_build_vx_discover_via_runner(
    store: &Arc<StoreClient>,
    executors: &BuildVxExecutors,
    activity_registry: &ActivityRegistry,
    progress_listener: Option<Arc<ProgressListenerClient>>,
    event_recorder: Option<vx_eval::EventRecorder>,
    request: EvalDiscoverRequest,
) -> std::result::Result<Value, PoisonError> {
    let demand_chain = request.demand_chain.clone();
    let discover_started = Instant::now();
    let executor_summary = describe_executor_selector(&request.executor);
    let _discover_span = event_recorder.as_ref().map(|recorder| {
        EventSink::begin_span_with_args(
            recorder,
            "discover.runtime",
            value!({
                "display_name": "discover(..)",
                "executor": ((executor_summary.clone()))
            }),
        )
    });
    let activity = start_build_vx_activity(
        activity_registry,
        "discover",
        &request.executor,
        summarize_build_vx_discover_request(&request),
    );
    emit_build_vx_progress_activity(
        &progress_listener,
        &activity,
        format!("resolving build.vx executor {}", executor_summary),
    )
    .await;
    let (exec, exec_store, store_supports_readthrough) = {
        let _resolve_executor_span = event_recorder.as_ref().map(|recorder| {
            EventSink::begin_span_with_args(
                recorder,
                "discover.resolve_executor",
                value!({
                    "executor": ((executor_summary.clone()))
                }),
            )
        });
        let exec = executors
            .runner_for(&request.executor, progress_listener.clone())
            .await
            .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?;
        let exec_store = executors
            .store_for(&request.executor, progress_listener.clone())
            .await
            .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?;
        let store_supports_readthrough = executors
            .store_supports_readthrough(&request.executor, progress_listener.clone())
            .await
            .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?;
        (exec, exec_store, store_supports_readthrough)
    };
    emit_build_vx_progress_activity(
        &progress_listener,
        &activity,
        format!("build.vx executor {} ready", executor_summary),
    )
    .await;
    let target_store = exec_store.as_deref().unwrap_or(store);
    let probe = if std::ptr::eq(target_store, &**store) || store_supports_readthrough {
        request.probe
    } else {
        mirror_shipped_function_store_refs(store, target_store, request.probe)
            .await
            .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?
    };
    let result = {
        let _runner_span = event_recorder.as_ref().map(|recorder| {
            EventSink::begin_span_with_args(
                recorder,
                "discover.runner.call",
                value!({
                    "display_name": "runner.discover(..)",
                    "executor": ((executor_summary.clone())),
                    "cross_store": ((!std::ptr::eq(target_store, &**store))),
                    "store_supports_readthrough": ((store_supports_readthrough))
                }),
            )
        });
        exec.discover(RunnerDiscoverRequest {
            executor: request.executor,
            module: request.module,
            probe,
        })
        .await
        .map_err(|error| {
            exec_bridge_poison(PoisonKind::UserFail, format!("{error:?}"), &demand_chain)
        })?
    };
    emit_build_vx_progress_activity(
        &progress_listener,
        &activity,
        format!(
            "build.vx executor {}: discover returned ({})",
            executor_summary,
            format_progress_elapsed(discover_started.elapsed())
        ),
    )
    .await;

    if let Some(poison) = result.poison {
        return Err(PoisonError::from(poison));
    }

    let value = {
        let _bridge_span = event_recorder.as_ref().map(|recorder| {
            EventSink::begin_span_with_args(
                recorder,
                "discover.result.bridge",
                value!({
                    "executor": ((executor_summary.clone())),
                    "cross_store": ((!std::ptr::eq(target_store, &**store)))
                }),
            )
        });
        if std::ptr::eq(target_store, &**store) {
            result.value.ok_or_else(|| {
                exec_bridge_poison(
                    PoisonKind::UserFail,
                    "runner did not return a discovery value",
                    &demand_chain,
                )
            })?
        } else {
            mirror_transport_value_store_refs(
                target_store,
                store,
                result.value.ok_or_else(|| {
                    exec_bridge_poison(
                        PoisonKind::UserFail,
                        "runner did not return a discovery value",
                        &demand_chain,
                    )
                })?,
            )
            .await
            .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?
        }
    };
    let value = Value::try_from(value).map_err(|error| {
        exec_bridge_poison(PoisonKind::UserFail, error.to_string(), &demand_chain)
    })?;
    let hydration_started = Instant::now();
    emit_build_vx_progress_activity(
        &progress_listener,
        &activity,
        format!(
            "build.vx executor {}: hydrating discovery result",
            executor_summary
        ),
    )
    .await;
    let value = {
        let _hydrate_span = event_recorder.as_ref().map(|recorder| {
            EventSink::begin_span_with_args(
                recorder,
                "discover.result.hydrate",
                value!({
                    "executor": ((executor_summary.clone()))
                }),
            )
        });
        hydrate_exec_value_from_store(Arc::clone(store), value, demand_chain.clone()).await?
    };
    emit_build_vx_progress_activity(
        &progress_listener,
        &activity,
        format!(
            "build.vx executor {}: discovery hydration finished ({})",
            executor_summary,
            format_progress_elapsed(hydration_started.elapsed())
        ),
    )
    .await;
    Ok(value)
}

fn exec_bridge_poison(
    kind: PoisonKind,
    message: impl Into<String>,
    demand_chain: &DemandChain,
) -> PoisonError {
    PoisonError::new(kind, message, demand_chain.clone())
}

fn summarize_items(items: &[String], limit: usize) -> String {
    if items.is_empty() {
        return "-".to_string();
    }

    let mut summary = items.iter().take(limit).cloned().collect::<Vec<_>>();
    if items.len() > limit {
        summary.push(format!("... ({} total)", items.len()));
    }
    summary.join(", ")
}

pub(crate) fn summarize_build_vx_exec_request(request: &ExecRequest) -> String {
    let argv = request
        .args
        .iter()
        .map(|arg| arg.as_str().to_string())
        .collect::<Vec<_>>();
    let mounts = request
        .mounts
        .iter()
        .map(|mount| mount.path.as_str().to_string())
        .collect::<Vec<_>>();
    let writable_roots = request
        .writable_roots
        .iter()
        .map(|path| path.as_str().to_string())
        .collect::<Vec<_>>();
    let create_dirs = request
        .create_dirs
        .iter()
        .map(|path| path.as_str().to_string())
        .collect::<Vec<_>>();

    format!(
        "build.vx exec[{}:{}]: {} (mounts: {}; writable: {}; create_dirs: {})",
        request.executor.backend.as_tag(),
        request.executor.id,
        summarize_items(&argv, 10),
        summarize_items(&mounts, 6),
        summarize_items(&writable_roots, 6),
        summarize_items(&create_dirs, 6),
    )
}

pub(crate) fn summarize_build_vx_discover_request(request: &EvalDiscoverRequest) -> String {
    format!(
        "build.vx discover[{}:{}]",
        request.executor.backend.as_tag(),
        request.executor.id
    )
}

pub(crate) fn summarize_build_vx_acquire_request(request: &EvalAcquireRequest) -> String {
    match &request.extract {
        Some(extract) => format!(
            "build.vx acquire: url={} digest={} extract={extract}",
            request.url, request.digest
        ),
        None => format!(
            "build.vx acquire: url={} digest={}",
            request.url, request.digest
        ),
    }
}

pub(crate) fn summarize_build_vx_render_request(request: &EvalRenderRequest) -> String {
    format!("build.vx render: template={}", request.template)
}

pub(crate) fn summarize_build_vx_stabilize_request(request: &EvalStabilizeRequest) -> String {
    format!(
        "build.vx stabilize: resolve={} witness_type={}",
        request.resolve.code_identity,
        request.witness.type_name(),
    )
}

pub(crate) fn summarize_build_vx_import_request(request: &EvalImportRequest) -> String {
    format!(
        "build.vx import: path={} from={}",
        request.path, request.from_source_id
    )
}

#[derive(Clone)]
struct BuildVxRuntime {
    store: Arc<StoreClient>,
    executors: Arc<BuildVxExecutors>,
    activity_registry: ActivityRegistry,
    exec_mount_prep_inflight: Arc<SyncMutex<HashMap<String, ExecMountPrepFuture>>>,
    workspace_root: Utf8PathBuf,
    config_module_path: Option<Utf8PathBuf>,
    progress_listener: Option<Arc<ProgressListenerClient>>,
    exec_fs_tree_snapshot_cache: Arc<SyncMutex<HashMap<String, BuildVxFsTreeSnapshotFuture>>>,
    module_cache: Arc<SyncMutex<HashMap<Utf8PathBuf, EvalModule>>>,
    module_stack: Arc<SyncMutex<HashSet<Utf8PathBuf>>>,
    event_recorder: Option<vx_eval::EventRecorder>,
}

async fn emit_build_vx_boundary_log(listener: &Option<Arc<ProgressListenerClient>>, message: &str) {
    if let Some(listener) = listener {
        let _ = listener.log_message(message.to_string()).await;
    }
}

async fn emit_build_vx_progress_message(
    listener: &Option<Arc<ProgressListenerClient>>,
    message: impl Into<String>,
) {
    let message = message.into();
    if let Some(listener) = listener
        && let Err(error) = listener.log_message(message).await
    {
        tracing::warn!("failed to emit build.vx progress message: {error:?}");
    }
}

fn start_build_vx_activity(
    registry: &ActivityRegistry,
    kind: &str,
    executor: &ExecutorSelector,
    label: impl Into<String>,
) -> ActivityHandle {
    registry.start(
        kind,
        label.into(),
        Some(describe_executor_selector(executor)),
    )
}

async fn emit_build_vx_progress_activity(
    listener: &Option<Arc<ProgressListenerClient>>,
    activity: &ActivityHandle,
    message: impl Into<String>,
) {
    let message = message.into();
    activity.update(message.clone());
    emit_build_vx_progress_message(listener, message).await;
}

async fn emit_build_vx_remote_progress_activity(
    listener: &Option<Arc<ProgressListenerClient>>,
    activity: &ActivityHandle,
    message: impl Into<String>,
) {
    let message = message.into();
    activity.update_remote(message.clone());
    emit_build_vx_progress_message(listener, message).await;
}

fn format_progress_elapsed(elapsed: std::time::Duration) -> String {
    if elapsed.as_secs_f64() >= 1.0 {
        format!("{:.2}s", elapsed.as_secs_f64())
    } else {
        format!("{}ms", elapsed.as_millis())
    }
}

fn describe_executor_selector(selector: &ExecutorSelector) -> String {
    format!("{}:{}", selector.backend.as_tag(), selector.id)
}

fn stabilize_cache_key(resolve: &[u8], witness: &[u8]) -> Blake3Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"stabilize-cache-v1\n");
    hasher.update(b"resolve:");
    hasher.update(resolve);
    hasher.update(b"\n");
    hasher.update(b"witness:");
    hasher.update(witness);
    hasher.update(b"\n");
    Blake3Hash(*hasher.finalize().as_bytes())
}

async fn load_stabilized_value_from_manifest(
    store: &StoreClient,
    manifest_hash: NodeHash,
) -> std::result::Result<Value, String> {
    let node_bytes = store
        .get_node(manifest_hash)
        .await
        .map_err(|error| format!("failed to load stabilize manifest: {error:?}"))?
        .ok_or_else(|| format!("stabilize manifest {manifest_hash} not found"))?;
    let node = CasNode::from_bytes(&node_bytes)
        .map_err(|error| format!("failed to decode stabilize manifest {manifest_hash}: {error}"))?;
    let CasNode::Manifest(manifest) = node else {
        return Err(format!(
            "stabilize cache entry {manifest_hash} is not a value manifest"
        ));
    };
    let [output] = manifest.outputs.as_slice() else {
        return Err(format!(
            "stabilize manifest {manifest_hash} must contain exactly one output"
        ));
    };
    let bytes = fetch_store_blob_bytes(store, &blob_ref_from_output_blob_ref(&output.blob))
        .await
        .map_err(|error| format!("failed to load stabilized value blob: {error}"))?;
    let transport = from_slice::<TransportValue>(&bytes)
        .map_err(|error| format!("failed to deserialize stabilized value: {error}"))?;
    Value::try_from(transport)
        .map_err(|error| format!("failed to decode stabilized value: {error}"))
}

impl BuildVxRuntime {
    fn build_engine(&self, module: &EvalModule) -> Arc<Engine> {
        let engine = Engine::from_module(module).with_runtime(Arc::new(self.clone()));
        if let Some(recorder) = &self.event_recorder {
            engine.with_event_sink(Arc::new(recorder.clone()))
        } else {
            engine
        }
    }

    async fn emit_boundary_log(&self, message: &str) {
        emit_build_vx_boundary_log(&self.progress_listener, message).await;
    }

    async fn load_imported_module(
        &self,
        request: EvalImportRequest,
    ) -> std::result::Result<EvalModule, PoisonError> {
        let module_path = self.resolve_import_path(&request)?;
        if let Some(module) = self.module_cache.lock().get(&module_path).cloned() {
            return Ok(module);
        }

        {
            let mut stack = self.module_stack.lock();
            if !stack.insert(module_path.clone()) {
                return Err(exec_bridge_poison(
                    PoisonKind::UserFail,
                    format!("cyclic import detected while loading {}", module_path),
                    &request.demand_chain,
                ));
            }
        }

        let outcome = async {
            let source = fs::read_to_string(&module_path).map_err(|error| {
                exec_bridge_poison(
                    PoisonKind::UserFail,
                    format!("failed to read imported module {}: {error}", module_path),
                    &request.demand_chain,
                )
            })?;

            let parse = vx_parser::parse(&source);
            if !parse.errors().is_empty() {
                let joined = parse
                    .errors()
                    .iter()
                    .map(|error| format!("{error:?}"))
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(exec_bridge_poison(
                    PoisonKind::UserFail,
                    format!(
                        "syntax error in imported module {}: {}",
                        module_path, joined
                    ),
                    &request.demand_chain,
                ));
            }

            let root = SourceFile::cast_root(parse.syntax());
            lower_source_file_with_source_id(&root, module_path.as_str().to_string()).map_err(
                |error| {
                    exec_bridge_poison(
                        PoisonKind::UserFail,
                        format!(
                            "language error in imported module {}: {}",
                            module_path, error.message
                        ),
                        &request.demand_chain,
                    )
                },
            )
        }
        .await;

        self.module_stack.lock().remove(&module_path);

        let module = outcome?;
        self.module_cache
            .lock()
            .insert(module_path.clone(), module.clone());
        Ok(module)
    }

    fn resolve_import_path(
        &self,
        request: &EvalImportRequest,
    ) -> std::result::Result<Utf8PathBuf, PoisonError> {
        resolve_build_vx_import_path(self.config_module_path.as_ref(), request)
    }
}

pub(crate) fn resolve_build_vx_import_path(
    config_module_path: Option<&Utf8PathBuf>,
    request: &EvalImportRequest,
) -> std::result::Result<Utf8PathBuf, PoisonError> {
    if request.path == BUILD_VX_CONFIG_IMPORT_PATH {
        return config_module_path.cloned().ok_or_else(|| {
            exec_bridge_poison(
                PoisonKind::UserFail,
                format!(
                    "import {} is unavailable because VIXEN_CONFIG_PATH is not set and no default config was found",
                    request.path
                ),
                &request.demand_chain,
            )
        });
    }

    let from_source = Utf8Path::new(&request.from_source_id);
    let from_dir = from_source.parent().ok_or_else(|| {
        exec_bridge_poison(
            PoisonKind::UserFail,
            format!(
                "cannot resolve import {} from source {}",
                request.path, request.from_source_id
            ),
            &request.demand_chain,
        )
    })?;

    let requested = Utf8Path::new(&request.path);
    let candidates = if requested.extension().is_some() {
        vec![from_dir.join(requested)]
    } else {
        vec![
            from_dir.join(requested),
            from_dir.join(format!("{}.vx", request.path)),
        ]
    };

    candidates
        .into_iter()
        .find(|candidate| candidate.exists())
        .ok_or_else(|| {
            exec_bridge_poison(
                PoisonKind::UserFail,
                format!(
                    "import {} from {} did not resolve to a file",
                    request.path, request.from_source_id
                ),
                &request.demand_chain,
            )
        })
}

impl BuildVxRuntime {
    async fn run_stabilize_via_store(
        &self,
        request: EvalStabilizeRequest,
    ) -> std::result::Result<Value, PoisonError> {
        let demand_chain = request.demand_chain.clone();
        let witness = TransportValue::try_from(&request.witness).map_err(|message| {
            exec_bridge_poison(
                PoisonKind::TypeError,
                format!("stabilize witness must be transportable: {message}"),
                &demand_chain,
            )
        })?;
        let witness_bytes = to_vec(&witness).map_err(|error| {
            exec_bridge_poison(
                PoisonKind::Internal,
                format!("failed to serialize stabilize witness: {error}"),
                &demand_chain,
            )
        })?;
        let resolve_bytes = to_vec(&request.resolve).map_err(|error| {
            exec_bridge_poison(
                PoisonKind::Internal,
                format!("failed to serialize stabilize resolve callback: {error}"),
                &demand_chain,
            )
        })?;
        let cache_key = stabilize_cache_key(&resolve_bytes, &witness_bytes);

        if let Some(cached) = self.store.lookup(cache_key).await.map_err(|error| {
            exec_bridge_poison(PoisonKind::Internal, format!("{error:?}"), &demand_chain)
        })? {
            return load_stabilized_value_from_manifest(&self.store, cached)
                .await
                .map_err(|message| {
                    exec_bridge_poison(PoisonKind::Internal, message, &demand_chain)
                });
        }

        let module: EvalModule = request.module.clone().into();
        let engine = self.build_engine(&module);
        let value = engine
            .call_shipped(&request.resolve, vec![request.witness])
            .await?;
        let transport = TransportValue::try_from(&value).map_err(|message| {
            exec_bridge_poison(
                PoisonKind::TypeError,
                format!("stabilize resolve result must be transportable: {message}"),
                &demand_chain,
            )
        })?;
        let bytes = to_vec(&transport).map_err(|error| {
            exec_bridge_poison(
                PoisonKind::Internal,
                format!("failed to serialize stabilized value: {error}"),
                &demand_chain,
            )
        })?;
        let prepared = prepare_blob(&bytes);
        let blob_ref = store_prepared_blob(&self.store, &prepared, &bytes)
            .await
            .map_err(|message| exec_bridge_poison(PoisonKind::Internal, message, &demand_chain))?;
        let manifest = NodeManifest {
            node_id: NodeId("stabilize:value".to_string()),
            cache_key,
            produced_at: String::new(),
            outputs: vec![OutputEntry {
                logical: "value".to_string(),
                filename: "stabilized".to_string(),
                blob: output_blob_ref_from_blob_ref(&blob_ref),
                executable: false,
            }],
        };
        let (node_bytes, manifest_hash) = CasNode::Manifest(manifest).to_bytes_and_hash();
        self.store
            .put_node(manifest_hash, node_bytes)
            .await
            .map_err(|error| {
                exec_bridge_poison(PoisonKind::Internal, format!("{error:?}"), &demand_chain)
            })?;
        publish_acquire_cache_entry(&self.store, cache_key, manifest_hash)
            .await
            .map_err(|message| exec_bridge_poison(PoisonKind::Internal, message, &demand_chain))?;
        Ok(value)
    }
}

impl BuildRuntime for BuildVxRuntime {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn has_exec(&self) -> bool {
        true
    }

    fn has_discover(&self) -> bool {
        true
    }

    fn store_hydration(
        &self,
        request: EvalStoreHydrationRequest,
    ) -> BoxFuture<'_, std::result::Result<Value, PoisonError>> {
        let value = request.value;
        let demand_chain = request.demand_chain;
        let store = Arc::clone(&self.store);
        Box::pin(async move { hydrate_exec_value_from_store(store, value, demand_chain).await })
    }

    fn acquire(
        &self,
        request: EvalAcquireRequest,
    ) -> BoxFuture<'_, std::result::Result<Value, PoisonError>> {
        Box::pin(async move {
            let summary = summarize_build_vx_acquire_request(&request);
            self.emit_boundary_log(&summary).await;
            run_build_vx_acquire_via_store(&self.store, request, self.event_recorder.as_ref())
                .await
                .map_err(|error| error.with_context(summary))
        })
    }

    fn import_module(
        &self,
        request: EvalImportRequest,
    ) -> BoxFuture<'_, std::result::Result<EvalModule, PoisonError>> {
        Box::pin(async move {
            let summary = summarize_build_vx_import_request(&request);
            self.emit_boundary_log(&summary).await;
            self.load_imported_module(request)
                .await
                .map_err(|error| error.with_context(summary))
        })
    }

    fn render(
        &self,
        request: EvalRenderRequest,
    ) -> BoxFuture<'_, std::result::Result<Value, PoisonError>> {
        Box::pin(async move {
            let summary = summarize_build_vx_render_request(&request);
            self.emit_boundary_log(&summary).await;
            run_build_vx_render_via_store(&self.store, &self.workspace_root, request)
                .await
                .map_err(|error| error.with_context(summary))
        })
    }

    fn stabilize(
        &self,
        request: EvalStabilizeRequest,
    ) -> BoxFuture<'_, std::result::Result<Value, PoisonError>> {
        Box::pin(async move {
            let summary = summarize_build_vx_stabilize_request(&request);
            self.emit_boundary_log(&summary).await;
            self.run_stabilize_via_store(request)
                .await
                .map_err(|error| error.with_context(summary))
        })
    }

    fn exec(&self, request: ExecRequest) -> BoxFuture<'_, std::result::Result<Value, PoisonError>> {
        Box::pin(async move {
            let summary = summarize_build_vx_exec_request(&request);
            self.emit_boundary_log(&summary).await;
            run_build_vx_exec_via_runner(
                &self.store,
                &self.executors,
                &self.activity_registry,
                &self.exec_mount_prep_inflight,
                &self.exec_fs_tree_snapshot_cache,
                self.progress_listener.clone(),
                self.event_recorder.clone(),
                request,
            )
            .await
            .map_err(|error| error.with_context(summary))
        })
    }

    fn discover(
        &self,
        request: EvalDiscoverRequest,
    ) -> BoxFuture<'_, std::result::Result<Value, PoisonError>> {
        Box::pin(async move {
            let summary = summarize_build_vx_discover_request(&request);
            self.emit_boundary_log(&summary).await;
            run_build_vx_discover_via_runner(
                &self.store,
                &self.executors,
                &self.activity_registry,
                self.progress_listener.clone(),
                self.event_recorder.clone(),
                request,
            )
            .await
            .map_err(|error| error.with_context(summary))
        })
    }
}

fn hydrate_exec_value_from_store(
    store: Arc<StoreClient>,
    value: Value,
    demand_chain: DemandChain,
) -> Pin<Box<dyn Future<Output = std::result::Result<Value, PoisonError>> + Send>> {
    Box::pin(async move {
        match value {
            Value::Blob(BlobValue::Store {
                blob_ref,
                executable,
            }) => {
                let bytes = fetch_store_blob_bytes(&store, &blob_ref)
                    .await
                    .map_err(|error| {
                        exec_bridge_poison(PoisonKind::Internal, error.to_string(), &demand_chain)
                    })?;
                Ok(Value::Blob(BlobValue::Inline {
                    bytes: Arc::from(bytes),
                    executable,
                }))
            }
            Value::Tree(TreeValue::Store(root)) => {
                let directory = load_store_directory(&store, root).await.map_err(|error| {
                    exec_bridge_poison(PoisonKind::Internal, error.to_string(), &demand_chain)
                })?;
                let mut entries = Vec::with_capacity(directory.entries.len());
                for entry in directory.entries {
                    let value = match entry.kind {
                        DirEntryKind::SmallFile {
                            content_hash,
                            size,
                            executable,
                        } => {
                            let blob_ref = BlobRef::Small {
                                chunk: ChunkHash(content_hash.0),
                                size,
                            };
                            hydrate_exec_value_from_store(
                                Arc::clone(&store),
                                Value::Blob(BlobValue::Store {
                                    blob_ref,
                                    executable,
                                }),
                                demand_chain.clone(),
                            )
                            .await?
                        }
                        DirEntryKind::LargeFile {
                            blob_node,
                            content_hash,
                            size,
                            executable,
                        } => {
                            let blob_ref = BlobRef::Large {
                                blob_node,
                                content_hash,
                                size,
                            };
                            hydrate_exec_value_from_store(
                                Arc::clone(&store),
                                Value::Blob(BlobValue::Store {
                                    blob_ref,
                                    executable,
                                }),
                                demand_chain.clone(),
                            )
                            .await?
                        }
                        DirEntryKind::Dir { hash, .. } => {
                            hydrate_exec_value_from_store(
                                Arc::clone(&store),
                                Value::Tree(TreeValue::Store(hash)),
                                demand_chain.clone(),
                            )
                            .await?
                        }
                        DirEntryKind::Symlink { target } => {
                            return Err(exec_bridge_poison(
                                PoisonKind::Internal,
                                format!(
                                    "store-backed symlink values are not supported in the build.vx evaluator bridge yet (target: {target})"
                                ),
                                &demand_chain,
                            ));
                        }
                    };
                    entries.push((Arc::from(entry.name), value));
                }
                Ok(Value::Tree(TreeValue::inline(Arc::from(entries))))
            }
            Value::Discovered(discovered) => {
                let inner = hydrate_exec_value_from_store(
                    Arc::clone(&store),
                    *discovered.value,
                    demand_chain,
                )
                .await?;
                Ok(Value::Discovered(DiscoveredValue {
                    value: Box::new(inner),
                    obligations: discovered.obligations,
                }))
            }
            Value::Tuple(values) => {
                let mut hydrated = Vec::with_capacity(values.len());
                for value in values.iter() {
                    hydrated.push(
                        hydrate_exec_value_from_store(
                            Arc::clone(&store),
                            value.clone(),
                            demand_chain.clone(),
                        )
                        .await?,
                    );
                }
                Ok(Value::Tuple(Arc::from(hydrated)))
            }
            Value::Array(values) => {
                let mut hydrated = Vec::with_capacity(values.len());
                for value in values.iter() {
                    hydrated.push(
                        hydrate_exec_value_from_store(
                            Arc::clone(&store),
                            value.clone(),
                            demand_chain.clone(),
                        )
                        .await?,
                    );
                }
                Ok(Value::Array(Arc::from(hydrated)))
            }
            Value::Map(map) => {
                let mut entries = Vec::with_capacity(map.entries.len());
                for (key, value) in map.entries.iter() {
                    entries.push((
                        hydrate_exec_value_from_store(
                            Arc::clone(&store),
                            key.clone(),
                            demand_chain.clone(),
                        )
                        .await?,
                        hydrate_exec_value_from_store(
                            Arc::clone(&store),
                            value.clone(),
                            demand_chain.clone(),
                        )
                        .await?,
                    ));
                }
                Ok(Value::Map(MapValue {
                    entries: Arc::from(entries),
                }))
            }
            Value::Struct(struct_value) => {
                let mut fields = Vec::with_capacity(struct_value.fields.len());
                for (name, value) in struct_value.fields.iter() {
                    fields.push((
                        name.clone(),
                        hydrate_exec_value_from_store(
                            Arc::clone(&store),
                            value.clone(),
                            demand_chain.clone(),
                        )
                        .await?,
                    ));
                }
                Ok(Value::Struct(StructValue {
                    type_name: struct_value.type_name,
                    fields: Arc::from(fields),
                }))
            }
            other => Ok(other),
        }
    })
}

impl VxdService {
    /// Report progress to CLI if connected
    async fn report_start_action(&self, action_type: ActionType) -> Option<u64> {
        let listener = self.progress_listener.lock().clone();
        if let Some(ref listener) = listener {
            listener.start_action(action_type).await.ok()
        } else {
            None
        }
    }

    async fn report_complete_action(&self, action_id: Option<u64>) {
        let listener = self.progress_listener.lock().clone();
        if let (Some(listener), Some(id)) = (listener.as_ref(), action_id) {
            let _ = listener.complete_action(id).await;
        }
    }

    async fn report_set_total(&self, total: u64) {
        let listener = self.progress_listener.lock().clone();
        if let Some(ref listener) = listener {
            let _ = listener.set_total(total).await;
        }
    }

    /// Get the VX_HOME directory.
    pub fn vx_home(&self) -> &Utf8PathBuf {
        &self.vx_home
    }

    /// Create a new daemon service
    pub(crate) async fn new(
        vx_home: Utf8PathBuf,
        exec_host_triple: String,
        deps: VxdServiceDeps,
    ) -> Self {
        let VxdServiceDeps {
            spawn_tracker,
            store,
            build_vx_executors,
            store_handle,
            activity_registry,
            build_vx_config_path,
        } = deps;
        let db = Database::new();
        let cache_path = vx_home.join("picante.cache");
        let wal_path = vx_home.join("picante.wal");

        // Load persisted picante cache
        let cache_loaded = match db.load_from_cache(&cache_path).await {
            Ok(true) => {
                trace!(path = %cache_path, "loaded picante cache");
                true
            }
            Ok(false) => {
                trace!(path = %cache_path, "no picante cache found, starting fresh");
                false
            }
            Err(e) => {
                warn!(path = %cache_path, error = %e, "failed to load picante cache, starting fresh");
                false
            }
        };

        // If cache was loaded, replay WAL to apply incremental changes
        if cache_loaded {
            match db.replay_wal(&wal_path).await {
                Ok(count) if count > 0 => {
                    info!(path = %wal_path, entries = count, "replayed WAL entries");
                }
                Ok(_) => {
                    debug!(path = %wal_path, "no WAL entries to replay");
                }
                Err(e) => {
                    warn!(path = %wal_path, error = %e, "failed to replay WAL, continuing without it");
                }
            }
        }

        // Create a fresh WAL file for this session based on current revision
        // (which includes any changes from the replayed WAL)
        let current_revision = db.runtime().current_revision().0;
        let wal_writer = match WalWriter::create(&wal_path, current_revision) {
            Ok(writer) => {
                debug!(path = %wal_path, base_revision = current_revision, "created WAL writer for incremental persistence");
                Some(writer)
            }
            Err(e) => {
                warn!(path = %wal_path, error = %e, "failed to create WAL writer, persistence will be disabled");
                None
            }
        };

        Self {
            store,
            build_vx_executors: Arc::new(build_vx_executors),
            _store_handle: store_handle,
            exec_host_triple,
            vx_home,
            db: Arc::new(db),
            picante_cache: cache_path,
            picante_wal: wal_path,
            wal_writer: Arc::new(SyncMutex::new("vxd/wal_writer", wal_writer)),
            toolchains: Arc::new(SyncMutex::new(
                "vxd/toolchains",
                AcquiredToolchains { rust: None },
            )),
            spawn_tracker,
            activity_registry,
            build_vx_config_path,
            exec_mount_prep_inflight: Arc::new(SyncMutex::new(
                "vxd/exec_mount_prep_inflight",
                HashMap::new(),
            )),
            progress_listener: Arc::new(SyncMutex::new("vxd/progress_listener", None)),
            session_id: Arc::new(SyncMutex::new("vxd/session_id", String::new())),
            active_build_cancel: Arc::new(SyncMutex::new("vxd/active_build_cancel", None)),
        }
    }

    /// Create a per-connection session with fresh ephemeral state.
    /// Shared state (booted, DB, toolchains, WAL) is still shared via Arc.
    pub fn new_session(&self) -> Self {
        Self {
            store: self.store.clone(),
            build_vx_executors: self.build_vx_executors.clone(),
            _store_handle: self._store_handle.clone(),
            exec_host_triple: self.exec_host_triple.clone(),
            vx_home: self.vx_home.clone(),
            db: Arc::new(Database::new()),
            picante_cache: self.picante_cache.clone(),
            picante_wal: self.picante_wal.clone(),
            wal_writer: self.wal_writer.clone(),
            toolchains: self.toolchains.clone(),
            spawn_tracker: self.spawn_tracker.clone(),
            activity_registry: self.activity_registry.clone(),
            build_vx_config_path: self.build_vx_config_path.clone(),
            exec_mount_prep_inflight: self.exec_mount_prep_inflight.clone(),
            // Per-connection state: fresh for each session
            progress_listener: Arc::new(SyncMutex::new("vxd/progress_listener", None)),
            session_id: Arc::new(SyncMutex::new("vxd/session_id", String::new())),
            active_build_cancel: Arc::new(SyncMutex::new("vxd/active_build_cancel", None)),
        }
    }

    /// Get the current session ID for this connection.
    pub async fn session_id(&self) -> String {
        self.session_id.lock().clone()
    }

    /// Update the progress listener for the current connection
    pub async fn set_progress_listener(&self, listener: Option<Arc<ProgressListenerClient>>) {
        *self.progress_listener.lock() = listener;
    }

    /// Cancel any active build. Called by the signal handler on SIGINT/SIGTERM.
    pub async fn cancel_active_build(&self) {
        if let Some(token) = self.active_build_cancel.lock().as_ref() {
            info!("cancelling active build");
            token.cancel();
        }
    }

    /// Ensure Rust toolchain exists in CAS. Returns info for cache keys.
    pub async fn ensure_rust_toolchain(&self, channel: RustChannel) -> Result<ToolchainInfo> {
        // Check if already acquired
        {
            let toolchains = self.toolchains.lock();
            if let Some(ref rust) = toolchains.rust {
                return Ok(ToolchainInfo {
                    manifest_hash: rust.manifest_hash,
                    toolchain_id: rust.toolchain_id,
                    version: rust.version.clone(),
                    manifest_date: rust.manifest_date.clone(),
                });
            }
        }

        // Track toolchain acquisition
        let action_id = self.report_start_action(ActionType::AcquireToolchain).await;

        let spec = RustToolchainSpec {
            channel,
            host: self.exec_host_triple.clone(),
            target: self.exec_host_triple.clone(),
            components: vec![RustComponent::Rustc, RustComponent::RustStd],
        };

        // Create a progress channel — progress is forwarded to the CLI's ProgressListener
        // if one is connected, otherwise silently dropped.
        let (progress_tx, mut progress_rx) = roam::channel::<vx_store_proto::StoreProgress>();

        // Forward store progress to the CLI's ProgressListener
        let forward_listener = self.progress_listener.lock().clone();
        let forward_action_id = action_id;
        let forward_handle = moire::task::spawn(
            async move {
                while let Ok(Some(sp)) = progress_rx
                    .recv()
                    .named("vxd.toolchain.progress.recv")
                    .await
                {
                    if let (Some(listener), Some(id)) = (&forward_listener, forward_action_id) {
                        let _ = listener
                            .update_progress(
                                id,
                                vxd_proto::ActionProgress {
                                    message: sp.message.clone(),
                                    current: sp.current,
                                    total: sp.total,
                                },
                            )
                            .await;
                    }
                }
            }
            .named("toolchain_progress_fwd"),
        );

        let result = self
            .store
            .ensure_rust_toolchain(spec, progress_tx)
            .await
            .map_err(|e| {
                // Complete action on error
                let this = self.clone();
                moire::task::spawn(
                    async move { this.report_complete_action(action_id).await }
                        .named("complete_action"),
                );
                VxdError::CasRpc(format!("{e:?}"))
            })?;

        // Wait for progress forwarding to drain
        let _ = forward_handle.await;

        if result.status == EnsureStatus::Failed {
            self.report_complete_action(action_id).await;
            return Err(VxdError::ToolchainAcquisition(
                result.error.unwrap_or_else(|| "unknown error".to_string()),
            ));
        }

        let manifest_hash = result.manifest_hash.ok_or_else(|| {
            let this = self.clone();
            moire::task::spawn(
                async move { this.report_complete_action(action_id).await }
                    .named("complete_action"),
            );
            VxdError::NoManifestHash
        })?;

        // Get manifest for version info (and toolchain_id on cache hit)
        let manifest = self
            .store
            .get_toolchain_manifest(manifest_hash)
            .await
            .map_err(|e| {
                let this = self.clone();
                moire::task::spawn(
                    async move { this.report_complete_action(action_id).await }
                        .named("complete_action"),
                );
                VxdError::CasRpc(format!("{e:?}"))
            })?
            .ok_or_else(|| {
                let this = self.clone();
                moire::task::spawn(
                    async move { this.report_complete_action(action_id).await }
                        .named("complete_action"),
                );
                VxdError::ToolchainManifestNotFound(manifest_hash)
            })?;

        // On cache hit, toolchain_id comes from manifest; on download, it's in the result
        let toolchain_id = result.toolchain_id.unwrap_or(manifest.toolchain_id);

        let info = ToolchainInfo {
            manifest_hash,
            toolchain_id,
            version: manifest.rust_version.clone(),
            manifest_date: manifest.rust_manifest_date.clone(),
        };

        // Cache it
        {
            let mut toolchains = self.toolchains.lock();
            toolchains.rust = Some(ToolchainInfo {
                manifest_hash,
                toolchain_id,
                version: manifest.rust_version,
                manifest_date: manifest.rust_manifest_date,
            });
        }

        // Complete the action
        self.report_complete_action(action_id).await;

        Ok(info)
    }

    /// Kill all spawned child services (casd, execd)
    pub async fn kill_spawned_services(&self) {
        self.spawn_tracker.lock().kill_all();
    }

    // r[impl eval.entry]
    async fn build_from_build_vx(
        &self,
        request: BuildRequest,
        build_vx_path: Utf8PathBuf,
        total_start: std::time::Instant,
    ) -> Result<BuildResult> {
        let build_label = build_vx_path
            .file_name()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "build.vx".to_string());
        self.report_set_total(1).await;
        let action_id = self
            .report_start_action(ActionType::Run(build_label.clone()))
            .await;

        let result = async {
            let source =
                fs::read_to_string(&build_vx_path).map_err(|error| VxdError::FileRead {
                    path: build_vx_path.clone(),
                    message: error.to_string(),
                })?;

            let tree = {
                let parse = vx_parser::parse(&source);
                if !parse.errors().is_empty() {
                    let failure =
                        build_failure::from_parse_errors(&build_vx_path, &source, parse.errors());
                    return Ok(build_failure_result(failure, 1));
                }

                let module = {
                    let root = SourceFile::cast_root(parse.syntax());
                    match lower_source_file_with_source_id(&root, build_vx_path.to_string()) {
                        Ok(module) => module,
                        Err(error) => {
                            let failure =
                                build_failure::from_lower_error(&build_vx_path, &source, &error);
                            return Ok(build_failure_result(failure, 1));
                        }
                    }
                };
                let event_recorder = std::env::var("VX_TRACE")
                    .ok()
                    .map(|_| vx_eval::EventRecorder::new());
                let build_runtime = BuildVxRuntime {
                    store: Arc::clone(&self.store),
                    executors: Arc::clone(&self.build_vx_executors),
                    activity_registry: self.activity_registry.clone(),
                    exec_mount_prep_inflight: self.exec_mount_prep_inflight.clone(),
                    workspace_root: request.project_path.clone(),
                    config_module_path: self.build_vx_config_path.clone(),
                    progress_listener: self.progress_listener.lock().clone(),
                    exec_fs_tree_snapshot_cache: Arc::new(SyncMutex::new(
                        "build_vx_fs_tree_snapshot_cache",
                        HashMap::new(),
                    )),
                    module_cache: Arc::new(SyncMutex::new("build_vx_module_cache", HashMap::new())),
                    module_stack: Arc::new(SyncMutex::new("build_vx_module_stack", HashSet::new())),
                    event_recorder: event_recorder.clone(),
                };
                let engine = build_runtime.build_engine(&module);
                let output = engine
                    .call_entry(
                        "build",
                        request.project_path.as_str(),
                        request.args.clone(),
                        request.env.iter().cloned().collect(),
                    )
                    .await
                    .map_err(|error| {
                        build_failure_result(
                            build_failure::from_poison_error(&request.project_path, &error),
                            1,
                        )
                    });

                if let Some(recorder) = &event_recorder {
                    let trace_path = request.project_path.join(".vx/trace.pftrace");
                    if let Err(error) = recorder.write_perfetto_trace(trace_path.as_std_path()) {
                        tracing::warn!(%error, path = %trace_path, "failed to write perfetto trace");
                    } else {
                        tracing::info!(path = %trace_path, "wrote perfetto trace");
                    }
                }

                let output = match output {
                    Ok(output) => output,
                    Err(result) => return Ok(result),
                };

                match output {
                    Value::Tree(tree) => tree,
                    Value::Discovered(discovered) => match *discovered.value {
                        Value::Tree(tree) => tree,
                        other => {
                            return Ok(build_failure_result(
                                BuildFailure {
                                    message: format!(
                                        "build entry must return Tree for `vx build`, got {}",
                                        other.type_name()
                                    ),
                                    sources: Vec::new(),
                                    reports: Vec::new(),
                                },
                                1,
                            ));
                        }
                    },
                    other => {
                        return Ok(build_failure_result(
                            BuildFailure {
                                message: format!(
                                    "build entry must return Tree for `vx build`, got {}",
                                    other.type_name()
                                ),
                                sources: Vec::new(),
                                reports: Vec::new(),
                            },
                            1,
                        ));
                    }
                }
            };

            if let Err(error) = vx_io::git::ensure_vx_gitignored(&request.project_path).await {
                warn!(error = %error, "failed to ensure /.vx is gitignored");
            }

            let output_root = request.project_path.join(".vx/build");
            self.materialize_build_vx_tree(&output_root, &tree)?;

            let duration = total_start.elapsed();
            Ok(BuildResult {
                success: true,
                message: format!("{build_label} completed"),
                cached: false,
                duration_ms: duration.as_millis() as u64,
                output_path: None,
                error: None,
                failure: None,
                total_actions: 1,
                cache_hits: 0,
                rebuilt: 1,
                cancelled: 0,
            })
        }
        .await;

        self.report_complete_action(action_id).await;
        result
    }

    fn materialize_build_vx_tree(&self, output_root: &Utf8Path, tree: &TreeValue) -> Result<()> {
        match fs::remove_dir_all(output_root) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(VxdError::WriteOutput {
                    path: output_root.to_path_buf(),
                    message: error.to_string(),
                });
            }
        }

        fs::create_dir_all(output_root).map_err(|error| VxdError::CreateDir {
            path: output_root.to_path_buf(),
            message: error.to_string(),
        })?;

        match tree {
            TreeValue::Inline { entries, .. } => {
                for (name, value) in entries.iter() {
                    self.materialize_build_vx_value(&output_root.join(name.as_ref()), value)?;
                }
            }
            TreeValue::FsDir(path) => {
                self.copy_build_vx_dir(path.as_str(), output_root)?;
            }
            TreeValue::Store(root) => {
                materialize_store_tree_into_dir(self.store.as_ref(), *root, output_root)?;
            }
        }

        Ok(())
    }

    fn materialize_build_vx_value(&self, path: &Utf8Path, value: &Value) -> Result<()> {
        match value {
            Value::Discovered(discovered) => {
                self.materialize_build_vx_value(path, discovered.value.as_ref())
            }
            Value::Blob(blob) => self.materialize_build_vx_blob(path, blob),
            Value::Tree(tree) => {
                fs::create_dir_all(path).map_err(|error| VxdError::CreateDir {
                    path: path.to_path_buf(),
                    message: error.to_string(),
                })?;
                match tree {
                    TreeValue::Inline { entries, .. } => {
                        for (name, value) in entries.iter() {
                            self.materialize_build_vx_value(&path.join(name.as_ref()), value)?;
                        }
                        Ok(())
                    }
                    TreeValue::FsDir(source) => self.copy_build_vx_dir(source.as_str(), path),
                    TreeValue::Store(root) => {
                        materialize_store_tree_into_dir(self.store.as_ref(), *root, path)
                    }
                }
            }
            other => Err(VxdError::Compilation {
                crate_name: "build.vx".to_string(),
                message: format!(
                    "build output trees can only contain Blob or Tree entries, got {}",
                    other.type_name()
                ),
            }),
        }
    }

    fn materialize_build_vx_blob(&self, path: &Utf8Path, blob: &BlobValue) -> Result<()> {
        let parent = path.parent().ok_or_else(|| VxdError::WriteOutput {
            path: path.to_path_buf(),
            message: "output path has no parent directory".to_string(),
        })?;
        fs::create_dir_all(parent).map_err(|error| VxdError::CreateDir {
            path: parent.to_path_buf(),
            message: error.to_string(),
        })?;

        match blob {
            BlobValue::Inline { bytes, executable } => {
                fs::write(path, bytes.as_ref()).map_err(|error| VxdError::WriteOutput {
                    path: path.to_path_buf(),
                    message: error.to_string(),
                })?;
                set_executable_if_needed(path, *executable)?;
                Ok(())
            }
            BlobValue::FsFile {
                path: source,
                executable,
            } => {
                fs::copy(source.as_str(), path).map_err(|error| VxdError::WriteOutput {
                    path: path.to_path_buf(),
                    message: error.to_string(),
                })?;
                set_executable_if_needed(path, *executable)?;
                Ok(())
            }
            BlobValue::Store {
                blob_ref,
                executable,
            } => materialize_store_blob_to_path(self.store.as_ref(), path, blob_ref, *executable),
        }
    }

    fn copy_build_vx_dir(&self, source: &str, destination: &Utf8Path) -> Result<()> {
        fs::create_dir_all(destination).map_err(|error| VxdError::CreateDir {
            path: destination.to_path_buf(),
            message: error.to_string(),
        })?;

        let mut read_dir = fs::read_dir(source).map_err(|error| VxdError::FileRead {
            path: Utf8PathBuf::from(source),
            message: error.to_string(),
        })?;
        let mut names = Vec::new();
        while let Some(entry) = read_dir
            .next()
            .transpose()
            .map_err(|error| VxdError::FileRead {
                path: Utf8PathBuf::from(source),
                message: error.to_string(),
            })?
        {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        names.sort();

        for name in names {
            let source_path = Utf8PathBuf::from(source).join(&name);
            let destination_path = destination.join(&name);
            let metadata = fs::metadata(&source_path).map_err(|error| VxdError::FileRead {
                path: source_path.clone(),
                message: error.to_string(),
            })?;
            if metadata.is_dir() {
                self.copy_build_vx_dir(source_path.as_str(), &destination_path)?;
            } else {
                self.materialize_build_vx_blob(
                    &destination_path,
                    &BlobValue::FsFile {
                        path: PathValue::new(source_path.as_str()),
                        executable: host_path_is_executable(&PathValue::new(source_path.as_str()))
                            .map_err(|message| VxdError::Compilation {
                                crate_name: "build.vx".to_string(),
                                message,
                            })?,
                    },
                )?;
            }
        }

        Ok(())
    }

    /// Build a project.
    pub async fn do_build(&self, request: BuildRequest) -> Result<BuildResult> {
        let project_path = &request.project_path;
        let total_start = std::time::Instant::now();

        let build_vx_path = request
            .build_file_path
            .clone()
            .unwrap_or_else(|| project_path.join("build.vx"));
        if build_vx_path.exists() {
            info!(path = %build_vx_path, "Found build file, evaluating build entry");
            return self
                .build_from_build_vx(request, build_vx_path, total_start)
                .await;
        }

        if request.build_file_path.is_some() {
            return Err(VxdError::InvalidManifest(format!(
                "expected build file at {}",
                build_vx_path
            )));
        }
        Err(VxdError::InvalidManifest(
            "expected build.vx at the project root".to_string(),
        ))
    }
}

impl vx_io::boot::GracefulShutdown for VxdService {
    async fn graceful_shutdown(&self) {
        // Compact WAL before shutdown to create a clean snapshot
        match self
            .db
            .compact_wal(&self.picante_cache, &self.picante_wal, false)
            .await
        {
            Ok(_revision) => {
                trace!(
                    cache = %self.picante_cache,
                    revision = _revision,
                    "compacted WAL to snapshot"
                );
            }
            Err(e) => {
                warn!(error = %e, "failed to compact WAL on shutdown");
            }
        }
    }
}

impl VxdRpc for VxdService {
    async fn build(&self, request: BuildRequest) -> BuildResult {
        match self.do_build(request).await {
            Ok(result) => result,
            Err(e) => BuildResult {
                success: false,
                message: "Build failed".to_string(),
                cached: false,
                duration_ms: 0,
                output_path: None,
                error: Some(e.to_string()),
                failure: None,
                total_actions: 0,
                cache_hits: 0,
                rebuilt: 0,
                cancelled: 0,
            },
        }
    }

    async fn set_session(&self, session_id: String) {
        info!(session_id = %session_id, "session ID set");
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

    async fn version(&self) -> ServiceVersion {
        ServiceVersion {
            service: "vxd".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: VXD_PROTOCOL_VERSION,
        }
    }
}

fn build_failure_result(failure: BuildFailure, total_actions: usize) -> BuildResult {
    BuildResult {
        success: false,
        message: "Build failed".to_string(),
        cached: false,
        duration_ms: 0,
        output_path: None,
        error: Some(failure.message.clone()),
        failure: Some(failure),
        total_actions,
        cache_hits: 0,
        rebuilt: 0,
        cancelled: 0,
    }
}
