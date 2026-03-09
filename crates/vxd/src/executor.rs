//! Action graph executor
//!
//! This module implements the execution engine that schedules and runs actions
//! from the action graph with optimal parallelism while respecting dependencies.

use ::moire::sync::SyncMutex;
use moire::task::FutureExt;
use petgraph::Direction::Incoming;
use petgraph::graph::NodeIndex;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace, warn};

use crate::action_graph::{Action, ActionGraph};
use crate::error::VxdError;
use vx_rs::CrateId;
use vx_runner_proto::RunnerClient;
use vx_store_proto::{
    Blake3Hash, BlobRef, CasNode, ChunkHash, ContentHash, DirEntryKind, NodeHash, OutputBlobRef,
    RustComponent, RustToolchainSpec, StoreClient, StoreProgress,
};
use vxd_proto::{ActionProgress, ProgressListenerClient};

/// State of an individual action
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionState {
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Result from executing an action
#[derive(Debug, Clone)]
pub enum ActionResult {
    /// Rust toolchain component tarball downloaded successfully.
    RustToolchainComponentDownloaded {
        /// Component that was downloaded.
        component: RustComponent,
        /// Tarball blob ref (None if full toolchain was already cached).
        tarball: Option<BlobRef>,
        /// Whether this was a cache hit
        was_cached: bool,
    },

    /// Rust toolchain component extracted successfully.
    RustToolchainComponentExtracted {
        /// Component that was extracted.
        component: RustComponent,
        /// Tree root node hash (None if full toolchain was already cached).
        tree_root: Option<NodeHash>,
        /// Whether this was a cache hit
        was_cached: bool,
    },

    /// Registry crate acquired successfully
    RegistryCrateAcquired {
        /// Crate name
        name: String,
        /// Crate version
        version: String,
        /// Manifest hash
        manifest_hash: Blake3Hash,
        /// Whether this was a cache hit
        was_cached: bool,
    },

    /// Zig toolchain acquired successfully
    ZigToolchainAcquired {
        /// Toolchain ID
        toolchain_id: Blake3Hash,
        /// Toolchain manifest hash
        manifest_hash: Blake3Hash,
        /// Whether this was a cache hit
        was_cached: bool,
    },

    /// Rust toolchain finalized (ToolchainManifest ready for runner).
    RustToolchainFinalized {
        toolchain_id: Blake3Hash,
        manifest_hash: Blake3Hash,
        was_cached: bool,
    },

    /// A source tree was ingested/extracted into a CAS tree manifest.
    RustSourceTreeReady {
        crate_id: CrateId,
        source_manifest: Blake3Hash,
        was_cached: bool,
        cas_cache_key: Blake3Hash,
    },

    /// A `run` node completed successfully.
    RunCompleted {
        /// Classification for the run (e.g. Rust compile vs generic tool invocation).
        kind: crate::action_graph::RunKind,
        /// Output manifest hash from CAS.
        output_manifest: Blake3Hash,
        /// Path to a materialized binary (if this run produced one).
        bin_output: Option<camino::Utf8PathBuf>,
        /// Whether this was a cache hit.
        was_cached: bool,
        /// Actual CAS cache key used for publish/lookup.
        cas_cache_key: Blake3Hash,
    },

    /// Synthetic planner node executed by the RulePlan lowering bridge.
    PlannerSyntheticExecuted { node_id: String, kind: String },
}

/// Statistics from build execution
#[derive(Debug, Clone, Default)]
pub struct ExecutionStats {
    /// Number of cache hits
    pub cache_hits: usize,
    /// Number of crates rebuilt
    pub rebuilt: usize,
    /// Number of actions cancelled
    pub cancelled: usize,
    /// Path to final bin output (if any)
    pub bin_output: Option<camino::Utf8PathBuf>,
    /// Per-node reports for build report generation
    pub node_reports: Vec<vx_report::NodeReport>,
    /// Graph topology for visualization
    pub graph_topology: Option<vx_report::GraphTopology>,
}

/// Message from spawned task to executor
enum ExecutorMessage {
    ActionCompleted {
        node_idx: NodeIndex,
        result: Result<ActionResult, VxdError>,
        execute_ms: u64,
        start_offset_ms: u64,
        lane: u32,
    },
}

/// Drop guard that sends a Cancelled result if the task is aborted without sending a result.
///
/// If JoinSet::abort_all() kills a task, the normal send at the end of spawn_action never
/// fires. This guard ensures the executor always receives a completion message.
struct CompletionGuard {
    node_idx: NodeIndex,
    tx: Option<moire::sync::mpsc::UnboundedSender<ExecutorMessage>>,
}

impl CompletionGuard {
    /// Disarm the guard (call this right before the normal send).
    fn disarm(&mut self) {
        self.tx = None;
    }
}

async fn load_manifest(
    cas: &Arc<StoreClient>,
    manifest_hash: NodeHash,
    crate_name: &str,
) -> Result<vx_store_proto::NodeManifest, VxdError> {
    let node_bytes = cas
        .get_node(manifest_hash)
        .await
        .map_err(|e| VxdError::Compilation {
            crate_name: crate_name.to_string(),
            message: format!("Failed to fetch manifest from CAS: {e:?}"),
        })?
        .ok_or(VxdError::OutputManifestNotFound(manifest_hash))?;
    match CasNode::from_bytes(&node_bytes) {
        Ok(CasNode::Manifest(m)) => Ok(m),
        Ok(_) => Err(VxdError::Compilation {
            crate_name: crate_name.to_string(),
            message: format!("Node {} is not a Manifest", manifest_hash.to_hex()),
        }),
        Err(e) => Err(VxdError::Compilation {
            crate_name: crate_name.to_string(),
            message: format!(
                "Failed to deserialize manifest node {}: {}",
                manifest_hash.to_hex(),
                e
            ),
        }),
    }
}

async fn fetch_blob_from_ref(
    cas: &Arc<StoreClient>,
    blob_ref: &BlobRef,
) -> Result<Vec<u8>, VxdError> {
    match blob_ref {
        BlobRef::Small { chunk, .. } => {
            let data = cas
                .get_chunk(*chunk)
                .await
                .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?
                .ok_or_else(|| VxdError::BlobNotFound(format!("chunk {}", chunk.to_hex())))?;
            Ok(data)
        }
        BlobRef::Large {
            blob_node,
            content_hash: _,
            size: _,
        } => {
            let node_bytes = cas
                .get_node(*blob_node)
                .await
                .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?
                .ok_or_else(|| VxdError::BlobNotFound(format!("blob node {}", blob_node)))?;
            let blob_node_data = match CasNode::from_bytes(&node_bytes) {
                Ok(CasNode::Blob(b)) => b,
                Ok(_) => {
                    return Err(VxdError::BlobNotFound(format!(
                        "node {} is not a BlobNode",
                        blob_node.to_hex()
                    )));
                }
                Err(e) => return Err(VxdError::CasRpc(format!("{e:?}"))),
            };

            let mut result = Vec::with_capacity(blob_node_data.total_size as usize);
            for chunk_entry in &blob_node_data.chunks {
                let chunk_data = cas
                    .get_chunk(chunk_entry.hash)
                    .await
                    .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?
                    .ok_or_else(|| {
                        VxdError::BlobNotFound(format!("chunk {}", chunk_entry.hash.to_hex()))
                    })?;
                result.extend_from_slice(&chunk_data);
            }
            Ok(result)
        }
    }
}

async fn fetch_output_blob_data(
    cas: &Arc<StoreClient>,
    blob_ref: &OutputBlobRef,
) -> Result<Vec<u8>, VxdError> {
    match blob_ref {
        OutputBlobRef::Small { chunk, .. } => {
            let data = cas
                .get_chunk(*chunk)
                .await
                .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?
                .ok_or_else(|| VxdError::BlobNotFound(format!("chunk {}", chunk)))?;
            Ok(data)
        }
        OutputBlobRef::Large {
            blob_node,
            content_hash: _,
            size: _,
        } => {
            let node_bytes = cas
                .get_node(*blob_node)
                .await
                .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?
                .ok_or_else(|| VxdError::BlobNotFound(format!("blob node {}", blob_node)))?;
            let blob_node_data = match CasNode::from_bytes(&node_bytes) {
                Ok(CasNode::Blob(b)) => b,
                Ok(_) => {
                    return Err(VxdError::BlobNotFound(format!(
                        "node {} is not a BlobNode",
                        blob_node
                    )));
                }
                Err(e) => {
                    return Err(VxdError::CasRpc(format!(
                        "failed to deserialize blob node {}: {}",
                        blob_node, e
                    )));
                }
            };

            let mut result = Vec::with_capacity(blob_node_data.total_size as usize);
            for chunk_entry in &blob_node_data.chunks {
                let chunk_data = cas
                    .get_chunk(chunk_entry.hash)
                    .await
                    .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?
                    .ok_or_else(|| VxdError::BlobNotFound(format!("chunk {}", chunk_entry.hash)))?;
                result.extend_from_slice(&chunk_data);
            }
            Ok(result)
        }
    }
}

fn namespaced_run_cache_key(base: Blake3Hash, session_id: &str) -> Blake3Hash {
    if session_id.is_empty() {
        return base;
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"vx-run-session-v1\n");
    hasher.update(b"session:");
    hasher.update(session_id.as_bytes());
    hasher.update(b"\nbase:");
    hasher.update(&base.0);
    Blake3Hash(*hasher.finalize().as_bytes())
}

fn namespaced_action_cache_key(base: Blake3Hash, session_id: &str, domain: &str) -> Blake3Hash {
    if session_id.is_empty() {
        return base;
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"vx-action-session-v1\n");
    hasher.update(b"domain:");
    hasher.update(domain.as_bytes());
    hasher.update(b"\nsession:");
    hasher.update(session_id.as_bytes());
    hasher.update(b"\nbase:");
    hasher.update(&base.0);
    Blake3Hash(*hasher.finalize().as_bytes())
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(ExecutorMessage::ActionCompleted {
                node_idx: self.node_idx,
                result: Err(VxdError::Cancelled),
                execute_ms: 0,
                start_offset_ms: 0,
                lane: 0,
            });
        }
    }
}

