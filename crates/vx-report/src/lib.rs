//! Build report types and persistence for vx
//!
//! This crate provides:
//! - Versioned report schema (BuildReport, NodeReport, etc.)
//! - ULID-based run ID generation
//! - Persistence to `.vx/runs/` directory
//!
//! Reports capture everything needed to answer "why did it rebuild?":
//! - What nodes ran vs hit cache
//! - What the cache key was
//! - What inputs contributed to the key
//! - Timing and diagnostic output (as CAS blob references)

use camino::{Utf8Path, Utf8PathBuf};
use facet::Facet;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Current report schema version.
/// Bump when schema changes in backwards-incompatible ways.
pub const REPORT_SCHEMA_VERSION: u32 = 2;

// =============================================================================
// RUN ID
// =============================================================================

/// A time-ordered unique run identifier.
///
/// Uses ULID format: 26 characters, lexicographically sortable by time.
/// Example: "01ARZ3NDEKTSV4RRFFQ69G5FAV"
#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct RunId(pub String);

impl RunId {
    /// Generate a new RunId based on current time.
    ///
    /// ULIDs are 128-bit identifiers that:
    /// - Are lexicographically sortable
    /// - Encode timestamp in the first 48 bits
    /// - Have 80 bits of randomness to avoid collisions
    pub fn new() -> Self {
        Self(ulid::Ulid::new().to_string())
    }
}

impl std::str::FromStr for RunId {
    type Err = &'static str;

    /// Parse a RunId from a string.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Basic validation: 26 characters, all valid Crockford base32
        if s.len() != 26 {
            return Err("RunId must be 26 characters");
        }
        for c in s.chars() {
            if !matches!(c, '0'..='9' | 'A'..='H' | 'J' | 'K' | 'M' | 'N' | 'P'..='T' | 'V'..='Z') {
                return Err("RunId contains invalid characters");
            }
        }
        Ok(Self(s.to_string()))
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// =============================================================================
// GRAPH TOPOLOGY
// =============================================================================

/// Canonical typed intermediate representation between rule evaluation and execution.
///
/// This schema is designed for deterministic explain/diff/caching workflows.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RulePlan {
    /// Interned source table shared by all provenance references.
    #[facet(default)]
    pub sources: Vec<RulePlanSource>,
    /// Typed graph nodes.
    pub nodes: Vec<RulePlanNode>,
    /// Typed graph edges.
    pub edges: Vec<RulePlanEdge>,
    /// Emitted facts with values.
    pub facts: Vec<RulePlanFact>,
}

impl RulePlan {
    /// Serialize this plan to canonical deterministic JSON.
    pub fn to_canonical_json(&self) -> Result<String, String> {
        let canonical = self.canonicalized();
        facet_json::to_string(&canonical).map_err(|e| e.to_string())
    }

    /// Deserialize a plan from JSON.
    pub fn from_json(json: &str) -> Result<Self, String> {
        facet_json::from_str(json).map_err(|e| e.to_string())
    }

    /// Validate structural invariants and type constraints.
    pub fn validate(&self) -> Result<(), RulePlanValidationError> {
        let mut diagnostics = Vec::new();
        let mut node_ids = HashSet::new();
        let mut node_kind_by_id = HashMap::new();
        let mut source_ids = HashSet::new();

        for source in &self.sources {
            if source.id.0.trim().is_empty() {
                diagnostics.push("source has empty id".to_string());
            }
            if !source_ids.insert(source.id.clone()) {
                diagnostics.push(format!("duplicate source id: {}", source.id.0));
            }
            if source.path.as_str().trim().is_empty() {
                diagnostics.push(format!("source {} has empty path", source.id.0));
            }
        }

        for node in &self.nodes {
            if node.id.0.trim().is_empty() {
                diagnostics.push("node has empty id".to_string());
            }
            if !node_ids.insert(node.id.clone()) {
                diagnostics.push(format!("duplicate node id: {}", node.id.0));
            }
            if let Some(issue) =
                validate_provenance(&node.provenance, "node", &node.id.0, &source_ids)
            {
                diagnostics.push(issue);
            }
            node_kind_by_id.insert(node.id.clone(), node.kind);
        }

        let mut fact_ids = HashSet::new();
        for fact in &self.facts {
            if fact.id.0.trim().is_empty() {
                diagnostics.push("fact has empty id".to_string());
            }
            if !fact_ids.insert(fact.id.clone()) {
                diagnostics.push(format!("duplicate fact id: {}", fact.id.0));
            }
            if fact.node_id.0.trim().is_empty() {
                diagnostics.push(format!("fact {} has empty node_id", fact.id.0));
            }
            match node_kind_by_id.get(&fact.node_id) {
                Some(RulePlanNodeKind::Fact) => {}
                Some(kind) => diagnostics.push(format!(
                    "fact {} references node {} with incompatible kind {:?}",
                    fact.id.0, fact.node_id.0, kind
                )),
                None => diagnostics.push(format!(
                    "fact {} references unknown node_id {}",
                    fact.id.0, fact.node_id.0
                )),
            }
            if let Some(issue) =
                validate_provenance(&fact.provenance, "fact", &fact.id.0, &source_ids)
            {
                diagnostics.push(issue);
            }
        }

        for edge in &self.edges {
            if edge.from.0.trim().is_empty() || edge.to.0.trim().is_empty() {
                diagnostics.push(format!(
                    "edge {} -> {} has empty endpoint id",
                    edge.from.0, edge.to.0
                ));
                continue;
            }

            let from_kind = node_kind_by_id.get(&edge.from);
            let to_kind = node_kind_by_id.get(&edge.to);

            if from_kind.is_none() {
                diagnostics.push(format!(
                    "edge {} -> {} references unknown from node {}",
                    edge.from.0, edge.to.0, edge.from.0
                ));
            }
            if to_kind.is_none() {
                diagnostics.push(format!(
                    "edge {} -> {} references unknown to node {}",
                    edge.from.0, edge.to.0, edge.to.0
                ));
            }

            if let (Some(from_kind), Some(to_kind)) = (from_kind, to_kind)
                && !edge_kind_allowed(edge.kind, *from_kind, *to_kind)
            {
                diagnostics.push(format!(
                    "edge {} -> {} with kind {:?} is invalid between {:?} and {:?}",
                    edge.from.0, edge.to.0, edge.kind, from_kind, to_kind
                ));
            }

            if let Some(issue) =
                validate_provenance(&edge.provenance, "edge", &edge.from.0, &source_ids)
            {
                diagnostics.push(issue);
            }
        }

        if diagnostics.is_empty() {
            Ok(())
        } else {
            Err(RulePlanValidationError { diagnostics })
        }
    }

