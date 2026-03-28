use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};

use arborium::advanced::{CompiledGrammar, GrammarConfig};
use arborium::{Config, GrammarStore, Highlighter, HtmlFormat};
use vx_resolve::SourceSpan;

use crate::{
    AnalysisResult, BodyId, BridgeCallSiteKind, BridgeLocalBindingId, ExprId, ExtractedNodeReason,
    RuntimeBoundaryKind, SingleSourceBridge, SourceExpr, SourceLocalCalleeOrigin,
    build_minimal_analysis,
};

pub type GraphMetadata = Vec<(String, String)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GraphNodeKind {
    Body,
    Kernel,
    RuntimeBoundary,
    ClosureLayout,
}

impl GraphNodeKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Body => "body",
            Self::Kernel => "kernel",
            Self::RuntimeBoundary => "runtime_boundary",
            Self::ClosureLayout => "closure_layout",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GraphNode {
    pub id: String,
    pub kind: GraphNodeKind,
    pub label: String,
    pub metadata: GraphMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GraphEdgeKind {
    BodyToKernel,
    BodyToRuntimeBoundary,
    CallToBody,
    CallToClosureLayout,
    RuntimeValueDependency,
    RuntimeBoundaryToClosureLayout,
}

impl GraphEdgeKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::BodyToKernel => "body_to_kernel",
            Self::BodyToRuntimeBoundary => "body_to_runtime_boundary",
            Self::CallToBody => "call_to_body",
            Self::CallToClosureLayout => "call_to_closure_layout",
            Self::RuntimeValueDependency => "runtime_value_dependency",
            Self::RuntimeBoundaryToClosureLayout => "runtime_boundary_to_closure_layout",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub kind: GraphEdgeKind,
    pub label: String,
    pub metadata: GraphMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisGraph {
    pub source_file: Option<String>,
    pub source_files: Vec<(u32, String)>,
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

    pub fn to_json_pretty(&self) -> String {
        let mut out = String::new();
        out.push_str("{\n");
        out.push_str("  \"graph_type\": \"structural_analysis_graph\",\n");
        out.push_str("  \"source_file\": ");
        match &self.source_file {
            Some(path) => push_json_str(&mut out, path),
            None => out.push_str("null"),
        }
        out.push_str(",\n");
        out.push_str("  \"source_files\": [\n");
        for (index, (source_id, path)) in self.source_files.iter().enumerate() {
            out.push_str("    { \"source_id\": ");
            out.push_str(&source_id.to_string());
            out.push_str(", \"path\": ");
            push_json_str(&mut out, path);
            out.push_str(" }");
            if index + 1 != self.source_files.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ],\n");
        out.push_str("  \"nodes\": [\n");
        for (index, node) in self.nodes.iter().enumerate() {
            out.push_str("    {\n");
            out.push_str("      \"id\": ");
            push_json_str(&mut out, &node.id);
            out.push_str(",\n");
            out.push_str("      \"kind\": ");
            push_json_str(&mut out, node.kind.as_str());
            out.push_str(",\n");
            out.push_str("      \"label\": ");
            push_json_str(&mut out, &node.label);
            out.push_str(",\n");
            out.push_str("      \"metadata\": ");
            push_json_metadata(&mut out, &node.metadata, 6);
            out.push_str("\n    }");
            if index + 1 != self.nodes.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ],\n");
        out.push_str("  \"edges\": [\n");
        for (index, edge) in self.edges.iter().enumerate() {
            out.push_str("    {\n");
            out.push_str("      \"from\": ");
            push_json_str(&mut out, &edge.from);
            out.push_str(",\n");
            out.push_str("      \"to\": ");
            push_json_str(&mut out, &edge.to);
            out.push_str(",\n");
            out.push_str("      \"kind\": ");
            push_json_str(&mut out, edge.kind.as_str());
            out.push_str(",\n");
            out.push_str("      \"label\": ");
            push_json_str(&mut out, &edge.label);
            out.push_str(",\n");
            out.push_str("      \"metadata\": ");
            push_json_metadata(&mut out, &edge.metadata, 6);
            out.push_str("\n    }");
            if index + 1 != self.edges.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ]\n");
        out.push_str("}\n");
        out
    }

    pub fn to_standalone_html(&self) -> String {
        let graph_json = self.to_json_pretty().replace("</", "<\\/");
        let source_label = self
            .source_file
            .as_deref()
            .unwrap_or("<inline-source>")
            .to_owned();
        let mut out = String::new();
        out.push_str("<!doctype html>\n");
        out.push_str("<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\" />\n");
        out.push_str(
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n",
        );
        out.push_str("<title>Vixen Structural Analysis Graph</title>\n");
        out.push_str("<style>\n");
        out.push_str("  :root { color-scheme: dark; }\n");
        out.push_str("  body { margin: 0; font-family: ui-sans-serif, system-ui, -apple-system, Segoe UI, sans-serif; background: #0f1724; color: #e2e8f0; }\n");
        out.push_str("  .header { padding: 16px 20px; border-bottom: 1px solid #243244; background: linear-gradient(90deg, #162233, #111b2a); }\n");
        out.push_str("  .title { margin: 0 0 6px 0; font-size: 18px; color: #f1f5f9; }\n");
        out.push_str("  .sub { margin: 0; color: #93adc8; font-size: 13px; }\n");
        out.push_str("  .layout { display: grid; grid-template-columns: minmax(0, 1fr) 380px; min-height: calc(100vh - 84px); align-items: start; }\n");
        out.push_str("  .canvas-wrap { padding: 10px; overflow: auto; min-width: 0; }\n");
        out.push_str("  .viewport-toolbar { display: flex; align-items: center; gap: 8px; margin: 0 10px 6px 10px; }\n");
        out.push_str("  .viewport-toolbar button { background: #162233; color: #dbe7f3; border: 1px solid #2a3c52; border-radius: 6px; padding: 4px 8px; font-size: 12px; cursor: pointer; }\n");
        out.push_str("  .viewport-toolbar button:hover { border-color: #4b647f; }\n");
        out.push_str("  .viewport-toolbar .zoom-status { color: #8ba5c1; font-size: 12px; min-width: 54px; text-align: right; }\n");
        out.push_str("  .graph-viewport { overflow: auto; border: 1px solid #243244; border-radius: 10px; background: #0b1420; }\n");
        out.push_str(
            "  .panel { position: sticky; top: 0; align-self: start; max-height: 100vh; overflow: auto; border-left: 1px solid #243244; background: #121d2b; padding: 12px; }\n",
        );
        out.push_str("  .panel h2 { margin: 0 0 8px 0; font-size: 14px; color: #dbe7f3; }\n");
        out.push_str("  .detail-card { border: 1px solid #2a3c52; border-radius: 10px; background: #0f1724; padding: 10px; margin-bottom: 10px; }\n");
        out.push_str("  .detail-title { margin: 0 0 8px 0; font-size: 13px; font-weight: 650; color: #dbe7f3; }\n");
        out.push_str("  .detail-card ul { margin: 6px 0 0 18px; padding: 0; }\n");
        out.push_str("  .detail-card li { margin: 4px 0; color: #c7d6e6; }\n");
        out.push_str(
            "  .dep-item { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }\n",
        );
        out.push_str("  .dep-jump { background: #162233; color: #dbe7f3; border: 1px solid #2a3c52; border-radius: 5px; padding: 2px 6px; font-size: 11px; cursor: pointer; }\n");
        out.push_str("  .detail-row { display: grid; grid-template-columns: 120px 1fr; gap: 8px; margin-bottom: 6px; font-size: 12px; }\n");
        out.push_str("  .detail-row:last-child { margin-bottom: 0; }\n");
        out.push_str("  .detail-key { color: #8ba5c1; }\n");
        out.push_str("  .detail-val { color: #e2e8f0; word-break: break-word; }\n");
        out.push_str("  .snippet { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; background: #0b1420; color: #dbe7f3; border: 1px solid #2a3c52; border-radius: 8px; padding: 8px; margin-top: 6px; }\n");
        out.push_str("  .snippet-line { display: grid; grid-template-columns: 2.4em 1fr; gap: 10px; margin-bottom: 2px; }\n");
        out.push_str("  .snippet-line:last-child { margin-bottom: 0; }\n");
        out.push_str("  .snippet-ln { color: #647b95; text-align: right; user-select: none; }\n");
        out.push_str(
            "  .snippet-code { color: #dbe7f3; white-space: pre-wrap; word-break: break-word; }\n",
        );
        out.push_str("  .source-registry { border: 1px solid #2a3c52; border-radius: 10px; background: #0f1724; padding: 10px; margin-bottom: 12px; font-size: 12px; }\n");
        out.push_str("  .source-registry ul { margin: 8px 0 0 0; padding-left: 18px; }\n");
        out.push_str(
            "  .source-registry li { margin: 4px 0; word-break: break-word; color: #c7d6e6; cursor: pointer; }\n",
        );
        out.push_str("  .source-registry li.active { color: #7dd3fc; font-weight: 600; }\n");
        out.push_str("  .source-registry li.focused { color: #a5f3fc; font-weight: 700; }\n");
        out.push_str("  .source-registry li.folded { opacity: 0.65; font-style: italic; }\n");
        out.push_str("  .lane-summary { border: 1px solid #2a3c52; border-radius: 10px; background: #0f1724; padding: 10px; margin-bottom: 12px; font-size: 12px; }\n");
        out.push_str(
            "  .lane-summary ul { margin: 8px 0 0 0; padding-left: 0; list-style: none; }\n",
        );
        out.push_str("  .lane-summary li { margin: 6px 0; color: #c7d6e6; border: 1px solid #1f2f43; border-radius: 8px; padding: 6px; }\n");
        out.push_str("  .lane-summary li.folded { opacity: 0.7; border-style: dashed; }\n");
        out.push_str("  .lane-summary .lane-head { display: flex; gap: 6px; align-items: center; justify-content: space-between; }\n");
        out.push_str("  .lane-summary .lane-label { font-weight: 600; color: #dbe7f3; }\n");
        out.push_str(
            "  .lane-summary .lane-meta { color: #8ba5c1; font-size: 11px; margin-top: 2px; }\n",
        );
        out.push_str(
            "  .lane-summary .lane-hint { color: #7dd3fc; font-size: 11px; margin-top: 2px; }\n",
        );
        out.push_str("  .lane-summary .lane-actions { display: flex; gap: 6px; }\n");
        out.push_str(
            "  .lane-summary .lane-tools { display: flex; gap: 8px; margin-bottom: 8px; }\n",
        );
        out.push_str("  .lane-summary .lane-tools button { background: #162233; color: #dbe7f3; border: 1px solid #2a3c52; border-radius: 5px; padding: 2px 6px; font-size: 11px; cursor: pointer; }\n");
        out.push_str("  .lane-summary button { background: #162233; color: #dbe7f3; border: 1px solid #2a3c52; border-radius: 5px; padding: 2px 6px; font-size: 11px; cursor: pointer; }\n");
        out.push_str("  .search-controls { border: 1px solid #2a3c52; border-radius: 10px; background: #0f1724; padding: 10px; margin-bottom: 12px; font-size: 12px; }\n");
        out.push_str("  .search-controls .search-row { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 8px; }\n");
        out.push_str("  .search-controls input { background: #132031; color: #dbe7f3; border: 1px solid #2a3c52; border-radius: 6px; padding: 5px 7px; font-size: 12px; }\n");
        out.push_str("  .search-controls button { background: #162233; color: #dbe7f3; border: 1px solid #2a3c52; border-radius: 6px; padding: 4px 8px; font-size: 12px; cursor: pointer; }\n");
        out.push_str("  .search-controls .search-status { margin-top: 6px; color: #8ba5c1; font-size: 11px; }\n");
        out.push_str("  .search-controls ul { margin: 8px 0 0 0; padding-left: 0; list-style: none; max-height: 180px; overflow: auto; }\n");
        out.push_str("  .search-controls li { margin: 4px 0; }\n");
        out.push_str("  .search-controls .search-hit { width: 100%; text-align: left; background: #132031; color: #dbe7f3; border: 1px solid #2a3c52; border-radius: 7px; padding: 5px 7px; cursor: pointer; }\n");
        out.push_str("  .search-controls .search-hit.active { border-color: #7dd3fc; box-shadow: 0 0 0 1px rgba(125,211,252,0.24); }\n");
        out.push_str(
            "  .search-controls .search-hit .kind { color: #8ba5c1; margin-right: 6px; }\n",
        );
        out.push_str("  .search-controls .search-hit .path { color: #8ba5c1; display: block; margin-top: 2px; font-size: 11px; }\n");
        out.push_str("  .semantic-controls { border: 1px solid #2a3c52; border-radius: 10px; background: #0f1724; padding: 10px; margin-bottom: 12px; font-size: 12px; }\n");
        out.push_str("  .semantic-controls .control-row { display: grid; grid-template-columns: 92px 1fr; gap: 8px; margin-bottom: 8px; align-items: center; }\n");
        out.push_str("  .semantic-controls .control-row:last-child { margin-bottom: 0; }\n");
        out.push_str("  .semantic-controls label { color: #8ba5c1; }\n");
        out.push_str("  .semantic-controls select { background: #132031; color: #dbe7f3; border: 1px solid #2a3c52; border-radius: 6px; padding: 4px 6px; font-size: 12px; }\n");
        out.push_str("  .semantic-controls .check { display: flex; align-items: center; gap: 8px; color: #c7d6e6; }\n");
        out.push_str("  .semantic-controls input[type=\"checkbox\"] { accent-color: #7dd3fc; }\n");
        out.push_str("  .selection-nav { display: flex; gap: 8px; margin-bottom: 10px; }\n");
        out.push_str("  .selection-nav button { background: #162233; color: #dbe7f3; border: 1px solid #2a3c52; border-radius: 6px; padding: 4px 8px; font-size: 12px; cursor: pointer; }\n");
        out.push_str("  .selection-nav button:disabled { opacity: 0.4; cursor: default; }\n");
        out.push_str("  .selection-nav .share-status { color: #8ba5c1; font-size: 11px; align-self: center; }\n");
        out.push_str("  details.raw-json { margin-top: 8px; }\n");
        out.push_str(
            "  details.raw-json > summary { cursor: pointer; font-size: 12px; color: #7dd3fc; }\n",
        );
        out.push_str("  .raw-json-pre { margin-top: 8px; white-space: pre-wrap; word-break: break-word; font-size: 11px; line-height: 1.35; background: #0b1420; color: #c7d6e6; border: 1px solid #2a3c52; border-radius: 8px; padding: 8px; }\n");
        out.push_str("  .legend { margin: 8px 0 0 0; font-size: 12px; color: #8ba5c1; }\n");
        out.push_str("  .graph-legend { margin: 10px; border: 1px solid #2a3c52; border-radius: 10px; background: #0f1724; padding: 10px; font-size: 12px; color: #c7d6e6; }\n");
        out.push_str(
            "  .graph-legend h3 { margin: 0 0 8px 0; font-size: 13px; color: #dbe7f3; }\n",
        );
        out.push_str(
            "  .legend-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }\n",
        );
        out.push_str(
            "  .legend-item { display: flex; align-items: center; gap: 8px; margin: 4px 0; }\n",
        );
        out.push_str("  .legend-swatch { width: 12px; height: 12px; border-radius: 3px; border: 1px solid #2f435a; }\n");
        out.push_str("  .legend-line { width: 24px; height: 0; border-top: 2px solid #5f7d9a; border-radius: 2px; }\n");
        out.push_str("  .source-lane-band { fill: #0d1623; stroke: #243244; stroke-width: 1; }\n");
        out.push_str("  .source-lane-band.alt { fill: #101b2a; }\n");
        out.push_str("  .source-lane-label { font-size: 11px; fill: #8ba5c1; }\n");
        out.push_str("  .source-lane-band.folded-lane { opacity: 0.35; }\n");
        out.push_str("  .source-lane-label.folded-lane { opacity: 0.5; }\n");
        out.push_str(
            "  .node rect { fill: #162233; stroke: #4f6b87; stroke-width: 1.2; rx: 8; ry: 8; }\n",
        );
        out.push_str("  .node text { font-size: 12px; fill: #dbe7f3; pointer-events: none; }\n");
        out.push_str("  .node-badge rect { fill: #0b1420; stroke: #3a5067; stroke-width: 1; rx: 4; ry: 4; }\n");
        out.push_str("  .node-badge text { font-size: 9px; fill: #c7d6e6; font-weight: 700; pointer-events: none; }\n");
        out.push_str("  .edge { stroke: #5f7d9a; stroke-width: 1.3; fill: none; }\n");
        out.push_str("  .edge-label { font-size: 11px; fill: #b6c8db; pointer-events: none; }\n");
        out.push_str("  .edge-label-hit { pointer-events: auto; cursor: pointer; }\n");
        out.push_str("  .node-kind-body rect { fill: #1b293c; stroke: #7dd3fc; }\n");
        out.push_str("  .node-kind-kernel rect { fill: #222636; stroke: #f9e2af; }\n");
        out.push_str("  .node-kind-runtime_boundary rect { fill: #2a2236; stroke: #f38ba8; }\n");
        out.push_str("  .node-kind-closure_layout rect { fill: #1f2f2a; stroke: #a6e3a1; }\n");
        out.push_str("  .edge-kind-body_to_kernel { stroke: #f9e2af; }\n");
        out.push_str("  .edge-kind-body_to_runtime_boundary { stroke: #f38ba8; }\n");
        out.push_str(
            "  .edge-kind-runtime_value_dependency { stroke: #fab387; stroke-dasharray: 6 4; }\n",
        );
        out.push_str("  .edge-kind-runtime_boundary_to_closure_layout { stroke: #a6e3a1; stroke-dasharray: 3 3; }\n");
        out.push_str("  .edge-kind-call_to_body { stroke: #89b4fa; }\n");
        out.push_str("  .edge-kind-call_to_closure_layout { stroke: #94e2d5; }\n");
        out.push_str("  .node-demand-demanded rect { stroke-width: 1.8; }\n");
        out.push_str(
            "  .node-demand-demanded .node-badge-demand rect { fill: #19351c; stroke: #5fd277; }\n",
        );
        out.push_str("  .node-demand-nondemanded .node-badge-demand rect { fill: #33211f; stroke: #f38ba8; }\n");
        out.push_str(
            "  .node-sharing-shared .node-badge-sharing rect { fill: #1f2538; stroke: #89b4fa; }\n",
        );
        out.push_str("  .node-sharing-closure_env .node-badge-sharing rect { fill: #1f2f2a; stroke: #94e2d5; }\n");
        out.push_str("  .node-runtime-role-exec .node-badge-runtime rect { fill: #2f1f35; stroke: #f38ba8; }\n");
        out.push_str("  .node-runtime-role-discover .node-badge-runtime rect { fill: #1f2a38; stroke: #89b4fa; }\n");
        out.push_str("  .node-runtime-role-shared_keyed .node-badge-runtime rect { fill: #2d2c1f; stroke: #f9e2af; }\n");
        out.push_str("  .edge-role-value_flow { stroke-width: 1.9; }\n");
        out.push_str("  .edge-role-ownership { stroke-dasharray: 5 4; }\n");
        out.push_str("  .edge-role-call { stroke-dasharray: 2 3; }\n");
        out.push_str("  .edge-label.edge-role-value_flow { fill: #f9cb9c; }\n");
        out.push_str("  .edge-label.edge-role-ownership { fill: #a6e3a1; }\n");
        out.push_str("  .edge-label.edge-role-call { fill: #89dceb; }\n");
        out.push_str("  .dimmed { opacity: 0.18; }\n");
        out.push_str("  .dim-focus { opacity: 0.07; }\n");
        out.push_str("  .focus-neighbor { opacity: 0.93; }\n");
        out.push_str("  .focus-primary { opacity: 1; }\n");
        out.push_str("  .node.focus-neighbor rect { stroke-width: 1.9; }\n");
        out.push_str("  .edge.focus-neighbor { stroke-width: 1.9; }\n");
        out.push_str("  .node.focus-primary rect { stroke-width: 2.5; }\n");
        out.push_str("  .edge.focus-primary { stroke-width: 2.6; }\n");
        out.push_str("  .noise-dim { opacity: 0.12; }\n");
        out.push_str("  .hidden-semantic { display: none; }\n");
        out.push_str("  .hidden-folded-lane { display: none; }\n");
        out.push_str("  .selected { opacity: 1 !important; }\n");
        out.push_str("  .node.selected rect { stroke: #f1f5f9; stroke-width: 2.4; filter: drop-shadow(0 0 4px rgba(241,245,249,0.35)); }\n");
        out.push_str("  .edge.selected { stroke-width: 2.5; filter: drop-shadow(0 0 3px rgba(241,245,249,0.35)); }\n");
        out.push_str("  .edge-label.selected { fill: #f8fafc; font-weight: 700; }\n");
        out.push_str("  .source-lane-band.selected { stroke: #7dd3fc; stroke-width: 1.6; }\n");
        out.push_str(
            "  .semantic-summary { margin: 8px 0 0 0; font-size: 12px; color: #8ba5c1; }\n",
        );
        out.push_str("  .semantic-explain { margin: 6px 0 0 0; font-size: 12px; line-height: 1.4; color: #c7d6e6; }\n");
        out.push_str("  .arb-snippet { white-space: pre-wrap; word-break: break-word; }\n");
        out.push_str("  .arb-snippet .arb-keyword { color: #f38ba8; }\n");
        out.push_str("  .arb-snippet .arb-function { color: #89b4fa; }\n");
        out.push_str("  .arb-snippet .arb-string { color: #a6e3a1; }\n");
        out.push_str("  .arb-snippet .arb-comment { color: #6c7f95; }\n");
        out.push_str("  .arb-snippet .arb-type { color: #f9e2af; }\n");
        out.push_str("  .arb-snippet .arb-variable { color: #cdd6f4; }\n");
        out.push_str("  .arb-snippet .arb-constant { color: #fab387; }\n");
        out.push_str("  .arb-snippet .arb-number { color: #fab387; }\n");
        out.push_str("  .arb-snippet .arb-property { color: #94e2d5; }\n");
        out.push_str("  .arb-snippet .arb-operator { color: #89dceb; }\n");
        out.push_str("  .arb-snippet .arb-punctuation { color: #bac2de; }\n");
        out.push_str("  .arb-snippet .arb-tag { color: #f38ba8; }\n");
        out.push_str("  .arb-snippet .arb-attribute { color: #89b4fa; }\n");
        out.push_str("</style>\n</head>\n<body>\n");
        out.push_str("<div class=\"header\">\n");
        out.push_str("<h1 class=\"title\">Vixen Structural Analysis Graph</h1>\n");
        out.push_str("<p class=\"sub\">Source: ");
        out.push_str(&escape_html(&source_label));
        out.push_str("</p>\n");
        out.push_str(
            "<p class=\"legend\">This is a structural analysis graph (not a scheduler/execution graph).</p>\n",
        );
        out.push_str("</div>\n");
        out.push_str("<div class=\"layout\">\n");
        out.push_str("<div class=\"canvas-wrap\"><div class=\"graph-legend\" aria-label=\"graph legend\"><h3>Graph Legend</h3><div class=\"legend-grid\"><div><div class=\"legend-item\"><span class=\"legend-swatch\" style=\"background:#1b293c;border-color:#7dd3fc\"></span>Body</div><div class=\"legend-item\"><span class=\"legend-swatch\" style=\"background:#222636;border-color:#f9e2af\"></span>Kernel</div><div class=\"legend-item\"><span class=\"legend-swatch\" style=\"background:#2a2236;border-color:#f38ba8\"></span>Runtime Boundary</div><div class=\"legend-item\"><span class=\"legend-swatch\" style=\"background:#1f2f2a;border-color:#a6e3a1\"></span>Closure Layout</div></div><div><div class=\"legend-item\"><span class=\"legend-line\" style=\"border-top-color:#f9e2af\"></span>body -> kernel</div><div class=\"legend-item\"><span class=\"legend-line\" style=\"border-top-color:#f38ba8\"></span>body -> runtime</div><div class=\"legend-item\"><span class=\"legend-line\" style=\"border-top-color:#fab387;border-top-style:dashed\"></span>runtime value/data</div><div class=\"legend-item\"><span class=\"legend-line\" style=\"border-top-color:#a6e3a1;border-top-style:dotted\"></span>boundary -> closure layout</div><div class=\"legend-item\"><span class=\"legend-line\" style=\"border-top-color:#89b4fa\"></span>source-local call</div></div></div></div><div class=\"viewport-toolbar\" aria-label=\"graph viewport controls\"><button id=\"zoom-out\" type=\"button\">-</button><button id=\"zoom-in\" type=\"button\">+</button><button id=\"zoom-reset\" type=\"button\">reset</button><span id=\"zoom-status\" class=\"zoom-status\">100%</span></div><div id=\"graph-viewport\" class=\"graph-viewport\"><svg id=\"graph-svg\" width=\"1100\" height=\"800\" aria-label=\"structural graph\"></svg></div></div>\n");
        out.push_str("<aside class=\"panel\"><h2>Source Registry</h2><div id=\"source-registry\" class=\"source-registry\"></div><h2>Lane Summary</h2><div id=\"lane-summary\" class=\"lane-summary\"></div><h2>Search</h2><div id=\"search-controls\" class=\"search-controls\"><div class=\"search-row\"><input id=\"search-query\" type=\"search\" placeholder=\"Search nodes, edges, sources\" aria-label=\"Search graph\" /><button id=\"search-clear\" type=\"button\">clear</button></div><div id=\"search-status\" class=\"search-status\"></div><ul id=\"search-results\"></ul></div><h2>Semantic Filters</h2><div id=\"semantic-controls\" class=\"semantic-controls\"><div class=\"control-row\"><label for=\"filter-demand\">Demand</label><select id=\"filter-demand\"><option value=\"all\">all</option><option value=\"demanded\">demanded</option><option value=\"nondemanded\">nondemanded</option></select></div><div class=\"control-row\"><label for=\"filter-runtime-role\">Runtime</label><select id=\"filter-runtime-role\"><option value=\"all\">all</option><option value=\"exec\">exec</option><option value=\"discover\">discover</option><option value=\"shared_keyed\">shared-keyed</option><option value=\"none\">none</option></select></div><div class=\"control-row\"><label for=\"filter-sharing\">Sharing</label><select id=\"filter-sharing\"><option value=\"all\">all</option><option value=\"shared\">shared</option><option value=\"closure_env\">closure-env</option><option value=\"local\">local</option></select></div><div class=\"control-row\"><label>Focus</label><label class=\"check\"><input id=\"focus-neighborhood-toggle\" type=\"checkbox\" />dependency neighborhood</label></div><div class=\"control-row\"><label for=\"focus-path-mode\">Trace Mode</label><select id=\"focus-path-mode\"><option value=\"neighborhood\">immediate</option><option value=\"upstream\">upstream path</option><option value=\"downstream\">downstream path</option></select></div><div class=\"control-row\"><label>Noise</label><label class=\"check\"><input id=\"hide-stdlib-noise-toggle\" type=\"checkbox\" />dim stdlib-heavy sources</label></div></div><h2>Selected Item</h2><div class=\"selection-nav\"><button id=\"selection-history-back\" type=\"button\">back</button><button id=\"selection-history-forward\" type=\"button\">forward</button><button id=\"copy-share-link\" type=\"button\" title=\"Copy current focused state link\">copy link</button><span id=\"share-link-status\" class=\"share-status\"></span></div><div id=\"details\"></div></aside>\n");
        out.push_str("</div>\n");
        out.push_str("<script id=\"graph-json\" type=\"application/json\">");
        out.push_str(&graph_json);
        out.push_str("</script>\n");
        out.push_str(
            r##"<script>
const graph = JSON.parse(document.getElementById("graph-json").textContent);
const svg = document.getElementById("graph-svg");
const graphViewport = document.getElementById("graph-viewport");
const zoomInButton = document.getElementById("zoom-in");
const zoomOutButton = document.getElementById("zoom-out");
const zoomResetButton = document.getElementById("zoom-reset");
const zoomStatus = document.getElementById("zoom-status");
const filterDemand = document.getElementById("filter-demand");
const filterRuntimeRole = document.getElementById("filter-runtime-role");
const filterSharing = document.getElementById("filter-sharing");
const focusNeighborhoodToggle = document.getElementById("focus-neighborhood-toggle");
const focusPathMode = document.getElementById("focus-path-mode");
const hideStdlibNoiseToggle = document.getElementById("hide-stdlib-noise-toggle");
const selectionHistoryBack = document.getElementById("selection-history-back");
const selectionHistoryForward = document.getElementById("selection-history-forward");
const copyShareLinkButton = document.getElementById("copy-share-link");
const shareLinkStatus = document.getElementById("share-link-status");
const searchQueryInput = document.getElementById("search-query");
const searchClearButton = document.getElementById("search-clear");
const searchStatus = document.getElementById("search-status");
const searchResults = document.getElementById("search-results");
const details = document.getElementById("details");
const sourceRegistry = document.getElementById("source-registry");
const laneSummary = document.getElementById("lane-summary");
let sourceFilterSourceId = "";
const sourceBoundElements = [];
const selectableElements = new Map();
const laneRectsBySource = new Map();
const laneLabelsBySource = new Map();
const sourceRegistryItemsBySource = new Map();
const laneSummaryItemsBySource = new Map();
const nodeElementsById = new Map();
const edgeElements = [];
let selectedGraphKey = "";
let selectedSourceId = "";
let viewportScale = 1;
let semanticDemandFilter = "all";
let semanticRuntimeRoleFilter = "all";
let semanticSharingFilter = "all";
let dependencyFocusEnabled = false;
let hideStdlibNoiseEnabled = false;
let dependencyFocusMode = "neighborhood";
let searchQuery = "";
let activeSearchResultIndex = -1;
let visibleSearchResults = [];
const foldedLaneSourceIds = new Set();
const selectionHistory = [];
let selectionHistoryIndex = -1;
let suppressUrlStateUpdate = false;
let dependencyJumpIndex = -1;

const nodeWidth = 196;
const nodeHeight = 54;
const yStart = 64;
const nodeSpacing = 72;
const denseGraph = graph.nodes.length >= 48;
const kindOrder = ["body", "kernel", "runtime_boundary", "closure_layout"];
const columns = {
  body: 120,
  kernel: 400,
  runtime_boundary: 680,
  closure_layout: 960,
};
const nodeById = new Map(graph.nodes.map((node) => [node.id, node]));

function sourceIdForNode(node) {
  const sourceId = node?.metadata?.span_source_id;
  return sourceId === undefined || sourceId === null || String(sourceId) === "" ? "unmapped" : String(sourceId);
}
function nodeSortKey(node) {
  return `${String(node.kind)}|${String(sourceIdForNode(node))}|${String(node.id)}`;
}
function sourcePathById(sourceId) {
  if (!Array.isArray(graph.source_files)) return "";
  const item = graph.source_files.find((f) => String(f.source_id) === String(sourceId));
  return item?.path ? String(item.path) : "";
}
function isStdlibSourceId(sourceId) {
  const path = String(sourcePathById(sourceId) || "").toLowerCase();
  return path.includes("/stdlib/") || path.includes("stdlib/") || path.includes("vixen-stdlib");
}
function sourceLabel(sourceId) {
  if (sourceId === "unmapped") return "unmapped";
  const path = sourcePathById(sourceId);
  if (!path) return `source ${sourceId}`;
  const slash = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return slash >= 0 ? path.slice(slash + 1) : path;
}
function sourceRank(sourceId, laneOrder) {
  const idx = laneOrder.indexOf(String(sourceId));
  return idx === -1 ? laneOrder.length + 1 : idx;
}
function edgeLinkCountByEndpoints(edges) {
  const counts = new Map();
  for (const edge of edges) {
    const key = `${edge.from}|${edge.to}`;
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return counts;
}
function hashDeterministic(value) {
  let h = 0;
  for (let i = 0; i < value.length; i++) {
    h = (h * 131 + value.charCodeAt(i)) >>> 0;
  }
  return h;
}
function metadataMap(entries) {
  if (Array.isArray(entries)) return Object.fromEntries(entries);
  if (entries && typeof entries === "object") return entries;
  return {};
}
function parsePositiveInt(value) {
  const n = Number.parseInt(String(value ?? ""), 10);
  return Number.isFinite(n) && n > 0 ? n : 0;
}
function observationToDemandState(raw) {
  const text = String(raw ?? "");
  if (text.includes("Need")) return "demanded";
  if (text.includes("NoObs")) return "nondemanded";
  return "unknown";
}
function nodeDemandState(node) {
  const metadata = metadataMap(node.metadata);
  if (node.kind === "body") return observationToDemandState(metadata.result_observation);
  if (node.kind === "kernel") return observationToDemandState(metadata.kernel_result_observation);
  if (node.kind === "closure_layout") return observationToDemandState(metadata.incoming_use_observation);
  return "unknown";
}
function nodeSharingState(node) {
  const metadata = metadataMap(node.metadata);
  const role = String(metadata.sharing_role ?? "");
  if (role === "shared" || role === "shared_reuse_source") return "shared";
  if (role === "closure_env") return "closure_env";
  if (parsePositiveInt(metadata.shared_extracted_node_count) > 0) return "shared";
  return "local";
}
function nodeRuntimeRole(node) {
  if (node.kind !== "runtime_boundary") return "none";
  const metadata = metadataMap(node.metadata);
  const role = String(metadata.runtime_role ?? "");
  return role || "none";
}
function edgeRoleForKind(kind) {
  if (kind === "runtime_value_dependency") return "value_flow";
  if (kind === "runtime_boundary_to_closure_layout") return "ownership";
  if (kind === "call_to_body" || kind === "call_to_closure_layout") return "call";
  if (kind === "body_to_kernel" || kind === "body_to_runtime_boundary") return "structural";
  return "structural";
}
function edgeSemanticLabel(edge) {
  const role = edgeRoleForKind(edge.kind);
  if (role === "value_flow") return `value flow: ${edge.label}`;
  if (role === "ownership") return "ownership: boundary -> closure layout";
  if (role === "call") return `source call: ${edge.label}`;
  if (edge.kind === "body_to_kernel") return "demand edge: body -> kernel";
  if (edge.kind === "body_to_runtime_boundary") return "extract edge: body -> runtime";
  return edge.label;
}
function edgeKeyFor(edge) {
  return `edge:${edge.from}->${edge.to}:${edge.kind}:${edge.label}`;
}
function nodeShortLabel(nodeId) {
  const node = nodeById.get(nodeId);
  if (!node) return nodeId;
  const firstLine = String(node.label || node.id).split("\n")[0];
  return firstLine.length > 84 ? `${firstLine.slice(0, 81)}...` : firstLine;
}
function edgeWhyText(edge) {
  const role = edgeRoleForKind(edge.kind);
  if (role === "value_flow") return "A runtime value from the source boundary feeds the destination boundary.";
  if (role === "ownership") return "The runtime boundary owns or carries a closure-layout argument needed for execution.";
  if (role === "call") return "A source-local call-site resolved this dependency between caller and callee.";
  if (edge.kind === "body_to_kernel") return "Kernel execution demand is seeded from its owning body.";
  if (edge.kind === "body_to_runtime_boundary") return "The body extracts a runtime boundary node from the source expression.";
  return "This edge preserves a structural dependency in the projected analysis graph.";
}
function dependencySummaryHtml(value) {
  if (!value || value.type !== "node" || !value.id) return "";
  const incoming = graph.edges
    .filter((edge) => edge.to === value.id)
    .slice()
    .sort((a, b) => `${a.kind}|${a.from}|${a.label}`.localeCompare(`${b.kind}|${b.from}|${b.label}`));
  const outgoing = graph.edges
    .filter((edge) => edge.from === value.id)
    .slice()
    .sort((a, b) => `${a.kind}|${a.to}|${a.label}`.localeCompare(`${b.kind}|${b.to}|${b.label}`));
  const incomingItems = incoming.length === 0
    ? `<li>None</li>`
    : incoming
        .map((edge) => `<li class="dep-item"><button class="dep-jump" data-select-node-id="${htmlEscape(edge.from)}">open node</button><button class="dep-jump" data-select-edge-key="${htmlEscape(edgeKeyFor(edge))}">open edge</button><span><strong>${htmlEscape(nodeShortLabel(edge.from))}</strong> via <code>${htmlEscape(edgeRoleForKind(edge.kind))}</code> (${htmlEscape(edgeSemanticLabel(edge))})</span></li>`)
        .join("");
  const outgoingItems = outgoing.length === 0
    ? `<li>None</li>`
    : outgoing
        .map((edge) => `<li class="dep-item"><button class="dep-jump" data-select-node-id="${htmlEscape(edge.to)}">open node</button><button class="dep-jump" data-select-edge-key="${htmlEscape(edgeKeyFor(edge))}">open edge</button><span><strong>${htmlEscape(nodeShortLabel(edge.to))}</strong> via <code>${htmlEscape(edgeRoleForKind(edge.kind))}</code> (${htmlEscape(edgeSemanticLabel(edge))})</span></li>`)
        .join("");
  return `
    <div class="detail-card">
      <div class="detail-title">Dependency Summary</div>
      <div class="semantic-summary">What this node depends on</div>
      <ul>${incomingItems}</ul>
      <div class="semantic-summary">What depends on this node</div>
      <ul>${outgoingItems}</ul>
    </div>
  `;
}
function semanticNarrative(value) {
  if (!value || typeof value !== "object") return "";
  if (value.type === "node") {
    const demand = nodeDemandState(value);
    const sharing = nodeSharingState(value);
    const runtimeRole = nodeRuntimeRole(value);
    if (runtimeRole !== "none") {
      const roleText = runtimeRole.replaceAll("_", "-");
      return `This runtime boundary acts as ${roleText}; it marks host/runtime interaction, and upstream demand determines whether it becomes active work.`;
    }
    if (sharing === "shared") {
      return demand === "demanded"
        ? "This node is both demanded and shared, so it is likely a key reuse point that feeds multiple consumers."
        : "This node is shared across uses, but currently not demanded; it remains available for reuse without being on the active path.";
    }
    if (sharing === "closure_env") {
      return "This closure layout carries captured values; demand here indicates call-sites are pulling data through the closure environment.";
    }
    if (demand === "demanded") {
      return "This node is currently demanded, so it contributes to the observed result path in this structural snapshot.";
    }
    if (demand === "nondemanded") {
      return "This node is currently not demanded; it exists structurally but is not on the active observed path.";
    }
    return "This node contributes structural context; semantic demand/sharing markers are not available for this node kind.";
  }
  if (value.type === "edge") {
    const role = edgeRoleForKind(value.kind);
    if (role === "value_flow") return "This edge carries runtime value/data flow between boundary nodes.";
    if (role === "ownership") return "This edge shows runtime ownership of a closure layout argument.";
    if (role === "call") return "This edge links a source-local call-site to its resolved callee body/layout.";
    return "This edge shows structural extraction or demand linkage in the current analysis graph.";
  }
  return "Use filters and selection to focus semantic demand, sharing, and runtime role signals.";
}
function semanticSummaryRows(value, metadata) {
  if (!value || typeof value !== "object") return "";
  const rows = [];
  if (value.type === "node") {
    const demand = nodeDemandState(value);
    if (demand !== "unknown") rows.push(detailRow("Demand", demand));
    const sharing = nodeSharingState(value);
    if (sharing !== "local") rows.push(detailRow("Sharing", sharing));
    const runtimeRole = nodeRuntimeRole(value);
    if (runtimeRole !== "none") rows.push(detailRow("Runtime Role", runtimeRole.replaceAll("_", "-")));
  } else if (value.type === "edge") {
    rows.push(detailRow("Edge Role", edgeRoleForKind(value.kind).replaceAll("_", " ")));
  }
  return rows.join("");
}
function edgeRoleHistogramForNode(nodeId) {
  const counts = { value_flow: 0, ownership: 0, call: 0, structural: 0 };
  for (const edge of graph.edges) {
    if (edge.from !== nodeId && edge.to !== nodeId) continue;
    const role = edgeRoleForKind(edge.kind);
    if (counts[role] !== undefined) counts[role] += 1;
  }
  return counts;
}
function connectionSummaryRows(value) {
  if (!value || typeof value !== "object") return "";
  if (value.type === "node" && value.id) {
    const incoming = graph.edges.filter((edge) => edge.to === value.id).length;
    const outgoing = graph.edges.filter((edge) => edge.from === value.id).length;
    const roleCounts = edgeRoleHistogramForNode(value.id);
    return [
      detailRow("Incoming", incoming),
      detailRow("Outgoing", outgoing),
      detailRow(
        "Edge Mix",
        `value ${roleCounts.value_flow}, ownership ${roleCounts.ownership}, call ${roleCounts.call}, structural ${roleCounts.structural}`
      ),
    ].join("");
  }
  if (value.type === "edge" && value.from && value.to) {
    return [
      detailRow("From", value.from),
      detailRow("To", value.to),
      detailRow("Role Meaning", edgeSemanticLabel(value)),
      detailRow("Why This Edge Exists", edgeWhyText(value)),
    ].join("");
  }
  return "";
}
function matchesNodeSemanticFilters(node) {
  const demand = nodeDemandState(node);
  const sharing = nodeSharingState(node);
  const runtimeRole = nodeRuntimeRole(node);
  if (semanticDemandFilter !== "all" && demand !== semanticDemandFilter) return false;
  if (semanticRuntimeRoleFilter !== "all" && runtimeRole !== semanticRuntimeRoleFilter) return false;
  if (semanticSharingFilter !== "all" && sharing !== semanticSharingFilter) return false;
  return true;
}
function applySemanticFilters() {
  const visibleNodeIds = new Set();
  for (const [nodeId, entry] of nodeElementsById.entries()) {
    const visible = matchesNodeSemanticFilters(entry.node);
    entry.element.classList.toggle("hidden-semantic", !visible);
    if (visible) visibleNodeIds.add(nodeId);
  }
  for (const entry of edgeElements) {
    const visible = visibleNodeIds.has(entry.edge.from) && visibleNodeIds.has(entry.edge.to);
    entry.path.classList.toggle("hidden-semantic", !visible);
    entry.label.classList.toggle("hidden-semantic", !visible);
  }
  applyDependencyFocus();
  applyLaneFolding();
  applyNoiseReduction();
}
function sortedFoldedLaneIds() {
  return Array.from(foldedLaneSourceIds.values()).sort((a, b) => sourceRank(a, laneOrder) - sourceRank(b, laneOrder));
}
function applyLaneFolding() {
  for (const entry of sourceBoundElements) {
    const sourceId = String(entry.sourceId ?? "");
    const shouldHide = sourceId !== "" && foldedLaneSourceIds.has(sourceId);
    entry.el.classList.toggle("hidden-folded-lane", shouldHide);
  }
  for (const [sourceId, rect] of laneRectsBySource.entries()) {
    const folded = foldedLaneSourceIds.has(sourceId);
    rect.classList.toggle("folded-lane", folded);
  }
  for (const [sourceId, label] of laneLabelsBySource.entries()) {
    const folded = foldedLaneSourceIds.has(sourceId);
    label.classList.toggle("folded-lane", folded);
  }
  for (const [sourceId, item] of sourceRegistryItemsBySource.entries()) {
    item.classList.toggle("folded", foldedLaneSourceIds.has(sourceId));
  }
  for (const [sourceId, item] of laneSummaryItemsBySource.entries()) {
    item.classList.toggle("folded", foldedLaneSourceIds.has(sourceId));
    const button = item.querySelector("button[data-lane-fold-source-id]");
    if (button) button.textContent = foldedLaneSourceIds.has(sourceId) ? "unfold" : "fold";
  }
}
function unfoldLaneForInteraction(sourceId) {
  const id = String(sourceId ?? "");
  if (id === "" || !foldedLaneSourceIds.has(id)) return;
  foldedLaneSourceIds.delete(id);
  applyLaneFolding();
}
function setLaneFoldState(sourceId, folded, options = {}) {
  const id = String(sourceId ?? "");
  if (id === "" || id === "unmapped") return;
  if (folded) {
    foldedLaneSourceIds.add(id);
    if (sourceFilterSourceId === id) sourceFilterSourceId = "";
  } else {
    foldedLaneSourceIds.delete(id);
  }
  applySourceFilter(sourceFilterSourceId, { skipPersist: true });
  applyLaneFolding();
  applyNoiseReduction();
  if (!options.skipPersist) persistViewerState(false);
}
function foldStdlibLanes(options = {}) {
  for (const sourceId of laneOrder) {
    if (shouldAutoFoldLane(sourceId)) foldedLaneSourceIds.add(sourceId);
  }
  if (sourceFilterSourceId && foldedLaneSourceIds.has(sourceFilterSourceId)) sourceFilterSourceId = "";
  applySourceFilter(sourceFilterSourceId, { skipPersist: true });
  applyLaneFolding();
  applyNoiseReduction();
  if (!options.skipPersist) persistViewerState(false);
}
function clearFoldedLanes(options = {}) {
  foldedLaneSourceIds.clear();
  applySourceFilter(sourceFilterSourceId, { skipPersist: true });
  applyLaneFolding();
  applyNoiseReduction();
  if (!options.skipPersist) persistViewerState(false);
}
function selectedNeighborhoodFromKey(key) {
  if (!key) return null;
  const primaryNodes = new Set();
  const primaryEdges = new Set();
  const neighborNodes = new Set();
  const neighborEdges = new Set();
  if (key.startsWith("node:")) {
    const nodeId = key.slice(5);
    primaryNodes.add(nodeId);
    for (const entry of edgeElements) {
      if (entry.edge.from === nodeId || entry.edge.to === nodeId) {
        neighborEdges.add(entry.key);
        neighborNodes.add(entry.edge.from);
        neighborNodes.add(entry.edge.to);
      }
    }
    neighborNodes.delete(nodeId);
  } else if (key.startsWith("edge:")) {
    const edgeEntry = edgeElements.find((entry) => entry.key === key);
    if (!edgeEntry) return null;
    primaryEdges.add(key);
    primaryNodes.add(edgeEntry.edge.from);
    primaryNodes.add(edgeEntry.edge.to);
    for (const entry of edgeElements) {
      if (entry.key === key) continue;
      if (
        entry.edge.from === edgeEntry.edge.from ||
        entry.edge.to === edgeEntry.edge.from ||
        entry.edge.from === edgeEntry.edge.to ||
        entry.edge.to === edgeEntry.edge.to
      ) {
        neighborEdges.add(entry.key);
      }
    }
  } else {
    return null;
  }
  return { primaryNodes, primaryEdges, neighborNodes, neighborEdges };
}
function selectedTraceFromNode(nodeId, mode) {
  const primaryNodes = new Set([nodeId]);
  const primaryEdges = new Set();
  const neighborNodes = new Set();
  const neighborEdges = new Set();
  const queue = [nodeId];
  const visitedNodes = new Set([nodeId]);
  while (queue.length > 0) {
    const current = queue.shift();
    for (const entry of edgeElements) {
      const next =
        mode === "upstream"
          ? (entry.edge.to === current ? entry.edge.from : null)
          : (entry.edge.from === current ? entry.edge.to : null);
      if (!next) continue;
      neighborEdges.add(entry.key);
      if (!visitedNodes.has(next)) {
        visitedNodes.add(next);
        queue.push(next);
      }
      if (next !== nodeId) neighborNodes.add(next);
    }
  }
  return { primaryNodes, primaryEdges, neighborNodes, neighborEdges };
}
function applyDependencyFocus() {
  for (const entry of edgeElements) {
    entry.path.classList.remove("focus-primary", "focus-neighbor", "dim-focus");
    entry.label.classList.remove("focus-primary", "focus-neighbor", "dim-focus");
  }
  for (const entry of nodeElementsById.values()) {
    entry.element.classList.remove("focus-primary", "focus-neighbor", "dim-focus");
  }
  if (!dependencyFocusEnabled) return;
  let neighborhood = selectedNeighborhoodFromKey(selectedGraphKey);
  if (
    dependencyFocusMode !== "neighborhood" &&
    selectedGraphKey.startsWith("node:")
  ) {
    neighborhood = selectedTraceFromNode(
      selectedGraphKey.slice(5),
      dependencyFocusMode
    );
  }
  if (!neighborhood) return;
  for (const [nodeId, entry] of nodeElementsById.entries()) {
    if (neighborhood.primaryNodes.has(nodeId)) {
      entry.element.classList.add("focus-primary");
    } else if (neighborhood.neighborNodes.has(nodeId)) {
      entry.element.classList.add("focus-neighbor");
    } else {
      entry.element.classList.add("dim-focus");
    }
  }
  for (const entry of edgeElements) {
    const isPrimary = neighborhood.primaryEdges.has(entry.key);
    const isNeighbor = neighborhood.neighborEdges.has(entry.key);
    if (isPrimary) {
      entry.path.classList.add("focus-primary");
      entry.label.classList.add("focus-primary");
    } else if (isNeighbor) {
      entry.path.classList.add("focus-neighbor");
      entry.label.classList.add("focus-neighbor");
    } else {
      entry.path.classList.add("dim-focus");
      entry.label.classList.add("dim-focus");
    }
  }
}
function applyNoiseReduction() {
  for (const entry of nodeElementsById.values()) {
    const sourceId = sourceIdForNode(entry.node);
    const lowInterest = hideStdlibNoiseEnabled && sourceId !== "" && isStdlibSourceId(sourceId);
    entry.element.classList.toggle("noise-dim", lowInterest && sourceId !== selectedSourceId);
  }
  for (const entry of edgeElements) {
    const sourceId = sourceIdForEdge(entry.edge);
    const lowInterest = hideStdlibNoiseEnabled && sourceId !== "" && isStdlibSourceId(sourceId);
    entry.path.classList.toggle("noise-dim", lowInterest && sourceId !== selectedSourceId);
    entry.label.classList.toggle("noise-dim", lowInterest && sourceId !== selectedSourceId);
  }
  for (const [sourceId, rect] of laneRectsBySource.entries()) {
    const lowInterest = hideStdlibNoiseEnabled && isStdlibSourceId(sourceId);
    rect.classList.toggle("noise-dim", lowInterest && sourceId !== selectedSourceId);
  }
  for (const [sourceId, label] of laneLabelsBySource.entries()) {
    const lowInterest = hideStdlibNoiseEnabled && isStdlibSourceId(sourceId);
    label.classList.toggle("noise-dim", lowInterest && sourceId !== selectedSourceId);
  }
  const items = sourceRegistry.querySelectorAll("li[data-source-id]");
  for (const item of items) {
    const sourceId = String(item.dataset.sourceId ?? "");
    const lowInterest = hideStdlibNoiseEnabled && isStdlibSourceId(sourceId);
    item.classList.toggle("noise-dim", lowInterest && sourceId !== selectedSourceId);
  }
  for (const item of laneSummaryItemsBySource.values()) {
    const sourceId = String(item.dataset.sourceId ?? "");
    const lowInterest = hideStdlibNoiseEnabled && isStdlibSourceId(sourceId);
    item.classList.toggle("noise-dim", lowInterest && sourceId !== selectedSourceId);
  }
}
function addNodeBadges(group, node) {
  const badges = [];
  const demand = nodeDemandState(node);
  if (demand !== "unknown") badges.push({ role: "demand", text: denseGraph ? (demand === "demanded" ? "D+" : "D-") : (demand === "demanded" ? "demanded" : "idle") });
  const sharing = nodeSharingState(node);
  if (sharing !== "local") badges.push({ role: "sharing", text: denseGraph ? (sharing === "shared" ? "S" : "C") : sharing.replaceAll("_", "-") });
  const runtimeRole = nodeRuntimeRole(node);
  if (runtimeRole !== "none") badges.push({ role: "runtime", text: denseGraph ? runtimeRole.slice(0, 2).toUpperCase() : runtimeRole.replaceAll("_", "-") });
  badges.slice(0, denseGraph ? 2 : 3).forEach((badge, idx) => {
    const badgeGroup = el("g", { class: `node-badge node-badge-${badge.role}` });
    const badgeWidth = Math.max(40, 10 + badge.text.length * 5.4);
    const x = nodeWidth - badgeWidth - 8;
    const y = 6 + idx * 14;
    badgeGroup.appendChild(el("rect", { x, y, width: badgeWidth, height: 12 }));
    const text = el("text", { x: x + 4, y: y + 9 });
    text.textContent = badge.text;
    badgeGroup.appendChild(text);
    group.appendChild(badgeGroup);
  });
}

const grouped = new Map(kindOrder.map((kind) => [kind, []]));
for (const node of graph.nodes) {
  const kind = kindOrder.includes(node.kind) ? node.kind : "body";
  grouped.get(kind).push(node);
}
for (const kind of kindOrder) {
  grouped.get(kind).sort((a, b) => nodeSortKey(a).localeCompare(nodeSortKey(b)));
}

const declaredSourceFiles = Array.isArray(graph.source_files) ? graph.source_files : [];
const declaredSourceIds = declaredSourceFiles
  .slice()
  .sort((a, b) => (a.source_id - b.source_id) || String(a.path).localeCompare(String(b.path)))
  .map((f) => String(f.source_id));
const discoveredSourceIds = Array.from(new Set(graph.nodes.map((node) => sourceIdForNode(node))));
const laneOrder = [
  ...declaredSourceIds,
  ...discoveredSourceIds
    .filter((sourceId) => !declaredSourceIds.includes(sourceId))
    .sort((a, b) => String(a).localeCompare(String(b))),
];
if (!laneOrder.includes("unmapped")) laneOrder.push("unmapped");

function sourceIdForEdge(edge) {
  const metadata = metadataMap(edge?.metadata);
  const sourceId = metadata.span_source_id;
  return sourceId === undefined || sourceId === null || String(sourceId) === "" ? "" : String(sourceId);
}
function laneStatsFor(sourceId) {
  const nodeStats = {
    body: 0,
    kernel: 0,
    runtime_boundary: 0,
    closure_layout: 0,
  };
  let totalNodes = 0;
  for (const node of graph.nodes) {
    if (sourceIdForNode(node) !== sourceId) continue;
    totalNodes += 1;
    if (nodeStats[node.kind] !== undefined) nodeStats[node.kind] += 1;
  }
  let totalEdges = 0;
  for (const edge of graph.edges) {
    if (sourceIdForEdge(edge) !== sourceId) continue;
    totalEdges += 1;
  }
  return {
    sourceId,
    totalNodes,
    totalEdges,
    nodeStats,
    isStdlib: isStdlibSourceId(sourceId),
  };
}
const laneStatsBySource = new Map(laneOrder.map((sourceId) => [sourceId, laneStatsFor(sourceId)]));
function laneHintFor(sourceId) {
  const stats = laneStatsBySource.get(sourceId) ?? laneStatsFor(sourceId);
  if (sourceId === "unmapped") return "unmapped provenance";
  if (stats.isStdlib) return "stdlib-heavy / low-interest";
  if (stats.totalNodes > 0 && stats.nodeStats.runtime_boundary * 2 >= stats.totalNodes) return "runtime-boundary heavy";
  if (stats.totalNodes <= 2 && stats.totalEdges <= 2) return "sparse lane";
  return "project lane";
}
function shouldAutoFoldLane(sourceId) {
  if (sourceId === "unmapped") return false;
  const stats = laneStatsBySource.get(sourceId) ?? laneStatsFor(sourceId);
  if (!stats.isStdlib) return false;
  return laneOrder.length >= 6 || graph.nodes.length >= 40;
}
function searchText(value) {
  return String(value ?? "").toLowerCase();
}
function metadataText(metadata) {
  const map = metadataMap(metadata);
  return Object.entries(map)
    .map(([key, value]) => `${key}:${String(value ?? "")}`)
    .join(" ");
}
function buildSearchEntries() {
  const entries = [];
  for (const node of graph.nodes) {
    const sourceId = sourceIdForNode(node);
    const sourcePath = sourcePathById(sourceId);
    entries.push({
      type: "node",
      key: `node:${node.id}`,
      sourceId,
      label: nodeShortLabel(node.id),
      path: sourcePath,
      haystack: searchText([
        node.id,
        node.label,
        node.kind,
        sourceLabel(sourceId),
        sourcePath,
        metadataText(node.metadata),
      ].join(" ")),
    });
  }
  for (const edge of graph.edges) {
    const sourceId = sourceIdForEdge(edge);
    const sourcePath = sourcePathById(sourceId);
    entries.push({
      type: "edge",
      key: edgeKeyFor(edge),
      sourceId,
      label: edgeSemanticLabel(edge),
      path: sourcePath,
      haystack: searchText([
        edge.kind,
        edge.label,
        edge.from,
        edge.to,
        edgeSemanticLabel(edge),
        sourceLabel(sourceId),
        sourcePath,
        metadataText(edge.metadata),
      ].join(" ")),
    });
  }
  for (const sourceId of laneOrder) {
    const path = sourcePathById(sourceId);
    const stats = laneStatsBySource.get(sourceId) ?? laneStatsFor(sourceId);
    const label = sourceId === "unmapped" ? "source unmapped" : `source ${sourceId}: ${sourceLabel(sourceId)}`;
    entries.push({
      type: "source",
      key: `source:${sourceId}`,
      sourceId,
      label,
      path,
      haystack: searchText([
        label,
        path,
        laneHintFor(sourceId),
        `body ${stats.nodeStats.body} kernel ${stats.nodeStats.kernel} runtime ${stats.nodeStats.runtime_boundary} layout ${stats.nodeStats.closure_layout}`,
      ].join(" ")),
    });
  }
  return entries.sort((a, b) => `${a.type}|${a.label}|${a.key}`.localeCompare(`${b.type}|${b.label}|${b.key}`));
}
const searchEntries = buildSearchEntries();

const laneHeightBySource = new Map();
for (const sourceId of laneOrder) {
  let maxNodesInKind = 0;
  for (const kind of kindOrder) {
    const count = grouped
      .get(kind)
      .filter((node) => sourceIdForNode(node) === sourceId)
      .length;
    maxNodesInKind = Math.max(maxNodesInKind, count);
  }
  laneHeightBySource.set(sourceId, Math.max(128, maxNodesInKind * nodeSpacing + 52));
}

const laneYStartBySource = new Map();
let laneCursor = yStart;
for (const sourceId of laneOrder) {
  laneYStartBySource.set(sourceId, laneCursor);
  laneCursor += laneHeightBySource.get(sourceId);
}

const edgeNeighborsByNode = new Map();
for (const edge of graph.edges) {
  if (!edgeNeighborsByNode.has(edge.from)) edgeNeighborsByNode.set(edge.from, []);
  if (!edgeNeighborsByNode.has(edge.to)) edgeNeighborsByNode.set(edge.to, []);
  edgeNeighborsByNode.get(edge.from).push(edge.to);
  edgeNeighborsByNode.get(edge.to).push(edge.from);
}

const orderedByKind = new Map();
for (const kind of kindOrder) {
  orderedByKind.set(kind, grouped.get(kind).slice());
}

const positions = new Map();
function recomputePositions() {
  positions.clear();
  for (const kind of kindOrder) {
    const x = columns[kind] ?? 120;
    const laneIndexBySource = new Map();
    for (const node of orderedByKind.get(kind)) {
      const sourceId = sourceIdForNode(node);
      const laneStart = laneYStartBySource.get(sourceId) ?? yStart;
      const idx = laneIndexBySource.get(sourceId) ?? 0;
      laneIndexBySource.set(sourceId, idx + 1);
      const y = laneStart + 26 + idx * nodeSpacing;
      positions.set(node.id, { x, y });
    }
  }
}
function barycenterForNode(nodeId) {
  const neighbors = edgeNeighborsByNode.get(nodeId) ?? [];
  if (neighbors.length === 0) return Number.POSITIVE_INFINITY;
  let total = 0;
  let count = 0;
  for (const neighbor of neighbors) {
    const p = positions.get(neighbor);
    if (!p) continue;
    total += p.y;
    count++;
  }
  if (count === 0) return Number.POSITIVE_INFINITY;
  return total / count;
}
recomputePositions();
for (let pass = 0; pass < 3; pass++) {
  for (const kind of kindOrder) {
    const next = orderedByKind.get(kind).slice();
    next.sort((a, b) => {
      const sourceCmp = sourceRank(sourceIdForNode(a), laneOrder) - sourceRank(sourceIdForNode(b), laneOrder);
      if (sourceCmp !== 0) return sourceCmp;
      const aCenter = barycenterForNode(a.id);
      const bCenter = barycenterForNode(b.id);
      if (Number.isFinite(aCenter) && Number.isFinite(bCenter) && aCenter !== bCenter) {
        return aCenter - bCenter;
      }
      if (Number.isFinite(aCenter) && !Number.isFinite(bCenter)) return -1;
      if (!Number.isFinite(aCenter) && Number.isFinite(bCenter)) return 1;
      return String(a.id).localeCompare(String(b.id));
    });
    orderedByKind.set(kind, next);
  }
  recomputePositions();
}

const maxY = Math.max(800, laneCursor + 36, ...Array.from(positions.values()).map((p) => p.y + 100));
const maxX = Math.max(1120, ...Array.from(positions.values()).map((p) => p.x + 220));
svg.setAttribute("height", String(maxY));
svg.setAttribute("width", String(maxX));

const svgns = "http://www.w3.org/2000/svg";
function el(name, attrs = {}) {
  const n = document.createElementNS(svgns, name);
  for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, String(v));
  return n;
}
function htmlEscape(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll("\"", "&quot;");
}
function registerSelectable(key, elements) {
  const list = elements.filter(Boolean);
  if (list.length === 0) return;
  selectableElements.set(key, list);
}
function setSearchStatus(text) {
  if (!searchStatus) return;
  searchStatus.textContent = text || "";
}
function activateSearchResult(entry) {
  if (!entry) return;
  if (entry.type === "node" || entry.type === "edge") {
    selectByGraphKey(entry.key, { pushUrlHistory: true });
    return;
  }
  if (entry.type === "source") {
    const sourceId = entry.sourceId || "";
    applySourceFilter(sourceId);
    if (sourceId) focusSourceLane(sourceId);
  }
}
function setActiveSearchResultIndex(index) {
  if (!searchResults) return;
  const next = Math.max(-1, Math.min(index, visibleSearchResults.length - 1));
  activeSearchResultIndex = next;
  const buttons = searchResults.querySelectorAll("button.search-hit");
  buttons.forEach((button, idx) => {
    button.classList.toggle("active", idx === activeSearchResultIndex);
  });
}
function renderSearchResults() {
  if (!searchResults) return;
  const trimmed = String(searchQuery || "").trim();
  if (trimmed === "") {
    visibleSearchResults = [];
    activeSearchResultIndex = -1;
    searchResults.innerHTML = "";
    setSearchStatus("Type to search nodes, edges, and source lanes.");
    return;
  }
  const tokens = trimmed
    .toLowerCase()
    .split(/\s+/)
    .filter((token) => token.length > 0);
  visibleSearchResults = searchEntries
    .filter((entry) => tokens.every((token) => entry.haystack.includes(token)))
    .slice(0, 40);
  if (visibleSearchResults.length === 0) {
    activeSearchResultIndex = -1;
    searchResults.innerHTML = "";
    setSearchStatus("No matches.");
    return;
  }
  if (activeSearchResultIndex >= visibleSearchResults.length) {
    activeSearchResultIndex = -1;
  }
  const items = visibleSearchResults
    .map((entry, idx) => {
      const label = htmlEscape(entry.label);
      const kind = htmlEscape(entry.type);
      const path = entry.path ? `<span class="path">${htmlEscape(entry.path)}</span>` : "";
      return `<li><button type="button" class="search-hit${idx === activeSearchResultIndex ? " active" : ""}" data-search-index="${idx}"><span class="kind">${kind}</span>${label}${path}</button></li>`;
    })
    .join("");
  searchResults.innerHTML = items;
  setSearchStatus(`${visibleSearchResults.length} result(s)`);
  const buttons = searchResults.querySelectorAll("button[data-search-index]");
  for (const button of buttons) {
    button.addEventListener("click", () => {
      const idx = Number.parseInt(String(button.getAttribute("data-search-index") ?? ""), 10);
      if (!Number.isFinite(idx) || idx < 0 || idx >= visibleSearchResults.length) return;
      setActiveSearchResultIndex(idx);
      activateSearchResult(visibleSearchResults[idx]);
    });
  }
}
function setSearchQuery(value, options = {}) {
  searchQuery = String(value ?? "");
  renderSearchResults();
  if (!options.skipPersist) persistViewerState(false);
}
function updateZoomStatus() {
  zoomStatus.textContent = `${Math.round(viewportScale * 100)}%`;
}
function setViewportScale(nextScale, anchorClientX, anchorClientY) {
  const clamped = Math.max(0.45, Math.min(2.2, nextScale));
  const before = viewportScale;
  viewportScale = clamped;
  svg.style.transformOrigin = "0 0";
  svg.style.transform = `scale(${viewportScale})`;
  updateZoomStatus();
  if (!graphViewport || before === viewportScale) return;
  if (Number.isFinite(anchorClientX) && Number.isFinite(anchorClientY)) {
    const viewportRect = graphViewport.getBoundingClientRect();
    const ox = anchorClientX - viewportRect.left + graphViewport.scrollLeft;
    const oy = anchorClientY - viewportRect.top + graphViewport.scrollTop;
    const ratio = viewportScale / before;
    graphViewport.scrollLeft = ox * ratio - (anchorClientX - viewportRect.left);
    graphViewport.scrollTop = oy * ratio - (anchorClientY - viewportRect.top);
  }
}
function zoomBy(factor) {
  const rect = graphViewport.getBoundingClientRect();
  setViewportScale(viewportScale * factor, rect.left + rect.width / 2, rect.top + rect.height / 2);
}
function scrollViewportTo(x, y) {
  const targetLeft = Math.max(0, x * viewportScale - graphViewport.clientWidth * 0.42);
  const targetTop = Math.max(0, y * viewportScale - graphViewport.clientHeight * 0.32);
  graphViewport.scrollTo({ left: targetLeft, top: targetTop, behavior: "smooth" });
}
function focusSourceLane(sourceId) {
  const normalized = String(sourceId ?? "");
  unfoldLaneForInteraction(normalized);
  const laneStart = laneYStartBySource.get(normalized);
  if (laneStart === undefined) return;
  const laneHeight = laneHeightBySource.get(normalized) ?? 128;
  scrollViewportTo(columns.kernel, laneStart + laneHeight / 2);
  for (const [id, rect] of laneRectsBySource.entries()) {
    rect.classList.toggle("selected", id === normalized);
  }
}
function setSelectedGraphElements(key) {
  selectedGraphKey = key || "";
  for (const [entryKey, elements] of selectableElements.entries()) {
    const selected = selectedGraphKey !== "" && entryKey === selectedGraphKey;
    for (const element of elements) {
      element.classList.toggle("selected", selected);
    }
  }
}
function readViewerStateFromUrl() {
  const raw = String(window.location.hash || "").replace(/^#/, "");
  const params = new URLSearchParams(raw);
  return {
    version: params.get("v") || "",
    selected: params.get("sel") || "",
    sourceFilter: params.get("sf") || "",
    demand: params.get("fd") || "all",
    runtimeRole: params.get("fr") || "all",
    sharing: params.get("fs") || "all",
    focus: params.get("focus") === "1",
    trace: params.get("trace") || "neighborhood",
    noise: params.get("noise") === "1",
    folded: params.get("fold") || "",
    query: params.get("q") || "",
  };
}
function writeViewerStateToUrl(push) {
  const params = new URLSearchParams();
  params.set("v", "1");
  if (selectedGraphKey) params.set("sel", selectedGraphKey);
  if (sourceFilterSourceId) params.set("sf", sourceFilterSourceId);
  if (semanticDemandFilter !== "all") params.set("fd", semanticDemandFilter);
  if (semanticRuntimeRoleFilter !== "all") params.set("fr", semanticRuntimeRoleFilter);
  if (semanticSharingFilter !== "all") params.set("fs", semanticSharingFilter);
  if (dependencyFocusEnabled) params.set("focus", "1");
  if (dependencyFocusMode !== "neighborhood") params.set("trace", dependencyFocusMode);
  if (hideStdlibNoiseEnabled) params.set("noise", "1");
  const folded = sortedFoldedLaneIds();
  if (folded.length > 0) params.set("fold", folded.join(","));
  if (searchQuery.trim() !== "") params.set("q", searchQuery.trim());
  const nextHash = `#${params.toString()}`;
  if (window.location.hash === nextHash) return;
  if (push) {
    window.history.pushState({ vixenViewerState: true }, "", nextHash);
  } else {
    window.history.replaceState({ vixenViewerState: true }, "", nextHash);
  }
}
function persistViewerState(push) {
  if (suppressUrlStateUpdate) return;
  writeViewerStateToUrl(Boolean(push));
}
function setShareStatus(text) {
  if (!shareLinkStatus) return;
  shareLinkStatus.textContent = text || "";
}
function copyShareLink() {
  persistViewerState(false);
  const url = window.location.href;
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText(url)
      .then(() => setShareStatus("link copied"))
      .catch(() => setShareStatus("copy failed"));
    return;
  }
  try {
    window.prompt("Copy link", url);
    setShareStatus("link ready");
  } catch (_error) {
    setShareStatus("copy unavailable");
  }
}
function updateSelectionHistoryButtons() {
  if (selectionHistoryBack) selectionHistoryBack.disabled = selectionHistoryIndex <= 0;
  if (selectionHistoryForward) selectionHistoryForward.disabled = selectionHistoryIndex === -1 || selectionHistoryIndex >= selectionHistory.length - 1;
}
function pushSelectionHistory(key) {
  if (!key) return;
  if (selectionHistoryIndex >= 0 && selectionHistory[selectionHistoryIndex] === key) {
    updateSelectionHistoryButtons();
    return;
  }
  selectionHistory.splice(selectionHistoryIndex + 1);
  selectionHistory.push(key);
  selectionHistoryIndex = selectionHistory.length - 1;
  updateSelectionHistoryButtons();
}
function selectGraphItem(key, detailsValue, sourceId, options = {}) {
  unfoldLaneForInteraction(sourceId);
  setSelectedGraphElements(key);
  selectedSourceId = String(sourceId ?? "");
  dependencyJumpIndex = -1;
  if (!options.fromHistory) pushSelectionHistory(key);
  setDetails(detailsValue);
  applySourceFilter(sourceFilterSourceId, { skipPersist: true });
  if (selectedSourceId) focusSourceLane(selectedSourceId);
  if (options.persistUrl !== false) {
    persistViewerState(Boolean(options.pushUrlHistory));
  }
}
function selectByGraphKey(key, options = {}) {
  if (!key) return;
  if (key.startsWith("node:")) {
    const nodeId = key.slice(5);
    const node = nodeById.get(nodeId);
    if (!node) return;
    const nodeMetadata = metadataMap(node.metadata);
    const nodeSourceId = String(nodeMetadata.span_source_id ?? "");
    selectGraphItem(key, { type: "node", ...node }, nodeSourceId, options);
    const p = positions.get(node.id);
    if (p) scrollViewportTo(p.x, p.y + nodeHeight / 2);
    return;
  }
  if (key.startsWith("edge:")) {
    const edgeEntry = edgeElements.find((entry) => entry.key === key);
    if (!edgeEntry) return;
    const edgeMetadata = metadataMap(edgeEntry.edge?.metadata);
    const edgeSourceId = String(edgeMetadata.span_source_id ?? "");
    selectGraphItem(key, { type: "edge", ...edgeEntry.edge }, edgeSourceId, options);
  }
}
function applyViewerStateFromUrl() {
  const state = readViewerStateFromUrl();
  suppressUrlStateUpdate = true;
  semanticDemandFilter = state.demand || "all";
  semanticRuntimeRoleFilter = state.runtimeRole || "all";
  semanticSharingFilter = state.sharing || "all";
  dependencyFocusEnabled = Boolean(state.focus);
  dependencyFocusMode = state.trace || "neighborhood";
  hideStdlibNoiseEnabled = Boolean(state.noise);
  searchQuery = String(state.query || "");
  foldedLaneSourceIds.clear();
  const foldedList = String(state.folded || "")
    .split(",")
    .map((item) => String(item).trim())
    .filter((item) => item !== "" && laneOrder.includes(item));
  for (const sourceId of foldedList) foldedLaneSourceIds.add(sourceId);
  if (filterDemand) filterDemand.value = semanticDemandFilter;
  if (filterRuntimeRole) filterRuntimeRole.value = semanticRuntimeRoleFilter;
  if (filterSharing) filterSharing.value = semanticSharingFilter;
  if (focusNeighborhoodToggle) focusNeighborhoodToggle.checked = dependencyFocusEnabled;
  if (focusPathMode) focusPathMode.value = dependencyFocusMode;
  if (hideStdlibNoiseToggle) hideStdlibNoiseToggle.checked = hideStdlibNoiseEnabled;
  if (searchQueryInput) searchQueryInput.value = searchQuery;
  renderSearchResults();
  applySourceFilter(state.sourceFilter || "", { skipPersist: true });
  applySemanticFilters();
  const hasSelection = Boolean(state.selected);
  if (hasSelection) {
    selectByGraphKey(state.selected, {
      fromHistory: true,
      persistUrl: false,
      pushUrlHistory: false,
    });
  }
  suppressUrlStateUpdate = false;
  return hasSelection;
}
function detailRow(key, value) {
  return `<div class="detail-row"><div class="detail-key">${htmlEscape(key)}</div><div class="detail-val">${htmlEscape(value ?? "")}</div></div>`;
}
function formatSpan(metadata) {
  if (!metadata.span_start || !metadata.span_end) return null;
  const source = metadata.span_source_id ? `source ${metadata.span_source_id}, ` : "";
  return `${source}bytes ${metadata.span_start}..${metadata.span_end}`;
}
function normalizeMetadata(value) {
  if (!value || typeof value !== "object") return {};
  return value;
}
function renderSnippet(snippet) {
  const lines = String(snippet).split("\n");
  const lineHtml = lines
    .map((line, idx) => `<div class="snippet-line"><span class="snippet-ln">${idx + 1}</span><span class="snippet-code">${htmlEscape(line)}</span></div>`)
    .join("");
  return `<div class="snippet">${lineHtml}</div>`;
}
function highlightSourceInRegistry(sourceId) {
  const items = sourceRegistry.querySelectorAll("li[data-source-id]");
  for (const item of items) {
    const isMatch = String(sourceId) !== "" && item.dataset.sourceId === String(sourceId);
    item.classList.toggle("active", isMatch);
    item.classList.toggle("focused", isMatch && sourceFilterSourceId === String(sourceId));
  }
}
function applySourceFilter(sourceId, options = {}) {
  sourceFilterSourceId = String(sourceId ?? "");
  unfoldLaneForInteraction(sourceFilterSourceId);
  for (const entry of sourceBoundElements) {
    const shouldDim =
      sourceFilterSourceId !== "" &&
      entry.sourceId !== "" &&
      entry.sourceId !== sourceFilterSourceId &&
      entry.sourceId !== selectedSourceId;
    entry.el.classList.toggle("dimmed", shouldDim);
  }
  const items = sourceRegistry.querySelectorAll("li[data-source-id]");
  for (const item of items) {
    item.classList.toggle("focused", sourceFilterSourceId !== "" && item.dataset.sourceId === sourceFilterSourceId);
  }
  applyDependencyFocus();
  applyNoiseReduction();
  if (!options.skipPersist) persistViewerState(false);
}
function wireDetailTraversal() {
  const nodeButtons = details.querySelectorAll("[data-select-node-id]");
  for (const button of nodeButtons) {
    button.addEventListener("click", () => {
      const nodeId = button.getAttribute("data-select-node-id") || "";
      if (!nodeId) return;
      selectByGraphKey(`node:${nodeId}`, { pushUrlHistory: true });
    });
  }
  const edgeButtons = details.querySelectorAll("[data-select-edge-key]");
  for (const button of edgeButtons) {
    button.addEventListener("click", () => {
      const edgeKey = button.getAttribute("data-select-edge-key") || "";
      if (!edgeKey) return;
      selectByGraphKey(edgeKey, { pushUrlHistory: true });
    });
  }
}
function moveDependencyFocus(step) {
  const buttons = Array.from(
    details.querySelectorAll("[data-select-node-id], [data-select-edge-key]")
  );
  if (buttons.length === 0) return;
  dependencyJumpIndex = (dependencyJumpIndex + step + buttons.length) % buttons.length;
  const target = buttons[dependencyJumpIndex];
  if (target && typeof target.click === "function") target.click();
}
function setDetails(value) {
  const metadata = normalizeMetadata(value.metadata);
  const kind = value.kind || value.type || "graph";
  const label = value.label || "";
  const sourcePath = metadata.source_path || value.source_file || "";
  const sourceId = metadata.span_source_id || "";
  const span = formatSpan(metadata);
  const snippet = metadata.source_snippet || "";
  const snippetHighlighted = metadata.source_snippet_highlighted_html || "";

  const baseRows = [
    detailRow("Type", value.type || "graph"),
    detailRow("Kind", kind),
    label ? detailRow("Label", label) : "",
    value.id ? detailRow("ID", value.id) : "",
    value.from && value.to ? detailRow("Endpoints", `${value.from} -> ${value.to}`) : "",
    sourceId ? detailRow("Source ID", sourceId) : "",
    sourcePath ? detailRow("Source Path", sourcePath) : "",
    span ? detailRow("Span", span) : "",
  ].join("");

  const metadataRows = Object.entries(metadata)
    .filter(([key]) => !["source_path", "source_snippet", "source_snippet_highlighted_html", "span_source_id", "span_start", "span_end"].includes(key))
    .sort((a, b) => a[0].localeCompare(b[0]))
    .map(([key, val]) => detailRow(key, val))
    .join("");
  const semanticRows = semanticSummaryRows(value, metadata);
  const connectionRows = connectionSummaryRows(value);
  const dependencySummary = dependencySummaryHtml(value);
  const semanticExplain = semanticNarrative(value);

  const snippetBlock = snippet
    ? `<div class="detail-card"><div class="detail-title">Source Snippet</div>${
        snippetHighlighted
          ? `<div class="snippet arb-snippet">${snippetHighlighted}</div>`
          : renderSnippet(snippet)
      }</div>`
    : "";

  const metadataBlock = metadataRows
    ? `<div class="detail-card"><div class="detail-title">Metadata</div>${metadataRows}</div>`
    : "";

  details.innerHTML = `
    <div class="detail-card">
      <div class="detail-title">Summary</div>
      ${baseRows}
      ${semanticRows ? `<div class="semantic-summary">Semantic View</div>${semanticRows}` : ""}
      ${connectionRows ? `<div class="semantic-summary">Connection Focus</div>${connectionRows}` : ""}
      ${semanticExplain ? `<div class="semantic-summary">Why It Matters</div><div class="semantic-explain">${htmlEscape(semanticExplain)}</div>` : ""}
      ${!label && !sourcePath && !span ? `<div class="detail-row"><div class="detail-key">Info</div><div class="detail-val">Select a node or edge.</div></div>` : ""}
    </div>
    ${dependencySummary}
    ${snippetBlock}
    ${metadataBlock}
    <details class="raw-json">
      <summary>Raw JSON</summary>
      <pre class="raw-json-pre">${htmlEscape(JSON.stringify(value, null, 2))}</pre>
    </details>
  `;
  wireDetailTraversal();
  highlightSourceInRegistry(sourceId);
  if (sourceFilterSourceId === "") {
    applySourceFilter(sourceId);
  }
}
function renderSourceRegistry(graph) {
  const files = Array.isArray(graph.source_files) ? graph.source_files : [];
  if (files.length === 0) {
    sourceRegistry.innerHTML = `<div>No source registry for this graph.</div>`;
    sourceRegistryItemsBySource.clear();
    return;
  }
  const list = files
    .slice()
    .sort((a, b) => (a.source_id - b.source_id) || String(a.path).localeCompare(String(b.path)))
    .map((file) => `<li data-source-id="${htmlEscape(file.source_id)}"><strong>${htmlEscape(file.source_id)}</strong>: ${htmlEscape(file.path)}</li>`)
    .join("");
  sourceRegistry.innerHTML = `
    <div>This graph includes ${files.length} source file(s).</div>
    <ul>${list}</ul>
  `;
  sourceRegistryItemsBySource.clear();
  const items = sourceRegistry.querySelectorAll("li[data-source-id]");
  for (const item of items) {
    const sourceId = String(item.dataset.sourceId ?? "");
    sourceRegistryItemsBySource.set(sourceId, item);
    item.addEventListener("click", () => {
      const next = item.dataset.sourceId === sourceFilterSourceId ? "" : item.dataset.sourceId;
      applySourceFilter(next);
      if (next) {
        focusSourceLane(next);
      }
    });
  }
}
function renderLaneSummary() {
  if (!laneSummary) return;
  const items = laneOrder
    .map((sourceId) => {
      const stats = laneStatsBySource.get(sourceId) ?? laneStatsFor(sourceId);
      const nodeMix = `body ${stats.nodeStats.body}, kernel ${stats.nodeStats.kernel}, runtime ${stats.nodeStats.runtime_boundary}, layout ${stats.nodeStats.closure_layout}`;
      const label = sourceId === "unmapped"
        ? "unmapped"
        : `${sourceId}: ${sourceLabel(sourceId)}`;
      return `<li data-source-id="${htmlEscape(sourceId)}"><div class="lane-head"><div class="lane-label">${htmlEscape(label)}</div><div class="lane-actions"><button type="button" data-lane-focus-source-id="${htmlEscape(sourceId)}">focus</button><button type="button" data-lane-fold-source-id="${htmlEscape(sourceId)}">${foldedLaneSourceIds.has(sourceId) ? "unfold" : "fold"}</button></div></div><div class="lane-meta">${stats.totalNodes} nodes, ${stats.totalEdges} edges</div><div class="lane-meta">${htmlEscape(nodeMix)}</div><div class="lane-hint">${htmlEscape(laneHintFor(sourceId))}</div></li>`;
    })
    .join("");
  laneSummary.innerHTML = `
    <div class="lane-tools">
      <button type="button" data-lane-action="fold-stdlib">fold stdlib lanes</button>
      <button type="button" data-lane-action="unfold-all">unfold all</button>
    </div>
    <div>Lane folding hides low-value sources while preserving drill-in via focus/select.</div>
    <ul>${items}</ul>
  `;
  laneSummaryItemsBySource.clear();
  const rows = laneSummary.querySelectorAll("li[data-source-id]");
  for (const row of rows) {
    const sourceId = String(row.dataset.sourceId ?? "");
    laneSummaryItemsBySource.set(sourceId, row);
  }
  const focusButtons = laneSummary.querySelectorAll("button[data-lane-focus-source-id]");
  for (const button of focusButtons) {
    button.addEventListener("click", () => {
      const sourceId = String(button.getAttribute("data-lane-focus-source-id") ?? "");
      if (!sourceId) return;
      applySourceFilter(sourceId);
      focusSourceLane(sourceId);
    });
  }
  const foldButtons = laneSummary.querySelectorAll("button[data-lane-fold-source-id]");
  for (const button of foldButtons) {
    button.addEventListener("click", () => {
      const sourceId = String(button.getAttribute("data-lane-fold-source-id") ?? "");
      if (!sourceId || sourceId === "unmapped") return;
      setLaneFoldState(sourceId, !foldedLaneSourceIds.has(sourceId));
    });
  }
  const foldStdlib = laneSummary.querySelector("button[data-lane-action=\"fold-stdlib\"]");
  if (foldStdlib) {
    foldStdlib.addEventListener("click", () => foldStdlibLanes());
  }
  const unfoldAll = laneSummary.querySelector("button[data-lane-action=\"unfold-all\"]");
  if (unfoldAll) {
    unfoldAll.addEventListener("click", () => clearFoldedLanes());
  }
  applyLaneFolding();
}

const defs = el("defs");
const marker = el("marker", { id: "arrow", viewBox: "0 0 10 10", refX: 10, refY: 5, markerWidth: 8, markerHeight: 8, orient: "auto-start-reverse" });
marker.appendChild(el("path", { d: "M 0 0 L 10 5 L 0 10 z", fill: "#64748b" }));
defs.appendChild(marker);
svg.appendChild(defs);

const laneLayer = el("g", { "aria-label": "source lanes" });
laneOrder.forEach((sourceId, idx) => {
  const laneStart = laneYStartBySource.get(sourceId) ?? yStart;
  const laneHeight = laneHeightBySource.get(sourceId) ?? 128;
  const laneRect = el("rect", {
    x: 16,
    y: laneStart,
    width: maxX - 32,
    height: laneHeight,
    class: `source-lane-band${idx % 2 === 1 ? " alt" : ""}`
  });
  laneRectsBySource.set(String(sourceId), laneRect);
  laneLayer.appendChild(laneRect);
  const laneText = el("text", { x: 24, y: laneStart + 16, class: "source-lane-label" });
  laneText.textContent = sourceId === "unmapped" ? "source: unmapped" : `source ${sourceId}: ${sourceLabel(sourceId)}`;
  laneLabelsBySource.set(String(sourceId), laneText);
  laneLayer.appendChild(laneText);
});
svg.appendChild(laneLayer);

const edgePairCounts = edgeLinkCountByEndpoints(graph.edges);
const edgePairSeen = new Map();

for (const edge of graph.edges) {
  const from = positions.get(edge.from);
  const to = positions.get(edge.to);
  if (!from || !to) continue;
  const x1 = from.x + nodeWidth / 2 + 2;
  const y1 = from.y + nodeHeight / 2 - 1;
  const x2 = to.x - nodeWidth / 2 - 2;
  const y2 = to.y + nodeHeight / 2 - 1;
  const pairKey = `${edge.from}|${edge.to}`;
  const parallelCount = edgePairCounts.get(pairKey) ?? 1;
  const parallelIndex = edgePairSeen.get(pairKey) ?? 0;
  edgePairSeen.set(pairKey, parallelIndex + 1);
  const det = hashDeterministic(`${edge.kind}|${edge.from}|${edge.to}|${edge.label}`);
  const bucket = ((det % 5) - 2) + (parallelIndex - (parallelCount - 1) / 2);
  const rawDy = y2 - y1;
  const bend = Math.max(-58, Math.min(58, rawDy * 0.16 + bucket * 8));
  const c1x = x1 + (x2 - x1) * 0.33;
  const c2x = x1 + (x2 - x1) * 0.67;
  const c1y = y1 + bend;
  const c2y = y2 - bend;
  const path = el("path", {
    d: `M ${x1} ${y1} C ${c1x} ${c1y}, ${c2x} ${c2y}, ${x2} ${y2}`,
    class: "edge",
    "marker-end": "url(#arrow)"
  });
  path.classList.add(`edge-kind-${edge.kind}`);
  const edgeRole = edgeRoleForKind(edge.kind);
  path.classList.add(`edge-role-${edgeRole}`);
  const edgeSourceId = sourceIdForEdge(edge);
  if (edgeSourceId) path.setAttribute("data-source-id", edgeSourceId);
  sourceBoundElements.push({ el: path, sourceId: edgeSourceId });
  const edgeKey = edgeKeyFor(edge);
  path.addEventListener("click", () => selectByGraphKey(edgeKey, { pushUrlHistory: true }));
  svg.appendChild(path);

  const dx = x2 - x1;
  const dy = y2 - y1;
  const len = Math.hypot(dx, dy) || 1;
  const nx = -dy / len;
  const ny = dx / len;
  const labelOffset = 10 + Math.abs(bucket) * 4;
  const labelX = (x1 + x2) / 2 + nx * labelOffset;
  const labelY = (y1 + y2) / 2 + ny * labelOffset - 3;
  const label = el("text", { x: labelX, y: labelY, class: `edge-label edge-role-${edgeRole}`, "text-anchor": "middle" });
  if (edgeSourceId) {
    label.setAttribute("data-source-id", edgeSourceId);
    label.classList.add("edge-label-hit");
    label.addEventListener("click", () => selectByGraphKey(edgeKey, { pushUrlHistory: true }));
  }
  sourceBoundElements.push({ el: label, sourceId: edgeSourceId });
  label.textContent = edgeSemanticLabel(edge);
  svg.appendChild(label);
  registerSelectable(edgeKey, [path, label]);
  edgeElements.push({ key: edgeKey, edge, path, label });
}

for (const node of graph.nodes) {
  const p = positions.get(node.id);
  if (!p) continue;
  const g = el("g", { class: "node", transform: `translate(${p.x - 98}, ${p.y})` });
  g.classList.add(`node-kind-${node.kind}`);
  g.classList.add(`node-demand-${nodeDemandState(node)}`);
  g.classList.add(`node-sharing-${nodeSharingState(node)}`);
  const runtimeRole = nodeRuntimeRole(node);
  if (runtimeRole !== "none") g.classList.add(`node-runtime-role-${runtimeRole}`);
  const nodeSourceId = sourceIdForNode(node);
  if (nodeSourceId) g.setAttribute("data-source-id", nodeSourceId);
  sourceBoundElements.push({ el: g, sourceId: nodeSourceId });
  const nodeKey = `node:${node.id}`;
  g.addEventListener("click", () => selectByGraphKey(nodeKey, { pushUrlHistory: true }));

  const rect = el("rect", { width: 196, height: 54 });
  g.appendChild(rect);
  addNodeBadges(g, node);

  const text = el("text", { x: 10, y: 18 });
  const lines = String(node.label).split("\n").slice(0, 2);
  lines.forEach((line, i) => {
    const tspan = el("tspan", { x: 10, dy: i === 0 ? 0 : 14 });
    tspan.textContent = line;
    text.appendChild(tspan);
  });
  g.appendChild(text);
  svg.appendChild(g);
  registerSelectable(nodeKey, [g]);
  nodeElementsById.set(node.id, { node, element: g });
}

for (const sourceId of laneOrder) {
  if (shouldAutoFoldLane(sourceId)) foldedLaneSourceIds.add(sourceId);
}
renderSourceRegistry(graph);
renderLaneSummary();
applySourceFilter("", { skipPersist: true });
if (filterDemand) {
  filterDemand.addEventListener("change", () => {
    semanticDemandFilter = filterDemand.value || "all";
    applySemanticFilters();
    persistViewerState(false);
  });
}
if (filterRuntimeRole) {
  filterRuntimeRole.addEventListener("change", () => {
    semanticRuntimeRoleFilter = filterRuntimeRole.value || "all";
    applySemanticFilters();
    persistViewerState(false);
  });
}
if (filterSharing) {
  filterSharing.addEventListener("change", () => {
    semanticSharingFilter = filterSharing.value || "all";
    applySemanticFilters();
    persistViewerState(false);
  });
}
if (focusNeighborhoodToggle) {
  focusNeighborhoodToggle.addEventListener("change", () => {
    dependencyFocusEnabled = Boolean(focusNeighborhoodToggle.checked);
    applyDependencyFocus();
    applyNoiseReduction();
    persistViewerState(false);
  });
}
if (focusPathMode) {
  focusPathMode.addEventListener("change", () => {
    dependencyFocusMode = focusPathMode.value || "neighborhood";
    applyDependencyFocus();
    applyNoiseReduction();
    persistViewerState(false);
  });
}
if (hideStdlibNoiseToggle) {
  hideStdlibNoiseToggle.addEventListener("change", () => {
    hideStdlibNoiseEnabled = Boolean(hideStdlibNoiseToggle.checked);
    applyNoiseReduction();
    persistViewerState(false);
  });
}
if (selectionHistoryBack) {
  selectionHistoryBack.addEventListener("click", () => {
    if (selectionHistoryIndex <= 0) return;
    selectionHistoryIndex -= 1;
    updateSelectionHistoryButtons();
    selectByGraphKey(selectionHistory[selectionHistoryIndex], { fromHistory: true, pushUrlHistory: false });
  });
}
if (selectionHistoryForward) {
  selectionHistoryForward.addEventListener("click", () => {
    if (selectionHistoryIndex === -1 || selectionHistoryIndex >= selectionHistory.length - 1) return;
    selectionHistoryIndex += 1;
    updateSelectionHistoryButtons();
    selectByGraphKey(selectionHistory[selectionHistoryIndex], { fromHistory: true, pushUrlHistory: false });
  });
}
if (copyShareLinkButton) {
  copyShareLinkButton.addEventListener("click", () => copyShareLink());
}
if (searchQueryInput) {
  searchQueryInput.addEventListener("input", () => {
    setSearchQuery(searchQueryInput.value || "");
  });
  searchQueryInput.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      if (visibleSearchResults.length === 0) return;
      setActiveSearchResultIndex((activeSearchResultIndex + 1) % visibleSearchResults.length);
      return;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      if (visibleSearchResults.length === 0) return;
      const next = activeSearchResultIndex <= 0 ? visibleSearchResults.length - 1 : activeSearchResultIndex - 1;
      setActiveSearchResultIndex(next);
      return;
    }
    if (event.key === "Enter") {
      if (visibleSearchResults.length === 0) return;
      event.preventDefault();
      const idx = activeSearchResultIndex >= 0 ? activeSearchResultIndex : 0;
      setActiveSearchResultIndex(idx);
      activateSearchResult(visibleSearchResults[idx]);
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      searchQueryInput.value = "";
      setSearchQuery("");
      return;
    }
  });
}
if (searchClearButton) {
  searchClearButton.addEventListener("click", () => {
    if (searchQueryInput) searchQueryInput.value = "";
    setSearchQuery("");
  });
}
window.addEventListener("keydown", (event) => {
  const target = event.target;
  const editing =
    (target instanceof HTMLInputElement) ||
    (target instanceof HTMLTextAreaElement) ||
    (target instanceof HTMLSelectElement) ||
    (target && target.isContentEditable);
  if (editing) return;
  if (event.altKey && event.key === "ArrowLeft") {
    event.preventDefault();
    if (selectionHistoryBack) selectionHistoryBack.click();
    return;
  }
  if (event.altKey && event.key === "ArrowRight") {
    event.preventDefault();
    if (selectionHistoryForward) selectionHistoryForward.click();
    return;
  }
  if (event.key === "j") {
    event.preventDefault();
    moveDependencyFocus(1);
    return;
  }
  if (event.key === "k") {
    event.preventDefault();
    moveDependencyFocus(-1);
    return;
  }
  if (event.key === "/" && searchQueryInput) {
    event.preventDefault();
    searchQueryInput.focus();
    searchQueryInput.select();
    return;
  }
});
window.addEventListener("popstate", () => {
  applyViewerStateFromUrl();
});
updateSelectionHistoryButtons();
applySemanticFilters();
const restoredFromUrl = applyViewerStateFromUrl();
svg.style.transformOrigin = "0 0";
svg.style.transform = "scale(1)";
updateZoomStatus();
if (zoomInButton) zoomInButton.addEventListener("click", () => zoomBy(1.15));
if (zoomOutButton) zoomOutButton.addEventListener("click", () => zoomBy(1 / 1.15));
if (zoomResetButton) zoomResetButton.addEventListener("click", () => setViewportScale(1));
if (graphViewport) {
  graphViewport.addEventListener("wheel", (event) => {
    if (!event.ctrlKey && !event.metaKey) return;
    event.preventDefault();
    const factor = event.deltaY < 0 ? 1.08 : 1 / 1.08;
    setViewportScale(viewportScale * factor, event.clientX, event.clientY);
  }, { passive: false });
  let isPanning = false;
  let panLastX = 0;
  let panLastY = 0;
  graphViewport.addEventListener("pointerdown", (event) => {
    if (event.button !== 1) return;
    isPanning = true;
    panLastX = event.clientX;
    panLastY = event.clientY;
    graphViewport.style.cursor = "grabbing";
  });
  graphViewport.addEventListener("pointermove", (event) => {
    if (!isPanning) return;
    graphViewport.scrollLeft -= event.clientX - panLastX;
    graphViewport.scrollTop -= event.clientY - panLastY;
    panLastX = event.clientX;
    panLastY = event.clientY;
  });
  const endPan = () => {
    isPanning = false;
    graphViewport.style.cursor = "";
  };
  graphViewport.addEventListener("pointerup", endPan);
  graphViewport.addEventListener("pointercancel", endPan);
  graphViewport.addEventListener("pointerleave", endPan);
}
if (!restoredFromUrl) {
  setDetails({
    type: "graph",
    graph_type: graph.graph_type,
    source_file: graph.source_file,
    source_file_count: Array.isArray(graph.source_files) ? graph.source_files.length : 0,
    node_count: graph.nodes.length,
    edge_count: graph.edges.length
  });
}
</script>"##,
        );
        out.push_str("\n</body>\n</html>\n");
        out
    }
}

