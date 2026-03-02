//! Action graph for declarative build execution
//!
//! This module defines the action-based execution model that replaces the
//! imperative sequential loop. Actions are nodes in a dependency graph,
//! and the executor schedules them for optimal parallelism.

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use std::collections::HashMap;
use vx_report::{RulePlan, RulePlanEdge, RulePlanEdgeKind, RulePlanNode, RulePlanNodeKind};
use vx_rs::crate_graph::{CrateGraph, CrateId, CrateSource, CrateType};
use vx_runner_proto::{RunFileMount, RunOutput, RunTreeMount};
use vx_store_proto::{RustChannel, RustComponent, RustToolchainSpec};

use crate::inputs::{BuildConfig, RustToolchain};

/// Dependency with source information (for building RustDep at execution time)
#[derive(Debug, Clone)]
pub struct ActionDep {
    /// The extern crate name
    pub extern_name: String,
    /// CrateId of the dependency
    pub crate_id: CrateId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannerSyntheticKind {
    Group,
    Action,
    Artifact,
    Fact,
}

impl PlannerSyntheticKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Action => "action",
            Self::Artifact => "artifact",
            Self::Fact => "fact",
        }
    }

    pub(crate) fn report_kind(self) -> &'static str {
        match self {
            Self::Group => "planner.group",
            Self::Action => "planner.action",
            Self::Artifact => "planner.artifact",
            Self::Fact => "planner.fact",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunToolchain {
    Zig,
    Rust,
}

#[derive(Debug, Clone)]
pub enum RunKind {
    Generic,
    RustCompile {
        crate_id: CrateId,
        crate_name: String,
        crate_type: CrateType,
    },
}

#[derive(Debug, Clone)]
pub struct RunCollectInput {
    pub logical: String,
    pub path_prefix: String,
    pub path_suffix: String,
    pub pad_width: usize,
}

#[derive(Debug, Clone)]
pub struct RunMaterialize {
    pub logical: String,
    pub workspace_rel_path: String,
}

#[derive(Debug, Clone)]
pub struct RunDepTreeMount {
    pub crate_id: CrateId,
    pub mount: String,
}

#[derive(Debug, Clone)]
pub struct RunDepFileMount {
    pub crate_id: CrateId,
    pub logical: String,
    pub path: String,
}

/// A single unit of work in the build process
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Action {
    /// Download a Rust toolchain component from static.rust-lang.org
    DownloadRustToolchainComponent {
        /// Rust channel (Stable, Beta, Nightly)
        channel: RustChannel,
        /// Target triple
        target_triple: String,
        /// Component to download
        component: RustComponent,
    },

    /// Extract a Rust toolchain component tarball into CAS trees
    ExtractRustToolchainComponent {
        /// Rust channel (Stable, Beta, Nightly)
        channel: RustChannel,
        /// Target triple
        target_triple: String,
        /// Component to extract
        component: RustComponent,
    },

    /// Acquire Zig toolchain for C/C++ compilation
    AcquireZigToolchain {
        /// Zig version (e.g., "0.13.0")
        version: String,
    },

    /// Acquire registry crate from crates.io
    AcquireRegistryCrate {
        /// Crate name
        name: String,
        /// Crate version
        version: String,
        /// Checksum for verification
        checksum: String,
    },

    /// Finalize a Rust toolchain (produces a ToolchainManifest hash).
    ///
    /// The store may return a cache hit if the toolchain was already finalized.
    FinalizeRustToolchain { spec: RustToolchainSpec },

    /// Ingest a Rust crate's workspace sources into a CAS tree manifest.
    IngestRustSourceTree {
        crate_id: CrateId,
        crate_name: String,
        edition: String,
        crate_type: CrateType,
        workspace_root: String,
        crate_root_rel: String,
    },

    /// Extract a registry crate tarball into a CAS tree manifest.
    ExtractRegistryCrateTree {
        crate_id: CrateId,
        name: String,
        version: String,
        crate_name: String,
        edition: String,
        crate_type: CrateType,
        crate_root_rel: String,
    },

    /// Run a sandboxed toolchain command with declared CAS/VFS inputs and outputs.
    Run {
        label: String,
        kind: RunKind,
        toolchain: RunToolchain,
        program_rel: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
        cwd: Option<String>,
        clear_env: bool,
        tree_mounts: Vec<RunTreeMount>,
        dep_tree_mounts: Vec<RunDepTreeMount>,
        file_mounts: Vec<RunFileMount>,
        dep_file_mounts: Vec<RunDepFileMount>,
        collect_file_mounts: Vec<RunCollectInput>,
        outputs: Vec<RunOutput>,
        materialize: Vec<RunMaterialize>,
    },

    // NOTE: Rust compilation is represented as `Action::Run { kind: RunKind::RustCompile { .. }, .. }`.
    /// Planner-lowered Rust compile action (metadata-backed migration step).
    PlannerCompileRustCrate {
        node_id: String,
        report_node_id: Option<String>,
        crate_name: String,
        crate_type: CrateType,
    },

    /// Synthetic planner node used by RulePlan lowering bridge.
    PlannerSynthetic {
        node_id: String,
        report_node_id: Option<String>,
        label: String,
        kind: PlannerSyntheticKind,
    },
}

impl Action {
    /// Get a display name for TUI tracking
    pub fn display_name(&self) -> String {
        match self {
            Action::DownloadRustToolchainComponent { component, .. } => {
                format!("download toolchain {:?}", component)
            }
            Action::ExtractRustToolchainComponent { component, .. } => {
                format!("extract toolchain {:?}", component)
            }
            Action::AcquireZigToolchain { version } => {
                format!("acquire zig {}", version)
            }
            Action::AcquireRegistryCrate { name, version, .. } => {
                format!("acquire {}@{}", name, version)
            }
            Action::FinalizeRustToolchain { .. } => "finalize rust toolchain".to_string(),
            Action::IngestRustSourceTree { crate_name, .. } => format!("ingest {}", crate_name),
            Action::ExtractRegistryCrateTree { name, version, .. } => {
                format!("extract {}@{}", name, version)
            }
            Action::Run { label, .. } => format!("run {label}"),
            Action::PlannerCompileRustCrate {
                node_id,
                report_node_id,
                crate_name,
                crate_type,
            } => {
                let crate_type = crate_type.as_str();
                if let Some(report_node_id) = report_node_id {
                    format!(
                        "planner compile {} ({}, legacy={})",
                        crate_name, node_id, report_node_id
                    )
                } else {
                    format!(
                        "planner compile {} ({}:{})",
                        crate_name, crate_type, node_id
                    )
                }
            }
            Action::PlannerSynthetic {
                node_id,
                report_node_id,
                kind,
                label,
                ..
            } => {
                if let Some(report_node_id) = report_node_id {
                    format!(
                        "planner {} {} ({}, legacy={})",
                        kind.label(),
                        node_id,
                        label,
                        report_node_id
                    )
                } else {
                    format!("planner {} {} ({})", kind.label(), node_id, label)
                }
            }
        }
    }

    /// Get the node_id as it appears in `NodeReport.node_id`.
    ///
    /// Must stay in sync with the node_id generation in `executor.rs`.
    pub fn report_node_id(&self) -> String {
        match self {
            Action::DownloadRustToolchainComponent { .. }
            | Action::ExtractRustToolchainComponent { .. }
            | Action::AcquireZigToolchain { .. }
            | Action::FinalizeRustToolchain { .. }
            | Action::IngestRustSourceTree { .. }
            | Action::ExtractRegistryCrateTree { .. }
            | Action::Run { .. } => self.display_name(),
            Action::AcquireRegistryCrate { name, version, .. } => {
                format!("acquire-registry:{name}:{version}")
            }
            Action::PlannerCompileRustCrate {
                crate_name,
                crate_type,
                report_node_id,
                ..
            } => report_node_id
                .clone()
                .unwrap_or_else(|| format!("compile-{}:{crate_name}", crate_type.as_str())),
            Action::PlannerSynthetic {
                node_id,
                report_node_id,
                kind,
                ..
            } => report_node_id
                .clone()
                .unwrap_or_else(|| format!("planner-{}:{node_id}", kind.label())),
        }
    }

    /// Get the kind string as it appears in `NodeReport.kind`.
    ///
    /// Must stay in sync with the kind generation in `executor.rs`.
    pub fn report_kind(&self) -> &'static str {
        match self {
            Action::DownloadRustToolchainComponent { .. } => "toolchain.component.download",
            Action::ExtractRustToolchainComponent { .. } => "toolchain.component.extract",
            Action::AcquireZigToolchain { .. } => "zig.toolchain.acquire",
            Action::AcquireRegistryCrate { .. } => "registry.acquire",
            Action::FinalizeRustToolchain { .. } => "toolchain.finalize",
            Action::IngestRustSourceTree { .. } => "source.ingest",
            Action::ExtractRegistryCrateTree { .. } => "registry.extract",
            Action::Run {
                kind: RunKind::RustCompile { crate_type, .. },
                ..
            } => {
                if crate_type.as_str() == "bin" {
                    "rust.compile_bin"
                } else {
                    "rust.compile_rlib"
                }
            }
            Action::Run { .. } => "run",
            Action::PlannerCompileRustCrate { crate_type, .. } => {
                if crate_type.as_str() == "bin" {
                    "rust.compile_bin"
                } else {
                    "rust.compile_rlib"
                }
            }
            Action::PlannerSynthetic { kind, .. } => kind.report_kind(),
        }
    }

    /// Convert to Progress ActionType
    pub fn to_progress_action_type(&self) -> vxd_proto::ActionType {
        match self {
            Action::DownloadRustToolchainComponent { component, .. } => {
                vxd_proto::ActionType::DownloadRustToolchainComponent(format!("{:?}", component))
            }
            Action::ExtractRustToolchainComponent { component, .. } => {
                vxd_proto::ActionType::ExtractRustToolchainComponent(format!("{:?}", component))
            }
            Action::AcquireZigToolchain { version } => {
                vxd_proto::ActionType::AcquireZigToolchain(version.clone())
            }
            Action::AcquireRegistryCrate { name, version, .. } => {
                vxd_proto::ActionType::AcquireRegistryCrate(name.clone(), version.clone())
            }
            Action::FinalizeRustToolchain { .. } => vxd_proto::ActionType::AcquireToolchain,
            Action::IngestRustSourceTree { crate_name, .. } => {
                vxd_proto::ActionType::IngestSource(crate_name.clone())
            }
            Action::ExtractRegistryCrateTree { name, version, .. } => {
                vxd_proto::ActionType::IngestSource(format!("{name}@{version}"))
            }
            Action::Run {
                kind: RunKind::RustCompile { crate_name, .. },
                ..
            } => vxd_proto::ActionType::CompileRust(crate_name.clone()),
            Action::Run { label, .. } => vxd_proto::ActionType::Run(label.clone()),
            Action::PlannerCompileRustCrate { crate_name, .. } => {
                vxd_proto::ActionType::CompileRust(crate_name.clone())
            }
            Action::PlannerSynthetic { node_id, kind, .. } => {
                vxd_proto::ActionType::CompileRust(format!("planner:{}:{}", kind.label(), node_id))
            }
        }
    }
}