    fn canonicalized(&self) -> Self {
        let mut canonical = self.clone();
        canonical.sources.sort_by(|a, b| {
            a.id.0
                .cmp(&b.id.0)
                .then_with(|| a.path.as_str().cmp(b.path.as_str()))
        });
        canonical.nodes.sort_by(|a, b| {
            a.id.0
                .cmp(&b.id.0)
                .then_with(|| a.kind.canonical_name().cmp(b.kind.canonical_name()))
                .then_with(|| a.label.cmp(&b.label))
        });
        canonical.edges.sort_by(|a, b| {
            a.from
                .0
                .cmp(&b.from.0)
                .then_with(|| a.to.0.cmp(&b.to.0))
                .then_with(|| a.kind.canonical_name().cmp(b.kind.canonical_name()))
        });
        canonical.facts.sort_by(|a, b| {
            a.id.0
                .cmp(&b.id.0)
                .then_with(|| a.node_id.0.cmp(&b.node_id.0))
        });
        canonical
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulePlanValidationError {
    pub diagnostics: Vec<String>,
}

impl std::fmt::Display for RulePlanValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RulePlan validation failed: {}",
            self.diagnostics.join("; ")
        )
    }
}

impl std::error::Error for RulePlanValidationError {}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct RulePlanNodeId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct RulePlanFactId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct RulePlanSourceId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Facet)]
pub struct RuleId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RulePlanValue(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RulePlanSource {
    pub id: RulePlanSourceId,
    pub path: Utf8PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RulePlanNode {
    pub id: RulePlanNodeId,
    pub kind: RulePlanNodeKind,
    pub label: String,
    pub provenance: RulePlanProvenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
#[facet(rename_all = "lowercase")]
pub enum RulePlanNodeKind {
    Action,
    Artifact,
    Fact,
    Group,
}

impl RulePlanNodeKind {
    fn canonical_name(self) -> &'static str {
        match self {
            Self::Action => "action",
            Self::Artifact => "artifact",
            Self::Fact => "fact",
            Self::Group => "group",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RulePlanEdge {
    pub from: RulePlanNodeId,
    pub to: RulePlanNodeId,
    pub kind: RulePlanEdgeKind,
    pub provenance: RulePlanProvenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
#[facet(rename_all = "lowercase")]
pub enum RulePlanEdgeKind {
    IfChanged,
    IfWritten,
    IfCreate,
    IfDeleted,
    Always,
}

impl RulePlanEdgeKind {
    fn canonical_name(self) -> &'static str {
        match self {
            Self::IfChanged => "ifchanged",
            Self::IfWritten => "ifwritten",
            Self::IfCreate => "ifcreate",
            Self::IfDeleted => "ifdeleted",
            Self::Always => "always",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RulePlanFact {
    pub id: RulePlanFactId,
    pub node_id: RulePlanNodeId,
    pub value: RulePlanValue,
    pub provenance: RulePlanProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RulePlanProvenance {
    pub rule_id: RuleId,
    pub source_id: RulePlanSourceId,
    pub span: RulePlanSpan,
    #[facet(default)]
    pub comment: Option<String>,
    #[facet(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct RulePlanSpan {
    pub line: u32,
    pub column: u32,
    #[facet(default)]
    pub end_line: u32,
    #[facet(default)]
    pub end_column: u32,
}

fn validate_provenance(
    provenance: &RulePlanProvenance,
    owner_kind: &str,
    owner_id: &str,
    source_ids: &HashSet<RulePlanSourceId>,
) -> Option<String> {
    if provenance.rule_id.0.trim().is_empty() {
        return Some(format!(
            "{} {} has empty provenance.rule_id",
            owner_kind, owner_id
        ));
    }
    if provenance.source_id.0.trim().is_empty() {
        return Some(format!(
            "{} {} has empty provenance.source_id",
            owner_kind, owner_id
        ));
    }
    if !source_ids.contains(&provenance.source_id) {
        return Some(format!(
            "{} {} references unknown provenance.source_id {}",
            owner_kind, owner_id, provenance.source_id.0
        ));
    }
    if provenance.span.line == 0 || provenance.span.column == 0 {
        return Some(format!(
            "{} {} has invalid provenance source position {}:{}",
            owner_kind, owner_id, provenance.span.line, provenance.span.column
        ));
    }
    None
}

fn edge_kind_allowed(
    edge_kind: RulePlanEdgeKind,
    from_kind: RulePlanNodeKind,
    to_kind: RulePlanNodeKind,
) -> bool {
    match edge_kind {
        RulePlanEdgeKind::IfWritten | RulePlanEdgeKind::IfCreate | RulePlanEdgeKind::IfDeleted => {
            from_kind == RulePlanNodeKind::Action && to_kind == RulePlanNodeKind::Artifact
        }
        RulePlanEdgeKind::IfChanged => {
            matches!(
                from_kind,
                RulePlanNodeKind::Artifact | RulePlanNodeKind::Fact
            ) && matches!(to_kind, RulePlanNodeKind::Action | RulePlanNodeKind::Group)
        }
        RulePlanEdgeKind::Always => {
            matches!(
                from_kind,
                RulePlanNodeKind::Action | RulePlanNodeKind::Group
            ) && matches!(to_kind, RulePlanNodeKind::Action | RulePlanNodeKind::Group)
        }
    }
}

/// Serialized build graph topology (DAG of action nodes and dependency edges).
#[derive(Debug, Clone, Facet)]
pub struct GraphTopology {
    /// Action nodes in the graph.
    pub nodes: Vec<GraphNode>,
    /// Dependency edges (from dependent → dependency).
    pub edges: Vec<GraphEdge>,
}

/// A node in the serialized build graph.
#[derive(Debug, Clone, Facet)]
pub struct GraphNode {
    /// Stable index for edge references.
    pub index: u32,
    /// Node identifier (matches `NodeReport.node_id`).
    pub node_id: String,
    /// Node kind (matches `NodeReport.kind`).
    pub kind: String,
    /// Human-readable display name.
    pub label: String,
}

/// A directed edge in the build graph: the node at `from` depends on the node at `to`.
#[derive(Debug, Clone, Facet)]
pub struct GraphEdge {
    /// Index of the dependent node.
    pub from: u32,
    /// Index of the dependency node.
    pub to: u32,
    /// Typed edge kind (for current plans this is always "always").
    #[facet(default)]
    pub kind: Option<String>,
}

// =============================================================================
// BUILD REPORT
// =============================================================================

/// Complete report for a single build invocation.
#[derive(Debug, Clone, Facet)]
pub struct BuildReport {
    /// Schema version for forward compatibility
    pub schema: u32,
    /// Unique run identifier
    pub run_id: RunId,
    /// When the build started (unix ms)
    pub started_at_unix_ms: u64,
    /// When the build finished (unix ms)
    pub ended_at_unix_ms: u64,

    /// Workspace root (relative paths are relative to this)
    pub workspace_root: String,
    /// Build profile ("debug" or "release")
    pub profile: String,
    /// Target triple (e.g., "aarch64-apple-darwin")
    pub target_triple: String,

    /// Toolchains used in this build
    pub toolchains: ToolchainsUsed,
    /// Per-node reports
    pub nodes: Vec<NodeReport>,

    /// Build graph topology (DAG structure).
    /// `None` for reports from schema version 1 (before graph serialization).
    #[facet(default)]
    pub graph: Option<GraphTopology>,

    /// Overall build success
    pub success: bool,
    /// Error message if build failed
    pub error: Option<String>,
}

impl BuildReport {
    /// Create a new BuildReport for a build that's starting.
    pub fn new(workspace_root: String, profile: String, target_triple: String) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            schema: REPORT_SCHEMA_VERSION,
            run_id: RunId::new(),
            started_at_unix_ms: now,
            ended_at_unix_ms: 0,
            workspace_root,
            profile,
            target_triple,
            toolchains: ToolchainsUsed::default(),
            nodes: Vec::new(),
            graph: None,
            success: false,
            error: None,
        }
    }

    /// Finalize the report with success/failure status.
    pub fn finalize(&mut self, success: bool, error: Option<String>) {
        use std::time::{SystemTime, UNIX_EPOCH};

        self.ended_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.success = success;
        self.error = error;
    }

    /// Add a node report.
    pub fn add_node(&mut self, node: NodeReport) {
        self.nodes.push(node);
    }

    /// Total build duration in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.ended_at_unix_ms
            .saturating_sub(self.started_at_unix_ms)
    }

    /// Count of cache hits.
    pub fn cache_hits(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| matches!(n.cache, CacheOutcome::Hit { .. }))
            .count()
    }

    /// Count of cache misses.
    pub fn cache_misses(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| matches!(n.cache, CacheOutcome::Miss { .. }))
            .count()
    }
}

// =============================================================================
// TOOLCHAINS
// =============================================================================

/// Toolchains used in a build.
#[derive(Debug, Clone, Default, Facet)]
pub struct ToolchainsUsed {
    /// Rust toolchain (rustc)
    pub rust: Option<ToolchainRef>,
    /// Zig toolchain (zig cc)
    pub zig: Option<ToolchainRef>,
}

/// Reference to a toolchain.
#[derive(Debug, Clone, Facet)]
pub struct ToolchainRef {
    /// Toolchain identifier (e.g., content hash for hermetic toolchains)
    pub id: String,
    /// Human-readable version string
    pub version: Option<String>,
}

// =============================================================================
// NODE REPORT
// =============================================================================

/// Report for a single build node.
#[derive(Debug, Clone, Facet)]
pub struct NodeReport {
    /// Human-readable node identifier (e.g., "compile-bin:hello:aarch64-apple-darwin:release")
    pub node_id: String,
    /// Node kind for categorization
    pub kind: String,
    /// Cache key (hex)
    pub cache_key: String,
    /// Cache outcome
    pub cache: CacheOutcome,
    /// Execution timing
    pub timing: NodeTiming,

    /// Inputs that contributed to the cache key
    pub inputs: Vec<InputRecord>,
    /// Dependencies used (for nodes with dependencies)
    pub deps: Vec<DependencyRecord>,
    /// Outputs produced (with hashes)
    pub outputs: Vec<OutputRecord>,
    /// Invocation details (if executed)
    pub invocation: Option<InvocationRecord>,

    /// Diagnostic output
    pub diagnostics: DiagnosticsRecord,

    /// Actual CAS cache key (Blake3 hex) used for store publish/unpublish.
    /// Distinct from `cache_key` which may be a human-readable identifier.
    /// `None` for infrastructure nodes (toolchains, registry crates) that
    /// use different storage paths.
    #[facet(default)]
    pub cas_cache_key: Option<String>,
}

/// Cache outcome for a node.
#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum CacheOutcome {
    /// Cache hit - output was retrieved from cache
    Hit {
        /// Manifest hash that was retrieved
        manifest: String,
    },
    /// Cache miss - node was executed
    Miss {
        /// Reason for the miss
        reason: MissReason,
    },
}

/// Reason for a cache miss.
#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum MissReason {
    /// First time building this node
    FirstBuild,
    /// Cache key not found in CAS
    KeyNotFound,
    /// A dependency changed
    DependencyChanged,
    /// Unknown reason (try not to use this)
    Unknown,
}

impl std::fmt::Display for MissReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MissReason::FirstBuild => write!(f, "first build"),
            MissReason::KeyNotFound => write!(f, "key not found"),
            MissReason::DependencyChanged => write!(f, "dependency changed"),
            MissReason::Unknown => write!(f, "unknown"),
        }
    }
}