pub fn project_analysis_graph(analysis: &AnalysisResult) -> AnalysisGraph {
    project_analysis_graph_internal(analysis, None, None)
}

pub fn project_analysis_graph_with_bridge(
    analysis: &AnalysisResult,
    bridge: &SingleSourceBridge,
    source_text: Option<&str>,
    source_file: Option<&Path>,
) -> AnalysisGraph {
    let context = BridgeGraphContext::from_bridge(bridge, source_text, source_file);
    project_analysis_graph_internal(analysis, Some(&context), context.source_file.clone())
}

pub fn graph_from_source_text(text: &str) -> AnalysisGraph {
    let bridge = SingleSourceBridge::from_source_text(text);
    let analysis = build_minimal_analysis(&bridge);
    project_analysis_graph_with_bridge(&analysis, &bridge, Some(text), None)
}

pub fn graph_from_file(path: &Path) -> std::io::Result<AnalysisGraph> {
    let bridge = SingleSourceBridge::from_entry_file(path)?;
    let text = fs::read_to_string(path)?;
    let analysis = build_minimal_analysis(&bridge);
    Ok(project_analysis_graph_with_bridge(
        &analysis,
        &bridge,
        Some(&text),
        Some(path),
    ))
}

pub fn export_graph_html_from_file(input: &Path, output: &Path) -> std::io::Result<()> {
    let graph = graph_from_file(input)?;
    fs::write(output, graph.to_standalone_html())
}

