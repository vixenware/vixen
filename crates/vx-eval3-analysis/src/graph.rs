use std::{collections::BTreeSet, fs, path::Path};

use crate::{
    AnalysisResult, BodyId, ExtractedNodeReason, RuntimeBoundaryKind, SingleSourceBridge,
    SourceLocalCalleeOrigin, build_minimal_analysis,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum GraphNodeKind {
    Body,
    Kernel,
    RuntimeBoundary,
    ClosureLayout,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GraphNode {
    pub id: String,
    pub kind: GraphNodeKind,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum GraphEdgeKind {
    BodyToKernel,
    BodyToRuntimeBoundary,
    CallToBody,
    CallToClosureLayout,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub kind: GraphEdgeKind,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisGraph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

impl AnalysisGraph {
    pub fn to_mermaid_flowchart(&self) -> String {
        let mut out = String::from("flowchart TD\n");
        for node in &self.nodes {
            out.push_str(&format!(
                "  {}[\"{}\"]\n",
                node.id,
                escape_mermaid(&node.label)
            ));
        }
        for edge in &self.edges {
            out.push_str(&format!(
                "  {} -->|{}| {}\n",
                edge.from,
                escape_mermaid(&edge.label),
                edge.to
            ));
        }
        out
    }
}

pub fn project_analysis_graph(analysis: &AnalysisResult) -> AnalysisGraph {
    let mut nodes = Vec::new();
    let mut edges = BTreeSet::new();

    for body in &analysis.bodies {
        nodes.push(GraphNode {
            id: body_node_id(body.id),
            kind: GraphNodeKind::Body,
            label: format!("Body B{}\\nresult={:?}", body.id.0, body.result_observation),
        });
    }

    for kernel in &analysis.kernels {
        nodes.push(GraphNode {
            id: kernel_node_id(kernel.id.0),
            kind: GraphNodeKind::Kernel,
            label: format!("Kernel K{}\\nresult={:?}", kernel.id.0, kernel.result_obs),
        });
    }

    for node in &analysis.runtime_nodes {
        let kind = node.boundary_kind();
        let label = match kind {
            Some(RuntimeBoundaryKind::Exec) => "Runtime Exec",
            Some(RuntimeBoundaryKind::Discover) => "Runtime Discover",
            Some(RuntimeBoundaryKind::SharedKeyed) => "Runtime SharedKeyed",
            None => "Runtime Lazy",
        };
        nodes.push(GraphNode {
            id: runtime_node_id(node.id().0),
            kind: GraphNodeKind::RuntimeBoundary,
            label: format!("{} R{}", label, node.id().0),
        });
    }

    for layout in &analysis.closure_layouts {
        nodes.push(GraphNode {
            id: layout_node_id(layout.id.0),
            kind: GraphNodeKind::ClosureLayout,
            label: format!(
                "Layout L{}\\nsource=B{}:E{}",
                layout.id.0, layout.source.body.0, layout.source.expr.0
            ),
        });
    }

    for body in &analysis.bodies {
        let from = body_node_id(body.id);
        for entry in &body.kernel_entries {
            edges.insert(GraphEdge {
                from: from.clone(),
                to: kernel_node_id(entry.kernel.0),
                kind: GraphEdgeKind::BodyToKernel,
                label: "body->kernel".to_owned(),
            });
        }

        for extracted in &body.extracted_nodes {
            if matches!(
                extracted.reason,
                ExtractedNodeReason::RuntimeBoundary { .. }
            ) {
                edges.insert(GraphEdge {
                    from: from.clone(),
                    to: runtime_node_id(extracted.id.0),
                    kind: GraphEdgeKind::BodyToRuntimeBoundary,
                    label: "body->runtime".to_owned(),
                });
            }
        }

        for call_edge in &body.source_local_call_edges {
            match call_edge.callee {
                SourceLocalCalleeOrigin::TopLevelBody(target) => {
                    edges.insert(GraphEdge {
                        from: from.clone(),
                        to: body_node_id(target),
                        kind: GraphEdgeKind::CallToBody,
                        label: format!("call E{} -> body", call_edge.call_expr.0),
                    });
                }
                SourceLocalCalleeOrigin::LocalClosure(_) => {
                    if let Some(layout_id) = body.callee_closure_layouts.get(&call_edge.call_expr) {
                        edges.insert(GraphEdge {
                            from: from.clone(),
                            to: layout_node_id(layout_id.0),
                            kind: GraphEdgeKind::CallToClosureLayout,
                            label: format!("call E{} -> layout", call_edge.call_expr.0),
                        });
                    }
                }
                SourceLocalCalleeOrigin::LocalDef(_) => {}
            }
        }
    }

    nodes.sort_by_key(|node| (node.kind.clone(), node.id.clone(), node.label.clone()));

    AnalysisGraph {
        nodes,
        edges: edges.into_iter().collect(),
    }
}

pub fn graph_from_source_text(text: &str) -> AnalysisGraph {
    let bridge = SingleSourceBridge::from_source_text(text);
    let analysis = build_minimal_analysis(&bridge);
    project_analysis_graph(&analysis)
}

pub fn graph_from_file(path: &Path) -> std::io::Result<AnalysisGraph> {
    let text = fs::read_to_string(path)?;
    Ok(graph_from_source_text(&text))
}

fn body_node_id(id: BodyId) -> String {
    format!("body_b{}", id.0)
}

fn kernel_node_id(id: u32) -> String {
    format!("kernel_k{}", id)
}

fn runtime_node_id(id: u32) -> String {
    format!("runtime_r{}", id)
}

fn layout_node_id(id: u32) -> String {
    format!("layout_l{}", id)
}

fn escape_mermaid(input: &str) -> String {
    input.replace('"', "\\\"")
}