/// A node in the action graph
#[derive(Debug, Clone)]
pub struct ActionNode {
    /// The actual work to perform
    pub action: Action,
}

/// The action dependency graph
pub struct ActionGraph {
    /// Directed graph: edges point from dependent → dependency
    /// (i.e., if A depends on B, there's an edge A → B)
    pub graph: DiGraph<ActionNode, String>,

    /// Rust toolchain spec used for finalization during compile.
    pub rust_toolchain_spec: Option<RustToolchainSpec>,
}

#[derive(Debug, Clone)]
pub struct RulePlanLoweringError {
    pub message: String,
    pub rule_id: String,
    pub source_id: String,
    pub line: u32,
    pub column: u32,
}

impl std::fmt::Display for RulePlanLoweringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (rule={}, source={}, at {}:{})",
            self.message, self.rule_id, self.source_id, self.line, self.column
        )
    }
}

impl std::error::Error for RulePlanLoweringError {}

impl ActionGraph {
    /// Create an empty action graph (for manual construction, e.g., C projects)
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            rust_toolchain_spec: None,
        }
    }

    /// Add an action to the graph and return its node index
    pub fn add_action(&mut self, action: Action) -> NodeIndex {
        self.graph.add_node(ActionNode { action })
    }

    /// Add a dependency edge from dependent to dependency
    /// (i.e., dependent depends on dependency, so dependency must execute first)
    pub fn add_edge(&mut self, dependency: NodeIndex, dependent: NodeIndex) {
        self.add_edge_with_kind(dependency, dependent, "always");
    }

    /// Add a dependency edge with a typed semantic kind.
    pub fn add_edge_with_kind(&mut self, dependency: NodeIndex, dependent: NodeIndex, kind: &str) {
        self.graph.add_edge(dependent, dependency, kind.to_string());
    }

    /// Build action graph from a CrateGraph
    ///
    /// This creates Rust toolchain component actions, AcquireRegistryCrate, and
    /// CompileRustCrate actions with proper dependency edges.
    /// actions with proper dependency edges.
    pub fn from_crate_graph(
        crate_graph: &CrateGraph,
        rust_channel: RustChannel,
        target_triple: String,
        toolchain: RustToolchain,
        config: BuildConfig,
        db: &crate::db::Database,
    ) -> Result<Self, crate::error::VxdError> {
        use tracing::debug;

        debug!("Building action graph from crate graph");
        let mut graph = DiGraph::new();
        let mut crate_to_node = HashMap::new();
        let mut registry_nodes = HashMap::new();

        // 1. Create per-component toolchain actions.
        debug!("Creating toolchain component actions");
        let download_rustc_node = graph.add_node(ActionNode {
            action: Action::DownloadRustToolchainComponent {
                channel: rust_channel.clone(),
                target_triple: target_triple.clone(),
                component: RustComponent::Rustc,
            },
        });
        let download_rust_std_node = graph.add_node(ActionNode {
            action: Action::DownloadRustToolchainComponent {
                channel: rust_channel.clone(),
                target_triple: target_triple.clone(),
                component: RustComponent::RustStd,
            },
        });
        let extract_rustc_node = graph.add_node(ActionNode {
            action: Action::ExtractRustToolchainComponent {
                channel: rust_channel.clone(),
                target_triple: target_triple.clone(),
                component: RustComponent::Rustc,
            },
        });
        let extract_rust_std_node = graph.add_node(ActionNode {
            action: Action::ExtractRustToolchainComponent {
                channel: rust_channel.clone(),
                target_triple: target_triple.clone(),
                component: RustComponent::RustStd,
            },
        });

        // Extraction depends on matching download.
        graph.add_edge(
            extract_rustc_node,
            download_rustc_node,
            "always".to_string(),
        );
        graph.add_edge(
            extract_rust_std_node,
            download_rust_std_node,
            "always".to_string(),
        );

        // 2. Create registry crate acquisition actions
        debug!(
            registry_count = crate_graph.iter_registry_crates().count(),
            "Creating registry acquisition actions"
        );
        for (i, registry_crate) in crate_graph.iter_registry_crates().enumerate() {
            let registry_action = graph.add_node(ActionNode {
                action: Action::AcquireRegistryCrate {
                    name: registry_crate.name.clone(),
                    version: registry_crate.version.clone(),
                    checksum: registry_crate.checksum.clone(),
                },
            });
            registry_nodes.insert(
                (registry_crate.name.clone(), registry_crate.version.clone()),
                registry_action,
            );
            if (i + 1) % 10 == 0 {
                debug!(created = i + 1, "Registry nodes created");
            }
        }
        debug!(total = registry_nodes.len(), "All registry nodes created");

        // 3. Create compilation action nodes for all crates
        debug!(
            crate_count = crate_graph.nodes.len(),
            "Creating compilation actions"
        );

        // Finalize Rust toolchain once after components are extracted.
        let toolchain_spec = RustToolchainSpec {
            channel: rust_channel,
            host: target_triple.clone(),
            target: target_triple.clone(),
            components: vec![RustComponent::Rustc, RustComponent::RustStd],
        };
        let finalize_node = graph.add_node(ActionNode {
            action: Action::FinalizeRustToolchain {
                spec: toolchain_spec.clone(),
            },
        });
        graph.add_edge(finalize_node, extract_rustc_node, "always".to_string());
        graph.add_edge(finalize_node, extract_rust_std_node, "always".to_string());

        // Create per-crate source tree prep nodes + per-crate rustc run nodes.
        for (i, crate_node) in crate_graph.nodes.values().enumerate() {
            // Build ActionDep list for --extern wiring
            let action_deps: Vec<ActionDep> = crate_node
                .deps
                .iter()
                .map(|dep| ActionDep {
                    extern_name: dep.extern_name.clone(),
                    crate_id: dep.crate_id,
                })
                .collect();

            let target_triple_str = toolchain
                .target(db)
                .map_err(|e| crate::error::VxdError::Picante(e.to_string()))?;
            let profile_str = config
                .profile(db)
                .map_err(|e| crate::error::VxdError::Picante(e.to_string()))?;

            let workspace_root_str = crate_graph.workspace_root.to_string();

            let source_node = match &crate_node.source {
                CrateSource::Path { .. } => graph.add_node(ActionNode {
                    action: Action::IngestRustSourceTree {
                        crate_id: crate_node.id,
                        crate_name: crate_node.crate_name.clone(),
                        edition: crate_node.edition.as_str().to_string(),
                        crate_type: crate_node.crate_type,
                        workspace_root: workspace_root_str.clone(),
                        crate_root_rel: crate_node.crate_root_rel.to_string(),
                    },
                }),
                CrateSource::Registry { name, version, .. } => {
                    // Depends on AcquireRegistryCrate.
                    let node_idx = graph.add_node(ActionNode {
                        action: Action::ExtractRegistryCrateTree {
                            crate_id: crate_node.id,
                            name: name.clone(),
                            version: version.clone(),
                            crate_name: crate_node.crate_name.clone(),
                            edition: crate_node.edition.as_str().to_string(),
                            crate_type: crate_node.crate_type,
                            crate_root_rel: crate_node.crate_root_rel.to_string(),
                        },
                    });
                    if let Some(&registry_idx) =
                        registry_nodes.get(&(name.clone(), version.clone()))
                    {
                        graph.add_edge(node_idx, registry_idx, "always".to_string());
                    }
                    node_idx
                }
            };

            let output_filename = if crate_node.crate_type.as_str() == "lib" {
                format!("lib{}.rlib", crate_node.crate_name)
            } else {
                crate_node.crate_name.clone()
            };
            let output_rel = format!("out/{output_filename}");

            // Keep paths relative to the action prefix; runner expands ${VX_TOOLCHAIN}.
            let mut args = vec![
                "--sysroot".to_string(),
                "${VX_TOOLCHAIN}/sysroot".to_string(),
                "--crate-name".to_string(),
                crate_node.crate_name.clone(),
                "--crate-type".to_string(),
                crate_node.crate_type.as_str().to_string(),
                "--edition".to_string(),
                crate_node.edition.as_str().to_string(),
                "--target".to_string(),
                target_triple_str.clone(),
                "-o".to_string(),
                output_rel.clone(),
            ];

            if profile_str == "release" {
                args.push("-C".to_string());
                args.push("opt-level=3".to_string());
            } else {
                args.push("-C".to_string());
                args.push("opt-level=0".to_string());
                args.push("-g".to_string());
            }

            // Add --extern flags; the corresponding .rlib files are mounted at deps/.
            for dep in &action_deps {
                args.push("--extern".to_string());
                args.push(format!(
                    "{}=deps/lib{}.rlib",
                    dep.extern_name, dep.extern_name
                ));
            }

            // Crate root
            args.push(format!("src/{}", crate_node.crate_root_rel));

            let outputs = vec![RunOutput {
                logical: if crate_node.crate_type.as_str() == "lib" {
                    "rlib".to_string()
                } else {
                    "bin".to_string()
                },
                path: output_rel,
                executable: crate_node.crate_type.as_str() == "bin",
            }];

            let action = Action::Run {
                label: format!("rustc {}", crate_node.crate_name),
                kind: RunKind::RustCompile {
                    crate_id: crate_node.id,
                    crate_name: crate_node.crate_name.clone(),
                    crate_type: crate_node.crate_type,
                },
                toolchain: RunToolchain::Rust,
                program_rel: "bin/rustc".to_string(),
                args,
                env: vec![],
                cwd: None,
                clear_env: true,
                tree_mounts: vec![],
                dep_tree_mounts: vec![RunDepTreeMount {
                    crate_id: crate_node.id,
                    mount: "src".to_string(),
                }],
                file_mounts: vec![],
                dep_file_mounts: action_deps
                    .iter()
                    .map(|dep| RunDepFileMount {
                        crate_id: dep.crate_id,
                        logical: "rlib".to_string(),
                        path: format!("deps/lib{}.rlib", dep.extern_name),
                    })
                    .collect(),
                collect_file_mounts: vec![],
                outputs,
                materialize: if crate_node.crate_type.as_str() == "bin" {
                    vec![RunMaterialize {
                        logical: "bin".to_string(),
                        workspace_rel_path: format!(
                            ".vx/build/{}/{}/{}",
                            target_triple_str, profile_str, crate_node.crate_name
                        ),
                    }]
                } else {
                    vec![]
                },
            };

            let node_idx = graph.add_node(ActionNode { action });
            crate_to_node.insert(crate_node.id, node_idx);

            // Dependencies:
            // - rustc run depends on toolchain finalization
            // - rustc run depends on its source tree prep node
            graph.add_edge(node_idx, finalize_node, "always".to_string());
            graph.add_edge(node_idx, source_node, "always".to_string());

            if (i + 1) % 10 == 0 {
                debug!(created = i + 1, "Compilation nodes created");
            }
        }
        debug!(total = crate_to_node.len(), "All compilation nodes created");

        // 4. Add dependency edges
        debug!("Adding dependency edges");
        // Edge from dependent → dependency means "dependent needs dependency to complete first"
        for crate_node in crate_graph.nodes.values() {
            let compile_idx = crate_to_node[&crate_node.id];

            // Add edges to crate dependencies
            for dep in &crate_node.deps {
                if let Some(&dep_compile_idx) = crate_to_node.get(&dep.crate_id) {
                    graph.add_edge(compile_idx, dep_compile_idx, "always".to_string());
                }
            }
        }

        debug!(
            nodes = graph.node_count(),
            edges = graph.edge_count(),
            "Action graph construction complete"
        );

        Ok(Self {
            graph,
            rust_toolchain_spec: Some(toolchain_spec),
        })
    }

    /// Lower a `RulePlan` into an executable `ActionGraph`.
    ///
    /// This bridge intentionally starts small: it materializes `group`/`action`/`artifact`/`fact`
    /// nodes as synthetic planner actions and preserves edge kinds in topology metadata.
    pub fn from_rule_plan(plan: &RulePlan) -> Result<Self, RulePlanLoweringError> {
        if let Err(err) = plan.validate() {
            return Err(RulePlanLoweringError {
                message: format!(
                    "RulePlan validation failed before lowering: {}",
                    err.diagnostics.join("; ")
                ),
                rule_id: "ruleplan.validate".to_string(),
                source_id: "ruleplan".to_string(),
                line: 1,
                column: 1,
            });
        }

        let mut lowered = Self::new();
        let mut node_map = HashMap::with_capacity(plan.nodes.len());

        for node in &plan.nodes {
            let action = action_from_rule_plan_node(node)?;
            let idx = lowered.add_action(action);
            node_map.insert(node.id.as_str().to_string(), idx);
        }

        for edge in &plan.edges {
            let from_idx = node_map.get(edge.from.as_str()).copied().ok_or_else(|| {
                lowering_error_for_edge(
                    edge,
                    format!("edge references missing from node '{}'", edge.from),
                )
            })?;
            let to_idx = node_map.get(edge.to.as_str()).copied().ok_or_else(|| {
                lowering_error_for_edge(
                    edge,
                    format!("edge references missing to node '{}'", edge.to),
                )
            })?;

            let edge_kind = planner_edge_kind(edge)?;
            lowered.add_edge_with_kind(from_idx, to_idx, edge_kind);
        }

        Ok(lowered)
    }

    /// Lower a `RulePlan` into an executable Rust `ActionGraph`.
    ///
    /// This is the first "RulePlan owns execution" slice:
    ///
    /// - action nodes with provenance metadata `{crate_name, crate_type}` are treated as Rust
    ///   compilation nodes and lowered to real `CompileRustCrate` actions.
    /// - all selected crates must include their full dependency closure in the plan, otherwise
    ///   lowering fails with a provenance-mapped error (the plan must be the build).
    /// - registry crates are materialized via `AcquireRegistryCrate` and compiled as part of the plan.
    ///
    /// For now, dependency edges are derived from the crate graph (not from RulePlan edges).
    pub fn from_rule_plan_executable(
        plan: &RulePlan,
        crate_graph: &CrateGraph,
        rust_channel: RustChannel,
        target_triple: String,
        toolchain: RustToolchain,
        config: BuildConfig,
        db: &crate::db::Database,
    ) -> Result<Self, RulePlanLoweringError> {
        if let Err(err) = plan.validate() {
            return Err(RulePlanLoweringError {
                message: format!(
                    "RulePlan validation failed before lowering: {}",
                    err.diagnostics.join("; ")
                ),
                rule_id: "ruleplan.validate".to_string(),
                source_id: "ruleplan".to_string(),
                line: 1,
                column: 1,
            });
        }

        // 1. Identify all crates mentioned by the plan (action nodes with crate metadata).
        let mut included = std::collections::HashSet::<CrateId>::new();
        let mut plan_node_by_crate = HashMap::<CrateId, RulePlanNode>::new();

        for node in &plan.nodes {
            if node.kind != RulePlanNodeKind::Action {
                continue;
            }
            let Some(crate_name) = node.provenance.metadata.get("crate_name") else {
                continue;
            };
            let Some(crate_type_raw) = node.provenance.metadata.get("crate_type") else {
                continue;
            };

            let crate_type = match crate_type_raw.as_str() {
                "bin" => CrateType::Bin,
                "lib" => CrateType::Lib,
                other => {
                    return Err(lowering_error_for_node(
                        node,
                        format!("invalid crate_type '{other}' (expected 'bin' or 'lib')"),
                    ));
                }
            };

            let mut matches = crate_graph
                .nodes
                .values()
                .filter(|crate_node| {
                    crate_node.crate_name == *crate_name && crate_node.crate_type == crate_type
                })
                .map(|crate_node| crate_node.id)
                .collect::<Vec<_>>();

            match matches.as_slice() {
                [] => {
                    return Err(lowering_error_for_node(
                        node,
                        format!(
                            "RulePlan references crate '{crate_name}' type '{crate_type_raw}' but it is not present in the crate graph"
                        ),
                    ));
                }
                [only] => {
                    if !included.insert(*only) {
                        return Err(lowering_error_for_node(
                            node,
                            format!(
                                "RulePlan contains duplicate compile action nodes for crate '{crate_name}' type '{crate_type_raw}'"
                            ),
                        ));
                    }
                    plan_node_by_crate.insert(*only, node.clone());
                }
                _ => {
                    matches.sort_by_key(|id| id.short_hex());
                    return Err(lowering_error_for_node(
                        node,
                        format!(
                            "RulePlan references crate '{crate_name}' type '{crate_type_raw}' but it matched multiple crate IDs: {}",
                            matches
                                .into_iter()
                                .map(|id| id.short_hex())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    ));
                }
            }
        }

        if included.is_empty() {
            return Err(RulePlanLoweringError {
                message: "RulePlan executable lowering requires at least one action node with provenance metadata {crate_name, crate_type}".to_string(),
                rule_id: "ruleplan.exec".to_string(),
                source_id: "ruleplan".to_string(),
                line: 1,
                column: 1,
            });
        }

        // 2. Enforce "plan is the build": all deps must be present in the plan.
        for crate_id in included.iter().copied().collect::<Vec<_>>() {
            let crate_node = &crate_graph.nodes[&crate_id];
            let plan_node = plan_node_by_crate
                .get(&crate_id)
                .expect("plan node exists for included crate");
            for dep in &crate_node.deps {
                if !included.contains(&dep.crate_id) {
                    let dep_node = &crate_graph.nodes[&dep.crate_id];
                    return Err(lowering_error_for_node(
                        plan_node,
                        format!(
                            "RulePlan compile crate '{}' is missing dependency crate '{}' (add it to the plan, e.g. via rust.transitive_closure)",
                            crate_node.crate_name, dep_node.crate_name
                        ),
                    ));
                }
            }
        }

        // 3. Build a real action graph for exactly the plan's crate set.
        let mut graph = DiGraph::new();
        let mut crate_to_node = HashMap::new();
        let mut registry_nodes = HashMap::new();

        // Toolchain actions.
        let download_rustc_node = graph.add_node(ActionNode {
            action: Action::DownloadRustToolchainComponent {
                channel: rust_channel.clone(),
                target_triple: target_triple.clone(),
                component: RustComponent::Rustc,
            },
        });
        let download_rust_std_node = graph.add_node(ActionNode {
            action: Action::DownloadRustToolchainComponent {
                channel: rust_channel.clone(),
                target_triple: target_triple.clone(),
                component: RustComponent::RustStd,
            },
        });
        let extract_rustc_node = graph.add_node(ActionNode {
            action: Action::ExtractRustToolchainComponent {
                channel: rust_channel.clone(),
                target_triple: target_triple.clone(),
                component: RustComponent::Rustc,
            },
        });
        let extract_rust_std_node = graph.add_node(ActionNode {
            action: Action::ExtractRustToolchainComponent {
                channel: rust_channel.clone(),
                target_triple: target_triple.clone(),
                component: RustComponent::RustStd,
            },
        });

        graph.add_edge(
            extract_rustc_node,
            download_rustc_node,
            "always".to_string(),
        );
        graph.add_edge(
            extract_rust_std_node,
            download_rust_std_node,
            "always".to_string(),
        );

        let toolchain_spec = RustToolchainSpec {
            channel: rust_channel.clone(),
            host: target_triple.clone(),
            target: target_triple.clone(),
            components: vec![RustComponent::Rustc, RustComponent::RustStd],
        };
        let finalize_node = graph.add_node(ActionNode {
            action: Action::FinalizeRustToolchain {
                spec: toolchain_spec.clone(),
            },
        });
        graph.add_edge(finalize_node, extract_rustc_node, "always".to_string());
        graph.add_edge(finalize_node, extract_rust_std_node, "always".to_string());

        // Registry acquisition for needed registry crates in the plan set.
        let mut needed_registry = std::collections::BTreeMap::<(String, String, String), ()>::new();
        for crate_id in &included {
            let node = &crate_graph.nodes[crate_id];
            if let CrateSource::Registry {
                name,
                version,
                checksum,
            } = &node.source
            {
                needed_registry.insert((name.clone(), version.clone(), checksum.clone()), ());
            }
        }

        for ((name, version, checksum), _) in needed_registry {
            let registry_action = graph.add_node(ActionNode {
                action: Action::AcquireRegistryCrate {
                    name: name.clone(),
                    version: version.clone(),
                    checksum,
                },
            });
            registry_nodes.insert((name, version), registry_action);
        }

        // Compile nodes in topo order for determinism.
        for crate_id in &crate_graph.topo_order {
            if !included.contains(crate_id) {
                continue;
            }
            let crate_node = &crate_graph.nodes[crate_id];

            let action_deps: Vec<ActionDep> = crate_node
                .deps
                .iter()
                .map(|dep| ActionDep {
                    extern_name: dep.extern_name.clone(),
                    crate_id: dep.crate_id,
                })
                .collect();

            let target_triple_str = toolchain.target(db).map_err(|e| {
                lowering_error_for_node(plan_node_by_crate.get(crate_id).unwrap(), e.to_string())
            })?;
            let profile_str = config.profile(db).map_err(|e| {
                lowering_error_for_node(plan_node_by_crate.get(crate_id).unwrap(), e.to_string())
            })?;

            let workspace_root_str = crate_graph.workspace_root.to_string();

            let source_node = match &crate_node.source {
                CrateSource::Path { .. } => graph.add_node(ActionNode {
                    action: Action::IngestRustSourceTree {
                        crate_id: crate_node.id,
                        crate_name: crate_node.crate_name.clone(),
                        edition: crate_node.edition.as_str().to_string(),
                        crate_type: crate_node.crate_type,
                        workspace_root: workspace_root_str.clone(),
                        crate_root_rel: crate_node.crate_root_rel.to_string(),
                    },
                }),
                CrateSource::Registry { name, version, .. } => {
                    let node_idx = graph.add_node(ActionNode {
                        action: Action::ExtractRegistryCrateTree {
                            crate_id: crate_node.id,
                            name: name.clone(),
                            version: version.clone(),
                            crate_name: crate_node.crate_name.clone(),
                            edition: crate_node.edition.as_str().to_string(),
                            crate_type: crate_node.crate_type,
                            crate_root_rel: crate_node.crate_root_rel.to_string(),
                        },
                    });
                    if let Some(&registry_idx) =
                        registry_nodes.get(&(name.clone(), version.clone()))
                    {
                        graph.add_edge(node_idx, registry_idx, "always".to_string());
                    }
                    node_idx
                }
            };

            let output_filename = if crate_node.crate_type.as_str() == "lib" {
                format!("lib{}.rlib", crate_node.crate_name)
            } else {
                crate_node.crate_name.clone()
            };
            let output_rel = format!("out/{output_filename}");

            let mut args = vec![
                "--sysroot".to_string(),
                "${VX_TOOLCHAIN}/sysroot".to_string(),
                "--crate-name".to_string(),
                crate_node.crate_name.clone(),
                "--crate-type".to_string(),
                crate_node.crate_type.as_str().to_string(),
                "--edition".to_string(),
                crate_node.edition.as_str().to_string(),
                "--target".to_string(),
                target_triple_str.clone(),
                "-o".to_string(),
                output_rel.clone(),
            ];

            if profile_str == "release" {
                args.push("-C".to_string());
                args.push("opt-level=3".to_string());
            } else {
                args.push("-C".to_string());
                args.push("opt-level=0".to_string());
                args.push("-g".to_string());
            }

            for dep in &action_deps {
                args.push("--extern".to_string());
                args.push(format!(
                    "{}=deps/lib{}.rlib",
                    dep.extern_name, dep.extern_name
                ));
            }

            args.push(format!("src/{}", crate_node.crate_root_rel));

            let outputs = vec![RunOutput {
                logical: if crate_node.crate_type.as_str() == "lib" {
                    "rlib".to_string()
                } else {
                    "bin".to_string()
                },
                path: output_rel,
                executable: crate_node.crate_type.as_str() == "bin",
            }];

            let action = Action::Run {
                label: format!("rustc {}", crate_node.crate_name),
                kind: RunKind::RustCompile {
                    crate_id: crate_node.id,
                    crate_name: crate_node.crate_name.clone(),
                    crate_type: crate_node.crate_type,
                },
                toolchain: RunToolchain::Rust,
                program_rel: "bin/rustc".to_string(),
                args,
                env: vec![],
                cwd: None,
                clear_env: true,
                tree_mounts: vec![],
                dep_tree_mounts: vec![RunDepTreeMount {
                    crate_id: crate_node.id,
                    mount: "src".to_string(),
                }],
                file_mounts: vec![],
                dep_file_mounts: action_deps
                    .iter()
                    .map(|dep| RunDepFileMount {
                        crate_id: dep.crate_id,
                        logical: "rlib".to_string(),
                        path: format!("deps/lib{}.rlib", dep.extern_name),
                    })
                    .collect(),
                collect_file_mounts: vec![],
                outputs,
                materialize: if crate_node.crate_type.as_str() == "bin" {
                    vec![RunMaterialize {
                        logical: "bin".to_string(),
                        workspace_rel_path: format!(
                            ".vx/build/{}/{}/{}",
                            target_triple_str, profile_str, crate_node.crate_name
                        ),
                    }]
                } else {
                    vec![]
                },
            };

            let node_idx = graph.add_node(ActionNode { action });
            crate_to_node.insert(crate_node.id, node_idx);

            graph.add_edge(node_idx, finalize_node, "always".to_string());
            graph.add_edge(node_idx, source_node, "always".to_string());
        }

        // Dependency edges.
        for crate_id in &crate_graph.topo_order {
            if !included.contains(crate_id) {
                continue;
            }
            let crate_node = &crate_graph.nodes[crate_id];
            let Some(&compile_idx) = crate_to_node.get(&crate_node.id) else {
                continue;
            };

            for dep in &crate_node.deps {
                let dep_compile_idx = crate_to_node.get(&dep.crate_id).copied().ok_or_else(|| {
                    lowering_error_for_node(
                        plan_node_by_crate.get(crate_id).unwrap(),
                        format!(
                            "internal lowering error: crate '{}' depends on '{}' but dependency node was not built",
                            crate_node.crate_name,
                            crate_graph.nodes[&dep.crate_id].crate_name
                        ),
                    )
                })?;
                graph.add_edge(compile_idx, dep_compile_idx, "always".to_string());
            }
        }

        Ok(Self {
            graph,
            rust_toolchain_spec: Some(RustToolchainSpec {
                channel: rust_channel,
                host: target_triple.clone(),
                target: target_triple,
                components: vec![RustComponent::Rustc, RustComponent::RustStd],
            }),
        })
    }

    /// Get total number of actions
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Serialize the graph structure into a `GraphTopology` for the build report.
    pub fn to_topology(&self) -> vx_report::GraphTopology {
        let mut index_map: HashMap<NodeIndex, u32> = HashMap::new();
        let mut nodes = Vec::with_capacity(self.graph.node_count());

        for (i, idx) in self.graph.node_indices().enumerate() {
            let action = &self.graph[idx].action;
            let index = i as u32;
            index_map.insert(idx, index);
            nodes.push(vx_report::GraphNode {
                index,
                node_id: action.report_node_id(),
                kind: action.report_kind().to_string(),
                label: action.display_name(),
            });
        }

        let mut edges = Vec::with_capacity(self.graph.edge_count());
        for edge in self.graph.edge_references() {
            let from = index_map[&edge.source()];
            let to = index_map[&edge.target()];
            edges.push(vx_report::GraphEdge {
                from,
                to,
                kind: Some(edge.weight().clone()),
            });
        }

        vx_report::GraphTopology { nodes, edges }
    }

    /// Dump graph structure for debugging
    pub fn dump_graph(&self) {
        use petgraph::Direction::Incoming;
        use tracing::debug;

        // Collect nodes with their dependency counts
        let nodes_info: Vec<_> = self
            .graph
            .node_indices()
            .map(|idx| {
                let node = &self.graph[idx];
                let outgoing = self.graph.neighbors(idx).count();
                let incoming = self.graph.neighbors_directed(idx, Incoming).count();
                (idx, node, outgoing, incoming)
            })
            .collect();

        // Count ready nodes
        let ready_count = nodes_info.iter().filter(|(_, _, out, _)| *out == 0).count();

        debug!(
            nodes = self.graph.node_count(),
            edges = self.graph.edge_count(),
            ready_nodes = ready_count,
            "Graph summary"
        );

        // Show first few ready nodes as examples
        let ready_examples: Vec<_> = nodes_info
            .iter()
            .filter(|(_, _, out, _)| *out == 0)
            .take(3)
            .collect();

        for (idx, node, _outgoing, incoming) in ready_examples {
            debug!(
                node = ?idx,
                action = node.action.display_name(),
                incoming = incoming,
                "Example ready node"
            );
        }

        // Show first few non-ready nodes with their deps
        let blocked_examples: Vec<_> = nodes_info
            .iter()
            .filter(|(_, _, out, _)| *out > 0)
            .take(2)
            .collect();

        for (idx, node, outgoing, _incoming) in blocked_examples {
            let deps: Vec<_> = self
                .graph
                .neighbors(*idx)
                .take(3)
                .map(|dep_idx| self.graph[dep_idx].action.display_name())
                .collect();
            debug!(
                node = ?idx,
                action = node.action.display_name(),
                dep_count = outgoing,
                deps = ?deps,
                "Example blocked node"
            );
        }
    }
}