fn project_analysis_graph_internal(
    analysis: &AnalysisResult,
    context: Option<&BridgeGraphContext<'_>>,
    source_file: Option<String>,
) -> AnalysisGraph {
    let mut nodes = Vec::new();
    let mut edges = BTreeSet::new();
    let mut shared_extraction_count_by_body = BTreeMap::new();
    let mut extracted_reason_by_runtime = BTreeMap::new();

    for body in &analysis.bodies {
        let mut shared_count = 0usize;
        for extracted in &body.extracted_nodes {
            let reason = match &extracted.reason {
                ExtractedNodeReason::SharedAcrossUses => {
                    shared_count += 1;
                    "shared_across_uses"
                }
                ExtractedNodeReason::EscapesAsLazyResult => "escapes_as_lazy_result",
                ExtractedNodeReason::CapturedByClosureEnv { .. } => "captured_by_closure_env",
                ExtractedNodeReason::LazyProductField { .. } => "lazy_product_field",
                ExtractedNodeReason::RuntimeBoundary { .. } => "runtime_boundary",
            };
            extracted_reason_by_runtime.insert(extracted.id.0, reason);
        }
        shared_extraction_count_by_body.insert(body.id, shared_count);
    }

    for body in &analysis.bodies {
        let body_name = context
            .and_then(|ctx| ctx.body_name(body.id))
            .unwrap_or_else(|| format!("body-{}", body.id.0));
        let shared_count = *shared_extraction_count_by_body
            .get(&body.id)
            .unwrap_or(&0usize);
        let mut metadata = vec![
            ("body_id".to_owned(), body.id.0.to_string()),
            (
                "incoming_use_observation".to_owned(),
                format!("{:?}", body.incoming_use_observation),
            ),
            (
                "result_observation".to_owned(),
                format!("{:?}", body.result_observation),
            ),
            (
                "shared_extracted_node_count".to_owned(),
                shared_count.to_string(),
            ),
            (
                "sharing_role".to_owned(),
                if shared_count > 0 {
                    "shared_reuse_source".to_owned()
                } else {
                    "local_only".to_owned()
                },
            ),
        ];
        if let Some(ctx) = context {
            metadata.push(("body_name".to_owned(), body_name.clone()));
            append_body_span_metadata(&mut metadata, ctx, body.id);
        }
        finalize_metadata(&mut metadata);

        nodes.push(GraphNode {
            id: body_node_id(body.id),
            kind: GraphNodeKind::Body,
            label: format!(
                "Body {} (B{})\nresult={:?}",
                body_name, body.id.0, body.result_observation
            ),
            metadata,
        });
    }

    for kernel in &analysis.kernels {
        let owner_name = context
            .and_then(|ctx| ctx.body_name(kernel.body))
            .unwrap_or_else(|| format!("B{}", kernel.body.0));
        let mut metadata = vec![
            ("kernel_id".to_owned(), kernel.id.0.to_string()),
            ("body_id".to_owned(), kernel.body.0.to_string()),
            (
                "kernel_result_observation".to_owned(),
                format!("{:?}", kernel.result_obs),
            ),
        ];
        if let Some(_ctx) = context {
            metadata.push(("body_name".to_owned(), owner_name.clone()));
        }
        if let Some(ctx) = context {
            append_body_span_metadata(&mut metadata, ctx, kernel.body);
        }
        finalize_metadata(&mut metadata);

        nodes.push(GraphNode {
            id: kernel_node_id(kernel.id.0),
            kind: GraphNodeKind::Kernel,
            label: format!(
                "Kernel K{} in {} (B{})\nresult={:?}",
                kernel.id.0, owner_name, kernel.body.0, kernel.result_obs
            ),
            metadata,
        });
    }

    for runtime in &analysis.runtime_nodes {
        let runtime_id = runtime.id().0;
        let source_expr = runtime_source(runtime);
        let boundary = runtime
            .boundary_kind()
            .map(boundary_kind_name)
            .unwrap_or("Lazy");
        let role_slug = match runtime.boundary_kind() {
            Some(RuntimeBoundaryKind::Exec) => "exec",
            Some(RuntimeBoundaryKind::Discover) => "discover",
            Some(RuntimeBoundaryKind::SharedKeyed) => "shared_keyed",
            None => "lazy",
        };
        let role_summary = match runtime.boundary_kind() {
            Some(RuntimeBoundaryKind::Exec) => "exec boundary",
            Some(RuntimeBoundaryKind::Discover) => "discover boundary",
            Some(RuntimeBoundaryKind::SharedKeyed) => "shared-keyed boundary",
            None => "lazy extracted node",
        };
        let extracted_reason = extracted_reason_by_runtime
            .get(&runtime_id)
            .copied()
            .unwrap_or("runtime_node");
        let sharing_role = if runtime.boundary_kind() == Some(RuntimeBoundaryKind::SharedKeyed)
            || extracted_reason == "shared_across_uses"
        {
            "shared"
        } else {
            "local"
        };
        let source_note = context
            .and_then(|ctx| ctx.expr_short_label(&source_expr))
            .unwrap_or_else(|| format!("B{}:E{}", source_expr.body.0, source_expr.expr.0));
        let mut metadata = vec![
            ("runtime_id".to_owned(), runtime_id.to_string()),
            ("runtime_kind".to_owned(), boundary.to_owned()),
            ("runtime_role".to_owned(), role_slug.to_owned()),
            ("runtime_role_summary".to_owned(), role_summary.to_owned()),
            ("extracted_reason".to_owned(), extracted_reason.to_owned()),
            ("sharing_role".to_owned(), sharing_role.to_owned()),
        ];
        append_source_expr_metadata(&mut metadata, &source_expr, context);
        finalize_metadata(&mut metadata);

        nodes.push(GraphNode {
            id: runtime_node_id(runtime_id),
            kind: GraphNodeKind::RuntimeBoundary,
            label: format!(
                "Runtime R{} [{}]\n{}",
                runtime_id, role_summary, source_note
            ),
            metadata,
        });
    }

    for layout in &analysis.closure_layouts {
        let closure_label = context
            .and_then(|ctx| ctx.closure_name(&layout.source))
            .unwrap_or_else(|| format!("B{}:E{}", layout.source.body.0, layout.source.expr.0));
        let mut metadata = vec![
            ("layout_id".to_owned(), layout.id.0.to_string()),
            (
                "incoming_use_observation".to_owned(),
                format!("{:?}", layout.incoming_use_observation),
            ),
            (
                "capture_count".to_owned(),
                layout.captures.len().to_string(),
            ),
            ("closure_origin".to_owned(), closure_label.clone()),
            (
                "sharing_role".to_owned(),
                if layout.captures.is_empty() {
                    "local".to_owned()
                } else {
                    "closure_env".to_owned()
                },
            ),
        ];
        append_source_expr_metadata(&mut metadata, &layout.source, context);
        finalize_metadata(&mut metadata);

        nodes.push(GraphNode {
            id: layout_node_id(layout.id.0),
            kind: GraphNodeKind::ClosureLayout,
            label: format!("Layout L{}\n{}", layout.id.0, closure_label),
            metadata,
        });
    }

    for body in &analysis.bodies {
        let from = body_node_id(body.id);
        for entry in &body.kernel_entries {
            let mut metadata = vec![
                ("body_id".to_owned(), body.id.0.to_string()),
                ("kernel_id".to_owned(), entry.kernel.0.to_string()),
                (
                    "demanded_result".to_owned(),
                    format!("{:?}", entry.demanded_result),
                ),
            ];
            append_source_expr_metadata(&mut metadata, &entry.source, context);
            finalize_metadata(&mut metadata);
            edges.insert(GraphEdge {
                from: from.clone(),
                to: kernel_node_id(entry.kernel.0),
                kind: GraphEdgeKind::BodyToKernel,
                label: "body->kernel".to_owned(),
                metadata,
            });
        }

        for extracted in &body.extracted_nodes {
            if matches!(
                extracted.reason,
                ExtractedNodeReason::RuntimeBoundary { .. }
            ) {
                let mut metadata = vec![
                    ("body_id".to_owned(), body.id.0.to_string()),
                    ("runtime_id".to_owned(), extracted.id.0.to_string()),
                    ("reason".to_owned(), "runtime_boundary".to_owned()),
                ];
                append_source_expr_metadata(&mut metadata, &extracted.source, context);
                finalize_metadata(&mut metadata);
                edges.insert(GraphEdge {
                    from: from.clone(),
                    to: runtime_node_id(extracted.id.0),
                    kind: GraphEdgeKind::BodyToRuntimeBoundary,
                    label: "body->runtime".to_owned(),
                    metadata,
                });
            }
        }

        for call_edge in &body.source_local_call_edges {
            match call_edge.callee {
                SourceLocalCalleeOrigin::TopLevelBody(target) => {
                    let target_name = context
                        .and_then(|ctx| ctx.body_name(target))
                        .unwrap_or_else(|| format!("B{}", target.0));
                    let mut metadata = vec![
                        ("caller_body_id".to_owned(), body.id.0.to_string()),
                        ("call_expr".to_owned(), call_edge.call_expr.0.to_string()),
                        ("callee_body_id".to_owned(), target.0.to_string()),
                        ("callee_body_name".to_owned(), target_name.clone()),
                    ];
                    append_source_expr_metadata(
                        &mut metadata,
                        &SourceExpr {
                            body: body.id,
                            expr: call_edge.call_expr,
                        },
                        context,
                    );
                    finalize_metadata(&mut metadata);
                    edges.insert(GraphEdge {
                        from: from.clone(),
                        to: body_node_id(target),
                        kind: GraphEdgeKind::CallToBody,
                        label: format!("call E{} -> {}", call_edge.call_expr.0, target_name),
                        metadata,
                    });
                }
                SourceLocalCalleeOrigin::LocalClosure(_) => {
                    if let Some(layout_id) = body.callee_closure_layouts.get(&call_edge.call_expr) {
                        let mut metadata = vec![
                            ("caller_body_id".to_owned(), body.id.0.to_string()),
                            ("call_expr".to_owned(), call_edge.call_expr.0.to_string()),
                            ("callee_layout_id".to_owned(), layout_id.0.to_string()),
                        ];
                        append_source_expr_metadata(
                            &mut metadata,
                            &SourceExpr {
                                body: body.id,
                                expr: call_edge.call_expr,
                            },
                            context,
                        );
                        finalize_metadata(&mut metadata);
                        edges.insert(GraphEdge {
                            from: from.clone(),
                            to: layout_node_id(layout_id.0),
                            kind: GraphEdgeKind::CallToClosureLayout,
                            label: format!("call E{} -> layout", call_edge.call_expr.0),
                            metadata,
                        });
                    }
                }
                SourceLocalCalleeOrigin::LocalDef(_) => {}
            }
        }
    }

    if let Some(ctx) = context {
        let mut boundary_runtime_by_call = BTreeMap::new();
        for body in &analysis.bodies {
            for extracted in &body.extracted_nodes {
                if matches!(
                    extracted.reason,
                    ExtractedNodeReason::RuntimeBoundary { .. }
                ) {
                    boundary_runtime_by_call.insert(
                        (extracted.source.body, extracted.source.expr),
                        extracted.id.0,
                    );
                }
            }
        }
        let layout_by_source = analysis
            .closure_layouts
            .iter()
            .map(|layout| ((layout.source.body, layout.source.expr), layout.id.0))
            .collect::<BTreeMap<_, _>>();
        let producer_call_by_binding = ctx
            .local_boundary_inits
            .iter()
            .map(|init| (init.binding, (init.body, init.call_expr)))
            .collect::<BTreeMap<_, _>>();

        for arg in &ctx.boundary_arg_bindings {
            let Some((producer_body, producer_call_expr)) =
                producer_call_by_binding.get(&arg.binding)
            else {
                continue;
            };
            let Some(producer_runtime_id) =
                boundary_runtime_by_call.get(&(*producer_body, *producer_call_expr))
            else {
                continue;
            };
            let Some(consumer_runtime_id) =
                boundary_runtime_by_call.get(&(arg.body, arg.call_expr))
            else {
                continue;
            };

            let binding_label = ctx.binding_name(arg.binding).unwrap_or("<binding>");
            let mut metadata = vec![
                ("binding_id".to_owned(), arg.binding.0.to_string()),
                ("binding_name".to_owned(), binding_label.to_owned()),
                (
                    "producer_runtime_id".to_owned(),
                    producer_runtime_id.to_string(),
                ),
                (
                    "consumer_runtime_id".to_owned(),
                    consumer_runtime_id.to_string(),
                ),
            ];
            append_source_expr_metadata(
                &mut metadata,
                &SourceExpr {
                    body: arg.body,
                    expr: arg.call_expr,
                },
                context,
            );
            finalize_metadata(&mut metadata);
            edges.insert(GraphEdge {
                from: runtime_node_id(*producer_runtime_id),
                to: runtime_node_id(*consumer_runtime_id),
                kind: GraphEdgeKind::RuntimeValueDependency,
                label: format!("runtime arg `{binding_label}`"),
                metadata,
            });
        }

        for closure_arg in &ctx.boundary_arg_closures {
            let Some(runtime_id) =
                boundary_runtime_by_call.get(&(closure_arg.body, closure_arg.call_expr))
            else {
                continue;
            };
            let Some(layout_id) = layout_by_source.get(&(
                closure_arg.closure_source.body,
                closure_arg.closure_source.expr,
            )) else {
                continue;
            };

            let mut metadata = vec![
                ("runtime_id".to_owned(), runtime_id.to_string()),
                ("layout_id".to_owned(), layout_id.to_string()),
            ];
            append_source_expr_metadata(&mut metadata, &closure_arg.closure_source, context);
            finalize_metadata(&mut metadata);
            edges.insert(GraphEdge {
                from: runtime_node_id(*runtime_id),
                to: layout_node_id(*layout_id),
                kind: GraphEdgeKind::RuntimeBoundaryToClosureLayout,
                label: "boundary owns closure arg".to_owned(),
                metadata,
            });
        }
    }

    nodes.sort_by_key(|node| (node.kind, node.id.clone(), node.label.clone()));

    AnalysisGraph {
        source_file,
        source_files: context.map_or_else(Vec::new, |ctx| ctx.source_files()),
        nodes,
        edges: edges.into_iter().collect(),
    }
}