/// Observed reason for a rebuild, computed by diffing reports.
///
/// This is distinct from MissReason (which is reported by the daemon).
/// ObservedReason is computed in vx explain by comparing NodeReports
/// across runs to determine what actually changed.
#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum ObservedReason {
    /// No previous run exists for this node
    FirstBuild,
    /// One or more inputs changed
    InputsChanged,
    /// One or more dependencies changed (rlib or manifest)
    DepsChanged,
    /// Both inputs and deps changed
    InputsAndDepsChanged,
    /// Cache miss but no diffs observed (potential tracking bug)
    Unknown,
}

impl std::fmt::Display for ObservedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ObservedReason::FirstBuild => write!(f, "first build"),
            ObservedReason::InputsChanged => write!(f, "inputs changed"),
            ObservedReason::DepsChanged => write!(f, "deps changed"),
            ObservedReason::InputsAndDepsChanged => write!(f, "inputs and deps changed"),
            ObservedReason::Unknown => write!(f, "unknown"),
        }
    }
}

/// Timing information for a node.
#[derive(Debug, Clone, Default, Facet)]
pub struct NodeTiming {
    /// Time spent in queue (if applicable)
    pub queued_ms: u64,
    /// Time spent executing
    pub execute_ms: u64,
    /// Milliseconds from build start to when execution began
    #[facet(default)]
    pub start_offset_ms: u64,
    /// Concurrency lane this action ran on (0-indexed)
    #[facet(default)]
    pub lane: u32,
}

// =============================================================================
// INPUTS AND OUTPUTS
// =============================================================================

/// Record of an input that contributed to a cache key.
#[derive(Debug, Clone, Facet)]
pub struct InputRecord {
    /// Label describing the input (e.g., "src/main.rs", "rustc", "Cargo.toml")
    pub label: String,
    /// Value of the input (hash, version string, etc.)
    pub value: String,
}

/// Record of a dependency used by a node.
///
/// This captures the full provenance of a dependency: what it's called,
/// what crate it is, and the exact rlib + manifest hashes that were used.
#[derive(Debug, Clone, Facet)]
pub struct DependencyRecord {
    /// Extern crate name (as used in --extern)
    pub extern_name: String,
    /// Unique crate identifier (hex)
    pub crate_id: String,
    /// Blake3 hash of the rlib file (hex)
    pub rlib_hash: String,
    /// Manifest hash from CAS (hex)
    pub manifest_hash: String,
}

// =============================================================================
// DIFF STRUCTURES (for vx explain)
// =============================================================================

/// Detailed explanation of why a node was rebuilt.
#[derive(Debug, Clone)]
pub struct NodeRebuildExplanation {
    /// Node ID
    pub node_id: String,
    /// Observed reason (computed from diffs)
    pub observed_reason: ObservedReason,
    /// Reported reason (from daemon)
    pub reported_reason: MissReason,
    /// Input changes
    pub input_changes: Vec<InputChange>,
    /// Dependency changes
    pub dep_changes: Vec<DepChange>,
}

