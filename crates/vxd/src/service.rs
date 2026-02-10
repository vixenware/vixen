//! VxdService implementation.
//!
//! Orchestrates builds by:
//! 1. Computing cache keys via picante
//! 2. Checking CAS for cache hits
//! 3. Sending compile requests to execd for cache misses
//! 4. Materializing outputs locally

use crate::SpawnTracker;
use crate::db::Database;
use crate::error::{Result, VxdError};
use crate::executor::Executor;
use crate::inputs::*;
use camino::Utf8PathBuf;
use facet::trace;
use picante::HasRuntime;
use picante::wal::WalWriter;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use peeps::locks::{DiagnosticAsyncMutex, DiagnosticAsyncRwLock};
use tracing::{debug, info, warn};
use vx_report::{
    BuildReport, GraphTopology, ReportStore, RuleId, RulePlan, RulePlanEdge, RulePlanEdgeKind,
    RulePlanFact, RulePlanFactId, RulePlanNode, RulePlanNodeId, RulePlanNodeKind,
    RulePlanProvenance, RulePlanSource, RulePlanSourceId, RulePlanSpan, RulePlanValue,
};
use vx_rs::CrateGraph;
use vx_runner_proto::RunnerClient;
use vx_store_proto::{
    Blake3Hash, ChunkHash, ContentHash, DirEntryKind, EnsureStatus, NodeHash, RustChannel,
    RustComponent, RustToolchainSpec, ServiceVersion, StoreClient,
};
use vxd_proto::{
    ActionType, BuildRequest, BuildResult, ProgressListenerClient, UnpublishResult,
    VXD_PROTOCOL_VERSION, Vxd,
};

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
#[allow(dead_code)]
pub struct AcquiredToolchains {
    /// Rust toolchain (if acquired)
    pub rust: Option<ToolchainInfo>,

    /// Zig toolchain (if acquired)
    pub zig: Option<ToolchainInfo>,
}

/// The daemon service implementation
#[derive(Clone)]
pub struct VxdService {
    /// CAS client for content-addressed storage
    cas: Arc<StoreClient<crate::UpstreamCaller>>,

    /// Exec client for compilation
    exec: Arc<RunnerClient<crate::UpstreamCaller>>,

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
    wal_writer: Arc<DiagnosticAsyncMutex<Option<WalWriter>>>,

    /// Acquired toolchains (manifest references only)
    toolchains: Arc<DiagnosticAsyncMutex<AcquiredToolchains>>,

    /// Spawn tracker for child services
    spawn_tracker: Arc<DiagnosticAsyncMutex<SpawnTracker>>,

    /// Progress listener client (for reporting progress to vx CLI)
    /// Uses RwLock for interior mutability so it can be updated per-connection
    progress_listener: Arc<
        DiagnosticAsyncRwLock<Option<Arc<ProgressListenerClient<roam_stream::ConnectionHandle>>>>,
    >,

    /// Session ID for cache key namespacing. Empty = session zero (shared).
    /// Per-connection state — each connection handler gets its own instance.
    session_id: Arc<DiagnosticAsyncRwLock<String>>,

    /// Cancellation token for the build active on this session (if any).
    /// Per-connection: when the CLI disconnects, only this session's build is cancelled.
    active_build_cancel: Arc<DiagnosticAsyncMutex<Option<CancellationToken>>>,
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
    /// Report progress to CLI if connected
    async fn report_start_action(&self, action_type: ActionType) -> Option<u64> {
        let listener_guard = self.progress_listener.read().await;
        if let Some(ref listener) = *listener_guard {
            listener.start_action(action_type).await.ok()
        } else {
            None
        }
    }

    async fn report_complete_action(&self, action_id: Option<u64>) {
        let listener_guard = self.progress_listener.read().await;
        if let (Some(listener), Some(id)) = (listener_guard.as_ref(), action_id) {
            let _ = listener.complete_action(id).await;
        }
    }

