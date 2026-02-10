//! Action graph for declarative build execution
//!
//! This module defines the action-based execution model that replaces the
//! imperative sequential loop. Actions are nodes in a dependency graph,
//! and the executor schedules them for optimal parallelism.

use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::HashMap;
use vx_rs::crate_graph::{CrateGraph, CrateId, CrateSource, CrateType};
use vx_store_proto::{Blake3Hash, RustChannel, RustComponent, RustToolchainSpec};

use crate::inputs::{BuildConfig, RustCrate, RustToolchain};

/// Dependency with source information (for building RustDep at execution time)
#[derive(Debug, Clone)]
pub struct ActionDep {
    /// The extern crate name
    pub extern_name: String,
    /// CrateId of the dependency
    pub crate_id: CrateId,
    /// Source of the dependency (to determine if registry_crate_manifest is needed)
    pub source: CrateSource,
}

/// A single unit of work in the build process
#[derive(Debug, Clone)]
#[allow(dead_code)]
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

    /// Compile a Rust crate (lib or bin)
    CompileRustCrate {
        crate_id: CrateId,
        crate_name: String,
        crate_type: CrateType,
        /// Rust edition
        edition: String,
        /// Crate root path (workspace-relative)
        crate_root_rel: String,
        /// Source type (Path or Registry)
        source: CrateSource,
        /// Dependencies this crate needs (with source info for RustDep construction)
        deps: Vec<ActionDep>,
        /// Workspace root path
        workspace_root: String,
        /// Target triple
        target_triple: String,
        /// Build profile
        profile: String,
        /// Picante input for this crate (for cache key computation)
        rust_crate: RustCrate,
        /// Toolchain to use (for cache key computation)
        toolchain: RustToolchain,
        /// Build configuration (for cache key computation)
        config: BuildConfig,
    },

    /// Compile a C source file to an object file
    CompileCObject {
        /// Source file path (workspace-relative)
        source: String,
        /// Output object file path (workspace-relative, usually .vx/obj/...)
        output: String,
        /// Source tree manifest hash in CAS
        source_manifest: Blake3Hash,
        /// Target triple
        target_triple: String,
        /// Build profile (debug/release)
        profile: String,
    },

    /// Link C object files into a binary
    LinkCBinary {
        /// Binary name
        name: String,
        /// Object files to link (workspace-relative)
        objects: Vec<String>,
        /// Output binary path (workspace-relative)
        output: String,
        /// Target triple
        target_triple: String,
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
            Action::CompileRustCrate { crate_name, .. } => {
                format!("compile {}", crate_name)
            }
            Action::CompileCObject { source, .. } => {
                format!("compile {}", source)
            }
            Action::LinkCBinary { name, .. } => {
                format!("link {}", name)
            }
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
            Action::CompileRustCrate { crate_name, .. } => {
                vxd_proto::ActionType::CompileRust(crate_name.clone())
            }
            Action::CompileCObject { source, .. } => {
                vxd_proto::ActionType::CompileC(source.clone())
            }
            Action::LinkCBinary { name, .. } => vxd_proto::ActionType::LinkC(name.clone()),
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
#[allow(dead_code)]
pub struct ActionGraph {
    /// Directed graph: edges point from dependent → dependency
    /// (i.e., if A depends on B, there's an edge A → B)
    pub graph: DiGraph<ActionNode, ()>,

    /// Map from CrateId to its action node (for resolving dependencies)
    pub crate_to_node: HashMap<CrateId, NodeIndex>,

    /// Rust toolchain extract nodes by component.
    pub rust_toolchain_extract_nodes: HashMap<RustComponent, NodeIndex>,

    /// Rust toolchain spec used for finalization during compile.
    pub rust_toolchain_spec: Option<RustToolchainSpec>,

    /// Map from (name, version) to registry crate acquisition node
    pub registry_nodes: HashMap<(String, String), NodeIndex>,
}

impl ActionGraph {
    /// Create an empty action graph (for manual construction, e.g., C projects)
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            crate_to_node: HashMap::new(),
            rust_toolchain_extract_nodes: HashMap::new(),
            rust_toolchain_spec: None,
            registry_nodes: HashMap::new(),
        }
    }

    /// Add an action to the graph and return its node index
    pub fn add_action(&mut self, action: Action) -> NodeIndex {
        self.graph.add_node(ActionNode { action })
    }

    /// Add a dependency edge from dependent to dependency
    /// (i.e., dependent depends on dependency, so dependency must execute first)
    pub fn add_edge(&mut self, dependency: NodeIndex, dependent: NodeIndex) {
        self.graph.add_edge(dependent, dependency, ());
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
        graph.add_edge(extract_rustc_node, download_rustc_node, ());
        graph.add_edge(extract_rust_std_node, download_rust_std_node, ());

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
        for (i, crate_node) in crate_graph.nodes.values().enumerate() {
            // Create RustCrate input
            let crate_id_hex = crate_node.id;
            let rust_crate = RustCrate::new(
                db,
                crate_id_hex.to_string(),
                crate_node.crate_name.clone(),
                crate_node.edition.as_str().to_string(),
                crate_node.crate_type.as_str().to_string(),
                crate_node.crate_root_rel.to_string(),
                Blake3Hash([0u8; 32]), // Placeholder, will be filled during execution
            )
            .map_err(|e| crate::error::VxdError::Picante(e.to_string()))?;

            // Build ActionDep list with source information
            let action_deps: Vec<ActionDep> = crate_node
                .deps
                .iter()
                .map(|dep| {
                    let dep_node = &crate_graph.nodes[&dep.crate_id];
                    ActionDep {
                        extern_name: dep.extern_name.clone(),
                        crate_id: dep.crate_id,
                        source: dep_node.source.clone(),
                    }
                })
                .collect();

            let target_triple_str = toolchain
                .target(db)
                .map_err(|e| crate::error::VxdError::Picante(e.to_string()))?;
            let profile_str = config
                .profile(db)
                .map_err(|e| crate::error::VxdError::Picante(e.to_string()))?;

            let action = Action::CompileRustCrate {
                crate_id: crate_node.id,
                crate_name: crate_node.crate_name.clone(),
                crate_type: crate_node.crate_type,
                edition: crate_node.edition.as_str().to_string(),
                crate_root_rel: crate_node.crate_root_rel.to_string(),
                source: crate_node.source.clone(),
                deps: action_deps,
                workspace_root: crate_graph.workspace_root.to_string(),
                target_triple: target_triple_str,
                profile: profile_str,
                rust_crate,
                toolchain,
                config,
            };

            let node_idx = graph.add_node(ActionNode { action });
            crate_to_node.insert(crate_node.id, node_idx);

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

            // Every compilation depends on both extracted toolchain components.
            graph.add_edge(compile_idx, extract_rustc_node, ());
            graph.add_edge(compile_idx, extract_rust_std_node, ());

            // Registry crates also depend on their registry acquisition
            if let CrateSource::Registry { name, version, .. } = &crate_node.source
                && let Some(&registry_idx) = registry_nodes.get(&(name.clone(), version.clone()))
            {
                graph.add_edge(compile_idx, registry_idx, ());
            }

            // Add edges to crate dependencies
            for dep in &crate_node.deps {
                if let Some(&dep_compile_idx) = crate_to_node.get(&dep.crate_id) {
                    graph.add_edge(compile_idx, dep_compile_idx, ());
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
            crate_to_node,
            rust_toolchain_extract_nodes: HashMap::from([
                (RustComponent::Rustc, extract_rustc_node),
                (RustComponent::RustStd, extract_rust_std_node),
            ]),
            rust_toolchain_spec: Some(RustToolchainSpec {
                channel: rust_channel,
                host: target_triple.clone(),
                target: target_triple,
                components: vec![RustComponent::Rustc, RustComponent::RustStd],
            }),
            registry_nodes,
        })
    }

    /// Get total number of actions
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
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