/// A change in an input between runs.
#[derive(Debug, Clone)]
pub struct InputChange {
    /// Input label
    pub label: String,
    /// Old value (None if added)
    pub old_value: Option<String>,
    /// New value (None if removed)
    pub new_value: Option<String>,
}

/// A change in a dependency between runs.
#[derive(Debug, Clone)]
pub struct DepChange {
    /// Extern name
    pub extern_name: String,
    /// Crate ID
    pub crate_id: String,
    /// What changed
    pub change: DepChangeKind,
}

/// Kind of dependency change.
#[derive(Debug, Clone)]
pub enum DepChangeKind {
    /// Dependency was added
    Added {
        rlib_hash: String,
        manifest_hash: String,
    },
    /// Dependency was removed
    Removed {
        rlib_hash: String,
        manifest_hash: String,
    },
    /// Rlib hash changed
    RlibChanged {
        old_rlib: String,
        new_rlib: String,
        old_manifest: String,
        new_manifest: String,
    },
    /// Manifest hash changed (but not rlib)
    ManifestChanged {
        rlib_hash: String,
        old_manifest: String,
        new_manifest: String,
    },
}

/// Record of an output produced by a node.
#[derive(Debug, Clone, Facet)]
pub struct OutputRecord {
    /// Logical name ("bin", "rlib", "o", "exe")
    pub logical: String,
    /// Manifest hash (if stored in CAS)
    pub manifest: Option<String>,
    /// Blob hash (if stored in CAS)
    pub blob: Option<String>,
    /// Materialized path (workspace-relative)
    pub path: Option<String>,
}

/// Invocation details for an executed node.
#[derive(Debug, Clone, Facet)]
pub struct InvocationRecord {
    /// Program that was executed
    pub program: String,
    /// Arguments passed
    pub args: Vec<String>,
    /// Working directory (workspace-relative if possible)
    pub cwd: String,
    /// Exit code
    pub exit_code: i32,
}

/// Diagnostic output from a node (stored as CAS blob references).
#[derive(Debug, Clone, Default, Facet)]
pub struct DiagnosticsRecord {
    /// Blob hash of stdout (if captured)
    pub stdout_blob: Option<String>,
    /// Blob hash of stderr (if captured)
    pub stderr_blob: Option<String>,
}

// =============================================================================
// PERSISTENCE
// =============================================================================

/// Persistence layer for build reports.
pub struct ReportStore {
    /// Path to .vx/runs/ directory
    runs_dir: Utf8PathBuf,
}

impl ReportStore {
    /// Create a new ReportStore for a project.
    pub fn new(project_root: &Utf8Path) -> Self {
        Self {
            runs_dir: project_root.join(".vx/runs"),
        }
    }

    /// Initialize the runs directory.
    pub fn init(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.runs_dir)
    }

    /// Save a build report.
    pub fn save(&self, report: &BuildReport) -> std::io::Result<()> {
        self.init()?;

        let json = facet_json::to_string(report)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        let path = self.runs_dir.join(format!("{}.json", report.run_id));

        // Atomic write via temp file
        let tmp_path = self.runs_dir.join(format!("{}.json.tmp", report.run_id));
        std::fs::write(&tmp_path, &json)?;
        std::fs::rename(&tmp_path, &path)?;

        // Update "latest" symlink
        let latest_path = self.runs_dir.join("latest");
        if let Err(e) = std::fs::remove_file(&latest_path) {
            // NotFound is expected if this is the first run
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!("Warning: failed to remove old 'latest' file: {e}");
            }
        }
        std::fs::write(&latest_path, &report.run_id.0)?;

        Ok(())
    }

    /// Load a build report by run ID.
    pub fn load(&self, run_id: &RunId) -> std::io::Result<Option<BuildReport>> {
        let path = self.runs_dir.join(format!("{}.json", run_id));
        if !path.exists() {
            return Ok(None);
        }

        let json = std::fs::read_to_string(&path)?;
        let report: BuildReport = facet_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        Ok(Some(report))
    }

    /// Get the run ID of the latest build.
    pub fn latest_run_id(&self) -> std::io::Result<Option<RunId>> {
        let latest_path = self.runs_dir.join("latest");
        if !latest_path.exists() {
            return Ok(None);
        }

        let id_str = std::fs::read_to_string(&latest_path)?;
        Ok(id_str.trim().parse().ok())
    }

    /// Load the latest build report.
    pub fn load_latest(&self) -> std::io::Result<Option<BuildReport>> {
        if let Some(run_id) = self.latest_run_id()? {
            self.load(&run_id)
        } else {
            Ok(None)
        }
    }

    /// List all run IDs in chronological order (oldest first).
    pub fn list_runs(&self) -> std::io::Result<Vec<RunId>> {
        if !self.runs_dir.exists() {
            return Ok(Vec::new());
        }

        let mut runs = Vec::new();
        for entry in std::fs::read_dir(&self.runs_dir)? {
            let entry = entry?;
            let path = entry.path();
            if let Some(ext) = path.extension()
                && ext == "json"
                && let Some(stem) = path.file_stem()
                && let Ok(run_id) = stem.to_string_lossy().parse::<RunId>()
            {
                runs.push(run_id);
            }
        }

        // Sort lexicographically (ULIDs are time-ordered, so this gives chronological order)
        runs.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(runs)
    }

    /// Get the previous run (second most recent).
    pub fn previous_run_id(&self) -> std::io::Result<Option<RunId>> {
        let runs = self.list_runs()?;
        if runs.len() >= 2 {
            Ok(Some(runs[runs.len() - 2].clone()))
        } else {
            Ok(None)
        }
    }

    /// Load the previous build report.
    pub fn load_previous(&self) -> std::io::Result<Option<BuildReport>> {
        if let Some(run_id) = self.previous_run_id()? {
            self.load(&run_id)
        } else {
            Ok(None)
        }
    }
}

// =============================================================================
// DIFF
// =============================================================================

/// Difference between two build runs.
#[derive(Debug, Clone)]
pub struct RunDiff {
    /// Run ID of the "before" run
    pub before: RunId,
    /// Run ID of the "after" run
    pub after: RunId,
    /// Nodes that changed from hit to miss
    pub flipped_to_miss: Vec<NodeDiff>,
    /// Nodes that changed from miss to hit
    pub flipped_to_hit: Vec<String>,
    /// Toolchain changes
    pub toolchain_changes: Vec<ToolchainChange>,
}

/// Difference in a single node between runs.
#[derive(Debug, Clone)]
pub struct NodeDiff {
    /// Node ID
    pub node_id: String,
    /// Changed inputs (label → (old_value, new_value))
    pub changed_inputs: Vec<(String, String, String)>,
}

/// A toolchain change between runs.
#[derive(Debug, Clone)]
pub struct ToolchainChange {
    /// Toolchain name ("rust", "zig", etc.)
    pub name: String,
    /// Old version/ID
    pub old: Option<String>,
    /// New version/ID
    pub new: Option<String>,
}