fn planner_kind_for_node(node: &RulePlanNode) -> PlannerSyntheticKind {
    match node.kind {
        RulePlanNodeKind::Group => PlannerSyntheticKind::Group,
        RulePlanNodeKind::Action => PlannerSyntheticKind::Action,
        RulePlanNodeKind::Artifact => PlannerSyntheticKind::Artifact,
        RulePlanNodeKind::Fact => PlannerSyntheticKind::Fact,
    }
}

fn action_from_rule_plan_node(node: &RulePlanNode) -> Result<Action, RulePlanLoweringError> {
    if let Some(action) = try_lower_compile_node(node)? {
        return Ok(action);
    }
    if let Some(action) = try_lower_registry_node(node)? {
        return Ok(action);
    }

    let synthetic_kind = planner_kind_for_node(node);
    Ok(Action::PlannerSynthetic {
        node_id: node.id.as_str().to_string(),
        report_node_id: node.provenance.metadata.get("legacy_node_id").cloned(),
        label: node.label.clone(),
        kind: synthetic_kind,
    })
}

fn try_lower_compile_node(node: &RulePlanNode) -> Result<Option<Action>, RulePlanLoweringError> {
    if !wants_compile_lowering(node) {
        return Ok(None);
    }
    if node.kind != RulePlanNodeKind::Action {
        return Err(lowering_error_for_node(
            node,
            "compile node lowering requires node kind 'action'".to_string(),
        ));
    }

    let Some(crate_name) = node.provenance.metadata.get("crate_name").cloned() else {
        return Err(lowering_error_for_node(
            node,
            "compile node is missing provenance metadata 'crate_name'".to_string(),
        ));
    };
    let Some(crate_type_raw) = node
        .provenance
        .metadata
        .get("crate_type")
        .map(|s| s.as_str())
    else {
        return Err(lowering_error_for_node(
            node,
            "compile node is missing provenance metadata 'crate_type'".to_string(),
        ));
    };
    let crate_type = match crate_type_raw {
        "bin" => CrateType::Bin,
        "lib" => CrateType::Lib,
        other => {
            return Err(lowering_error_for_node(
                node,
                format!("compile node has invalid crate_type '{other}' (expected 'bin' or 'lib')"),
            ));
        }
    };

    Ok(Some(Action::PlannerCompileRustCrate {
        node_id: node.id.as_str().to_string(),
        report_node_id: node.provenance.metadata.get("legacy_node_id").cloned(),
        crate_name,
        crate_type,
    }))
}