/// Executor for action graph
pub struct Executor {
    /// The action graph (owned by executor, no lock needed)
    graph: ActionGraph,

    /// Current state of each action (owned by executor)
    states: HashMap<NodeIndex, ActionState>,

    /// Results from completed actions (shared read-only with spawned tasks)
    results: Arc<SyncMutex<HashMap<NodeIndex, ActionResult>>>,

    /// Remaining dependency count for each action (owned by executor)
    remaining_deps: HashMap<NodeIndex, usize>,

    /// Queue of actions ready to execute (owned by executor)
    ready_queue: VecDeque<NodeIndex>,

    /// Semaphore limiting concurrency
    concurrency_limit: Arc<Semaphore>,

    /// Channel for messages from spawned tasks
    message_tx: moire::sync::mpsc::UnboundedSender<ExecutorMessage>,
    message_rx: moire::sync::mpsc::UnboundedReceiver<ExecutorMessage>,

    /// In-flight action tasks (for waiting on cancellation)
    in_flight: moire::task::JoinSet<()>,

    /// Cancellation token — cancel all in-flight actions
    cancel_token: CancellationToken,

    /// Progress listener client (for reporting progress to vx CLI)
    progress_listener: Option<Arc<ProgressListenerClient>>,

    /// CAS client (CAS)
    cas: Arc<StoreClient>,

    /// Runner client for execution
    exec: Arc<RunnerClient>,

    /// Picante database (Arc-shareable with internal synchronization)
    db: Arc<crate::db::Database>,

    /// Per-node timing data: (execute_ms, start_offset_ms, lane)
    node_timings: HashMap<NodeIndex, (u64, u64, u32)>,

    /// Build start instant for computing start_offset_ms
    build_start: std::time::Instant,

    /// Free-list of concurrency lane IDs (0..max_concurrency)
    free_lanes: Arc<SyncMutex<Vec<u32>>>,

    /// Execution statistics (owned by executor)
    stats: ExecutionStats,

    /// First error encountered during execution (owned by executor)
    first_error: Option<VxdError>,

    /// Workspace root for bin materialization
    workspace_root: camino::Utf8PathBuf,

    /// Target triple for bin materialization
    target_triple: String,

    /// Profile for bin materialization
    profile: String,

    /// Session namespace for cache key isolation (tests/parallel builds).
    session_id: String,

    /// CommandLineTools version on macOS (for C/C++ cache keys)
    macos_commandlinetools_version: Option<String>,
}