impl RunDiff {
    /// Compute the diff between two build reports.
    pub fn compute(before: &BuildReport, after: &BuildReport) -> Self {
        let mut diff = RunDiff {
            before: before.run_id.clone(),
            after: after.run_id.clone(),
            flipped_to_miss: Vec::new(),
            flipped_to_hit: Vec::new(),
            toolchain_changes: Vec::new(),
        };

        // Build maps for quick lookup
        let before_nodes: std::collections::HashMap<&str, &NodeReport> = before
            .nodes
            .iter()
            .map(|n| (n.node_id.as_str(), n))
            .collect();

        let after_nodes: std::collections::HashMap<&str, &NodeReport> = after
            .nodes
            .iter()
            .map(|n| (n.node_id.as_str(), n))
            .collect();

        // Find nodes that flipped
        for (node_id, after_node) in &after_nodes {
            if let Some(before_node) = before_nodes.get(node_id) {
                let was_hit = matches!(before_node.cache, CacheOutcome::Hit { .. });
                let is_hit = matches!(after_node.cache, CacheOutcome::Hit { .. });

                if was_hit && !is_hit {
                    // Flipped from hit to miss - find what changed
                    let changed_inputs = Self::diff_inputs(&before_node.inputs, &after_node.inputs);
                    diff.flipped_to_miss.push(NodeDiff {
                        node_id: node_id.to_string(),
                        changed_inputs,
                    });
                } else if !was_hit && is_hit {
                    diff.flipped_to_hit.push(node_id.to_string());
                }
            }
        }

        // Check toolchain changes
        if let (Some(before_rust), Some(after_rust)) =
            (&before.toolchains.rust, &after.toolchains.rust)
            && before_rust.id != after_rust.id
        {
            diff.toolchain_changes.push(ToolchainChange {
                name: "rust".to_string(),
                old: before_rust.version.clone(),
                new: after_rust.version.clone(),
            });
        }

        if let (Some(before_zig), Some(after_zig)) = (&before.toolchains.zig, &after.toolchains.zig)
            && before_zig.id != after_zig.id
        {
            diff.toolchain_changes.push(ToolchainChange {
                name: "zig".to_string(),
                old: before_zig.version.clone(),
                new: after_zig.version.clone(),
            });
        }

        diff
    }

    fn diff_inputs(before: &[InputRecord], after: &[InputRecord]) -> Vec<(String, String, String)> {
        let before_map: std::collections::HashMap<&str, &str> = before
            .iter()
            .map(|i| (i.label.as_str(), i.value.as_str()))
            .collect();

        let mut changes = Vec::new();
        for input in after {
            if let Some(&old_value) = before_map.get(input.label.as_str()) {
                if old_value != input.value {
                    changes.push((
                        input.label.clone(),
                        old_value.to_string(),
                        input.value.clone(),
                    ));
                }
            } else {
                // New input
                changes.push((
                    input.label.clone(),
                    "(none)".to_string(),
                    input.value.clone(),
                ));
            }
        }

        changes
    }
}

impl NodeRebuildExplanation {
    /// Compute the rebuild explanation for a node by comparing it to the previous run.
    ///
    /// Returns None if the node was a cache hit.
    pub fn compute(current_node: &NodeReport, previous_node: Option<&NodeReport>) -> Option<Self> {
        // Only compute for cache misses
        let reported_reason = match &current_node.cache {
            CacheOutcome::Hit { .. } => return None,
            CacheOutcome::Miss { reason } => reason.clone(),
        };

        // If no previous node, it's a first build
        let Some(prev) = previous_node else {
            return Some(NodeRebuildExplanation {
                node_id: current_node.node_id.clone(),
                observed_reason: ObservedReason::FirstBuild,
                reported_reason,
                input_changes: Vec::new(),
                dep_changes: Vec::new(),
            });
        };

        // Compute input changes
        let input_changes = Self::compute_input_changes(&prev.inputs, &current_node.inputs);

        // Compute dependency changes
        let dep_changes = Self::compute_dep_changes(&prev.deps, &current_node.deps);

        // Determine observed reason based on what changed
        let observed_reason = match (input_changes.is_empty(), dep_changes.is_empty()) {
            (true, true) => ObservedReason::Unknown,
            (false, true) => ObservedReason::InputsChanged,
            (true, false) => ObservedReason::DepsChanged,
            (false, false) => ObservedReason::InputsAndDepsChanged,
        };

        Some(NodeRebuildExplanation {
            node_id: current_node.node_id.clone(),
            observed_reason,
            reported_reason,
            input_changes,
            dep_changes,
        })
    }

    fn compute_input_changes(before: &[InputRecord], after: &[InputRecord]) -> Vec<InputChange> {
        use std::collections::HashMap;

        let before_map: HashMap<&str, &str> = before
            .iter()
            .map(|i| (i.label.as_str(), i.value.as_str()))
            .collect();

        let after_map: HashMap<&str, &str> = after
            .iter()
            .map(|i| (i.label.as_str(), i.value.as_str()))
            .collect();

        let mut changes = Vec::new();

        // Find changed and added inputs
        for input in after {
            if let Some(&old_value) = before_map.get(input.label.as_str()) {
                if old_value != input.value {
                    changes.push(InputChange {
                        label: input.label.clone(),
                        old_value: Some(old_value.to_string()),
                        new_value: Some(input.value.clone()),
                    });
                }
            } else {
                // Added input
                changes.push(InputChange {
                    label: input.label.clone(),
                    old_value: None,
                    new_value: Some(input.value.clone()),
                });
            }
        }

        // Find removed inputs
        for input in before {
            if !after_map.contains_key(input.label.as_str()) {
                changes.push(InputChange {
                    label: input.label.clone(),
                    old_value: Some(input.value.clone()),
                    new_value: None,
                });
            }
        }

        // Sort by label for stable output
        changes.sort_by(|a, b| a.label.cmp(&b.label));
        changes
    }