fn try_lower_registry_node(node: &RulePlanNode) -> Result<Option<Action>, RulePlanLoweringError> {
    if !wants_registry_lowering(node) {
        return Ok(None);
    }
    if node.kind != RulePlanNodeKind::Artifact {
        return Err(lowering_error_for_node(
            node,
            "registry node lowering requires node kind 'artifact'".to_string(),
        ));
    }

    let Some(name) = node.provenance.metadata.get("registry_name").cloned() else {
        return Err(lowering_error_for_node(
            node,
            "registry node is missing provenance metadata 'registry_name'".to_string(),
        ));
    };
    let Some(version) = node.provenance.metadata.get("registry_version").cloned() else {
        return Err(lowering_error_for_node(
            node,
            "registry node is missing provenance metadata 'registry_version'".to_string(),
        ));
    };
    let Some(checksum) = node.provenance.metadata.get("registry_checksum").cloned() else {
        return Err(lowering_error_for_node(
            node,
            "registry node is missing provenance metadata 'registry_checksum'".to_string(),
        ));
    };

    Ok(Some(Action::AcquireRegistryCrate {
        name,
        version,
        checksum,
    }))
}

fn planner_edge_kind(edge: &RulePlanEdge) -> Result<&'static str, RulePlanLoweringError> {
    match edge.kind {
        RulePlanEdgeKind::Always => Ok("always"),
        RulePlanEdgeKind::IfChanged => Ok("ifchanged"),
        RulePlanEdgeKind::IfWritten => Ok("ifwritten"),
        RulePlanEdgeKind::IfCreate => Err(lowering_error_for_edge(
            edge,
            "edge kind 'ifcreate' is not supported by the starter lowering bridge".to_string(),
        )),
        RulePlanEdgeKind::IfDeleted => Err(lowering_error_for_edge(
            edge,
            "edge kind 'ifdeleted' is not supported by the starter lowering bridge".to_string(),
        )),
    }
}

