//! VxdService implementation.
//!
//! Orchestrates builds by:
//! 1. Computing cache keys via picante
//! 2. Checking CAS for cache hits
//! 3. Sending compile requests to execd for cache misses
//! 4. Materializing outputs locally

use crate::SpawnTracker;
use crate::build_failure;
use crate::db::Database;
use crate::error::{Result, VxdError};
use crate::executor::Executor;
use crate::inputs::*;
use ::moire::sync::SyncMutex;
use camino::{Utf8Path, Utf8PathBuf};
use facet::trace;
use facet_postcard::{from_slice, to_vec};
use moire::task::FutureExt;
use picante::HasRuntime;
use picante::wal::WalWriter;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::future::Future;
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;
use tokio::runtime::Handle;
use tokio::task::block_in_place;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use vx_eval::{
    AcquireRequest as EvalAcquireRequest, BlobValue, DemandChain,
    DiscoverRequest as EvalDiscoverRequest, DiscoveredValue, Engine, ExecArgValue, ExecMount,
    ExecRequest, ExecutorBackendKind, ExecutorSelector, MapValue, Module as EvalModule, PathValue,
    PoisonError, PoisonKind, RenderRequest as EvalRenderRequest, ShippedFunction,
    StabilizeRequest as EvalStabilizeRequest, StructValue, TransportBlob, TransportBuiltinValue,
    TransportTree, TransportValue, TreeValue, Value, lower_source_file_with_source_id,
};
use vx_io::spawn::ServiceHandle;
use vx_parser::SourceFile;
use vx_report::{
    BuildReport, GraphTopology, ReportStore, RuleId, RulePlan, RulePlanEdge, RulePlanEdgeKind,
    RulePlanFact, RulePlanFactId, RulePlanNode, RulePlanNodeId, RulePlanNodeKind,
    RulePlanProvenance, RulePlanSource, RulePlanSourceId, RulePlanSpan, RulePlanValue,
};
use vx_rs::CrateGraph;
use vx_runner_proto::{
    DiscoverRequest as RunnerDiscoverRequest, RunArg, RunEvalPayload, RunFailureKind, RunRequest,
    RunTreeMount, RunnerClient,
};
use vx_store_proto::chunking::{prepare_blob, store_prepared_blob};
use vx_store_proto::{
    Blake3Hash, BlobRef, CasNode, ChunkHash, ContentHash, DirEntry, DirEntryKind, DirectoryNode,
    EnsureStatus, NodeHash, NodeId, NodeManifest, OutputBlobRef, OutputEntry, RustChannel,
    RustComponent, RustToolchainSpec, ServiceVersion, StoreClient, StoreProgress, TreeEntryKind,
    TreeManifest,
};
use vxd_proto::{
    ActionType, BuildFailure, BuildRequest, BuildResult, ProgressListenerClient, UnpublishResult,
    VXD_PROTOCOL_VERSION, Vxd as VxdRpc,
};

mod build_vx_render;
pub(crate) use build_vx_render::run_build_vx_render_via_store;

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

/// Acquired toolchains (manifest references only)
pub struct AcquiredToolchains {
    /// Rust toolchain (if acquired)
    pub rust: Option<ToolchainInfo>,
}

#[derive(Clone)]
pub(crate) struct BuildVxExecutor {
    id: String,
    backend: ExecutorBackendKind,
    runner: Option<Arc<RunnerClient>>,
    store: Option<Arc<StoreClient>>,
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
            runner: Some(runner),
            store: None,
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
            runner: Some(runner),
            store: None,
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
    pub(crate) runner_handle: Option<Arc<ServiceHandle>>,
    pub(crate) store_handle: Option<Arc<ServiceHandle>>,
}