    fn compute_dep_changes(
        before: &[DependencyRecord],
        after: &[DependencyRecord],
    ) -> Vec<DepChange> {
        use std::collections::HashMap;

        // Key by (extern_name, crate_id)
        let before_map: HashMap<(&str, &str), &DependencyRecord> = before
            .iter()
            .map(|d| ((d.extern_name.as_str(), d.crate_id.as_str()), d))
            .collect();

        let after_map: HashMap<(&str, &str), &DependencyRecord> = after
            .iter()
            .map(|d| ((d.extern_name.as_str(), d.crate_id.as_str()), d))
            .collect();

        let mut changes = Vec::new();

        // Find changed and added deps
        for dep in after {
            let key = (dep.extern_name.as_str(), dep.crate_id.as_str());
            if let Some(&old_dep) = before_map.get(&key) {
                // Check what changed
                let rlib_changed = old_dep.rlib_hash != dep.rlib_hash;
                let manifest_changed = old_dep.manifest_hash != dep.manifest_hash;

                if rlib_changed && manifest_changed {
                    changes.push(DepChange {
                        extern_name: dep.extern_name.clone(),
                        crate_id: dep.crate_id.clone(),
                        change: DepChangeKind::RlibChanged {
                            old_rlib: old_dep.rlib_hash.clone(),
                            new_rlib: dep.rlib_hash.clone(),
                            old_manifest: old_dep.manifest_hash.clone(),
                            new_manifest: dep.manifest_hash.clone(),
                        },
                    });
                } else if manifest_changed {
                    changes.push(DepChange {
                        extern_name: dep.extern_name.clone(),
                        crate_id: dep.crate_id.clone(),
                        change: DepChangeKind::ManifestChanged {
                            rlib_hash: dep.rlib_hash.clone(),
                            old_manifest: old_dep.manifest_hash.clone(),
                            new_manifest: dep.manifest_hash.clone(),
                        },
                    });
                }
            } else {
                // Added dependency
                changes.push(DepChange {
                    extern_name: dep.extern_name.clone(),
                    crate_id: dep.crate_id.clone(),
                    change: DepChangeKind::Added {
                        rlib_hash: dep.rlib_hash.clone(),
                        manifest_hash: dep.manifest_hash.clone(),
                    },
                });
            }
        }

        // Find removed deps
        for dep in before {
            let key = (dep.extern_name.as_str(), dep.crate_id.as_str());
            if !after_map.contains_key(&key) {
                changes.push(DepChange {
                    extern_name: dep.extern_name.clone(),
                    crate_id: dep.crate_id.clone(),
                    change: DepChangeKind::Removed {
                        rlib_hash: dep.rlib_hash.clone(),
                        manifest_hash: dep.manifest_hash.clone(),
                    },
                });
            }
        }

        // Sort by (extern_name, crate_id) for stable output
        changes.sort_by(|a, b| {
            a.extern_name
                .cmp(&b.extern_name)
                .then(a.crate_id.cmp(&b.crate_id))
        });
        changes
    }
}

/// Fanout analysis showing which rebuilds triggered which downstream rebuilds.
#[derive(Debug, Clone)]
pub struct FanoutAnalysis {
    /// Map from crate_id to list of dependent node IDs that rebuilt
    pub fanout_map: std::collections::HashMap<String, Vec<String>>,
}

impl FanoutAnalysis {
    /// Compute fanout from a build report.
    ///
    /// A node A fans out to node B if:
    /// - B.deps contains A.crate_id
    /// - and A rebuilt in this run
    /// - and B rebuilt in this run
    pub fn compute(report: &BuildReport) -> Self {
        use std::collections::{HashMap, HashSet};

        // Track which nodes rebuilt
        let mut rebuilt_nodes: HashSet<String> = HashSet::new();

        for node in &report.nodes {
            // Track rebuilt nodes
            if matches!(node.cache, CacheOutcome::Miss { .. }) {
                rebuilt_nodes.insert(node.node_id.clone());
            }
        }

        // Build reverse dependency map: crate_id -> Vec<node_id that depends on it>
        let mut fanout_map: HashMap<String, Vec<String>> = HashMap::new();

        for node in &report.nodes {
            // Only consider rebuilt nodes
            if !rebuilt_nodes.contains(&node.node_id) {
                continue;
            }

            // For each dependency of this node
            for dep in &node.deps {
                // Find if the dependency's crate rebuilt
                // We need to match crate_id to nodes
                // For now, we'll track fanout by crate_id
                fanout_map
                    .entry(dep.crate_id.clone())
                    .or_default()
                    .push(node.node_id.clone());
            }
        }

        // Deduplicate and sort for stable output
        for dependents in fanout_map.values_mut() {
            dependents.sort();
            dependents.dedup();
        }

        FanoutAnalysis { fanout_map }
    }