impl Executor {
    /// Create a new executor
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        graph: ActionGraph,
        progress_listener: Option<Arc<ProgressListenerClient>>,
        cas: Arc<StoreClient>,
        exec: Arc<RunnerClient>,
        db: Arc<crate::db::Database>,
        workspace_root: camino::Utf8PathBuf,
        target_triple: String,
        profile: String,
        session_id: String,
        max_concurrency: usize,
    ) -> Self {
        trace!("starting initialization");
        trace!("graph has {} nodes", graph.graph.node_count());

        // Initialize remaining deps count and ready queue
        // We own the graph here, so no locking needed
        let mut remaining_deps = HashMap::new();
        let mut ready_queue = VecDeque::new();

        for node_idx in graph.graph.node_indices() {
            // Count outgoing edges (dependencies)
            let dep_count = graph.graph.neighbors(node_idx).count();
            let action = &graph.graph[node_idx].action;
            trace!(
                "node {:?} action={} dep_count={}",
                node_idx,
                action.display_name(),
                dep_count
            );
            remaining_deps.insert(node_idx, dep_count);

            // If no dependencies, it's ready immediately
            if dep_count == 0 {
                debug!("READY node {:?} action={}", node_idx, action.display_name());
                ready_queue.push_back(node_idx);
            }
        }

        debug!(
            "initialized with ready_queue_size={} total_nodes={}",
            ready_queue.len(),
            graph.graph.node_count()
        );

        // Log first few ready actions for debugging
        for (i, &node_idx) in ready_queue.iter().enumerate().take(3) {
            let action = &graph.graph[node_idx].action;
            debug!("ready[{}] = {:?} {}", i, node_idx, action.display_name());
        }

        let (message_tx, message_rx) = moire::sync::mpsc::unbounded_channel("executor_messages");

        // Detect CommandLineTools version on macOS
        #[cfg(target_os = "macos")]
        let macos_commandlinetools_version = {
            match vx_toolchain::detect_command_line_tools_version() {
                Ok(Some(version)) => {
                    let version_str = version.as_cache_key_string();
                    info!("Detected CommandLineTools version: {}", version_str);
                    Some(version_str)
                }
                Ok(None) => {
                    warn!("No CommandLineTools found on macOS");
                    None
                }
                Err(e) => {
                    warn!("Failed to detect CommandLineTools version: {}", e);
                    None
                }
            }
        };

        #[cfg(not(target_os = "macos"))]
        let macos_commandlinetools_version = None;

        Self {
            graph,
            states: HashMap::new(),
            results: Arc::new(SyncMutex::new("vxd/executor_results", HashMap::new())),
            remaining_deps,
            ready_queue,
            concurrency_limit: Arc::new(Semaphore::new(max_concurrency)),
            message_tx,
            message_rx,
            in_flight: moire::task::JoinSet::named("vxd/executor_in_flight"),
            cancel_token: CancellationToken::new(),
            progress_listener,
            cas,
            exec,
            db,
            node_timings: HashMap::new(),
            build_start: std::time::Instant::now(),
            free_lanes: Arc::new(SyncMutex::new(
                "vxd/executor_free_lanes",
                (0..max_concurrency as u32).collect(),
            )),
            stats: ExecutionStats::default(),
            first_error: None,
            workspace_root,
            target_triple,
            profile,
            session_id,
            macos_commandlinetools_version,
        }
    }

    /// Get execution statistics
    pub fn get_stats(&self) -> ExecutionStats {
        self.stats.clone()
    }

    /// Get the cancellation token for this executor.
    ///
    /// External code (e.g. signal handlers) can call `.cancel()` on this token
    /// to request cancellation of all in-flight actions.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Wait for all in-flight tasks to finish after cancellation.
    ///
    /// Gives tasks a brief window to observe the cancellation token and exit.
    /// After the timeout, remaining tasks are aborted.
    async fn drain_in_flight(&mut self) {
        const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

        if self.in_flight.is_empty() {
            return;
        }

        let in_flight_count = self.in_flight.len();
        debug!(
            count = in_flight_count,
            "draining in-flight tasks after cancellation"
        );

        // Wait for tasks to finish, with a timeout
        let drain_result = tokio::time::timeout(DRAIN_TIMEOUT, async {
            while self.in_flight.join_next().await.is_some() {}
        })
        .await;

        if drain_result.is_err() {
            let remaining = self.in_flight.len();
            warn!(remaining, "timed out waiting for in-flight tasks, aborting");
            self.in_flight.abort_all();
        }

        // Mark any still-Running actions as Cancelled
        let mut newly_cancelled = 0usize;
        for (_, state) in self.states.iter_mut() {
            if *state == ActionState::Running {
                *state = ActionState::Cancelled;
                newly_cancelled += 1;
            }
        }
        if newly_cancelled > 0 {
            info!(newly_cancelled, "marked in-flight actions as cancelled");
        }

        // Count all cancelled actions (those cancelled via token + those just force-cancelled)
        self.stats.cancelled = self
            .states
            .values()
            .filter(|s| **s == ActionState::Cancelled)
            .count();
    }

    /// Main execution loop
    pub async fn execute(&mut self) -> Result<(), VxdError> {
        info!("starting action graph execution");
        self.build_start = std::time::Instant::now();

        let result = self.execute_inner().await;

        if result.is_err() {
            // Signal all in-flight tasks to stop
            self.cancel_token.cancel();
            // Wait for them to finish (or abort after timeout)
            self.drain_in_flight().await;
        }

        result
    }

    /// Inner execution loop (separated so cancellation cleanup always runs)
    async fn execute_inner(&mut self) -> Result<(), VxdError> {
        loop {
            // Check if cancellation was requested externally
            if self.cancel_token.is_cancelled() {
                warn!("cancellation requested");
                return Err(VxdError::Cancelled);
            }

            // Spawn all ready actions
            while let Some(node_idx) = self.ready_queue.pop_front() {
                debug!("spawning action for node {:?}", node_idx);
                self.states.insert(node_idx, ActionState::Running);
                let permit = self
                    .concurrency_limit
                    .clone()
                    .acquire_owned()
                    .named("executor.concurrency.acquire_owned")
                    .await
                    .unwrap();
                self.spawn_action(node_idx, permit);
            }

            // Check if we're done
            let total = self.graph.graph.node_count();
            let completed = self
                .states
                .values()
                .filter(|&&s| s == ActionState::Completed)
                .count();
            if completed == total && total > 0 {
                info!("all {} actions completed", total);
                break;
            }

            // Wait for a completion message
            match self.message_rx.recv().await {
                Some(ExecutorMessage::ActionCompleted {
                    node_idx,
                    result,
                    execute_ms,
                    start_offset_ms,
                    lane,
                }) => {
                    self.node_timings
                        .insert(node_idx, (execute_ms, start_offset_ms, lane));
                    match result {
                        Ok(action_result) => {
                            debug!(
                                "action {:?} completed successfully ({execute_ms}ms)",
                                node_idx
                            );
                            self.states.insert(node_idx, ActionState::Completed);
                            self.results.lock().insert(node_idx, action_result);

                            // Decrement dependency counts and add newly ready actions to queue
                            for dependent_idx in
                                self.graph.graph.neighbors_directed(node_idx, Incoming)
                            {
                                let count = self.remaining_deps.get_mut(&dependent_idx).unwrap();
                                *count -= 1;
                                if *count == 0 {
                                    debug!("action {:?} now ready", dependent_idx);
                                    self.ready_queue.push_back(dependent_idx);
                                }
                            }
                        }
                        Err(ref error) if matches!(error, VxdError::Cancelled) => {
                            debug!("action {:?} cancelled", node_idx);
                            self.states.insert(node_idx, ActionState::Cancelled);
                        }
                        Err(error) => {
                            debug!("action {:?} failed: {}", node_idx, error);
                            self.states.insert(node_idx, ActionState::Failed);
                            self.first_error = Some(error.clone());
                            return Err(error);
                        }
                    }
                }
                None => {
                    // Channel closed
                    break;
                }
            }
        }

        // Compute stats from results
        let results_lock = self.results.lock();
        let mut cache_hits = 0;
        let mut rebuilt = 0;
        let mut bin_output = None;
        let mut node_reports = Vec::new();
        let node_timings = &self.node_timings;

        for (node_idx, result) in results_lock.iter() {
            let action = &self.graph.graph[*node_idx].action;

            match result {
                ActionResult::RustToolchainComponentDownloaded {
                    component,
                    tarball,
                    was_cached,
                } => {
                    if *was_cached {
                        cache_hits += 1;
                    } else {
                        rebuilt += 1;
                    }

                    let tarball_id = tarball.as_ref().map(|r| match r {
                        BlobRef::Small { chunk, .. } => chunk.to_hex(),
                        BlobRef::Large { blob_node, .. } => blob_node.to_hex(),
                    });
                    let cache_key = tarball_id
                        .clone()
                        .unwrap_or_else(|| format!("rust-toolchain-download:{:?}", component));
                    node_reports.push(vx_report::NodeReport {
                        node_id: action.display_name(),
                        kind: "toolchain.component.download".to_string(),
                        cache_key: cache_key.clone(),
                        cache: if *was_cached {
                            vx_report::CacheOutcome::Hit {
                                manifest: cache_key,
                            }
                        } else {
                            vx_report::CacheOutcome::Miss {
                                reason: vx_report::MissReason::FirstBuild,
                            }
                        },
                        timing: {
                            let &(exec, start, ln) =
                                node_timings.get(node_idx).unwrap_or(&(0, 0, 0));
                            vx_report::NodeTiming {
                                execute_ms: exec,
                                start_offset_ms: start,
                                lane: ln,
                                ..Default::default()
                            }
                        },
                        inputs: vec![],
                        deps: vec![],
                        outputs: vec![vx_report::OutputRecord {
                            logical: format!("tarball_{:?}", component).to_lowercase(),
                            manifest: tarball_id,
                            blob: None,
                            path: None,
                        }],
                        invocation: None,
                        diagnostics: vx_report::DiagnosticsRecord::default(),
                        cas_cache_key: None,
                    });
                }
                ActionResult::RustToolchainComponentExtracted {
                    component,
                    tree_root,
                    was_cached,
                } => {
                    if *was_cached {
                        cache_hits += 1;
                    } else {
                        rebuilt += 1;
                    }

                    let cache_key = tree_root
                        .map(|h| h.to_hex())
                        .unwrap_or_else(|| format!("rust-toolchain-extract:{:?}", component));
                    node_reports.push(vx_report::NodeReport {
                        node_id: action.display_name(),
                        kind: "toolchain.component.extract".to_string(),
                        cache_key: cache_key.clone(),
                        cache: if *was_cached {
                            vx_report::CacheOutcome::Hit {
                                manifest: cache_key,
                            }
                        } else {
                            vx_report::CacheOutcome::Miss {
                                reason: vx_report::MissReason::FirstBuild,
                            }
                        },
                        timing: {
                            let &(exec, start, ln) =
                                node_timings.get(node_idx).unwrap_or(&(0, 0, 0));
                            vx_report::NodeTiming {
                                execute_ms: exec,
                                start_offset_ms: start,
                                lane: ln,
                                ..Default::default()
                            }
                        },
                        inputs: vec![],
                        deps: vec![],
                        outputs: vec![vx_report::OutputRecord {
                            logical: format!("tree_{:?}", component).to_lowercase(),
                            manifest: tree_root.map(|h| h.to_hex()),
                            blob: None,
                            path: None,
                        }],
                        invocation: None,
                        diagnostics: vx_report::DiagnosticsRecord::default(),
                        cas_cache_key: None,
                    });
                }
                ActionResult::RegistryCrateAcquired {
                    name,
                    version,
                    manifest_hash,
                    was_cached,
                } => {
                    if *was_cached {
                        cache_hits += 1;
                    } else {
                        rebuilt += 1;
                    }

                    // Generate node report for registry crate acquisition
                    node_reports.push(vx_report::NodeReport {
                        node_id: format!("acquire-registry:{}:{}", name, version),
                        kind: "registry.acquire".to_string(),
                        cache_key: manifest_hash.to_hex(),
                        cache: if *was_cached {
                            vx_report::CacheOutcome::Hit {
                                manifest: manifest_hash.to_hex(),
                            }
                        } else {
                            vx_report::CacheOutcome::Miss {
                                reason: vx_report::MissReason::FirstBuild,
                            }
                        },
                        timing: {
                            let &(exec, start, ln) =
                                node_timings.get(node_idx).unwrap_or(&(0, 0, 0));
                            vx_report::NodeTiming {
                                execute_ms: exec,
                                start_offset_ms: start,
                                lane: ln,
                                ..Default::default()
                            }
                        },
                        inputs: vec![vx_report::InputRecord {
                            label: "crate".to_string(),
                            value: format!("{}:{}", name, version),
                        }],
                        deps: vec![],
                        outputs: vec![vx_report::OutputRecord {
                            logical: "source".to_string(),
                            manifest: Some(manifest_hash.to_hex()),
                            blob: None,
                            path: None,
                        }],
                        invocation: None,
                        diagnostics: vx_report::DiagnosticsRecord::default(),
                        cas_cache_key: None,
                    });
                }
                ActionResult::RustToolchainFinalized {
                    toolchain_id,
                    manifest_hash,
                    was_cached,
                } => {
                    if *was_cached {
                        cache_hits += 1;
                    } else {
                        rebuilt += 1;
                    }

                    node_reports.push(vx_report::NodeReport {
                        node_id: action.report_node_id(),
                        kind: action.report_kind().to_string(),
                        cache_key: toolchain_id.to_hex(),
                        cache: if *was_cached {
                            vx_report::CacheOutcome::Hit {
                                manifest: manifest_hash.to_hex(),
                            }
                        } else {
                            vx_report::CacheOutcome::Miss {
                                reason: vx_report::MissReason::FirstBuild,
                            }
                        },
                        timing: {
                            let &(exec, start, ln) =
                                node_timings.get(node_idx).unwrap_or(&(0, 0, 0));
                            vx_report::NodeTiming {
                                execute_ms: exec,
                                start_offset_ms: start,
                                lane: ln,
                                ..Default::default()
                            }
                        },
                        inputs: vec![],
                        deps: vec![],
                        outputs: vec![vx_report::OutputRecord {
                            logical: "toolchain".to_string(),
                            manifest: Some(manifest_hash.to_hex()),
                            blob: None,
                            path: None,
                        }],
                        invocation: None,
                        diagnostics: vx_report::DiagnosticsRecord::default(),
                        cas_cache_key: None,
                    });
                }
                ActionResult::RustSourceTreeReady {
                    crate_id,
                    source_manifest,
                    was_cached,
                    cas_cache_key,
                } => {
                    if *was_cached {
                        cache_hits += 1;
                    } else {
                        rebuilt += 1;
                    }

                    node_reports.push(vx_report::NodeReport {
                        node_id: action.report_node_id(),
                        kind: action.report_kind().to_string(),
                        cache_key: crate_id.to_string(),
                        cache: if *was_cached {
                            vx_report::CacheOutcome::Hit {
                                manifest: source_manifest.to_hex(),
                            }
                        } else {
                            vx_report::CacheOutcome::Miss {
                                reason: vx_report::MissReason::KeyNotFound,
                            }
                        },
                        timing: {
                            let &(exec, start, ln) =
                                node_timings.get(node_idx).unwrap_or(&(0, 0, 0));
                            vx_report::NodeTiming {
                                execute_ms: exec,
                                start_offset_ms: start,
                                lane: ln,
                                ..Default::default()
                            }
                        },
                        inputs: vec![],
                        deps: vec![],
                        outputs: vec![vx_report::OutputRecord {
                            logical: "source".to_string(),
                            manifest: Some(source_manifest.to_hex()),
                            blob: None,
                            path: None,
                        }],
                        invocation: None,
                        diagnostics: vx_report::DiagnosticsRecord::default(),
                        cas_cache_key: Some(cas_cache_key.to_hex()),
                    });
                }
                ActionResult::ZigToolchainAcquired {
                    toolchain_id,
                    manifest_hash: _,
                    was_cached,
                } => {
                    if *was_cached {
                        cache_hits += 1;
                    } else {
                        rebuilt += 1;
                    }

                    // Generate node report for Zig toolchain acquisition
                    node_reports.push(vx_report::NodeReport {
                        node_id: action.display_name(),
                        kind: "zig.toolchain.acquire".to_string(),
                        cache_key: toolchain_id.to_hex(),
                        cache: if *was_cached {
                            vx_report::CacheOutcome::Hit {
                                manifest: toolchain_id.to_hex(),
                            }
                        } else {
                            vx_report::CacheOutcome::Miss {
                                reason: vx_report::MissReason::FirstBuild,
                            }
                        },
                        timing: {
                            let &(exec, start, ln) =
                                node_timings.get(node_idx).unwrap_or(&(0, 0, 0));
                            vx_report::NodeTiming {
                                execute_ms: exec,
                                start_offset_ms: start,
                                lane: ln,
                                ..Default::default()
                            }
                        },
                        inputs: vec![],
                        deps: vec![],
                        outputs: vec![vx_report::OutputRecord {
                            logical: "zig_toolchain".to_string(),
                            manifest: Some(toolchain_id.to_hex()),
                            blob: None,
                            path: None,
                        }],
                        invocation: None,
                        diagnostics: vx_report::DiagnosticsRecord::default(),
                        cas_cache_key: None,
                    });
                }
                ActionResult::RunCompleted {
                    kind,
                    output_manifest,
                    bin_output: run_bin_output,
                    was_cached,
                    cas_cache_key,
                } => {
                    if *was_cached {
                        cache_hits += 1;
                    } else {
                        rebuilt += 1;
                    }

                    if let Some(p) = run_bin_output {
                        bin_output = Some(p.clone());
                    }

                    match kind {
                        crate::action_graph::RunKind::RustCompile {
                            crate_id,
                            crate_type,
                            ..
                        } => {
                            node_reports.push(vx_report::NodeReport {
                                node_id: action.report_node_id(),
                                kind: action.report_kind().to_string(),
                                cache_key: crate_id.to_string(),
                                cache: if *was_cached {
                                    vx_report::CacheOutcome::Hit {
                                        manifest: output_manifest.to_hex(),
                                    }
                                } else {
                                    vx_report::CacheOutcome::Miss {
                                        reason: vx_report::MissReason::KeyNotFound,
                                    }
                                },
                                timing: {
                                    let &(exec, start, ln) =
                                        node_timings.get(node_idx).unwrap_or(&(0, 0, 0));
                                    vx_report::NodeTiming {
                                        execute_ms: exec,
                                        start_offset_ms: start,
                                        lane: ln,
                                        ..Default::default()
                                    }
                                },
                                inputs: vec![vx_report::InputRecord {
                                    label: "source_closure".to_string(),
                                    value: crate_id.to_string(),
                                }],
                                deps: vec![],
                                outputs: vec![vx_report::OutputRecord {
                                    logical: if crate_type.as_str() == "bin" {
                                        "bin".to_string()
                                    } else {
                                        "rlib".to_string()
                                    },
                                    manifest: Some(output_manifest.to_hex()),
                                    blob: None,
                                    path: run_bin_output.as_ref().map(|p| p.to_string()),
                                }],
                                invocation: None,
                                diagnostics: vx_report::DiagnosticsRecord::default(),
                                cas_cache_key: Some(cas_cache_key.to_hex()),
                            });
                        }
                        crate::action_graph::RunKind::Generic => {
                            node_reports.push(vx_report::NodeReport {
                                node_id: action.display_name(),
                                kind: action.report_kind().to_string(),
                                cache_key: cas_cache_key.to_hex(),
                                cache: if *was_cached {
                                    vx_report::CacheOutcome::Hit {
                                        manifest: output_manifest.to_hex(),
                                    }
                                } else {
                                    vx_report::CacheOutcome::Miss {
                                        reason: vx_report::MissReason::KeyNotFound,
                                    }
                                },
                                timing: {
                                    let &(exec, start, ln) =
                                        node_timings.get(node_idx).unwrap_or(&(0, 0, 0));
                                    vx_report::NodeTiming {
                                        execute_ms: exec,
                                        start_offset_ms: start,
                                        lane: ln,
                                        ..Default::default()
                                    }
                                },
                                inputs: vec![],
                                deps: vec![],
                                outputs: vec![],
                                invocation: None,
                                diagnostics: vx_report::DiagnosticsRecord::default(),
                                cas_cache_key: Some(cas_cache_key.to_hex()),
                            });
                        }
                    }
                }
                ActionResult::PlannerSyntheticExecuted { node_id, kind } => {
                    rebuilt += 1;
                    node_reports.push(vx_report::NodeReport {
                        node_id: node_id.clone(),
                        kind: kind.clone(),
                        cache_key: node_id.clone(),
                        cache: vx_report::CacheOutcome::Miss {
                            reason: vx_report::MissReason::FirstBuild,
                        },
                        timing: {
                            let &(exec, start, ln) =
                                node_timings.get(node_idx).unwrap_or(&(0, 0, 0));
                            vx_report::NodeTiming {
                                execute_ms: exec,
                                start_offset_ms: start,
                                lane: ln,
                                ..Default::default()
                            }
                        },
                        inputs: vec![],
                        deps: vec![],
                        outputs: vec![],
                        invocation: None,
                        diagnostics: vx_report::DiagnosticsRecord::default(),
                        cas_cache_key: None,
                    });
                }
            }
        }

        self.stats.cache_hits = cache_hits;
        self.stats.rebuilt = rebuilt;
        self.stats.bin_output = bin_output;
        self.stats.node_reports = node_reports;
        self.stats.graph_topology = Some(self.graph.to_topology());

        Ok(())
    }

    /// Spawn an action for execution
    fn spawn_action(&mut self, node_idx: NodeIndex, _permit: tokio::sync::OwnedSemaphorePermit) {
        // Get action before spawning (we own the graph, no lock needed)
        let action = self.graph.graph[node_idx].action.clone();
        let mut deps: Vec<NodeIndex> = self
            .graph
            .graph
            .neighbors_directed(node_idx, petgraph::Direction::Outgoing)
            .collect();
        deps.sort();

        let message_tx = self.message_tx.clone();
        let cas = self.cas.clone();
        let exec = self.exec.clone();
        let db = self.db.clone();
        let progress_listener = self.progress_listener.clone();
        let workspace_root = self.workspace_root.clone();
        let target_triple = self.target_triple.clone();
        let profile = self.profile.clone();
        let session_id = self.session_id.clone();
        let results = self.results.clone(); // Cloned Arc for execute_action
        let rust_toolchain_spec = self.graph.rust_toolchain_spec.clone();
        let macos_commandlinetools_version = self.macos_commandlinetools_version.clone();
        let cancel_token = self.cancel_token.clone();
        let free_lanes = self.free_lanes.clone();
        let build_start = self.build_start;

        self.in_flight.spawn(async move {
            // Drop guard: if this task is aborted (e.g. by JoinSet::abort_all), send
            // a Cancelled result back so the executor doesn't lose track of this action.
            let guard_tx = message_tx.clone();
            let mut _completion_guard = CompletionGuard {
                node_idx,
                tx: Some(guard_tx),
            };

            debug!("started for {:?}", node_idx);

            // Start progress tracking
            let action_type = action.to_progress_action_type();
            let action_id = if let Some(ref listener) = progress_listener {
                listener.start_action(action_type).await.ok()
            } else {
                None
            };

            // Create a progress channel for the store RPC.
            // The store sends StoreProgress on tx; we receive on rx and forward
            // to the CLI's ProgressListener as ActionProgress.
            let (store_progress_tx, mut store_progress_rx) = roam::channel::<StoreProgress>();

            // Spawn a task to forward StoreProgress → ActionProgress
            let forward_listener = progress_listener.clone();
            let forward_action_id = action_id;
            let forward_handle = moire::task::spawn(
                async move {
                    while let Ok(Some(sp)) = store_progress_rx
                        .recv()
                        .named("executor.store_progress.recv")
                        .await
                    {
                        if let (Some(listener), Some(id)) = (&forward_listener, forward_action_id) {
                            let _ = listener
                                .update_progress(
                                    id,
                                    ActionProgress {
                                        message: sp.message.clone(),
                                        current: sp.current,
                                        total: sp.total,
                                    },
                                )
                                .await;
                        }
                    }
                }
                .named("store_progress_forward"),
            );

            // Acquire a lane ID for timeline tracking
            let lane = free_lanes.lock().pop().unwrap_or(0);

            debug!("calling execute_action for {:?} on lane {}", node_idx, lane);

            // Execute action with cancellation support, measuring wall-clock time
            let execute_start = std::time::Instant::now();
            let start_offset_ms = execute_start.duration_since(build_start).as_millis() as u64;
            let result = tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    debug!("action {:?} cancelled via token", node_idx);
                    Err(VxdError::Cancelled)
                }
                result = execute_action(
                    action,
                    deps,
                    &cas,
                    &exec,
                    &db,
                    &results,
                    &workspace_root,
                    &target_triple,
                    &profile,
                    &session_id,
                    rust_toolchain_spec,
                    store_progress_tx,
                    macos_commandlinetools_version,
                ) => result,
            };

            debug!(
                "execute_action returned for {:?}: {:?}",
                node_idx,
                result.is_ok()
            );

            // Wait for progress forwarding to drain
            let _ = forward_handle.await;

            // Complete progress tracking
            if let (Some(listener), Some(id)) = (&progress_listener, action_id) {
                let _ = listener.complete_action(id).await;
            }

            let execute_ms = execute_start.elapsed().as_millis() as u64;

            // Release the lane back to the pool
            free_lanes.lock().push(lane);

            debug!("sending completion message for {:?}", node_idx);
            // Send result back to executor — disarm the drop guard since we're sending explicitly
            _completion_guard.disarm();
            let _ = message_tx.send(ExecutorMessage::ActionCompleted {
                node_idx,
                result,
                execute_ms,
                start_offset_ms,
                lane,
            });

            debug!("done for {:?}", node_idx);
            // Permit dropped here, allowing next action to run
        });
    }
}