impl SshBootstrapExecutor {
    fn into_build_vx_executor(self) -> BuildVxExecutor {
        BuildVxExecutor {
            id: self.id,
            backend: self.backend,
            runner: Some(self.runner),
            store: self.store,
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
            runner: Some(runner),
            store: Some(store),
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
                runner: Some(runner),
                store,
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
                runner: None,
                store: None,
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

        if ((selector.backend == ExecutorBackendKind::Local && selector.id == "local")
            || is_pending_local_class)
            && let Some(factory) = &self.local_factory
        {
            let synthesized = factory(progress_listener).await?;
            let mut state = self.state.lock().expect("build_vx_executors poisoned");
            state
                .executors
                .entry("local".to_string())
                .or_insert_with(|| synthesized);
            return Ok(());
        }

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
        let Some(factory) = &self.ssh_bootstrap_factory else {
            return Err(format!(
                "build.vx executor class:{} is not available on this host",
                selector.id
            ));
        };
        let class_label = selector.id.clone();
        let synthesized =
            factory(class_label.clone(), target.to_string(), progress_listener).await?;
        let mut state = self.state.lock().expect("build_vx_executors poisoned");
        if self.resolve_executor_locked(&state, selector).is_err() {
            let executor_id = synthesized.id.clone();
            state
                .executors
                .insert(executor_id.clone(), synthesized.into_build_vx_executor());
            state.class_aliases.insert(class_label, executor_id);
        }
        Ok(())
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
            .get_node(hash)
            .await
            .map_err(|error| format!("{error:?}"))?
            .is_some()
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
}

fn rule_plan_bridge_enabled() -> bool {
    std::env::var("VX_RULE_PLAN_BRIDGE")
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn rule_plan_exec_enabled() -> bool {
    std::env::var("VX_RULE_PLAN_EXEC")
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn vx_rules_file() -> Option<camino::Utf8PathBuf> {
    std::env::var("VX_RULES_FILE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(camino::Utf8PathBuf::from)
}

fn rule_plan_parity_strict_enabled() -> bool {
    std::env::var("VX_RULE_PLAN_PARITY_STRICT")
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn parity_focus_node_id(node_id: &str) -> bool {
    node_id.starts_with("compile-") || node_id.starts_with("acquire-registry:")
}

#[derive(Debug, Default)]
pub(crate) struct RulePlanParityDiff {
    pub legacy_focus_count: usize,
    pub rule_plan_focus_count: usize,
    pub missing_in_rule_plan: Vec<String>,
    pub extra_in_rule_plan: Vec<String>,
}

impl RulePlanParityDiff {
    fn has_drift(&self) -> bool {
        !self.missing_in_rule_plan.is_empty() || !self.extra_in_rule_plan.is_empty()
    }
}

#[derive(Debug, Default)]
pub(crate) struct RulePlanParityEdgeDiff {
    pub missing_in_rule_plan: Vec<String>,
    pub extra_in_rule_plan: Vec<String>,
}

impl RulePlanParityEdgeDiff {
    fn has_drift(&self) -> bool {
        !self.missing_in_rule_plan.is_empty() || !self.extra_in_rule_plan.is_empty()
    }
}

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
                matches.sort_by_key(|id| id.short_hex());
                return Err(VxdError::InvalidManifest(format!(
                    "RulePlan selected crate '{crate_name}' type '{crate_type}' but it matched multiple crate IDs: {}",
                    matches
                        .into_iter()
                        .map(|id| id.short_hex())
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

async fn store_exec_blob(
    store: &StoreClient,
    blob: &BlobValue,
) -> std::result::Result<DirEntryKind, String> {
    match blob {
        BlobValue::Inline { bytes, executable } => {
            let prepared = prepare_blob(bytes.as_ref());
            let blob_ref = store_prepared_blob(store, &prepared, bytes.as_ref()).await?;
            Ok(exec_blob_kind_from_blob_ref(&blob_ref, *executable))
        }
        BlobValue::Store {
            blob_ref,
            executable,
        } => Ok(exec_blob_kind_from_blob_ref(blob_ref, *executable)),
        BlobValue::FsFile { path, executable } => {
            let bytes = fs::read(path.as_str())
                .map_err(|error| format!("failed to read {}: {error}", path.as_str()))?;
            let prepared = prepare_blob(&bytes);
            let blob_ref = store_prepared_blob(store, &prepared, &bytes).await?;
            Ok(exec_blob_kind_from_blob_ref(&blob_ref, *executable))
        }
    }
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

pub(crate) async fn store_exec_mounts(
    store: &StoreClient,
    mounts: &[ExecMount],
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

    while let Some(item) = pending.pop() {
        match item {
            PendingMount::Value(path, value) => match value {
                Value::Discovered(discovered) => {
                    pending.push(PendingMount::Value(path, *discovered.value));
                }
                Value::Blob(blob) => files.push(crate::cas_tree::TreeFile {
                    path,
                    kind: store_exec_blob(store, &blob).await?,
                }),
                Value::Tree(TreeValue::Inline(entries)) => {
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
    }

    if files.is_empty() {
        return Ok(None);
    }

    crate::cas_tree::build_merkle_tree(store, files)
        .await
        .map(|(root, _)| Some(root))
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

    fn verify(self, bytes: &[u8]) -> std::result::Result<(), String> {
        match self {
            Self::Sha256(expected) => {
                let actual = Sha256::digest(bytes);
                if actual.as_slice() == expected.as_slice() {
                    Ok(())
                } else {
                    Err(format!(
                        "acquire digest mismatch: expected sha256:{}, got sha256:{}",
                        hex::encode(expected),
                        hex::encode(actual)
                    ))
                }
            }
            Self::Blake3(expected) => {
                let actual = *blake3::hash(bytes).as_bytes();
                if actual == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "acquire digest mismatch: expected blake3:{}, got blake3:{}",
                        hex::encode(expected),
                        hex::encode(actual)
                    ))
                }
            }
        }
    }
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
                DirEntryKind::Dir { total_size, .. } => *total_size,
                DirEntryKind::Symlink { .. } => 0,
            }
        });

        let node = CasNode::Directory(DirectoryNode {
            entries: entries.clone(),
            total_size,
        });
        let (node_bytes, node_hash) = node.to_bytes_and_hash();
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
    store: &StoreClient,
    request: EvalAcquireRequest,
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

    let acquired = if let Some(cached) = store.lookup(cache_key).await.map_err(|error| {
        exec_bridge_poison(PoisonKind::Internal, format!("{error:?}"), &demand_chain)
    })? {
        match extract_format {
            Some(_) => Value::Tree(TreeValue::Store(cached)),
            None => load_acquired_blob_from_manifest(store, cached)
                .await
                .map_err(|message| {
                    exec_bridge_poison(PoisonKind::Internal, message, &demand_chain)
                })?,
        }
    } else {
        let bytes = vx_http::get_bytes(&request.url).await.map_err(|error| {
            exec_bridge_poison(
                PoisonKind::UserFail,
                format!("failed to acquire {}: {error}", request.url),
                &demand_chain,
            )
        })?;
        digest
            .verify(&bytes)
            .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?;

        match extract_format {
            Some(format) => {
                let files = Arc::new(SyncMutex::new("vxd/acquire_file_info", HashMap::new()));
                let files_for_extract = Arc::clone(&files);
                let tree = vx_tarball::extract_to_tree(
                    std::io::Cursor::new(bytes),
                    format.compression(),
                    0,
                    async move |data: Vec<u8>| {
                        let content_hash = ContentHash(Blake3Hash::from_bytes(&data));
                        let prepared = prepare_blob(&data);
                        let blob_ref = store_prepared_blob(store, &prepared, &data).await?;
                        files_for_extract.lock().insert(content_hash.0, blob_ref);
                        Ok(content_hash.0)
                    },
                )
                .await
                .map_err(|error| {
                    exec_bridge_poison(
                        PoisonKind::UserFail,
                        format!("failed to extract {}: {error}", request.url),
                        &demand_chain,
                    )
                })?;
                let files = files.lock().clone();
                let (root, _) = store_acquired_tree_manifest(store, &tree, &files)
                    .await
                    .map_err(|message| {
                        exec_bridge_poison(PoisonKind::Internal, message, &demand_chain)
                    })?;
                publish_acquire_cache_entry(store, cache_key, root)
                    .await
                    .map_err(|message| {
                        exec_bridge_poison(PoisonKind::Internal, message, &demand_chain)
                    })?;
                Value::Tree(TreeValue::Store(root))
            }
            None => {
                let prepared = prepare_blob(&bytes);
                let blob_ref = store_prepared_blob(store, &prepared, &bytes)
                    .await
                    .map_err(|message| {
                        exec_bridge_poison(PoisonKind::Internal, message, &demand_chain)
                    })?;

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
                publish_acquire_cache_entry(store, cache_key, manifest_hash)
                    .await
                    .map_err(|message| {
                        exec_bridge_poison(PoisonKind::Internal, message, &demand_chain)
                    })?;
                Value::Blob(BlobValue::Store {
                    blob_ref,
                    executable: false,
                })
            }
        }
    };

    hydrate_exec_value_from_store(store, acquired, &demand_chain).await
}

async fn run_build_vx_exec_via_runner(
    store: &StoreClient,
    executors: &BuildVxExecutors,
    progress_listener: Option<Arc<ProgressListenerClient>>,
    request: ExecRequest,
) -> std::result::Result<Value, PoisonError> {
    let demand_chain = request.demand_chain.clone();
    let executor_summary = describe_executor_selector(&request.executor);
    let exec_started = Instant::now();
    emit_build_vx_progress_message(
        &progress_listener,
        format!("resolving build.vx executor {}", executor_summary),
    )
    .await;
    let exec = executors
        .runner_for(&request.executor, progress_listener.clone())
        .await
        .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?;
    let exec_store = executors
        .store_for(&request.executor, progress_listener.clone())
        .await
        .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?;
    emit_build_vx_progress_message(
        &progress_listener,
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
    let root_mount = store_exec_mounts(store, &request.mounts)
        .await
        .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?;
    tracing::info!(
        executor = %describe_executor_selector(&request.executor),
        root_mount = ?root_mount.map(|hash| hash.0),
        cross_store = !std::ptr::eq(target_store, store),
        "build.vx exec prepared root mount"
    );
    if !std::ptr::eq(target_store, store)
        && let Some(root) = root_mount
    {
        mirror_node(store, target_store, root)
            .await
            .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?;
    }
    emit_build_vx_progress_message(
        &progress_listener,
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
    let progress_handle = moire::task::spawn(
        async move {
            while let Ok(Some(update)) = progress_rx.recv().await {
                let message = format!(
                    "build.vx executor {}: {}",
                    executor_summary_for_progress, update.message
                );
                emit_build_vx_progress_message(&progress_listener_for_exec, message).await;
            }
        }
        .named("build_vx_exec_progress_fwd"),
    );
    let observer = if std::ptr::eq(target_store, store) {
        request.observer
    } else {
        mirror_shipped_function_store_refs(store, target_store, request.observer)
            .await
            .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?
    };

    let result = exec
        .run(
            RunRequest {
                executor: request.executor.clone(),
                toolchain_manifest: Blake3Hash([0; 32]),
                program_rel: String::new(),
                program: Some(exec_arg_to_run_arg(&program)),
                args: args.map(|arg| exec_arg_to_run_arg(&arg)).collect(),
                tree_mounts: root_mount
                    .into_iter()
                    .map(|root| RunTreeMount {
                        mount: String::new(),
                        manifest: root.0,
                    })
                    .collect(),
                file_mounts: Vec::new(),
                outputs: Vec::new(),
                writable_roots: request
                    .writable_roots
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
        .await
        .map_err(|error| {
            exec_bridge_poison(PoisonKind::UserFail, format!("{error:?}"), &demand_chain)
        })?;
    let _ = progress_handle.await;
    emit_build_vx_progress_message(
        &progress_listener,
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
    emit_build_vx_progress_message(
        &progress_listener,
        format!(
            "build.vx executor {}: bridging observer value",
            executor_summary
        ),
    )
    .await;
    tracing::info!(executor = %executor_summary, "build.vx exec starting observer-value bridge");
    let observer_value = if std::ptr::eq(target_store, store) {
        observer_value
    } else {
        mirror_transport_value_store_refs(target_store, store, observer_value)
            .await
            .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?
    };
    tracing::info!(executor = %executor_summary, "build.vx exec finished observer-value bridge");
    emit_build_vx_progress_message(
        &progress_listener,
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
    let hydration_started = Instant::now();
    emit_build_vx_progress_message(
        &progress_listener,
        format!("build.vx executor {}: hydrating result", executor_summary),
    )
    .await;
    tracing::info!(executor = %executor_summary, "build.vx exec starting hydration");
    let value = hydrate_exec_value_from_store(store, value, &demand_chain).await?;
    tracing::info!(executor = %executor_summary, "build.vx exec finished hydration");
    emit_build_vx_progress_message(
        &progress_listener,
        format!(
            "build.vx executor {}: hydration finished ({})",
            executor_summary,
            format_progress_elapsed(hydration_started.elapsed())
        ),
    )
    .await;
    Ok(value)
}

async fn run_build_vx_discover_via_runner(
    store: &StoreClient,
    executors: &BuildVxExecutors,
    progress_listener: Option<Arc<ProgressListenerClient>>,
    request: EvalDiscoverRequest,
) -> std::result::Result<Value, PoisonError> {
    let demand_chain = request.demand_chain.clone();
    let discover_started = Instant::now();
    let executor_summary = describe_executor_selector(&request.executor);
    emit_build_vx_progress_message(
        &progress_listener,
        format!("resolving build.vx executor {}", executor_summary),
    )
    .await;
    let exec = executors
        .runner_for(&request.executor, progress_listener.clone())
        .await
        .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?;
    let exec_store = executors
        .store_for(&request.executor, progress_listener.clone())
        .await
        .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?;
    emit_build_vx_progress_message(
        &progress_listener,
        format!("build.vx executor {} ready", executor_summary),
    )
    .await;
    let target_store = exec_store.as_deref().unwrap_or(store);
    let probe = if std::ptr::eq(target_store, store) {
        request.probe
    } else {
        mirror_shipped_function_store_refs(store, target_store, request.probe)
            .await
            .map_err(|message| exec_bridge_poison(PoisonKind::UserFail, message, &demand_chain))?
    };
    let result = exec
        .discover(RunnerDiscoverRequest {
            executor: request.executor,
            module: request.module,
            probe,
        })
        .await
        .map_err(|error| {
            exec_bridge_poison(PoisonKind::UserFail, format!("{error:?}"), &demand_chain)
        })?;
    emit_build_vx_progress_message(
        &progress_listener,
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

    let value = if std::ptr::eq(target_store, store) {
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
    };
    let value = Value::try_from(value).map_err(|error| {
        exec_bridge_poison(PoisonKind::UserFail, error.to_string(), &demand_chain)
    })?;
    let hydration_started = Instant::now();
    emit_build_vx_progress_message(
        &progress_listener,
        format!(
            "build.vx executor {}: hydrating discovery result",
            executor_summary
        ),
    )
    .await;
    let value = hydrate_exec_value_from_store(store, value, &demand_chain).await?;
    emit_build_vx_progress_message(
        &progress_listener,
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

    format!(
        "build.vx exec[{}:{}]: {} (mounts: {}; writable: {})",
        request.executor.backend.as_tag(),
        request.executor.id,
        summarize_items(&argv, 10),
        summarize_items(&mounts, 6),
        summarize_items(&writable_roots, 6),
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

#[derive(Clone)]
struct BuildVxRuntime {
    store: Arc<StoreClient>,
    executors: Arc<BuildVxExecutors>,
    workspace_root: Utf8PathBuf,
    progress_listener: Option<Arc<ProgressListenerClient>>,
}

fn block_on_runtime<T>(runtime: &Handle, future: impl Future<Output = T>) -> T {
    if Handle::try_current().is_ok() {
        block_in_place(|| runtime.block_on(future))
    } else {
        runtime.block_on(future)
    }
}

fn emit_build_vx_boundary_log(
    runtime: &Handle,
    listener: &Option<Arc<ProgressListenerClient>>,
    message: &str,
) {
    if let Some(listener) = listener {
        let _ = block_on_runtime(runtime, listener.log_message(message.to_string()));
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
    fn build_engine(&self, module: &EvalModule) -> Engine {
        let runtime = Handle::current();
        let hydration_store = Arc::clone(&self.store);
        let hydration_runtime = runtime.clone();
        let acquire_runtime_state = self.clone();
        let acquire_runtime = runtime.clone();
        let render_runtime_state = self.clone();
        let render_runtime = runtime.clone();
        let stabilize_runtime_state = self.clone();
        let stabilize_runtime = runtime.clone();
        let exec_store = Arc::clone(&self.store);
        let exec_executors = Arc::clone(&self.executors);
        let exec_runtime = runtime.clone();
        let discover_store = Arc::clone(&self.store);
        let discover_executors = Arc::clone(&self.executors);
        let discover_runtime = runtime;
        let progress_listener = self.progress_listener.clone();
        let acquire_listener = progress_listener.clone();
        let render_listener = progress_listener.clone();
        let stabilize_listener = progress_listener.clone();
        let exec_listener = progress_listener.clone();
        let discover_listener = progress_listener;

        Engine::from_module(module)
            .with_store_hydration_handler(Arc::new(move |request| {
                let store = Arc::clone(&hydration_store);
                let runtime = hydration_runtime.clone();
                block_on_runtime(
                    &runtime,
                    hydrate_exec_value_from_store(&store, request.value, &request.demand_chain),
                )
            }))
            .with_acquire_handler(Arc::new(move |request| {
                let summary = summarize_build_vx_acquire_request(&request);
                let runtime = acquire_runtime.clone();
                emit_build_vx_boundary_log(&runtime, &acquire_listener, &summary);
                block_on_runtime(
                    &runtime,
                    run_build_vx_acquire_via_store(&acquire_runtime_state.store, request),
                )
                .map_err(|error| error.with_context(summary))
            }))
            .with_render_handler(Arc::new(move |request| {
                let summary = summarize_build_vx_render_request(&request);
                let runtime = render_runtime.clone();
                emit_build_vx_boundary_log(&runtime, &render_listener, &summary);
                block_on_runtime(
                    &runtime,
                    run_build_vx_render_via_store(
                        &render_runtime_state.store,
                        &render_runtime_state.workspace_root,
                        request,
                    ),
                )
                .map_err(|error| error.with_context(summary))
            }))
            .with_stabilize_handler(Arc::new(move |request| {
                let summary = summarize_build_vx_stabilize_request(&request);
                let runtime = stabilize_runtime.clone();
                emit_build_vx_boundary_log(&runtime, &stabilize_listener, &summary);
                block_on_runtime(
                    &runtime,
                    stabilize_runtime_state.run_stabilize_via_store(request),
                )
                .map_err(|error| error.with_context(summary))
            }))
            .with_exec_handler(Arc::new(move |request| {
                let summary = summarize_build_vx_exec_request(&request);
                let runtime = exec_runtime.clone();
                emit_build_vx_boundary_log(&runtime, &exec_listener, &summary);
                block_on_runtime(
                    &runtime,
                    run_build_vx_exec_via_runner(
                        &exec_store,
                        &exec_executors,
                        exec_listener.clone(),
                        request,
                    ),
                )
                .map_err(|error| error.with_context(summary))
            }))
            .with_discover_handler(Arc::new(move |request| {
                let summary = summarize_build_vx_discover_request(&request);
                let runtime = discover_runtime.clone();
                emit_build_vx_boundary_log(&runtime, &discover_listener, &summary);
                block_on_runtime(
                    &runtime,
                    run_build_vx_discover_via_runner(
                        &discover_store,
                        &discover_executors,
                        discover_listener.clone(),
                        request,
                    ),
                )
                .map_err(|error| error.with_context(summary))
            }))
    }

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
        let value = engine.call_shipped(&request.resolve, vec![request.witness])?;
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

fn hydrate_exec_value_from_store<'a>(
    store: &'a StoreClient,
    value: Value,
    demand_chain: &'a DemandChain,
) -> Pin<Box<dyn Future<Output = std::result::Result<Value, PoisonError>> + 'a>> {
    Box::pin(async move {
        match value {
            Value::Blob(BlobValue::Store {
                blob_ref,
                executable,
            }) => {
                let bytes = fetch_store_blob_bytes(store, &blob_ref)
                    .await
                    .map_err(|error| {
                        exec_bridge_poison(PoisonKind::Internal, error.to_string(), demand_chain)
                    })?;
                Ok(Value::Blob(BlobValue::Inline {
                    bytes: Arc::from(bytes),
                    executable,
                }))
            }
            Value::Tree(TreeValue::Store(root)) => {
                let directory = load_store_directory(store, root).await.map_err(|error| {
                    exec_bridge_poison(PoisonKind::Internal, error.to_string(), demand_chain)
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
                                store,
                                Value::Blob(BlobValue::Store {
                                    blob_ref,
                                    executable,
                                }),
                                demand_chain,
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
                                store,
                                Value::Blob(BlobValue::Store {
                                    blob_ref,
                                    executable,
                                }),
                                demand_chain,
                            )
                            .await?
                        }
                        DirEntryKind::Dir { hash, .. } => {
                            hydrate_exec_value_from_store(
                                store,
                                Value::Tree(TreeValue::Store(hash)),
                                demand_chain,
                            )
                            .await?
                        }
                        DirEntryKind::Symlink { target } => {
                            return Err(exec_bridge_poison(
                                PoisonKind::Internal,
                                format!(
                                    "store-backed symlink values are not supported in the build.vx evaluator bridge yet (target: {target})"
                                ),
                                demand_chain,
                            ));
                        }
                    };
                    entries.push((Arc::from(entry.name), value));
                }
                Ok(Value::Tree(TreeValue::Inline(Arc::from(entries))))
            }
            Value::Discovered(discovered) => {
                let inner =
                    hydrate_exec_value_from_store(store, *discovered.value, demand_chain).await?;
                Ok(Value::Discovered(DiscoveredValue {
                    value: Box::new(inner),
                    obligations: discovered.obligations,
                }))
            }
            Value::Tuple(values) => {
                let mut hydrated = Vec::with_capacity(values.len());
                for value in values.iter() {
                    hydrated.push(
                        hydrate_exec_value_from_store(store, value.clone(), demand_chain).await?,
                    );
                }
                Ok(Value::Tuple(Arc::from(hydrated)))
            }
            Value::Array(values) => {
                let mut hydrated = Vec::with_capacity(values.len());
                for value in values.iter() {
                    hydrated.push(
                        hydrate_exec_value_from_store(store, value.clone(), demand_chain).await?,
                    );
                }
                Ok(Value::Array(Arc::from(hydrated)))
            }
            Value::Map(map) => {
                let mut entries = Vec::with_capacity(map.entries.len());
                for (key, value) in map.entries.iter() {
                    entries.push((
                        hydrate_exec_value_from_store(store, key.clone(), demand_chain).await?,
                        hydrate_exec_value_from_store(store, value.clone(), demand_chain).await?,
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
                        hydrate_exec_value_from_store(store, value.clone(), demand_chain).await?,
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

fn build_rule_plan_from_crate_graph(graph: &CrateGraph) -> RulePlan {
    let source_id = RulePlanSourceId::from("rust_planner".to_string());
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut facts = Vec::new();

    let base_provenance = |rule: &str, line: u32| RulePlanProvenance {
        rule_id: RuleId::from(rule.to_string()),
        source_id: source_id.clone(),
        span: RulePlanSpan {
            line,
            column: 1,
            end_line: line,
            end_column: 1,
        },
        comment: None,
        metadata: std::collections::BTreeMap::new(),
    };

    let group_id = RulePlanNodeId::from("n_group_build".to_string());
    nodes.push(RulePlanNode {
        id: group_id.clone(),
        kind: RulePlanNodeKind::Group,
        label: "build".to_string(),
        provenance: base_provenance("rule.group.build", 1),
    });

    let fact_node_id = RulePlanNodeId::from("n_fact_workspace_changed".to_string());
    nodes.push(RulePlanNode {
        id: fact_node_id.clone(),
        kind: RulePlanNodeKind::Fact,
        label: "workspace_changed".to_string(),
        provenance: base_provenance("rule.fact.workspace_changed", 2),
    });
    facts.push(RulePlanFact {
        id: RulePlanFactId::from("f_workspace_changed".to_string()),
        node_id: fact_node_id.clone(),
        value: RulePlanValue::from("true".to_string()),
        provenance: base_provenance("rule.fact.workspace_changed", 3),
    });

    let mut crate_nodes = graph.nodes.values().collect::<Vec<_>>();
    crate_nodes.sort_by(|a, b| {
        a.crate_name
            .cmp(&b.crate_name)
            .then_with(|| a.crate_type.as_str().cmp(b.crate_type.as_str()))
            .then_with(|| a.id.to_string().cmp(&b.id.to_string()))
    });

    let mut artifact_by_crate_id = std::collections::HashMap::new();
    let mut action_by_crate_id = std::collections::HashMap::new();
    let mut registry_artifact_by_name_version = std::collections::HashMap::new();

    let mut registry_crates = graph.iter_registry_crates().collect::<Vec<_>>();
    registry_crates.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.version.cmp(&b.version))
            .then_with(|| a.checksum.cmp(&b.checksum))
    });
    for (idx, registry) in registry_crates.iter().enumerate() {
        let key = format!(
            "{}_{}",
            sanitize_id_component(&registry.name),
            sanitize_id_component(&registry.version)
        );
        let node_id = RulePlanNodeId::from(format!("n_registry_artifact_{key}"));
        let registry_action_node_id =
            format!("acquire-registry:{}:{}", registry.name, registry.version);
        let mut provenance = base_provenance("rule.registry.artifact", 6 + idx as u32);
        provenance.metadata.insert(
            "legacy_node_id".to_string(),
            registry_action_node_id.clone(),
        );
        provenance
            .metadata
            .insert("registry_name".to_string(), registry.name.clone());
        provenance
            .metadata
            .insert("registry_version".to_string(), registry.version.clone());
        provenance
            .metadata
            .insert("registry_checksum".to_string(), registry.checksum.clone());
        nodes.push(RulePlanNode {
            id: node_id.clone(),
            kind: RulePlanNodeKind::Artifact,
            label: registry_action_node_id,
            provenance,
        });
        registry_artifact_by_name_version
            .insert((registry.name.clone(), registry.version.clone()), node_id);
    }

    for (idx, crate_node) in crate_nodes.iter().enumerate() {
        let key = format!(
            "{}_{}_{}",
            sanitize_id_component(&crate_node.crate_name),
            crate_node.crate_type.as_str(),
            sanitize_id_component(&crate_node.id.to_string())
        );
        let action_id = RulePlanNodeId::from(format!("n_action_{key}"));
        let artifact_id = RulePlanNodeId::from(format!("n_artifact_{key}"));
        let legacy_node_id =
            legacy_compile_node_id(&crate_node.crate_name, crate_node.crate_type.as_str());
        let mut action_provenance = base_provenance("rule.rust.compile", 10 + idx as u32);
        action_provenance
            .metadata
            .insert("legacy_node_id".to_string(), legacy_node_id.clone());
        action_provenance
            .metadata
            .insert("crate_name".to_string(), crate_node.crate_name.clone());
        action_provenance.metadata.insert(
            "crate_type".to_string(),
            crate_node.crate_type.as_str().to_string(),
        );

        nodes.push(RulePlanNode {
            id: action_id.clone(),
            kind: RulePlanNodeKind::Action,
            label: legacy_node_id,
            provenance: action_provenance,
        });
        nodes.push(RulePlanNode {
            id: artifact_id.clone(),
            kind: RulePlanNodeKind::Artifact,
            label: format!("artifact:{}", crate_node.crate_name),
            provenance: base_provenance("rule.rust.artifact", 30 + idx as u32),
        });

        edges.push(RulePlanEdge {
            from: group_id.clone(),
            to: action_id.clone(),
            kind: RulePlanEdgeKind::Always,
            provenance: base_provenance("rule.group.depends-on-action", 50 + idx as u32),
        });
        edges.push(RulePlanEdge {
            from: fact_node_id.clone(),
            to: action_id.clone(),
            kind: RulePlanEdgeKind::IfChanged,
            provenance: base_provenance("rule.fact.triggers-action", 70 + idx as u32),
        });
        edges.push(RulePlanEdge {
            from: action_id.clone(),
            to: artifact_id.clone(),
            kind: RulePlanEdgeKind::IfWritten,
            provenance: base_provenance("rule.action.writes-artifact", 90 + idx as u32),
        });

        artifact_by_crate_id.insert(crate_node.id, artifact_id);
        action_by_crate_id.insert(crate_node.id, action_id);
    }

    for (idx, crate_node) in crate_nodes.iter().enumerate() {
        if let Some(action_id) = action_by_crate_id.get(&crate_node.id) {
            for dep in &crate_node.deps {
                if let Some(dep_artifact_id) = artifact_by_crate_id.get(&dep.crate_id) {
                    edges.push(RulePlanEdge {
                        from: dep_artifact_id.clone(),
                        to: action_id.clone(),
                        kind: RulePlanEdgeKind::IfChanged,
                        provenance: base_provenance(
                            "rule.dep-artifact.triggers-action",
                            120 + idx as u32,
                        ),
                    });
                }
            }

            if let vx_rs::crate_graph::CrateSource::Registry { name, version, .. } =
                &crate_node.source
                && let Some(registry_artifact_id) =
                    registry_artifact_by_name_version.get(&(name.clone(), version.clone()))
            {
                edges.push(RulePlanEdge {
                    from: registry_artifact_id.clone(),
                    to: action_id.clone(),
                    kind: RulePlanEdgeKind::IfChanged,
                    provenance: base_provenance("rule.registry.triggers-action", 140 + idx as u32),
                });
            }
        }
    }

    RulePlan {
        sources: vec![RulePlanSource {
            id: source_id,
            path: Utf8PathBuf::from("planner/rust-static.edn"),
        }],
        nodes,
        edges,
        facts,
    }
}

impl VxdService {
    async fn local_runner(&self) -> Result<Arc<RunnerClient>> {
        let listener = self.progress_listener.lock().clone();
        self.build_vx_executors
            .runner_for(&ExecutorSelector::class("default"), listener)
            .await
            .map_err(VxdError::ExecRpc)
    }

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

                let root = SourceFile::cast_root(parse.syntax());
                let module = lower_source_file_with_source_id(&root, build_vx_path.to_string());
                let build_runtime = BuildVxRuntime {
                    store: Arc::clone(&self.store),
                    executors: Arc::clone(&self.build_vx_executors),
                    workspace_root: request.project_path.clone(),
                    progress_listener: self.progress_listener.lock().clone(),
                };
                let engine = build_runtime.build_engine(&module);
                let output = engine
                    .call_entry(
                        "build",
                        request.project_path.as_str(),
                        Vec::new(),
                        Default::default(),
                    )
                    .map_err(|error| {
                        build_failure_result(
                            build_failure::from_poison_error(&request.project_path, &error),
                            1,
                        )
                    });

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
            TreeValue::Inline(entries) => {
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
                    TreeValue::Inline(entries) => {
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

    /// Build from vx.styx manifest (C, Rust, or mixed projects)
    async fn build_from_vx_styx(
        &self,
        _request: BuildRequest,
        vx_styx_path: camino::Utf8PathBuf,
    ) -> Result<BuildResult> {
        use vx_project::{Language, VxManifest};

        // Parse vx.styx
        let manifest = VxManifest::from_path(&vx_styx_path)
            .map_err(|e| VxdError::InvalidManifest(format!("Failed to parse vx.styx: {}", e)))?;

        manifest
            .validate()
            .map_err(|e| VxdError::InvalidManifest(format!("Invalid vx.styx: {}", e)))?;

        // Determine project language
        let language = manifest.project.language().ok_or_else(|| {
            VxdError::InvalidManifest(format!("Unknown language: {}", manifest.project.lang))
        })?;

        match language {
            Language::C => {
                info!(
                    project = %manifest.project.name,
                    bins = manifest.bins.len(),
                    "C project detected"
                );

                // Get target triple from request or use default
                let rust_target_triple = self.exec_host_triple.clone();

                // Convert Rust target triple to Zig target format
                let target_triple = rust_target_triple
                    .replace("apple-darwin", "macos")
                    .replace("unknown-linux-gnu", "linux-gnu")
                    .replace("pc-windows-msvc", "windows");
                let profile = if _request.release { "release" } else { "debug" };

                // Ingest source tree to CAS
                // For C projects, we need to collect all source files and ingest them
                let project_dir = vx_styx_path.parent().unwrap();

                // Collect all .c and .h files (simple approach for MVP)
                // Ingest each file as a chunk and build a Merkle directory tree
                let mut tree_files = Vec::new();

                for bin in &manifest.bins {
                    let source_path = project_dir.join(&bin.sources);
                    if source_path.exists() {
                        let contents = ::moire::fs::read(&source_path).await.map_err(|e| {
                            VxdError::FileRead {
                                path: source_path.clone(),
                                message: e.to_string(),
                            }
                        })?;

                        let size = contents.len() as u64;
                        let content_hash = ContentHash(Blake3Hash::from_bytes(&contents));
                        let chunk_hash = ChunkHash(content_hash.0);

                        // Store as chunk
                        self.store
                            .put_chunk(chunk_hash, contents)
                            .await
                            .map(|_| ())
                            .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?;

                        tree_files.push(crate::cas_tree::TreeFile {
                            path: bin.sources.clone(),
                            kind: DirEntryKind::SmallFile {
                                content_hash,
                                size,
                                executable: false,
                            },
                        });
                    }
                }

                // Build recursive Merkle directory tree
                let (node_hash, _total_size) =
                    crate::cas_tree::build_merkle_tree(&self.store, tree_files)
                        .await
                        .map_err(VxdError::CasRpc)?;

                let source_manifest = node_hash.0;

                info!(
                    source_manifest = %source_manifest,
                    "Source tree ingested"
                );

                // Create action graph for C project
                use crate::action_graph::{Action, ActionGraph};
                let mut graph = ActionGraph::new();

                // Add AcquireZigToolchain action (root node)
                let zig_version = "0.15.2".to_string();
                let zig_node = graph.add_action(Action::AcquireZigToolchain {
                    version: zig_version.clone(),
                });

                // Process each binary target
                for bin in &manifest.bins {
                    // Parse source files (simple glob for now - just one file)
                    let sources = vec![bin.sources.clone()];

                    // Add run actions for each source file -> produces logical "object"
                    let mut object_nodes = Vec::new();
                    for source in &sources {
                        let output =
                            format!(".vx/obj/{}.o", source.replace('/', "_").replace(".c", ""));
                        let object_filename = output
                            .rsplit_once('/')
                            .map(|(_, f)| f.to_string())
                            .unwrap_or_else(|| output.clone());
                        let out_rel = format!("out/{object_filename}");

                        let mut args = vec![
                            vx_runner_proto::RunArg::literal("cc"),
                            vx_runner_proto::RunArg::literal("-c"),
                            vx_runner_proto::RunArg::path(format!("src/{source}")),
                            vx_runner_proto::RunArg::literal("-o"),
                            vx_runner_proto::RunArg::path(out_rel.clone()),
                            vx_runner_proto::RunArg::literal("-target"),
                            vx_runner_proto::RunArg::literal(target_triple.clone()),
                        ];

                        if profile == "release" {
                            args.push(vx_runner_proto::RunArg::literal("-O3"));
                        } else {
                            args.push(vx_runner_proto::RunArg::literal("-O0"));
                            args.push(vx_runner_proto::RunArg::literal("-g"));
                        }

                        let compile_node = graph.add_action(Action::Run {
                            label: format!("cc {source}"),
                            kind: crate::action_graph::RunKind::Generic,
                            toolchain: crate::action_graph::RunToolchain::Zig,
                            program_rel: "zig".to_string(),
                            args,
                            env: vec![],
                            cwd: None,
                            clear_env: true,
                            tree_mounts: vec![vx_runner_proto::RunTreeMount {
                                mount: "src".to_string(),
                                manifest: source_manifest,
                            }],
                            dep_tree_mounts: vec![],
                            file_mounts: vec![],
                            dep_file_mounts: vec![],
                            collect_file_mounts: vec![],
                            outputs: vec![vx_runner_proto::RunOutput {
                                logical: "object".to_string(),
                                path: out_rel,
                                executable: false,
                            }],
                            writable_roots: vec!["out".to_string()],
                            materialize: vec![],
                        });

                        // Depends on Zig toolchain
                        graph.add_edge(zig_node, compile_node);
                        object_nodes.push(compile_node);
                    }

                    // Add run action to link -> produces logical "binary"
                    let binary_output = format!("bin/{}", bin.name);
                    let binary_filename = binary_output
                        .rsplit_once('/')
                        .map(|(_, f)| f.to_string())
                        .unwrap_or_else(|| binary_output.clone());
                    let out_rel = format!("out/{binary_filename}");

                    let mut args = vec![
                        vx_runner_proto::RunArg::literal("cc"),
                        vx_runner_proto::RunArg::literal("-target"),
                        vx_runner_proto::RunArg::literal(target_triple.clone()),
                    ];
                    for (idx, _) in sources.iter().enumerate() {
                        args.push(vx_runner_proto::RunArg::path(format!("obj/{idx:04}.o")));
                    }
                    args.push(vx_runner_proto::RunArg::literal("-o"));
                    args.push(vx_runner_proto::RunArg::path(out_rel.clone()));

                    let link_node = graph.add_action(Action::Run {
                        label: format!("link {}", bin.name),
                        kind: crate::action_graph::RunKind::Generic,
                        toolchain: crate::action_graph::RunToolchain::Zig,
                        program_rel: "zig".to_string(),
                        args,
                        env: vec![],
                        cwd: None,
                        clear_env: true,
                        tree_mounts: vec![],
                        dep_tree_mounts: vec![],
                        file_mounts: vec![],
                        dep_file_mounts: vec![],
                        collect_file_mounts: vec![crate::action_graph::RunCollectInput {
                            logical: "object".to_string(),
                            path_prefix: "obj/".to_string(),
                            path_suffix: ".o".to_string(),
                            pad_width: 4,
                        }],
                        outputs: vec![vx_runner_proto::RunOutput {
                            logical: "binary".to_string(),
                            path: out_rel,
                            executable: true,
                        }],
                        writable_roots: vec!["out".to_string()],
                        materialize: vec![crate::action_graph::RunMaterialize {
                            logical: "binary".to_string(),
                            workspace_rel_path: binary_output.clone(),
                        }],
                    });

                    // Depends on all object files and Zig toolchain
                    graph.add_edge(zig_node, link_node);
                    for obj_node in object_nodes {
                        graph.add_edge(obj_node, link_node);
                    }
                }

                // Dump graph for debugging
                graph.dump_graph();

                // Set total number of actions for progress tracking
                let total_actions = graph.node_count();
                self.report_set_total(total_actions as u64).await;

                // Execute action graph
                let max_concurrency = std::thread::available_parallelism()
                    .map(|n| n.get() * 2)
                    .unwrap_or(16);

                let progress_listener = self.progress_listener.lock().clone();
                let session_id = self.session_id().await;
                let exec = self.local_runner().await?;

                let mut executor = crate::executor::Executor::new(
                    graph,
                    progress_listener,
                    self.store.clone(),
                    exec,
                    self.db.clone(),
                    project_dir.to_path_buf(),
                    target_triple.clone(),
                    profile.to_string(),
                    session_id,
                    max_concurrency,
                );

                // Register the executor's cancel token so signal handlers can cancel this build
                *self.active_build_cancel.lock() = Some(executor.cancel_token());

                let build_start = std::time::Instant::now();
                let exec_result = executor.execute().await;
                let duration = build_start.elapsed();

                // Clear the cancel token now that the build is done
                *self.active_build_cancel.lock() = None;
                exec_result?;

                // Get execution statistics
                let exec_stats = executor.get_stats();

                info!(
                    duration_ms = duration.as_millis(),
                    cache_hits = exec_stats.cache_hits,
                    rebuilt = exec_stats.rebuilt,
                    "C build completed successfully"
                );

                Ok(BuildResult {
                    success: true,
                    message: format!("Built {} C binary targets", manifest.bins.len()),
                    cached: exec_stats.cache_hits == total_actions,
                    duration_ms: duration.as_millis() as u64,
                    output_path: exec_stats.bin_output,
                    error: None,
                    failure: None,
                    total_actions,
                    cache_hits: exec_stats.cache_hits,
                    rebuilt: exec_stats.rebuilt,
                    cancelled: exec_stats.cancelled,
                })
            }
            Language::Rust => {
                // TODO: Convert vx.styx Rust project to Cargo.toml path
                // For now, fail with helpful message
                Err(VxdError::InvalidManifest(
                    "Rust projects via vx.styx not yet supported - use Cargo.toml for now"
                        .to_string(),
                ))
            }
        }
    }

    /// Build a project.
    pub async fn do_build(&self, request: BuildRequest) -> Result<BuildResult> {
        let project_path = &request.project_path;
        let total_start = std::time::Instant::now();

        let target_triple = self.exec_host_triple.clone();

        // Check for vx.styx first (unified project manifest)
        let vx_styx_path = project_path.join("vx.styx");
        if vx_styx_path.exists() {
            info!("Found vx.styx, using unified project manifest");
            return self.build_from_vx_styx(request, vx_styx_path).await;
        }

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

        let cargo_manifest_path = project_path.join("Cargo.toml");
        if !cargo_manifest_path.exists() {
            return Err(VxdError::InvalidManifest(
                "expected vx.styx or Cargo.toml at the project root".to_string(),
            ));
        }

        // r[impl eval.entry.cargo-compat]
        // Fall back to Cargo.toml (Rust-only projects) only when no build.vx exists.
        // Build the crate graph with lockfile support
        let graph = CrateGraph::build_with_lockfile(project_path)
            .map_err(|e| VxdError::CrateGraph(format!("{e:?}")))?;

        let profile = if request.release { "release" } else { "debug" };

        info!(
            workspace_root = %graph.workspace_root,
            path_crate_count = graph.nodes.len(),
            registry_crate_count = graph.iter_registry_crates().count(),
            "resolved crate graph, building action graph"
        );

        let mut rule_plan_plan: Option<RulePlan> = None;
        let mut rule_plan_topology: Option<vx_report::GraphTopology> = None;
        if rule_plan_bridge_enabled() {
            info!(
                "VX_RULE_PLAN_BRIDGE is enabled; validating/lowering RulePlan and using RulePlan topology in report"
            );
            let plan = if let Some(rules_file) = vx_rules_file() {
                let rules_path = if rules_file.is_absolute() {
                    rules_file
                } else {
                    graph.workspace_root.join(rules_file)
                };
                let source_rel = rules_path
                    .strip_prefix(&graph.workspace_root)
                    .ok()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| rules_path.clone());

                info!(rules = %source_rel, "loading .vx rules file for RulePlan bridge");
                let contents = ::moire::fs::read_to_string(&rules_path)
                    .await
                    .map_err(|e| VxdError::FileRead {
                        path: rules_path.clone(),
                        message: e.to_string(),
                    })?;

                let source_id =
                    vx_plan::RulePlanSourceId::from(format!("vx:{}", source_rel.as_str()));
                let source = vx_plan::RulePlanSource {
                    id: source_id.clone(),
                    path: source_rel.clone(),
                };

                let program = vx_plan::vx_lang::parse_vx_program(
                    rules_path.as_path(),
                    source_id.clone(),
                    &contents,
                )
                .map_err(|e| {
                    VxdError::InvalidManifest(format!("failed to parse {}: {}", source_rel, e))
                })?;

                struct VxdRulesHost<'a> {
                    inner: vx_plan::runtime::RulePlanHost,
                    crate_graph: &'a CrateGraph,
                }

                impl<'a> VxdRulesHost<'a> {
                    fn into_inner(self) -> vx_plan::runtime::RulePlanHost {
                        self.inner
                    }
                }

                impl<'a> vx_plan::runtime::PlannerHost for VxdRulesHost<'a> {
                    fn register_builtin(
                        &mut self,
                        builtin: vx_plan::runtime::PlannerBuiltin,
                    ) -> std::result::Result<(), String> {
                        self.inner.register_builtin(builtin)
                    }

                    fn invoke_builtin(
                        &mut self,
                        builtin: vx_plan::runtime::PlannerBuiltinId,
                        args: vx_plan::runtime::PlannerBuiltinArgs,
                        site: &vx_plan::runtime::PlannerCallSite,
                    ) -> std::result::Result<vx_plan::runtime::PlannerValue, String>
                    {
                        use vx_plan::runtime::{PlannerBuiltinArgs, PlannerBuiltinId};

                        match (builtin, args) {
                            (
                                PlannerBuiltinId::RustCrateGraph,
                                PlannerBuiltinArgs::RustCrateGraph,
                            ) => Ok(crate_graph_to_planner_value(self.crate_graph)),
                            (builtin, args) => self.inner.invoke_builtin(builtin, args, site),
                        }
                    }
                }

                fn crate_graph_to_planner_value(
                    crate_graph: &CrateGraph,
                ) -> vx_plan::runtime::PlannerValue {
                    use vx_plan::runtime::PlannerValue;

                    let mut crates = crate_graph.nodes.values().collect::<Vec<_>>();
                    crates.sort_by(|a, b| {
                        a.crate_name
                            .cmp(&b.crate_name)
                            .then_with(|| a.crate_type.as_str().cmp(b.crate_type.as_str()))
                            .then_with(|| a.id.to_string().cmp(&b.id.to_string()))
                    });

                    let crates_value = crates
                        .into_iter()
                        .map(|node| {
                            let deps = {
                                let mut deps = node
                                    .deps
                                    .iter()
                                    .map(|d| d.crate_id.to_string())
                                    .collect::<Vec<_>>();
                                deps.sort();
                                deps.into_iter()
                                    .map(PlannerValue::String)
                                    .collect::<Vec<_>>()
                            };

                            let mut source = std::collections::BTreeMap::new();
                            match &node.source {
                                vx_rs::crate_graph::CrateSource::Path { workspace_rel } => {
                                    source.insert(
                                        "kind".to_string(),
                                        PlannerValue::Keyword("path".to_string()),
                                    );
                                    source.insert(
                                        "workspace_rel".to_string(),
                                        PlannerValue::String(workspace_rel.to_string()),
                                    );
                                }
                                vx_rs::crate_graph::CrateSource::Registry {
                                    name,
                                    version,
                                    checksum,
                                } => {
                                    source.insert(
                                        "kind".to_string(),
                                        PlannerValue::Keyword("registry".to_string()),
                                    );
                                    source.insert(
                                        "name".to_string(),
                                        PlannerValue::String(name.clone()),
                                    );
                                    source.insert(
                                        "version".to_string(),
                                        PlannerValue::String(version.clone()),
                                    );
                                    source.insert(
                                        "checksum".to_string(),
                                        PlannerValue::String(checksum.clone()),
                                    );
                                }
                            }

                            let mut m = std::collections::BTreeMap::new();
                            m.insert("id".to_string(), PlannerValue::String(node.id.to_string()));
                            m.insert(
                                "name".to_string(),
                                PlannerValue::String(node.crate_name.clone()),
                            );
                            m.insert(
                                "type".to_string(),
                                PlannerValue::String(node.crate_type.as_str().to_string()),
                            );
                            m.insert("deps".to_string(), PlannerValue::Vector(deps));
                            m.insert("source".to_string(), PlannerValue::Map(source));
                            PlannerValue::Map(m)
                        })
                        .collect::<Vec<_>>();

                    let topo = crate_graph
                        .topo_order
                        .iter()
                        .map(|id| PlannerValue::String(id.to_string()))
                        .collect::<Vec<_>>();

                    let mut out = std::collections::BTreeMap::new();
                    out.insert(
                        "workspace_root".to_string(),
                        PlannerValue::String(crate_graph.workspace_root.to_string()),
                    );
                    out.insert(
                        "root".to_string(),
                        PlannerValue::String(crate_graph.root_crate.to_string()),
                    );
                    out.insert("topo".to_string(), PlannerValue::Vector(topo));
                    out.insert("crates".to_string(), PlannerValue::Vector(crates_value));

                    PlannerValue::Map(out)
                }

                let mut inner = vx_plan::runtime::RulePlanHost::with_sources(vec![source]);
                inner.set_env_profile(profile.to_string());
                inner.set_env_target_triple(target_triple.to_string());

                let host = VxdRulesHost {
                    inner,
                    crate_graph: &graph,
                };

                let mut runtime = vx_plan::runtime::PlannerRuntime::new(
                    host,
                    vx_plan::runtime::PlannerPolicy::deterministic(),
                    vx_plan::runtime::planner_builtins_with_rust(),
                )
                .map_err(VxdError::InvalidManifest)?;

                runtime.evaluate_program(&program).map_err(|e| {
                    VxdError::InvalidManifest(format!("VX rules runtime error: {e}"))
                })?;
                let host = runtime.into_host().into_inner();
                let plan = host.into_plan();
                plan.validate()
                    .map_err(|e| VxdError::InvalidManifest(e.to_string()))?;
                plan
            } else {
                build_rule_plan_from_crate_graph(&graph)
            };
            rule_plan_plan = Some(plan.clone());
            let lowered = crate::action_graph::ActionGraph::from_rule_plan(&plan)
                .map_err(|e| VxdError::InvalidManifest(format!("RulePlan lowering failed: {e}")))?;
            rule_plan_topology = Some(lowered.to_topology());
        }

        use crate::action_graph::ActionGraph;
        // Execution remains on the real Rust action graph while RulePlan evolves.
        // This keeps behavior stable while letting us move planner semantics forward.
        let placeholder_hash = Blake3Hash([0u8; 32]);
        let toolchain = RustToolchain::new(
            &*self.db,
            placeholder_hash,
            placeholder_hash,
            target_triple.clone(),
            target_triple.clone(),
        )
        .map_err(|e| VxdError::Picante(e.to_string()))?;

        RustToolchainManifest::set(&*self.db, placeholder_hash)
            .map_err(|e| VxdError::Picante(e.to_string()))?;

        let session_id = self.session_id().await;
        let build_key = BuildConfig::compute_key(
            profile,
            &target_triple,
            graph.workspace_root.as_ref(),
            &session_id,
        );
        let config = BuildConfig::new(
            &*self.db,
            build_key,
            profile.to_string(),
            target_triple.clone(),
            graph.workspace_root.to_string(),
            session_id,
        )
        .map_err(|e| VxdError::Picante(e.to_string()))?;

        use vx_store_proto::RustChannel;
        let action_graph = if rule_plan_exec_enabled() {
            if !rule_plan_bridge_enabled() {
                return Err(VxdError::InvalidManifest(
                    "VX_RULE_PLAN_EXEC requires VX_RULE_PLAN_BRIDGE=1".to_string(),
                ));
            }
            let Some(plan) = rule_plan_plan.as_ref() else {
                return Err(VxdError::InvalidManifest(
                    "VX_RULE_PLAN_EXEC is enabled but RulePlan was not produced".to_string(),
                ));
            };
            info!(
                nodes = plan.nodes.len(),
                edges = plan.edges.len(),
                "RulePlan execution enabled; executing ActionGraph lowered from RulePlan"
            );
            ActionGraph::from_rule_plan_executable(
                plan,
                &graph,
                RustChannel::Stable,
                target_triple.clone(),
                toolchain,
                config,
                &self.db,
            )
            .map_err(|e| {
                VxdError::InvalidManifest(format!("RulePlan executable lowering failed: {e}"))
            })?
        } else {
            ActionGraph::from_crate_graph(
                &graph,
                RustChannel::Stable,
                target_triple.clone(),
                toolchain,
                config,
                &self.db,
            )?
        };

        // In exec mode, the executed graph is the topology we want to report.
        if rule_plan_exec_enabled() {
            rule_plan_topology = Some(action_graph.to_topology());
        }

        // Parity harness compares the legacy crate-graph planner to RulePlan topology. Once we are
        // executing RulePlan directly, parity is no longer meaningful (and will drift by design).
        let legacy_topology = action_graph.to_topology();
        if !rule_plan_exec_enabled()
            && let Some(rule_plan_topology_ref) = rule_plan_topology.as_ref()
        {
            let parity = compare_rule_plan_parity(&legacy_topology, rule_plan_topology_ref);
            if parity.has_drift() {
                let max_examples = 5;
                warn!(
                    missing = ?parity
                        .missing_in_rule_plan
                        .iter()
                        .take(max_examples)
                        .collect::<Vec<_>>(),
                    extra = ?parity
                        .extra_in_rule_plan
                        .iter()
                        .take(max_examples)
                        .collect::<Vec<_>>(),
                    legacy_focus_count = parity.legacy_focus_count,
                    rule_plan_focus_count = parity.rule_plan_focus_count,
                    "RulePlan parity drift detected for compile/registry nodes"
                );
                if rule_plan_parity_strict_enabled() {
                    return Err(VxdError::InvalidManifest(
                            "RulePlan parity drift detected (set VX_RULE_PLAN_PARITY_STRICT=0 to run non-strict)"
                                .to_string(),
                        ));
                }
            } else {
                info!(
                    focus_node_count = parity.legacy_focus_count,
                    "RulePlan parity check passed for compile/registry nodes"
                );
            }

            let edge_parity =
                compare_rule_plan_edge_parity(&legacy_topology, rule_plan_topology_ref);
            if edge_parity.has_drift() {
                let max_examples = 5;
                warn!(
                    missing = ?edge_parity
                        .missing_in_rule_plan
                        .iter()
                        .take(max_examples)
                        .collect::<Vec<_>>(),
                    extra = ?edge_parity
                        .extra_in_rule_plan
                        .iter()
                        .take(max_examples)
                        .collect::<Vec<_>>(),
                    "RulePlan edge parity drift detected for compile/registry node dependencies"
                );
                if rule_plan_parity_strict_enabled() {
                    return Err(VxdError::InvalidManifest(
                            "RulePlan edge parity drift detected (set VX_RULE_PLAN_PARITY_STRICT=0 to run non-strict)"
                                .to_string(),
                        ));
                }
            } else {
                info!("RulePlan edge parity check passed for compile/registry node dependencies");
            }
        }

        // Dump graph for debugging
        action_graph.dump_graph();

        // Set total number of actions for progress tracking
        // Total = 1 toolchain + N registry crates + M compilations
        let total_actions = action_graph.node_count();
        self.report_set_total(total_actions as u64).await;

        let max_concurrency = std::thread::available_parallelism()
            .map(|n| n.get() * 2)
            .unwrap_or(16);
        tracing::debug!("creating executor with max_concurrency={}", max_concurrency);

        // Get current progress listener (clones the Option<Arc<...>> inside the RwLock)
        let progress_listener = self.progress_listener.lock().clone();
        let session_id = self.session_id().await;
        let exec = self.local_runner().await?;

        let mut executor = Executor::new(
            action_graph,
            progress_listener,
            self.store.clone(),
            exec,
            self.db.clone(),
            graph.workspace_root.clone(),
            target_triple.clone(),
            profile.to_string(),
            session_id,
            max_concurrency,
        );

        // Register the executor's cancel token so signal handlers can cancel this build
        *self.active_build_cancel.lock() = Some(executor.cancel_token());

        tracing::debug!("calling executor.execute()");
        let exec_result = executor.execute().await;
        tracing::debug!("executor.execute() completed");

        // Clear the cancel token now that the build is done
        *self.active_build_cancel.lock() = None;
        exec_result?;

        // Append changes to WAL after build (incremental persistence)
        // Take the writer out of the mutex so we don't hold the guard across await
        let wal_writer = self.wal_writer.lock().take();
        if let Some(mut wal) = wal_writer {
            match self.db.append_to_wal(&mut wal).await {
                Ok(count) if count > 0 => {
                    info!(path = %self.picante_wal, entries = count, "appended changes to WAL");
                    if let Err(e) = wal.flush() {
                        warn!(path = %self.picante_wal, error = %e, "failed to flush WAL");
                    }
                }
                Ok(_) => {
                    debug!("no WAL changes to persist");
                }
                Err(e) => {
                    warn!(path = %self.picante_wal, error = %e, "failed to append to WAL");
                }
            }
            // Put it back
            *self.wal_writer.lock() = Some(wal);
        }

        // Get execution statistics
        let mut exec_stats = executor.get_stats();
        if let Some(rule_plan_graph) = rule_plan_topology {
            exec_stats.graph_topology = Some(rule_plan_graph);
        }
        let duration = total_start.elapsed();

        // Create and save build report
        let mut report = BuildReport::new(
            graph.workspace_root.to_string(),
            profile.to_string(),
            target_triple.clone(),
        );

        // Add node reports from execution
        for node_report in exec_stats.node_reports.iter() {
            report.add_node(node_report.clone());
        }

        // Add graph topology
        report.graph = exec_stats.graph_topology;

        // Finalize the report
        report.finalize(true, None);

        // Save the report to the project's .vx/runs/ directory
        let report_store = ReportStore::new(&graph.workspace_root);
        if let Err(e) = report_store.save(&report).await {
            warn!(error = %e, "failed to save build report");
        } else {
            debug!(run_id = %report.run_id, "saved build report");
        }

        Ok(BuildResult {
            success: true,
            message: format!(
                "Build completed: {} rebuilt, {} cached",
                exec_stats.rebuilt, exec_stats.cache_hits
            ),
            cached: exec_stats.rebuilt == 0 && exec_stats.cache_hits > 0,
            duration_ms: duration.as_millis() as u64,
            output_path: exec_stats.bin_output,
            error: None,
            failure: None,
            total_actions,
            cache_hits: exec_stats.cache_hits,
            rebuilt: exec_stats.rebuilt,
            cancelled: exec_stats.cancelled,
        })
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