    /// Get the list of nodes that depend on the given crate_id and rebuilt.
    pub fn get_dependents(&self, crate_id: &str) -> &[String] {
        self.fanout_map
            .get(crate_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Check if a crate_id has any dependents that rebuilt.
    pub fn has_fanout(&self, crate_id: &str) -> bool {
        self.fanout_map
            .get(crate_id)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_id_generation() {
        let id1 = RunId::new();
        let id2 = RunId::new();

        // Should be different
        assert_ne!(id1.0, id2.0);

        // Should be 26 characters
        assert_eq!(id1.0.len(), 26);
        assert_eq!(id2.0.len(), 26);

        // Should parse back
        assert!(id1.0.parse::<RunId>().is_ok());
    }

    #[test]
    fn test_run_id_ordering() {
        // Generate IDs with a small delay to ensure different timestamps
        let id1 = RunId::new();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let id2 = RunId::new();

        // id2 should sort after id1 (lexicographic ordering = chronological for ULIDs)
        assert!(id2.0 >= id1.0);
    }

    #[test]
    fn test_build_report_creation() {
        let report = BuildReport::new(
            "/home/user/project".to_string(),
            "release".to_string(),
            "aarch64-apple-darwin".to_string(),
        );

        assert_eq!(report.schema, REPORT_SCHEMA_VERSION);
        assert_eq!(report.profile, "release");
        assert!(!report.success);
        assert!(report.nodes.is_empty());
    }

    #[test]
    fn test_build_report_finalize() {
        let mut report = BuildReport::new(
            "/home/user/project".to_string(),
            "debug".to_string(),
            "x86_64-unknown-linux-gnu".to_string(),
        );

        report.finalize(true, None);

        assert!(report.success);
        assert!(report.ended_at_unix_ms >= report.started_at_unix_ms);
    }

    // =========================================================================
    // Lane 2 Tests
    // =========================================================================

    #[test]
    fn test_node_rebuild_explanation_first_build() {
        let node = NodeReport {
            node_id: "test-node".to_string(),
            kind: "rust.compile_bin".to_string(),
            cache_key: "abc123".to_string(),
            cache: CacheOutcome::Miss {
                reason: MissReason::KeyNotFound,
            },
            timing: NodeTiming::default(),
            inputs: vec![],
            deps: vec![],
            outputs: vec![],
            invocation: None,
            diagnostics: DiagnosticsRecord::default(),
            cas_cache_key: None,
        };

        let explanation = NodeRebuildExplanation::compute(&node, None).unwrap();

        assert_eq!(explanation.node_id, "test-node");
        assert!(matches!(
            explanation.observed_reason,
            ObservedReason::FirstBuild
        ));
        assert!(explanation.input_changes.is_empty());
        assert!(explanation.dep_changes.is_empty());
    }

    #[test]
    fn test_node_rebuild_explanation_input_changed() {
        let prev_node = NodeReport {
            node_id: "test-node".to_string(),
            kind: "rust.compile_bin".to_string(),
            cache_key: "abc123".to_string(),
            cache: CacheOutcome::Miss {
                reason: MissReason::KeyNotFound,
            },
            timing: NodeTiming::default(),
            inputs: vec![InputRecord {
                label: "src".to_string(),
                value: "hash_old".to_string(),
            }],
            deps: vec![],
            outputs: vec![],
            invocation: None,
            diagnostics: DiagnosticsRecord::default(),
            cas_cache_key: None,
        };

        let current_node = NodeReport {
            inputs: vec![InputRecord {
                label: "src".to_string(),
                value: "hash_new".to_string(),
            }],
            ..prev_node.clone()
        };

        let explanation = NodeRebuildExplanation::compute(&current_node, Some(&prev_node)).unwrap();

        assert!(matches!(
            explanation.observed_reason,
            ObservedReason::InputsChanged
        ));
        assert_eq!(explanation.input_changes.len(), 1);
        assert_eq!(explanation.input_changes[0].label, "src");
        assert_eq!(
            explanation.input_changes[0].old_value,
            Some("hash_old".to_string())
        );
        assert_eq!(
            explanation.input_changes[0].new_value,
            Some("hash_new".to_string())
        );
    }

    #[test]
    fn test_node_rebuild_explanation_dep_changed() {
        let prev_node = NodeReport {
            node_id: "test-node".to_string(),
            kind: "rust.compile_bin".to_string(),
            cache_key: "abc123".to_string(),
            cache: CacheOutcome::Miss {
                reason: MissReason::KeyNotFound,
            },
            timing: NodeTiming::default(),
            inputs: vec![],
            deps: vec![DependencyRecord {
                extern_name: "util".to_string(),
                crate_id: "crate_123".to_string(),
                rlib_hash: "rlib_old".to_string(),
                manifest_hash: "manifest_old".to_string(),
            }],
            outputs: vec![],
            invocation: None,
            diagnostics: DiagnosticsRecord::default(),
            cas_cache_key: None,
        };

        let current_node = NodeReport {
            deps: vec![DependencyRecord {
                extern_name: "util".to_string(),
                crate_id: "crate_123".to_string(),
                rlib_hash: "rlib_new".to_string(),
                manifest_hash: "manifest_new".to_string(),
            }],
            ..prev_node.clone()
        };

        let explanation = NodeRebuildExplanation::compute(&current_node, Some(&prev_node)).unwrap();

        assert!(matches!(
            explanation.observed_reason,
            ObservedReason::DepsChanged
        ));
        assert_eq!(explanation.dep_changes.len(), 1);
        assert_eq!(explanation.dep_changes[0].extern_name, "util");
        assert_eq!(explanation.dep_changes[0].crate_id, "crate_123");
    }

    #[test]
    fn test_fanout_analysis() {
        let mut report = BuildReport::new(
            "/test".to_string(),
            "debug".to_string(),
            "test-triple".to_string(),
        );

        // Add a rebuilt lib node
        report.nodes.push(NodeReport {
            node_id: "lib-util".to_string(),
            kind: "rust.compile_rlib".to_string(),
            cache_key: "key1".to_string(),
            cache: CacheOutcome::Miss {
                reason: MissReason::KeyNotFound,
            },
            timing: NodeTiming::default(),
            inputs: vec![],
            deps: vec![],
            outputs: vec![],
            invocation: None,
            diagnostics: DiagnosticsRecord::default(),
            cas_cache_key: None,
        });

        // Add a rebuilt app node that depends on util
        report.nodes.push(NodeReport {
            node_id: "bin-app".to_string(),
            kind: "rust.compile_bin".to_string(),
            cache_key: "key2".to_string(),
            cache: CacheOutcome::Miss {
                reason: MissReason::KeyNotFound,
            },
            timing: NodeTiming::default(),
            inputs: vec![],
            deps: vec![DependencyRecord {
                extern_name: "util".to_string(),
                crate_id: "crate_util".to_string(),
                rlib_hash: "rlib_hash".to_string(),
                manifest_hash: "manifest_hash".to_string(),
            }],
            outputs: vec![],
            invocation: None,
            diagnostics: DiagnosticsRecord::default(),
            cas_cache_key: None,
        });

        // Add a cache hit node (should not appear in fanout)
        report.nodes.push(NodeReport {
            node_id: "bin-other".to_string(),
            kind: "rust.compile_bin".to_string(),
            cache_key: "key3".to_string(),
            cache: CacheOutcome::Hit {
                manifest: "manifest".to_string(),
            },
            timing: NodeTiming::default(),
            inputs: vec![],
            deps: vec![DependencyRecord {
                extern_name: "util".to_string(),
                crate_id: "crate_util".to_string(),
                rlib_hash: "rlib_hash".to_string(),
                manifest_hash: "manifest_hash".to_string(),
            }],
            outputs: vec![],
            invocation: None,
            diagnostics: DiagnosticsRecord::default(),
            cas_cache_key: None,
        });

        let fanout = FanoutAnalysis::compute(&report);

        // crate_util should have bin-app as a dependent (but not bin-other since it was a hit)
        assert!(fanout.has_fanout("crate_util"));
        let dependents = fanout.get_dependents("crate_util");
        assert_eq!(dependents.len(), 1);
        assert_eq!(dependents[0], "bin-app");
    }

    #[test]
    fn test_observed_reason_unknown() {
        // Test case where there's a miss but no observable changes (potential bug)
        let prev_node = NodeReport {
            node_id: "test-node".to_string(),
            kind: "rust.compile_bin".to_string(),
            cache_key: "abc123".to_string(),
            cache: CacheOutcome::Hit {
                manifest: "manifest".to_string(),
            },
            timing: NodeTiming::default(),
            inputs: vec![InputRecord {
                label: "src".to_string(),
                value: "hash_same".to_string(),
            }],
            deps: vec![],
            outputs: vec![],
            invocation: None,
            diagnostics: DiagnosticsRecord::default(),
            cas_cache_key: None,
        };

        let current_node = NodeReport {
            cache: CacheOutcome::Miss {
                reason: MissReason::KeyNotFound,
            },
            ..prev_node.clone()
        };

        let explanation = NodeRebuildExplanation::compute(&current_node, Some(&prev_node)).unwrap();

        // No inputs or deps changed, but it's a miss - should be Unknown
        assert!(matches!(
            explanation.observed_reason,
            ObservedReason::Unknown
        ));
    }

    #[test]
    fn test_graph_topology_roundtrip() {
        let graph = GraphTopology {
            nodes: vec![
                GraphNode {
                    index: 0,
                    node_id: "compile-rlib:serde".to_string(),
                    kind: "rust.compile_rlib".to_string(),
                    label: "compile serde".to_string(),
                },
                GraphNode {
                    index: 1,
                    node_id: "compile-bin:app".to_string(),
                    kind: "rust.compile_bin".to_string(),
                    label: "compile app".to_string(),
                },
            ],
            edges: vec![GraphEdge {
                from: 1,
                to: 0,
                kind: Some("always".to_string()),
            }],
        };

        let mut report = BuildReport::new(
            "/test".to_string(),
            "debug".to_string(),
            "test-triple".to_string(),
        );
        report.graph = Some(graph);
        report.finalize(true, None);

        let json = facet_json::to_string(&report).expect("serialize");
        let parsed: BuildReport = facet_json::from_str(&json).expect("deserialize");

        let g = parsed.graph.expect("graph should be present");
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.nodes[0].node_id, "compile-rlib:serde");
        assert_eq!(g.nodes[1].node_id, "compile-bin:app");
        assert_eq!(g.edges[0].from, 1);
        assert_eq!(g.edges[0].to, 0);
        assert_eq!(g.edges[0].kind.as_deref(), Some("always"));
    }

    #[test]
    fn test_report_without_graph_deserializes() {
        // Simulate a schema v1 report that has no graph field
        let report = BuildReport::new(
            "/test".to_string(),
            "debug".to_string(),
            "test-triple".to_string(),
        );
        // graph is None by default
        assert!(report.graph.is_none());

        let json = facet_json::to_string(&report).expect("serialize");
        let parsed: BuildReport = facet_json::from_str(&json).expect("deserialize");
        assert!(parsed.graph.is_none());
    }

    fn sample_sources() -> Vec<RulePlanSource> {
        vec![RulePlanSource {
            id: RulePlanSourceId("src_0".to_string()),
            path: Utf8PathBuf::from("fixtures/rules/sample.edn"),
        }]
    }

    fn sample_provenance(rule_id: &str, line: u32) -> RulePlanProvenance {
        RulePlanProvenance {
            rule_id: RuleId(rule_id.to_string()),
            source_id: RulePlanSourceId("src_0".to_string()),
            span: RulePlanSpan {
                line,
                column: 1,
                end_line: line,
                end_column: 10,
            },
            comment: Some("fixture".to_string()),
            metadata: BTreeMap::from([
                ("phase".to_string(), "planner".to_string()),
                ("fixture".to_string(), "true".to_string()),
            ]),
        }
    }

    fn sample_rule_plan() -> RulePlan {
        RulePlan {
            sources: sample_sources(),
            nodes: vec![
                RulePlanNode {
                    id: RulePlanNodeId("n_group_build".to_string()),
                    kind: RulePlanNodeKind::Group,
                    label: "group build".to_string(),
                    provenance: sample_provenance("rule.group.build", 1),
                },
                RulePlanNode {
                    id: RulePlanNodeId("n_action_compile".to_string()),
                    kind: RulePlanNodeKind::Action,
                    label: "compile app".to_string(),
                    provenance: sample_provenance("rule.compile.app", 5),
                },
                RulePlanNode {
                    id: RulePlanNodeId("n_artifact_bin".to_string()),
                    kind: RulePlanNodeKind::Artifact,
                    label: "target/app".to_string(),
                    provenance: sample_provenance("rule.compile.app", 8),
                },
                RulePlanNode {
                    id: RulePlanNodeId("n_fact_src_changed".to_string()),
                    kind: RulePlanNodeKind::Fact,
                    label: "src_changed".to_string(),
                    provenance: sample_provenance("rule.detect.src", 12),
                },
            ],
            edges: vec![
                RulePlanEdge {
                    from: RulePlanNodeId("n_group_build".to_string()),
                    to: RulePlanNodeId("n_action_compile".to_string()),
                    kind: RulePlanEdgeKind::Always,
                    provenance: sample_provenance("rule.group.build", 2),
                },
                RulePlanEdge {
                    from: RulePlanNodeId("n_action_compile".to_string()),
                    to: RulePlanNodeId("n_artifact_bin".to_string()),
                    kind: RulePlanEdgeKind::IfWritten,
                    provenance: sample_provenance("rule.compile.app", 9),
                },
                RulePlanEdge {
                    from: RulePlanNodeId("n_fact_src_changed".to_string()),
                    to: RulePlanNodeId("n_action_compile".to_string()),
                    kind: RulePlanEdgeKind::IfChanged,
                    provenance: sample_provenance("rule.detect.src", 13),
                },
            ],
            facts: vec![RulePlanFact {
                id: RulePlanFactId("fact.src_changed".to_string()),
                node_id: RulePlanNodeId("n_fact_src_changed".to_string()),
                value: RulePlanValue("true".to_string()),
                provenance: sample_provenance("rule.detect.src", 14),
            }],
        }
    }

    #[test]
    fn test_rule_plan_roundtrip_canonical_json_stable() {
        let plan = sample_rule_plan();
        plan.validate().expect("sample plan should validate");

        let canonical_1 = plan.to_canonical_json().expect("canonical json");
        let parsed = RulePlan::from_json(&canonical_1).expect("parse canonical json");
        let canonical_2 = parsed.to_canonical_json().expect("canonical json");

        assert_eq!(canonical_1, canonical_2);
    }

    #[test]
    fn test_rule_plan_validation_reports_actionable_diagnostics() {
        let mut invalid = sample_rule_plan();
        invalid.nodes.push(RulePlanNode {
            id: RulePlanNodeId("n_action_compile".to_string()),
            kind: RulePlanNodeKind::Action,
            label: "duplicate".to_string(),
            provenance: sample_provenance("rule.duplicate", 20),
        });
        invalid.edges.push(RulePlanEdge {
            from: RulePlanNodeId("n_artifact_bin".to_string()),
            to: RulePlanNodeId("n_action_compile".to_string()),
            kind: RulePlanEdgeKind::IfWritten,
            provenance: sample_provenance("rule.invalid.edge", 21),
        });
        invalid.facts.push(RulePlanFact {
            id: RulePlanFactId("fact.bad_ref".to_string()),
            node_id: RulePlanNodeId("missing_node".to_string()),
            value: RulePlanValue("x".to_string()),
            provenance: sample_provenance("rule.bad.fact", 22),
        });

        let err = invalid.validate().expect_err("invalid plan should fail");

        assert!(
            err.diagnostics
                .iter()
                .any(|d| d.contains("duplicate node id"))
        );
        assert!(err.diagnostics.iter().any(|d| {
            d.contains("IfWritten") && d.contains("invalid between Artifact and Action")
        }));
        assert!(
            err.diagnostics
                .iter()
                .any(|d| d.contains("references unknown node_id"))
        );
    }

    #[test]
    fn test_rule_plan_fixture_provenance_is_populated() {
        let plan = sample_rule_plan();

        assert_eq!(plan.sources.len(), 1);
        assert!(!plan.sources[0].id.0.is_empty());
        assert!(!plan.sources[0].path.as_str().is_empty());

        for node in &plan.nodes {
            assert!(!node.provenance.rule_id.0.is_empty());
            assert!(!node.provenance.source_id.0.is_empty());
            assert!(node.provenance.span.line > 0);
            assert!(node.provenance.span.column > 0);
        }
        for edge in &plan.edges {
            assert!(!edge.provenance.rule_id.0.is_empty());
            assert!(!edge.provenance.source_id.0.is_empty());
            assert!(edge.provenance.span.line > 0);
            assert!(edge.provenance.span.column > 0);
        }
        for fact in &plan.facts {
            assert!(!fact.provenance.rule_id.0.is_empty());
            assert!(!fact.provenance.source_id.0.is_empty());
            assert!(fact.provenance.span.line > 0);
            assert!(fact.provenance.span.column > 0);
        }
    }
}