    async fn report_set_total(&self, total: u64) {
        let listener_guard = self.progress_listener.read().await;
        if let Some(ref listener) = *listener_guard {
            let _ = listener.set_total(total).await;
        }
    }

    /// Create a new daemon service
    pub async fn new(
        cas: Arc<StoreClient<crate::UpstreamCaller>>,
        exec: Arc<RunnerClient<crate::UpstreamCaller>>,
        vx_home: Utf8PathBuf,
        exec_host_triple: String,
        spawn_tracker: Arc<DiagnosticAsyncMutex<SpawnTracker>>,
    ) -> Self {
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
            cas,
            exec,
            exec_host_triple,
            vx_home,
            db: Arc::new(db),
            picante_cache: cache_path,
            picante_wal: wal_path,
            wal_writer: Arc::new(DiagnosticAsyncMutex::new("vxd/wal_writer", wal_writer)),
            toolchains: Arc::new(DiagnosticAsyncMutex::new(
                "vxd/toolchains",
                AcquiredToolchains {
                    rust: None,
                    zig: None,
                },
            )),
            spawn_tracker,
            progress_listener: Arc::new(DiagnosticAsyncRwLock::new("vxd/progress_listener", None)),
            session_id: Arc::new(DiagnosticAsyncRwLock::new("vxd/session_id", String::new())),
            active_build_cancel: Arc::new(DiagnosticAsyncMutex::new(
                "vxd/active_build_cancel",
                None,
            )),
        }
    }

    /// Create a per-connection session with fresh ephemeral state.
    /// Shared state (CAS, exec, DB, toolchains, WAL) is still shared via Arc.
    pub fn new_session(&self) -> Self {
        Self {
            cas: self.cas.clone(),
            exec: self.exec.clone(),
            exec_host_triple: self.exec_host_triple.clone(),
            vx_home: self.vx_home.clone(),
            // db: self.db.clone(),
            db: Arc::new(Database::new()),
            picante_cache: self.picante_cache.clone(),
            picante_wal: self.picante_wal.clone(),
            wal_writer: self.wal_writer.clone(),
            toolchains: self.toolchains.clone(),
            spawn_tracker: self.spawn_tracker.clone(),
            // Per-connection state: fresh for each session
            progress_listener: Arc::new(DiagnosticAsyncRwLock::new("vxd/progress_listener", None)),
            session_id: Arc::new(DiagnosticAsyncRwLock::new("vxd/session_id", String::new())),
            active_build_cancel: Arc::new(DiagnosticAsyncMutex::new(
                "vxd/active_build_cancel",
                None,
            )),
        }
    }

    /// Get the current session ID for this connection.
    pub async fn session_id(&self) -> String {
        self.session_id.read().await.clone()
    }

    /// Update the progress listener for the current connection
    pub async fn set_progress_listener(
        &self,
        listener: Option<Arc<ProgressListenerClient<roam_stream::ConnectionHandle>>>,
    ) {
        *self.progress_listener.write().await = listener;
    }

    /// Cancel any active build. Called by the signal handler on SIGINT/SIGTERM.
    pub async fn cancel_active_build(&self) {
        if let Some(token) = self.active_build_cancel.lock().await.as_ref() {
            info!("cancelling active build");
            token.cancel();
        }
    }

    /// Ensure Rust toolchain exists in CAS. Returns info for cache keys.
    pub async fn ensure_rust_toolchain(&self, channel: RustChannel) -> Result<ToolchainInfo> {
        // Check if already acquired
        {
            let toolchains = self.toolchains.lock().await;
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
        let forward_listener = self.progress_listener.read().await.clone();
        let forward_action_id = action_id;
        let forward_handle = peeps::tasks::spawn_tracked(
            format!("toolchain_progress_fwd_{:?}", action_id),
            async move {
                while let Ok(Some(sp)) = progress_rx.recv().await {
                    if let (Some(listener), Some(id)) = (&forward_listener, forward_action_id) {
                        let _ = listener
                            .update_progress(
                                id,
                                vxd_proto::ActionProgress {
                                    message: sp.message,
                                    current: sp.current,
                                    total: sp.total,
                                },
                            )
                            .await;
                    }
                }
            },
        );

        let result = self
            .cas
            .ensure_rust_toolchain(spec, progress_tx)
            .await
            .map_err(|e| {
                // Complete action on error
                let this = self.clone();
                peeps::tasks::spawn_tracked(
                    format!("complete_action_{:?}", action_id),
                    async move { this.report_complete_action(action_id).await },
                );
                VxdError::CasRpc(e.to_string())
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
            peeps::tasks::spawn_tracked(
                format!("complete_action_{:?}", action_id),
                async move { this.report_complete_action(action_id).await },
            );
            VxdError::NoManifestHash
        })?;

        // Get manifest for version info (and toolchain_id on cache hit)
        let manifest = self
            .cas
            .get_toolchain_manifest(manifest_hash)
            .await
            .map_err(|e| {
                let this = self.clone();
                peeps::tasks::spawn_tracked(
                    format!("complete_action_{:?}", action_id),
                    async move { this.report_complete_action(action_id).await },
                );
                VxdError::CasRpc(e.to_string())
            })?
            .ok_or_else(|| {
                let this = self.clone();
                peeps::tasks::spawn_tracked(
                    format!("complete_action_{:?}", action_id),
                    async move { this.report_complete_action(action_id).await },
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
            let mut toolchains = self.toolchains.lock().await;
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
        self.spawn_tracker.lock().await.kill_all();
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
                        let contents = tokio::fs::read(&source_path).await.map_err(|e| {
                            VxdError::FileRead {
                                path: source_path.clone(),
                                message: e.to_string(),
                            }
                        })?;

                        let size = contents.len() as u64;
                        let content_hash = ContentHash(Blake3Hash::from_bytes(&contents));
                        let chunk_hash = ChunkHash(content_hash.0);

                        // Store as chunk
                        self.cas
                            .put_chunk(chunk_hash, contents)
                            .await
                            .map_err(|e| VxdError::CasRpc(e.to_string()))?;

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
                    crate::cas_tree::build_merkle_tree(&self.cas, tree_files)
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

                    // Add CompileCObject actions for each source file
                    let mut object_nodes = Vec::new();
                    for source in &sources {
                        let output =
                            format!(".vx/obj/{}.o", source.replace('/', "_").replace(".c", ""));
                        let compile_node = graph.add_action(Action::CompileCObject {
                            source: source.clone(),
                            output: output.clone(),
                            source_manifest,
                            target_triple: target_triple.clone(),
                            profile: profile.to_string(),
                        });

                        // CompileCObject depends on Zig toolchain
                        graph.add_edge(zig_node, compile_node);
                        object_nodes.push(compile_node);
                    }

                    // Add LinkCBinary action
                    let binary_output = format!("bin/{}", bin.name);
                    let link_node = graph.add_action(Action::LinkCBinary {
                        name: bin.name.clone(),
                        objects: sources
                            .iter()
                            .map(|s| format!(".vx/obj/{}.o", s.replace('/', "_").replace(".c", "")))
                            .collect(),
                        output: binary_output.clone(),
                        target_triple: target_triple.clone(),
                    });

                    // LinkCBinary depends on all object files and Zig toolchain
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

                let progress_listener = self.progress_listener.read().await.clone();

                let mut executor = crate::executor::Executor::new(
                    graph,
                    progress_listener,
                    self.cas.clone(),
                    self.exec.clone(),
                    self.db.clone(),
                    project_dir.to_path_buf(),
                    target_triple.clone(),
                    profile.to_string(),
                    max_concurrency,
                );

                // Register the executor's cancel token so signal handlers can cancel this build
                *self.active_build_cancel.lock().await = Some(executor.cancel_token());

                let build_start = std::time::Instant::now();
                let exec_result = executor.execute().await;
                let duration = build_start.elapsed();

                // Clear the cancel token now that the build is done
                *self.active_build_cancel.lock().await = None;
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

        // Fall back to Cargo.toml (Rust-only projects)
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

        let mut rule_plan_topology: Option<vx_report::GraphTopology> = None;
        if rule_plan_bridge_enabled() {
            info!(
                "VX_RULE_PLAN_BRIDGE is enabled; validating/lowering RulePlan and using RulePlan topology in report"
            );
            let plan = build_rule_plan_from_crate_graph(&graph);
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
        let action_graph = ActionGraph::from_crate_graph(
            &graph,
            RustChannel::Stable,
            target_triple.clone(),
            toolchain,
            config,
            &self.db,
        )?;
        let legacy_topology = action_graph.to_topology();
        if let Some(rule_plan_topology_ref) = rule_plan_topology.as_ref() {
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
        let progress_listener = self.progress_listener.read().await.clone();

        let mut executor = Executor::new(
            action_graph,
            progress_listener,
            self.cas.clone(),
            self.exec.clone(),
            self.db.clone(),
            graph.workspace_root.clone(),
            target_triple.clone(),
            profile.to_string(),
            max_concurrency,
        );

        // Register the executor's cancel token so signal handlers can cancel this build
        *self.active_build_cancel.lock().await = Some(executor.cancel_token());

        tracing::debug!("calling executor.execute()");
        let exec_result = executor.execute().await;
        tracing::debug!("executor.execute() completed");

        // Clear the cancel token now that the build is done
        *self.active_build_cancel.lock().await = None;
        exec_result?;

        // Append changes to WAL after build (incremental persistence)
        if let Some(ref mut wal) = *self.wal_writer.lock().await {
            match self.db.append_to_wal(wal).await {
                Ok(count) if count > 0 => {
                    info!(path = %self.picante_wal, entries = count, "appended changes to WAL");
                    // Explicit flush to ensure durability (WAL auto-flushes after threshold, but we flush after each build)
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
        if let Err(e) = report_store.save(&report) {
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
            total_actions,
            cache_hits: exec_stats.cache_hits,
            rebuilt: exec_stats.rebuilt,
            cancelled: exec_stats.cancelled,
        })
    }
}

impl Vxd for VxdService {
    async fn build(&self, _cx: &roam::Context, request: BuildRequest) -> BuildResult {
        match self.do_build(request).await {
            Ok(result) => result,
            Err(e) => BuildResult {
                success: false,
                message: "Build failed".to_string(),
                cached: false,
                duration_ms: 0,
                output_path: None,
                // Convert structured error to string at RPC boundary
                error: Some(e.to_string()),
                total_actions: 0,
                cache_hits: 0,
                rebuilt: 0,
                cancelled: 0,
            },
        }
    }

    async fn set_session(&self, _cx: &roam::Context, session_id: String) {
        info!(session_id = %session_id, "session ID set");
        *self.session_id.write().await = session_id;
    }

    async fn unpublish_cache_keys(
        &self,
        _cx: &roam::Context,
        cache_keys: Vec<vx_store_proto::CacheKey>,
    ) -> UnpublishResult {
        let mut removed = 0u32;
        let mut not_found = 0u32;

        for key in cache_keys {
            if self.cas.unpublish(key).await.unwrap_or(false) {
                removed += 1;
            } else {
                not_found += 1;
            }
        }

        UnpublishResult { removed, not_found }
    }

    async fn shutdown(&self, _cx: &roam::Context) {
        tracing::info!("Shutdown requested");

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

        tracing::info!("Spawned services killed, exiting daemon");
        std::process::exit(0);
    }

    async fn version(&self, _cx: &roam::Context) -> ServiceVersion {
        ServiceVersion {
            service: "vxd".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: VXD_PROTOCOL_VERSION,
        }
    }
}