/// Execute a single action
#[allow(clippy::too_many_arguments)]
async fn execute_action(
    action: Action,
    deps: Vec<NodeIndex>,
    cas: &Arc<StoreClient>,
    exec: &Arc<RunnerClient>,
    db: &Arc<crate::db::Database>,
    results: &Arc<SyncMutex<HashMap<NodeIndex, ActionResult>>>,
    workspace_root: &camino::Utf8PathBuf,
    _target_triple: &str,
    _profile: &str,
    session_id: &str,
    rust_toolchain_spec: Option<RustToolchainSpec>,
    progress: roam::Tx<StoreProgress>,
    macos_commandlinetools_version: Option<String>,
) -> Result<ActionResult, VxdError> {
    use camino::Utf8PathBuf;
    use vx_runner_proto::{RunFileMount, RunRequest};

    debug!("processing action {}", action.display_name());

    match action {
        Action::DownloadRustToolchainComponent {
            channel,
            target_triple,
            component,
        } => {
            use vx_store_proto::{EnsureStatus, RustToolchainSpec};
            let started = Instant::now();

            let spec = RustToolchainSpec {
                channel,
                host: target_triple.clone(),
                target: target_triple.clone(),
                components: vec![RustComponent::Rustc, RustComponent::RustStd],
            };

            let result = cas
                .download_rust_toolchain_component(spec, component.clone(), progress)
                .await
                .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?;

            if result.status == EnsureStatus::Failed {
                return Err(VxdError::ToolchainAcquisition(
                    result.error.unwrap_or_else(|| "unknown error".to_string()),
                ));
            }

            info!(
                component = ?component,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "toolchain component downloaded"
            );

            Ok(ActionResult::RustToolchainComponentDownloaded {
                component,
                tarball: result.tarball,
                was_cached: result.status == EnsureStatus::Hit,
            })
        }
        Action::ExtractRustToolchainComponent {
            channel,
            target_triple,
            component,
        } => {
            use vx_store_proto::{EnsureStatus, RustToolchainSpec};
            let spec = RustToolchainSpec {
                channel,
                host: target_triple.clone(),
                target: target_triple.clone(),
                components: vec![RustComponent::Rustc, RustComponent::RustStd],
            };

            // Locate corresponding download result for this component.
            let tarball = {
                let results_lock = results.lock();
                results_lock.values().find_map(|r| match r {
                    ActionResult::RustToolchainComponentDownloaded {
                        component: downloaded_component,
                        tarball: Some(blob),
                        ..
                    } if *downloaded_component == component => Some(blob.clone()),
                    _ => None,
                })
            };

            let tarball = match tarball {
                Some(blob) => blob,
                None => {
                    // If full toolchain is already cached, extraction call can still return Hit
                    // with a placeholder blob value that won't be used.
                    BlobRef::Small {
                        chunk: ChunkHash(Blake3Hash([0; 32])),
                        size: 0,
                    }
                }
            };

            let result = cas
                .extract_rust_toolchain_component(spec, component.clone(), tarball, progress)
                .await
                .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?;

            if result.status == EnsureStatus::Failed {
                return Err(VxdError::ToolchainAcquisition(
                    result.error.unwrap_or_else(|| "unknown error".to_string()),
                ));
            }

            Ok(ActionResult::RustToolchainComponentExtracted {
                component,
                tree_root: result.tree_root,
                was_cached: result.status == EnsureStatus::Hit,
            })
        }

        Action::AcquireRegistryCrate {
            name,
            version,
            checksum,
        } => {
            use vx_store_proto::{EnsureStatus, RegistrySpec};

            let spec = RegistrySpec {
                registry_url: "https://crates.io".to_string(),
                name: name.clone(),
                version: version.clone(),
                checksum,
            };

            debug!(name = %name, version = %version, "ensuring registry crate");

            let result = cas
                .ensure_registry_crate(spec, progress)
                .await
                .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?;

            match result.status {
                EnsureStatus::Hit | EnsureStatus::Downloaded => {
                    let manifest_hash =
                        result
                            .manifest_hash
                            .ok_or_else(|| VxdError::RegistryCrateNoManifest {
                                name: name.clone(),
                                version: version.clone(),
                            })?;

                    let was_cached = result.status == EnsureStatus::Hit;

                    debug!(
                        name = %name,
                        version = %version,
                        manifest_hash = %manifest_hash,
                        was_cached = was_cached,
                        "acquired registry crate"
                    );

                    Ok(ActionResult::RegistryCrateAcquired {
                        name,
                        version,
                        manifest_hash: manifest_hash.0,
                        was_cached,
                    })
                }
                EnsureStatus::Failed => Err(VxdError::RegistryCrateAcquisition {
                    name,
                    version,
                    reason: result
                        .error
                        .unwrap_or_else(|| "acquisition failed".to_string()),
                }),
            }
        }

        Action::FinalizeRustToolchain { spec } => {
            use vx_store_proto::EnsureStatus;

            let finalize_result = cas
                .finalize_rust_toolchain(spec)
                .await
                .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?;
            if finalize_result.status == EnsureStatus::Failed {
                return Err(VxdError::ToolchainAcquisition(
                    finalize_result
                        .error
                        .unwrap_or_else(|| "toolchain finalization failed".to_string()),
                ));
            }

            let toolchain_manifest = finalize_result.manifest_hash.ok_or_else(|| {
                VxdError::ToolchainAcquisition("missing toolchain manifest".into())
            })?;

            let manifest = cas
                .get_toolchain_manifest(toolchain_manifest)
                .await
                .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?
                .ok_or(VxdError::ToolchainManifestNotFound(toolchain_manifest))?;
            let toolchain_id = finalize_result
                .toolchain_id
                .unwrap_or(manifest.toolchain_id);

            // Update the RustToolchainManifest and RustToolchain in picante with real values.
            use crate::inputs::{RustToolchain as RustToolchainInput, RustToolchainManifest};
            RustToolchainManifest::set(&**db, toolchain_manifest.0)
                .map_err(|e| VxdError::Picante(e.to_string()))?;

            if let Some(toolchain) = rust_toolchain_spec {
                let host = toolchain.host;
                let target = toolchain.target;
                let _ = RustToolchainInput::new(
                    &**db,
                    toolchain_id,
                    toolchain_manifest.0,
                    host,
                    target,
                )
                .map_err(|e| VxdError::Picante(e.to_string()))?;
            }

            Ok(ActionResult::RustToolchainFinalized {
                toolchain_id,
                manifest_hash: toolchain_manifest.0,
                was_cached: finalize_result.status == EnsureStatus::Hit,
            })
        }

        Action::IngestRustSourceTree {
            crate_id,
            crate_name,
            edition,
            crate_type,
            workspace_root: workspace_root_str,
            crate_root_rel,
        } => {
            let workspace_root_path = Utf8PathBuf::from(&workspace_root_str);
            let crate_root_abs = workspace_root_path.join(&crate_root_rel);

            let paths =
                vx_rs::rust_source_closure(&crate_root_abs, &workspace_root_path).map_err(|e| {
                    VxdError::SourceClosure {
                        crate_name: crate_name.clone(),
                        message: e.to_string(),
                    }
                })?;

            let closure_hash = vx_rs::hash_source_closure(&paths, &workspace_root_path)
                .map_err(|e| VxdError::SourceHash(e.to_string()))?;

            // Update RustCrate input with actual closure hash.
            let _ = crate::inputs::RustCrate::new(
                &**db,
                crate_id.to_string(),
                crate_name.clone(),
                edition.clone(),
                crate_type.as_str().to_string(),
                crate_root_rel.clone(),
                closure_hash,
            )
            .map_err(|e| VxdError::Picante(e.to_string()))?;

            let cache_key = {
                let mut h = blake3::Hasher::new();
                h.update(b"vx-ingest-rust-src-v1\n");
                h.update(&closure_hash.0);
                let base = Blake3Hash(*h.finalize().as_bytes());
                namespaced_action_cache_key(base, session_id, "ingest-rust-source")
            };

            let (source_manifest, was_cached) = if let Some(cached) = cas
                .lookup(cache_key)
                .await
                .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?
            {
                (cached.0, true)
            } else {
                let mut tree_files = Vec::with_capacity(paths.len());
                for rel_path in &paths {
                    let abs_path = workspace_root_path.join(rel_path);
                    let contents =
                        ::moire::fs::read(&abs_path)
                            .await
                            .map_err(|e| VxdError::FileRead {
                                path: abs_path.clone(),
                                message: e.to_string(),
                            })?;
                    let size = contents.len() as u64;
                    let content_hash = ContentHash(Blake3Hash::from_bytes(&contents));
                    let chunk_hash = ChunkHash(content_hash.0);
                    cas.put_chunk(chunk_hash, contents)
                        .await
                        .map(|_| ())
                        .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?;

                    tree_files.push(crate::cas_tree::TreeFile {
                        path: rel_path.to_string(),
                        kind: DirEntryKind::SmallFile {
                            content_hash,
                            size,
                            executable: false,
                        },
                    });
                }

                let (source_node_hash, _total_size) =
                    crate::cas_tree::build_merkle_tree(cas.as_ref(), tree_files)
                        .await
                        .map_err(VxdError::CasRpc)?;
                let source_manifest = source_node_hash.0;

                cas.publish(cache_key, source_node_hash)
                    .await
                    .map(|_| ())
                    .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?;

                (source_manifest, false)
            };

            Ok(ActionResult::RustSourceTreeReady {
                crate_id,
                source_manifest,
                was_cached,
                cas_cache_key: cache_key,
            })
        }

        Action::ExtractRegistryCrateTree {
            crate_id,
            name,
            version,
            crate_name,
            edition,
            crate_type,
            crate_root_rel,
        } => {
            use vx_tarball::{Compression, extract_to_tree};

            let registry_manifest = {
                let results_lock = results.lock();
                deps.iter()
                    .find_map(|idx| match results_lock.get(idx) {
                        Some(ActionResult::RegistryCrateAcquired {
                            name: n,
                            version: v,
                            manifest_hash,
                            ..
                        }) if *n == name && *v == version => Some(*manifest_hash),
                        _ => None,
                    })
                    .ok_or_else(|| VxdError::RegistryCrateNoManifest {
                        name: name.clone(),
                        version: version.clone(),
                    })?
            };

            let cache_key = {
                let mut h = blake3::Hasher::new();
                h.update(b"vx-extract-registry-v1\n");
                h.update(&registry_manifest.0);
                let base = Blake3Hash(*h.finalize().as_bytes());
                namespaced_action_cache_key(base, session_id, "extract-registry-tree")
            };

            let (source_manifest, was_cached) = if let Some(cached) = cas
                .lookup(cache_key)
                .await
                .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?
            {
                (cached.0, true)
            } else {
                let manifest = cas
                    .get_registry_manifest(NodeHash(registry_manifest))
                    .await
                    .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?
                    .ok_or_else(|| VxdError::RegistryCrateNoManifest {
                        name: name.clone(),
                        version: version.clone(),
                    })?;

                let tarball = fetch_blob_from_ref(cas, &manifest.crate_tarball).await?;

                let cas_for_blobs = cas.clone();
                let tree = extract_to_tree(
                    tokio::io::BufReader::new(std::io::Cursor::new(tarball)),
                    Compression::Gzip,
                    1,
                    async move |data| {
                        let hash = Blake3Hash::from_bytes(&data);
                        cas_for_blobs
                            .put_chunk(ChunkHash(hash), data)
                            .await
                            .map(|_| ())
                            .map_err(|e| format!("{e:?}"))?;
                        Ok(hash)
                    },
                )
                .await
                .map_err(|e| VxdError::TreeIngestion(e.to_string()))?;

                let mut tree_files = Vec::new();
                for entry in tree.entries {
                    match entry.kind {
                        vx_store_proto::TreeEntryKind::File {
                            blob,
                            size,
                            executable,
                        } => tree_files.push(crate::cas_tree::TreeFile {
                            path: entry.path,
                            kind: DirEntryKind::SmallFile {
                                content_hash: ContentHash(blob),
                                size,
                                executable,
                            },
                        }),
                        vx_store_proto::TreeEntryKind::Symlink { target } => {
                            tree_files.push(crate::cas_tree::TreeFile {
                                path: entry.path,
                                kind: DirEntryKind::Symlink { target },
                            })
                        }
                        vx_store_proto::TreeEntryKind::Directory => {}
                    }
                }

                let (source_node_hash, _total_size) =
                    crate::cas_tree::build_merkle_tree(cas.as_ref(), tree_files)
                        .await
                        .map_err(VxdError::CasRpc)?;
                let source_manifest = source_node_hash.0;

                cas.publish(cache_key, source_node_hash)
                    .await
                    .map(|_| ())
                    .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?;

                (source_manifest, false)
            };

            // For registry crates, treat the extracted tree manifest hash as the source closure hash.
            let _ = crate::inputs::RustCrate::new(
                &**db,
                crate_id.to_string(),
                crate_name.clone(),
                edition.clone(),
                crate_type.as_str().to_string(),
                crate_root_rel.clone(),
                source_manifest,
            )
            .map_err(|e| VxdError::Picante(e.to_string()))?;

            Ok(ActionResult::RustSourceTreeReady {
                crate_id,
                source_manifest,
                was_cached,
                cas_cache_key: cache_key,
            })
        }

        Action::AcquireZigToolchain { version } => {
            use vx_store_proto::{EnsureStatus, ZigToolchainSpec};
            use vx_toolchain::zig::HostPlatform;
            let started = Instant::now();

            let host = HostPlatform::detect()
                .map_err(|e| VxdError::ToolchainAcquisition(e.to_string()))?;

            let spec = ZigToolchainSpec {
                version: version.clone(),
                host_platform: host.to_string(),
            };

            let result = cas
                .ensure_zig_toolchain(spec, progress)
                .await
                .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?;

            if result.status == EnsureStatus::Failed {
                return Err(VxdError::ToolchainAcquisition(
                    result.error.unwrap_or_else(|| "unknown error".to_string()),
                ));
            }

            let manifest_hash = result.manifest_hash.ok_or(VxdError::Compilation {
                crate_name: format!("zig-{}", version),
                message: "No manifest hash returned from CAS".to_string(),
            })?;

            // Get toolchain_id from result if available (download case),
            // otherwise fetch from manifest (cache hit case)
            let toolchain_id = if let Some(id) = result.toolchain_id {
                id
            } else {
                // Cache hit - fetch toolchain_id from manifest
                let manifest = cas
                    .get_toolchain_manifest(manifest_hash)
                    .await
                    .map_err(|e| VxdError::Compilation {
                        crate_name: format!("zig-{}", version),
                        message: format!("Failed to fetch toolchain manifest: {e:?}"),
                    })?
                    .ok_or(VxdError::ToolchainManifestNotFound(manifest_hash))?;
                manifest.toolchain_id
            };

            info!(
                version = %version,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "Zig toolchain acquired"
            );

            Ok(ActionResult::ZigToolchainAcquired {
                toolchain_id,
                manifest_hash: manifest_hash.0,
                was_cached: result.status == EnsureStatus::Hit,
            })
        }

        Action::Run {
            label,
            kind,
            toolchain,
            program_rel,
            args,
            env,
            cwd,
            clear_env,
            tree_mounts,
            dep_tree_mounts,
            file_mounts,
            dep_file_mounts,
            collect_file_mounts,
            outputs,
            writable_roots,
            materialize,
        } => {
            let toolchain_manifest = match toolchain {
                crate::action_graph::RunToolchain::Zig => {
                    let results_lock = results.lock();
                    deps.iter()
                        .find_map(|idx| match results_lock.get(idx) {
                            Some(ActionResult::ZigToolchainAcquired { manifest_hash, .. }) => {
                                Some(*manifest_hash)
                            }
                            _ => None,
                        })
                        .ok_or_else(|| VxdError::Compilation {
                            crate_name: label.clone(),
                            message: "Zig toolchain not acquired before run".to_string(),
                        })?
                }
                crate::action_graph::RunToolchain::Rust => {
                    let results_lock = results.lock();
                    deps.iter()
                        .find_map(|idx| match results_lock.get(idx) {
                            Some(ActionResult::RustToolchainFinalized {
                                manifest_hash, ..
                            }) => Some(*manifest_hash),
                            _ => None,
                        })
                        .ok_or_else(|| VxdError::Compilation {
                            crate_name: label.clone(),
                            message: "Rust toolchain not finalized before run".to_string(),
                        })?
                }
            };

            let mut resolved_tree_mounts = tree_mounts.clone();
            if !dep_tree_mounts.is_empty() {
                let results_lock = results.lock();
                for spec in &dep_tree_mounts {
                    let source_manifest = deps
                        .iter()
                        .find_map(|idx| match results_lock.get(idx) {
                            Some(ActionResult::RustSourceTreeReady {
                                crate_id,
                                source_manifest,
                                ..
                            }) if *crate_id == spec.crate_id => Some(*source_manifest),
                            _ => None,
                        })
                        .ok_or_else(|| VxdError::Compilation {
                            crate_name: label.clone(),
                            message: format!(
                                "Rust source tree not prepared before run (crate_id={})",
                                spec.crate_id
                            ),
                        })?;
                    resolved_tree_mounts.push(vx_runner_proto::RunTreeMount {
                        mount: spec.mount.clone(),
                        manifest: source_manifest,
                    });
                }
            }

            let mut resolved_file_mounts = file_mounts.clone();
            if !dep_file_mounts.is_empty() {
                let results_lock = results.lock();
                for spec in &dep_file_mounts {
                    let manifest_hash = deps
                        .iter()
                        .find_map(|idx| match results_lock.get(idx) {
                            Some(ActionResult::RunCompleted {
                                kind: crate::action_graph::RunKind::RustCompile { crate_id, .. },
                                output_manifest,
                                ..
                            }) if *crate_id == spec.crate_id => Some(*output_manifest),
                            _ => None,
                        })
                        .ok_or_else(|| VxdError::Compilation {
                            crate_name: label.clone(),
                            message: format!(
                                "Dependency crate not compiled before run (crate_id={})",
                                spec.crate_id
                            ),
                        })?;
                    resolved_file_mounts.push(RunFileMount {
                        manifest: manifest_hash,
                        logical: spec.logical.clone(),
                        path: spec.path.clone(),
                    });
                }
            }
            if !collect_file_mounts.is_empty() {
                // Collect all dep manifests under the lock, then release it
                let dep_manifests: Vec<Blake3Hash> = {
                    let results_lock = results.lock();
                    deps.iter()
                        .filter_map(|idx| match results_lock.get(idx) {
                            Some(ActionResult::RunCompleted {
                                output_manifest, ..
                            }) => Some(*output_manifest),
                            _ => None,
                        })
                        .collect()
                };

                for spec in &collect_file_mounts {
                    let mut i = 0usize;
                    for mh in &dep_manifests {
                        let manifest = load_manifest(cas, NodeHash(*mh), &label).await?;

                        if manifest.outputs.iter().any(|o| o.logical == spec.logical) {
                            let path = format!(
                                "{}{:0width$}{}",
                                spec.path_prefix,
                                i,
                                spec.path_suffix,
                                width = spec.pad_width
                            );
                            resolved_file_mounts.push(RunFileMount {
                                manifest: *mh,
                                logical: spec.logical.clone(),
                                path,
                            });
                            i += 1;
                        }
                    }
                }
            }

            let request = RunRequest {
                toolchain_manifest,
                program_rel,
                args,
                cwd,
                clear_env,
                env,
                tree_mounts: resolved_tree_mounts,
                file_mounts: resolved_file_mounts,
                outputs,
                writable_roots,
                macos_commandlinetools_version: macos_commandlinetools_version.clone(),
            };

            let cache_key = namespaced_run_cache_key(vx_cache::run_cache_key(&request), session_id);
            let (output_manifest, was_cached) = if let Some(cached) = cas
                .lookup(cache_key)
                .await
                .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?
            {
                (cached.0, true)
            } else {
                let result = exec
                    .run(request, progress)
                    .await
                    .map_err(|e| VxdError::ExecRpc(format!("{e:?}")))?;
                if !result.success {
                    return Err(VxdError::Compilation {
                        crate_name: label.clone(),
                        message: result.error.unwrap_or(result.stderr),
                    });
                }
                let output_manifest =
                    result
                        .output_manifest
                        .ok_or_else(|| VxdError::NoOutputManifest {
                            crate_name: label.clone(),
                        })?;

                cas.publish(cache_key, NodeHash(output_manifest))
                    .await
                    .map(|_| ())
                    .map_err(|e| VxdError::CasRpc(format!("{e:?}")))?;

                (output_manifest, false)
            };

            let mut bin_output = None;
            if !materialize.is_empty() {
                let manifest = load_manifest(cas, NodeHash(output_manifest), &label).await?;

                let vx_root = workspace_root.join(".vx");
                let mut ensured_vx_gitignore = false;

                for m in &materialize {
                    let entry = manifest
                        .outputs
                        .iter()
                        .find(|o| o.logical == m.logical)
                        .ok_or_else(|| VxdError::Compilation {
                            crate_name: label.clone(),
                            message: format!("Output '{}' not found in manifest", m.logical),
                        })?;

                    let out_path = workspace_root.join(&m.workspace_rel_path);
                    let parent = out_path.parent().ok_or_else(|| VxdError::Compilation {
                        crate_name: label.clone(),
                        message: format!("Invalid output path: {out_path}"),
                    })?;
                    ::moire::fs::create_dir_all(parent).await.map_err(|e| {
                        VxdError::Compilation {
                            crate_name: label.clone(),
                            message: format!("Failed to create output directory: {e}"),
                        }
                    })?;

                    if !ensured_vx_gitignore && out_path.starts_with(&vx_root) {
                        match vx_io::git::ensure_vx_gitignored(workspace_root).await {
                            Ok(true) => debug!("Added /.vx to .gitignore"),
                            Ok(false) => {}
                            Err(e) => warn!("Failed to update .gitignore: {}", e),
                        }
                        ensured_vx_gitignore = true;
                    }

                    let data = fetch_output_blob_data(cas, &entry.blob)
                        .await
                        .map_err(|e| VxdError::Compilation {
                            crate_name: label.clone(),
                            message: format!("Failed to fetch output blob from CAS: {e}"),
                        })?;

                    ::moire::fs::write(&out_path, &data).await.map_err(|e| {
                        VxdError::Compilation {
                            crate_name: label.clone(),
                            message: format!("Failed to write output to {out_path}: {e}"),
                        }
                    })?;

                    if entry.executable {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let mut perms = ::moire::fs::metadata(&out_path)
                                .await
                                .map_err(|e| VxdError::Compilation {
                                    crate_name: label.clone(),
                                    message: format!("Failed to get output metadata: {e}"),
                                })?
                                .permissions();
                            perms.set_mode(0o755);
                            ::moire::fs::set_permissions(&out_path, perms)
                                .await
                                .map_err(|e| VxdError::Compilation {
                                    crate_name: label.clone(),
                                    message: format!("Failed to set output permissions: {e}"),
                                })?;
                        }

                        // Expose the last executable output as the "binary output" for callers.
                        bin_output = Some(out_path.clone());
                    }
                }
            }

            Ok(ActionResult::RunCompleted {
                kind,
                output_manifest,
                bin_output,
                was_cached,
                cas_cache_key: cache_key,
            })
        }

        Action::PlannerCompileRustCrate {
            crate_name,
            crate_type,
            report_node_id,
            ..
        } => Ok(ActionResult::PlannerSyntheticExecuted {
            node_id: report_node_id
                .unwrap_or_else(|| format!("compile-{}:{crate_name}", crate_type.as_str())),
            kind: if crate_type.as_str() == "bin" {
                "rust.compile_bin".to_string()
            } else {
                "rust.compile_rlib".to_string()
            },
        }),

        Action::PlannerSynthetic {
            node_id,
            report_node_id,
            kind,
            ..
        } => Ok(ActionResult::PlannerSyntheticExecuted {
            node_id: report_node_id.unwrap_or(node_id),
            kind: kind.report_kind().to_string(),
        }),
    }
}
