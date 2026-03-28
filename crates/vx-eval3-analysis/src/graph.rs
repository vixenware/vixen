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
        out.push_str("  .selected { opacity: 1 !important; }\n");
        out.push_str("  .node.selected rect { stroke: #f1f5f9; stroke-width: 2.4; filter: drop-shadow(0 0 4px rgba(241,245,249,0.35)); }\n");
        out.push_str("  .edge.selected { stroke-width: 2.5; filter: drop-shadow(0 0 3px rgba(241,245,249,0.35)); }\n");
        out.push_str("  .edge-label.selected { fill: #f8fafc; font-weight: 700; }\n");
        out.push_str("  .source-lane-band.selected { stroke: #7dd3fc; stroke-width: 1.6; }\n");
        out.push_str(
            "  .semantic-summary { margin: 8px 0 0 0; font-size: 12px; color: #8ba5c1; }\n",
        );
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
        out.push_str("<aside class=\"panel\"><h2>Source Registry</h2><div id=\"source-registry\" class=\"source-registry\"></div><h2>Selected Item</h2><div id=\"details\"></div></aside>\n");
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
const details = document.getElementById("details");
const sourceRegistry = document.getElementById("source-registry");
let sourceFilterSourceId = "";
const sourceBoundElements = [];
const selectableElements = new Map();
const laneRectsBySource = new Map();
let selectedGraphKey = "";
let selectedSourceId = "";
let viewportScale = 1;

const nodeWidth = 196;
const nodeHeight = 54;
const yStart = 64;
const nodeSpacing = 72;
const kindOrder = ["body", "kernel", "runtime_boundary", "closure_layout"];
const columns = {
  body: 120,
  kernel: 400,
  runtime_boundary: 680,
  closure_layout: 960,
};

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
  if (!Array.isArray(entries)) return {};
  return Object.fromEntries(entries);
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
function addNodeBadges(group, node) {
  const badges = [];
  const demand = nodeDemandState(node);
  if (demand !== "unknown") badges.push({ role: "demand", text: demand === "demanded" ? "demanded" : "idle" });
  const sharing = nodeSharingState(node);
  if (sharing !== "local") badges.push({ role: "sharing", text: sharing.replaceAll("_", "-") });
  const runtimeRole = nodeRuntimeRole(node);
  if (runtimeRole !== "none") badges.push({ role: "runtime", text: runtimeRole.replaceAll("_", "-") });
  badges.slice(0, 3).forEach((badge, idx) => {
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
  const laneStart = laneYStartBySource.get(String(sourceId));
  if (laneStart === undefined) return;
  const laneHeight = laneHeightBySource.get(String(sourceId)) ?? 128;
  scrollViewportTo(columns.kernel, laneStart + laneHeight / 2);
  for (const [id, rect] of laneRectsBySource.entries()) {
    rect.classList.toggle("selected", id === String(sourceId));
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
function selectGraphItem(key, detailsValue, sourceId) {
  setSelectedGraphElements(key);
  selectedSourceId = String(sourceId ?? "");
  setDetails(detailsValue);
  applySourceFilter(sourceFilterSourceId);
  if (selectedSourceId) focusSourceLane(selectedSourceId);
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
function applySourceFilter(sourceId) {
  sourceFilterSourceId = String(sourceId ?? "");
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
      ${!label && !sourcePath && !span ? `<div class="detail-row"><div class="detail-key">Info</div><div class="detail-val">Select a node or edge.</div></div>` : ""}
    </div>
    ${snippetBlock}
    ${metadataBlock}
    <details class="raw-json">
      <summary>Raw JSON</summary>
      <pre class="raw-json-pre">${htmlEscape(JSON.stringify(value, null, 2))}</pre>
    </details>
  `;
  highlightSourceInRegistry(sourceId);
  if (sourceFilterSourceId === "") {
    applySourceFilter(sourceId);
  }
}
function renderSourceRegistry(graph) {
  const files = Array.isArray(graph.source_files) ? graph.source_files : [];
  if (files.length === 0) {
    sourceRegistry.innerHTML = `<div>No source registry for this graph.</div>`;
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
  const items = sourceRegistry.querySelectorAll("li[data-source-id]");
  for (const item of items) {
    item.addEventListener("click", () => {
      const next = item.dataset.sourceId === sourceFilterSourceId ? "" : item.dataset.sourceId;
      applySourceFilter(next);
      if (next) {
        focusSourceLane(next);
      }
    });
  }
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
  const edgeSourceId = String(edge?.metadata?.span_source_id ?? "");
  if (edgeSourceId) path.setAttribute("data-source-id", edgeSourceId);
  sourceBoundElements.push({ el: path, sourceId: edgeSourceId });
  const edgeKey = `edge:${edge.from}->${edge.to}:${edge.kind}:${edge.label}`;
  path.addEventListener("click", () => selectGraphItem(edgeKey, { type: "edge", ...edge }, edgeSourceId));
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
    label.addEventListener("click", () => selectGraphItem(edgeKey, { type: "edge", ...edge }, edgeSourceId));
  }
  sourceBoundElements.push({ el: label, sourceId: edgeSourceId });
  label.textContent = edgeSemanticLabel(edge);
  svg.appendChild(label);
  registerSelectable(edgeKey, [path, label]);
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
  const nodeSourceId = String(node?.metadata?.span_source_id ?? "");
  if (nodeSourceId) g.setAttribute("data-source-id", nodeSourceId);
  sourceBoundElements.push({ el: g, sourceId: nodeSourceId });
  const nodeKey = `node:${node.id}`;
  g.addEventListener("click", () => {
    selectGraphItem(nodeKey, { type: "node", ...node }, nodeSourceId);
    const p = positions.get(node.id);
    if (p) scrollViewportTo(p.x, p.y + nodeHeight / 2);
  });

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
}

renderSourceRegistry(graph);
applySourceFilter("");
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
setDetails({
  type: "graph",
  graph_type: graph.graph_type,
  source_file: graph.source_file,
  source_file_count: Array.isArray(graph.source_files) ? graph.source_files.length : 0,
  node_count: graph.nodes.length,
  edge_count: graph.edges.length
});
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