fn lowering_error_for_edge(edge: &RulePlanEdge, message: String) -> RulePlanLoweringError {
    RulePlanLoweringError {
        message,
        rule_id: edge.provenance.rule_id.as_str().to_string(),
        source_id: edge.provenance.source_id.as_str().to_string(),
        line: edge.provenance.span.line,
        column: edge.provenance.span.column,
    }
}

fn lowering_error_for_node(node: &RulePlanNode, message: String) -> RulePlanLoweringError {
    RulePlanLoweringError {
        message,
        rule_id: node.provenance.rule_id.as_str().to_string(),
        source_id: node.provenance.source_id.as_str().to_string(),
        line: node.provenance.span.line,
        column: node.provenance.span.column,
    }
}

fn wants_compile_lowering(node: &RulePlanNode) -> bool {
    if node.kind != RulePlanNodeKind::Action {
        return false;
    }
    node.label.starts_with("compile-")
        || node.provenance.metadata.contains_key("crate_name")
        || node.provenance.metadata.contains_key("crate_type")
}

fn wants_registry_lowering(node: &RulePlanNode) -> bool {
    if node.kind != RulePlanNodeKind::Artifact {
        return false;
    }
    node.label.starts_with("acquire-registry:")
        || node.provenance.metadata.contains_key("registry_name")
        || node.provenance.metadata.contains_key("registry_version")
        || node.provenance.metadata.contains_key("registry_checksum")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use vx_report::{
        RuleId, RulePlan, RulePlanEdge, RulePlanFact, RulePlanFactId, RulePlanNode, RulePlanNodeId,
        RulePlanProvenance, RulePlanSource, RulePlanSourceId, RulePlanSpan, RulePlanValue,
    };

    fn provenance(rule: &str, line: u32) -> RulePlanProvenance {
        RulePlanProvenance {
            rule_id: RuleId::from(rule.to_string()),
            source_id: RulePlanSourceId::from("src0".to_string()),
            span: RulePlanSpan {
                line,
                column: 1,
                end_line: line,
                end_column: 8,
            },
            comment: None,
            metadata: BTreeMap::new(),
        }
    }

    fn sample_plan() -> RulePlan {
        RulePlan {
            sources: vec![RulePlanSource {
                id: RulePlanSourceId::from("src0".to_string()),
                path: camino::Utf8PathBuf::from("rules/sample.edn"),
            }],
            nodes: vec![
                RulePlanNode {
                    id: RulePlanNodeId::from("n_group".to_string()),
                    kind: RulePlanNodeKind::Group,
                    label: "build".to_string(),
                    provenance: provenance("rule.group", 1),
                },
                RulePlanNode {
                    id: RulePlanNodeId::from("n_action".to_string()),
                    kind: RulePlanNodeKind::Action,
                    label: "compile".to_string(),
                    provenance: provenance("rule.action", 2),
                },
                RulePlanNode {
                    id: RulePlanNodeId::from("n_artifact".to_string()),
                    kind: RulePlanNodeKind::Artifact,
                    label: "target/app".to_string(),
                    provenance: provenance("rule.artifact", 3),
                },
                RulePlanNode {
                    id: RulePlanNodeId::from("n_fact".to_string()),
                    kind: RulePlanNodeKind::Fact,
                    label: "src-changed".to_string(),
                    provenance: provenance("rule.fact", 4),
                },
            ],
            edges: vec![
                RulePlanEdge {
                    from: RulePlanNodeId::from("n_group".to_string()),
                    to: RulePlanNodeId::from("n_action".to_string()),
                    kind: RulePlanEdgeKind::Always,
                    provenance: provenance("rule.edge.always", 10),
                },
                RulePlanEdge {
                    from: RulePlanNodeId::from("n_action".to_string()),
                    to: RulePlanNodeId::from("n_artifact".to_string()),
                    kind: RulePlanEdgeKind::IfWritten,
                    provenance: provenance("rule.edge.ifwritten", 11),
                },
                RulePlanEdge {
                    from: RulePlanNodeId::from("n_fact".to_string()),
                    to: RulePlanNodeId::from("n_action".to_string()),
                    kind: RulePlanEdgeKind::IfChanged,
                    provenance: provenance("rule.edge.ifchanged", 12),
                },
            ],
            facts: vec![RulePlanFact {
                id: RulePlanFactId::from("f0".to_string()),
                node_id: RulePlanNodeId::from("n_fact".to_string()),
                value: RulePlanValue::from("true".to_string()),
                provenance: provenance("rule.fact.emit", 13),
            }],
        }
    }

    #[test]
    fn lowers_rule_plan_to_action_graph_with_edge_kinds() {
        let graph = ActionGraph::from_rule_plan(&sample_plan()).expect("lowering should pass");
        assert_eq!(graph.graph.node_count(), 4);
        assert_eq!(graph.graph.edge_count(), 3);

        let topology = graph.to_topology();
        let mut kinds = topology
            .edges
            .into_iter()
            .map(|edge| edge.kind.expect("edge kind"))
            .collect::<Vec<_>>();
        kinds.sort();
        assert_eq!(
            kinds,
            vec![
                "always".to_string(),
                "ifchanged".to_string(),
                "ifwritten".to_string()
            ]
        );
    }

    #[test]
    fn unsupported_rule_plan_edge_kind_returns_provenance_error() {
        let mut plan = sample_plan();
        plan.edges.push(RulePlanEdge {
            from: RulePlanNodeId::from("n_action".to_string()),
            to: RulePlanNodeId::from("n_artifact".to_string()),
            kind: RulePlanEdgeKind::IfDeleted,
            provenance: provenance("rule.edge.ifdeleted", 99),
        });

        let err = match ActionGraph::from_rule_plan(&plan) {
            Ok(_) => panic!("lowering should fail"),
            Err(err) => err,
        };
        assert!(err.message.contains("ifdeleted"));
        assert_eq!(err.rule_id, "rule.edge.ifdeleted");
        assert_eq!(err.source_id, "src0");
        assert_eq!(err.line, 99);
        assert_eq!(err.column, 1);
    }

    #[test]
    fn lowering_uses_legacy_node_id_metadata_for_report_node_ids() {
        let mut plan = sample_plan();
        let action_node = plan
            .nodes
            .iter_mut()
            .find(|node| node.id.as_str() == "n_action")
            .expect("action node");
        action_node.provenance.metadata.insert(
            "legacy_node_id".to_string(),
            "compile-bin:example".to_string(),
        );

        let graph = ActionGraph::from_rule_plan(&plan).expect("lowering should pass");
        let topology = graph.to_topology();
        assert!(
            topology
                .nodes
                .iter()
                .any(|node| node.node_id == "compile-bin:example")
        );
    }

    #[test]
    fn lowering_registry_metadata_emits_real_registry_action_node() {
        let mut plan = sample_plan();
        let artifact_node = plan
            .nodes
            .iter_mut()
            .find(|node| node.id.as_str() == "n_artifact")
            .expect("artifact node");
        artifact_node
            .provenance
            .metadata
            .insert("registry_name".to_string(), "serde".to_string());
        artifact_node
            .provenance
            .metadata
            .insert("registry_version".to_string(), "1.0.0".to_string());
        artifact_node
            .provenance
            .metadata
            .insert("registry_checksum".to_string(), "abc123".to_string());

        let graph = ActionGraph::from_rule_plan(&plan).expect("lowering should pass");
        let topology = graph.to_topology();
        assert!(topology.nodes.iter().any(|node| {
            node.node_id == "acquire-registry:serde:1.0.0" && node.kind == "registry.acquire"
        }));
    }

    #[test]
    fn lowering_compile_metadata_emits_planner_compile_action_node() {
        let mut plan = sample_plan();
        let action_node = plan
            .nodes
            .iter_mut()
            .find(|node| node.id.as_str() == "n_action")
            .expect("action node");
        action_node
            .provenance
            .metadata
            .insert("crate_name".to_string(), "app".to_string());
        action_node
            .provenance
            .metadata
            .insert("crate_type".to_string(), "bin".to_string());
        action_node
            .provenance
            .metadata
            .insert("legacy_node_id".to_string(), "compile-bin:app".to_string());

        let graph = ActionGraph::from_rule_plan(&plan).expect("lowering should pass");
        let topology = graph.to_topology();
        assert!(
            topology
                .nodes
                .iter()
                .any(|node| node.node_id == "compile-bin:app" && node.kind == "rust.compile_bin")
        );
    }

    #[test]
    fn compile_lowering_missing_crate_type_returns_provenance_error() {
        let mut plan = sample_plan();
        let action_node = plan
            .nodes
            .iter_mut()
            .find(|node| node.id.as_str() == "n_action")
            .expect("action node");
        action_node
            .provenance
            .metadata
            .insert("crate_name".to_string(), "app".to_string());

        let err = match ActionGraph::from_rule_plan(&plan) {
            Ok(_) => panic!("lowering should fail"),
            Err(err) => err,
        };
        assert!(err.message.contains("crate_type"));
        assert_eq!(err.rule_id, "rule.action");
        assert_eq!(err.source_id, "src0");
        assert_eq!(err.line, 2);
        assert_eq!(err.column, 1);
    }

    #[test]
    fn compile_lowering_invalid_crate_type_returns_provenance_error() {
        let mut plan = sample_plan();
        let action_node = plan
            .nodes
            .iter_mut()
            .find(|node| node.id.as_str() == "n_action")
            .expect("action node");
        action_node
            .provenance
            .metadata
            .insert("crate_name".to_string(), "app".to_string());
        action_node
            .provenance
            .metadata
            .insert("crate_type".to_string(), "proc-macro".to_string());

        let err = match ActionGraph::from_rule_plan(&plan) {
            Ok(_) => panic!("lowering should fail"),
            Err(err) => err,
        };
        assert!(err.message.contains("invalid crate_type"));
        assert_eq!(err.rule_id, "rule.action");
        assert_eq!(err.source_id, "src0");
        assert_eq!(err.line, 2);
        assert_eq!(err.column, 1);
    }

    #[test]
    fn registry_lowering_missing_checksum_returns_provenance_error() {
        let mut plan = sample_plan();
        let artifact_node = plan
            .nodes
            .iter_mut()
            .find(|node| node.id.as_str() == "n_artifact")
            .expect("artifact node");
        artifact_node
            .provenance
            .metadata
            .insert("registry_name".to_string(), "serde".to_string());
        artifact_node
            .provenance
            .metadata
            .insert("registry_version".to_string(), "1.0.0".to_string());

        let err = match ActionGraph::from_rule_plan(&plan) {
            Ok(_) => panic!("lowering should fail"),
            Err(err) => err,
        };
        assert!(err.message.contains("registry_checksum"));
        assert_eq!(err.rule_id, "rule.artifact");
        assert_eq!(err.source_id, "src0");
        assert_eq!(err.line, 3);
        assert_eq!(err.column, 1);
    }
}