fn append_body_span_metadata(
    metadata: &mut GraphMetadata,
    context: &BridgeGraphContext<'_>,
    body: BodyId,
) {
    if let Some(span) = context.body_span(body) {
        metadata.push(("span_source_id".to_owned(), span.source.0.to_string()));
        metadata.push(("span_start".to_owned(), u32::from(span.start()).to_string()));
        metadata.push(("span_end".to_owned(), u32::from(span.end()).to_string()));
        if let Some(path) = context.source_path_for_source_id(span.source) {
            metadata.push(("source_path".to_owned(), path.clone()));
        }
    }
}

fn append_source_expr_metadata(
    metadata: &mut GraphMetadata,
    source: &SourceExpr,
    context: Option<&BridgeGraphContext<'_>>,
) {
    metadata.push(("source_body_id".to_owned(), source.body.0.to_string()));
    metadata.push(("source_expr_id".to_owned(), source.expr.0.to_string()));
    if let Some(ctx) = context {
        if let Some(span) = ctx.expr_span(source.expr) {
            metadata.push(("span_source_id".to_owned(), span.source.0.to_string()));
            metadata.push(("span_start".to_owned(), u32::from(span.start()).to_string()));
            metadata.push(("span_end".to_owned(), u32::from(span.end()).to_string()));
            if let Some(path) = ctx.source_path_for_source_id(span.source) {
                metadata.push(("source_path".to_owned(), path.clone()));
            }
        }
        if let Some(snippet) = ctx.expr_snippet(source.expr) {
            metadata.push(("source_snippet".to_owned(), snippet.to_owned()));
            if let Some(highlighted) = highlight_vixen_snippet_html(snippet) {
                metadata.push(("source_snippet_highlighted_html".to_owned(), highlighted));
            }
        }
    }
}

fn runtime_source(runtime: &crate::RuntimeNode) -> SourceExpr {
    match runtime {
        crate::RuntimeNode::Lazy { source, .. }
        | crate::RuntimeNode::Exec { source, .. }
        | crate::RuntimeNode::Discover { source, .. }
        | crate::RuntimeNode::SharedKeyed { source, .. } => source.clone(),
    }
}

fn boundary_kind_name(kind: RuntimeBoundaryKind) -> &'static str {
    match kind {
        RuntimeBoundaryKind::Exec => "Exec",
        RuntimeBoundaryKind::Discover => "Discover",
        RuntimeBoundaryKind::SharedKeyed => "SharedKeyed",
    }
}

fn finalize_metadata(metadata: &mut GraphMetadata) {
    metadata.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    metadata.dedup();
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

fn escape_json(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 8);
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn push_json_str(out: &mut String, value: &str) {
    out.push('"');
    out.push_str(&escape_json(value));
    out.push('"');
}

fn push_json_metadata(out: &mut String, metadata: &GraphMetadata, indent: usize) {
    let pad = " ".repeat(indent);
    let pad_inner = " ".repeat(indent + 2);
    if metadata.is_empty() {
        out.push_str("{}");
        return;
    }
    out.push_str("{\n");
    for (index, (key, value)) in metadata.iter().enumerate() {
        out.push_str(&pad_inner);
        push_json_str(out, key);
        out.push_str(": ");
        push_json_str(out, value);
        if index + 1 != metadata.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str(&pad);
    out.push('}');
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[derive(Debug, Clone)]
struct BridgeGraphContext<'a> {
    body_names: BTreeMap<BodyId, String>,
    binding_names: BTreeMap<BridgeLocalBindingId, String>,
    body_spans: BTreeMap<BodyId, SourceSpan>,
    expr_spans: BTreeMap<ExprId, SourceSpan>,
    expr_snippets: BTreeMap<ExprId, String>,
    closure_names: BTreeMap<(BodyId, ExprId), String>,
    call_site_kinds: BTreeMap<(BodyId, ExprId), BridgeCallSiteKind>,
    boundary_arg_bindings: Vec<crate::BridgeBoundaryArgBinding>,
    boundary_arg_closures: Vec<crate::BridgeBoundaryArgClosure>,
    local_boundary_inits: Vec<crate::BridgeLocalBoundaryInit>,
    source_paths: BTreeMap<u32, String>,
    source_file: Option<String>,
    _source_text: Option<&'a str>,
}

impl<'a> BridgeGraphContext<'a> {
    fn from_bridge(
        bridge: &SingleSourceBridge,
        source_text: Option<&'a str>,
        source_file: Option<&Path>,
    ) -> Self {
        let body_names = bridge
            .bodies
            .iter()
            .map(|body| (body.id, body.name.clone()))
            .collect::<BTreeMap<_, _>>();
        let binding_names = bridge
            .local_bindings
            .iter()
            .map(|binding| (binding.id, binding.name.clone()))
            .collect::<BTreeMap<_, _>>();
        let body_spans = bridge
            .bodies
            .iter()
            .map(|body| (body.id, body.span))
            .collect::<BTreeMap<_, _>>();
        let expr_spans = bridge
            .exprs
            .iter()
            .map(|expr| (expr.id, expr.span))
            .collect::<BTreeMap<_, _>>();

        let mut expr_snippets = BTreeMap::new();
        if let Some(text) = source_text {
            for expr in &bridge.exprs {
                if expr.span.source != bridge.source_id {
                    continue;
                }
                if let Some(snippet) = snippet_for_span(text, expr.span) {
                    expr_snippets.insert(expr.id, snippet);
                }
            }
        }

        let binding_name_refs = bridge
            .local_bindings
            .iter()
            .map(|binding| (binding.id, binding.name.as_str()))
            .collect::<BTreeMap<BridgeLocalBindingId, &str>>();
        let mut closure_names = BTreeMap::new();
        for init in &bridge.local_closure_inits {
            let name = binding_name_refs
                .get(&init.binding)
                .copied()
                .unwrap_or("<closure>");
            closure_names.insert(
                (init.source.body, init.source.expr),
                format!("closure `{name}`"),
            );
        }

        let call_site_kinds = bridge
            .call_sites
            .iter()
            .map(|site| ((site.body, site.expr), site.kind))
            .collect::<BTreeMap<_, _>>();
        let source_paths = bridge
            .source_paths
            .iter()
            .map(|(id, path)| (*id, path.clone()))
            .collect::<BTreeMap<_, _>>();

        Self {
            body_names,
            binding_names,
            body_spans,
            expr_spans,
            expr_snippets,
            closure_names,
            call_site_kinds,
            boundary_arg_bindings: bridge.boundary_arg_bindings.clone(),
            boundary_arg_closures: bridge.boundary_arg_closures.clone(),
            local_boundary_inits: bridge.local_boundary_inits.clone(),
            source_paths,
            source_file: source_file.map(|path| path.display().to_string()),
            _source_text: source_text,
        }
    }

    fn body_name(&self, body: BodyId) -> Option<String> {
        self.body_names.get(&body).cloned()
    }

    fn body_span(&self, body: BodyId) -> Option<SourceSpan> {
        self.body_spans.get(&body).copied()
    }

    fn expr_span(&self, expr: ExprId) -> Option<SourceSpan> {
        self.expr_spans.get(&expr).copied()
    }

    fn expr_snippet(&self, expr: ExprId) -> Option<&str> {
        self.expr_snippets.get(&expr).map(String::as_str)
    }

    fn expr_short_label(&self, source: &SourceExpr) -> Option<String> {
        let snippet = self.expr_snippet(source.expr)?;
        let call_kind = self
            .call_site_kinds
            .get(&(source.body, source.expr))
            .copied()
            .map(call_site_kind_name)
            .unwrap_or("expr");
        Some(format!("{call_kind}: {snippet}"))
    }

    fn closure_name(&self, source: &SourceExpr) -> Option<String> {
        if let Some(name) = self.closure_names.get(&(source.body, source.expr)) {
            return Some(name.clone());
        }
        self.expr_snippet(source.expr)
            .map(|snippet| format!("closure `{snippet}`"))
    }

    fn source_path_for_source_id(&self, source_id: vx_resolve::SourceId) -> Option<&String> {
        self.source_paths
            .get(&source_id.0)
            .or(self.source_file.as_ref())
    }

    fn source_files(&self) -> Vec<(u32, String)> {
        self.source_paths
            .iter()
            .map(|(id, path)| (*id, path.clone()))
            .collect()
    }

    fn binding_name(&self, binding: BridgeLocalBindingId) -> Option<&str> {
        self.binding_names.get(&binding).map(String::as_str)
    }
}

fn call_site_kind_name(kind: BridgeCallSiteKind) -> &'static str {
    match kind {
        BridgeCallSiteKind::DirectNamedFunction => "direct-call",
        BridgeCallSiteKind::LocalCallableBinding => "local-call",
        BridgeCallSiteKind::StdlibBoundary(_) => "boundary-call",
        BridgeCallSiteKind::Other => "call",
    }
}

fn snippet_for_span(source_text: &str, span: SourceSpan) -> Option<String> {
    let start: usize = u32::from(span.start()) as usize;
    let end: usize = u32::from(span.end()) as usize;
    let slice = source_text.get(start..end)?;
    let trimmed = slice.trim();
    if trimmed.is_empty() {
        None
    } else {
        let max = 96;
        let shortened: String = trimmed.chars().take(max).collect();
        if trimmed.chars().count() <= max {
            Some(trimmed.to_owned())
        } else {
            Some(format!("{shortened}…"))
        }
    }
}

struct SnippetHighlighter {
    highlighter: Highlighter,
}

impl SnippetHighlighter {
    fn try_new() -> Result<Self, String> {
        let grammar = Arc::new(
            CompiledGrammar::new(GrammarConfig {
                language: tree_sitter_vixen::language().into(),
                highlights_query: tree_sitter_vixen::HIGHLIGHTS_QUERY,
                injections_query: tree_sitter_vixen::INJECTIONS_QUERY,
                locals_query: tree_sitter_vixen::LOCALS_QUERY,
            })
            .map_err(|err| err.to_string())?,
        );

        let store = Arc::new(GrammarStore::new());
        store.insert("vixen", grammar.clone());
        store.insert("vx", grammar);
        let config = Config {
            html_format: HtmlFormat::ClassNamesWithPrefix("arb".to_owned()),
            ..Config::default()
        };

        Ok(Self {
            highlighter: Highlighter::with_store_and_config(store, config),
        })
    }

    fn highlight_vixen(&mut self, source: &str) -> Option<String> {
        self.highlighter
            .highlight("vixen", source)
            .or_else(|_| self.highlighter.highlight("vx", source))
            .ok()
    }
}

fn highlight_vixen_snippet_html(snippet: &str) -> Option<String> {
    static HIGHLIGHTER: OnceLock<Mutex<Option<SnippetHighlighter>>> = OnceLock::new();
    let cell = HIGHLIGHTER.get_or_init(|| Mutex::new(SnippetHighlighter::try_new().ok()));
    let mut guard = cell.lock().ok()?;
    let highlighter = guard.as_mut()?;
    highlighter.highlight_vixen(snippet)
}
