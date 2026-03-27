use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

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
        out.push_str("  :root { color-scheme: light; }\n");
        out.push_str("  body { margin: 0; font-family: ui-sans-serif, system-ui, -apple-system, Segoe UI, sans-serif; background: #f8fafc; color: #0f172a; }\n");
        out.push_str("  .header { padding: 16px 20px; border-bottom: 1px solid #cbd5e1; background: linear-gradient(90deg, #e2e8f0, #f8fafc); }\n");
        out.push_str("  .title { margin: 0 0 6px 0; font-size: 18px; }\n");
        out.push_str("  .sub { margin: 0; color: #334155; font-size: 13px; }\n");
        out.push_str("  .layout { display: grid; grid-template-columns: 1fr 380px; min-height: calc(100vh - 84px); }\n");
        out.push_str("  .canvas-wrap { padding: 10px; overflow: auto; }\n");
        out.push_str(
            "  .panel { border-left: 1px solid #cbd5e1; background: #ffffff; padding: 12px; }\n",
        );
        out.push_str("  .panel h2 { margin: 0 0 8px 0; font-size: 14px; }\n");
        out.push_str("  .detail-card { border: 1px solid #e2e8f0; border-radius: 10px; background: #f8fafc; padding: 10px; margin-bottom: 10px; }\n");
        out.push_str("  .detail-title { margin: 0 0 8px 0; font-size: 13px; font-weight: 650; }\n");
        out.push_str("  .detail-row { display: grid; grid-template-columns: 120px 1fr; gap: 8px; margin-bottom: 6px; font-size: 12px; }\n");
        out.push_str("  .detail-row:last-child { margin-bottom: 0; }\n");
        out.push_str("  .detail-key { color: #334155; }\n");
        out.push_str("  .detail-val { color: #0f172a; word-break: break-word; }\n");
        out.push_str("  .snippet { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; white-space: pre-wrap; background: #0f172a; color: #e2e8f0; border-radius: 8px; padding: 8px; margin-top: 6px; }\n");
        out.push_str("  .source-registry { border: 1px solid #e2e8f0; border-radius: 10px; background: #f8fafc; padding: 10px; margin-bottom: 12px; font-size: 12px; }\n");
        out.push_str("  .source-registry ul { margin: 8px 0 0 0; padding-left: 18px; }\n");
        out.push_str("  .source-registry li { margin: 4px 0; word-break: break-word; }\n");
        out.push_str("  details.raw-json { margin-top: 8px; }\n");
        out.push_str(
            "  details.raw-json > summary { cursor: pointer; font-size: 12px; color: #1d4ed8; }\n",
        );
        out.push_str("  .raw-json-pre { margin-top: 8px; white-space: pre-wrap; word-break: break-word; font-size: 11px; line-height: 1.35; background: #fff; border: 1px solid #e2e8f0; border-radius: 8px; padding: 8px; }\n");
        out.push_str("  .legend { margin: 8px 0 0 0; font-size: 12px; color: #334155; }\n");
        out.push_str(
            "  .node rect { fill: #fff; stroke: #475569; stroke-width: 1.2; rx: 8; ry: 8; }\n",
        );
        out.push_str("  .node text { font-size: 12px; fill: #0f172a; pointer-events: none; }\n");
        out.push_str("  .edge { stroke: #64748b; stroke-width: 1.3; fill: none; }\n");
        out.push_str("  .edge-label { font-size: 11px; fill: #1e293b; }\n");
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
        out.push_str("<div class=\"canvas-wrap\"><svg id=\"graph-svg\" width=\"1100\" height=\"800\" aria-label=\"structural graph\"></svg></div>\n");
        out.push_str("<aside class=\"panel\"><h2>Source Registry</h2><div id=\"source-registry\" class=\"source-registry\"></div><h2>Selected Item</h2><div id=\"details\"></div></aside>\n");
        out.push_str("</div>\n");
        out.push_str("<script id=\"graph-json\" type=\"application/json\">");
        out.push_str(&graph_json);
        out.push_str("</script>\n");
        out.push_str(
            r##"<script>
const graph = JSON.parse(document.getElementById("graph-json").textContent);
const svg = document.getElementById("graph-svg");
const details = document.getElementById("details");
const sourceRegistry = document.getElementById("source-registry");

const columns = {
  body: 120,
  kernel: 390,
  runtime_boundary: 670,
  closure_layout: 940,
};
const ySpacing = 112;
const yStart = 70;

const grouped = new Map();
for (const node of graph.nodes) {
  if (!grouped.has(node.kind)) grouped.set(node.kind, []);
  grouped.get(node.kind).push(node);
}

const positions = new Map();
for (const [kind, items] of grouped.entries()) {
  const x = columns[kind] ?? 120;
  items.forEach((node, i) => {
    const y = yStart + i * ySpacing;
    positions.set(node.id, { x, y });
  });
}

const maxY = Math.max(800, ...Array.from(positions.values()).map(p => p.y + 100));
svg.setAttribute("height", String(maxY));

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
function setDetails(value) {
  const metadata = normalizeMetadata(value.metadata);
  const kind = value.kind || value.type || "graph";
  const label = value.label || "";
  const sourcePath = metadata.source_path || value.source_file || "";
  const span = formatSpan(metadata);
  const snippet = metadata.source_snippet || "";

  const baseRows = [
    detailRow("Type", value.type || "graph"),
    detailRow("Kind", kind),
    label ? detailRow("Label", label) : "",
    value.id ? detailRow("ID", value.id) : "",
    value.from && value.to ? detailRow("Endpoints", `${value.from} -> ${value.to}`) : "",
    sourcePath ? detailRow("Source Path", sourcePath) : "",
    span ? detailRow("Span", span) : "",
  ].join("");

  const metadataRows = Object.entries(metadata)
    .filter(([key]) => !["source_path", "source_snippet", "span_source_id", "span_start", "span_end"].includes(key))
    .sort((a, b) => a[0].localeCompare(b[0]))
    .map(([key, val]) => detailRow(key, val))
    .join("");

  const snippetBlock = snippet
    ? `<div class="detail-card"><div class="detail-title">Source Snippet</div><div class="snippet">${htmlEscape(snippet)}</div></div>`
    : "";

  const metadataBlock = metadataRows
    ? `<div class="detail-card"><div class="detail-title">Metadata</div>${metadataRows}</div>`
    : "";

  details.innerHTML = `
    <div class="detail-card">
      <div class="detail-title">Summary</div>
      ${baseRows}
      ${!label && !sourcePath && !span ? `<div class="detail-row"><div class="detail-key">Info</div><div class="detail-val">Select a node or edge.</div></div>` : ""}
    </div>
    ${snippetBlock}
    ${metadataBlock}
    <details class="raw-json">
      <summary>Raw JSON</summary>
      <pre class="raw-json-pre">${htmlEscape(JSON.stringify(value, null, 2))}</pre>
    </details>
  `;
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
    .map((file) => `<li><strong>${htmlEscape(file.source_id)}</strong>: ${htmlEscape(file.path)}</li>`)
    .join("");
  sourceRegistry.innerHTML = `
    <div>This graph includes ${files.length} source file(s).</div>
    <ul>${list}</ul>
  `;
}

const defs = el("defs");
const marker = el("marker", { id: "arrow", viewBox: "0 0 10 10", refX: 10, refY: 5, markerWidth: 8, markerHeight: 8, orient: "auto-start-reverse" });
marker.appendChild(el("path", { d: "M 0 0 L 10 5 L 0 10 z", fill: "#64748b" }));
defs.appendChild(marker);
svg.appendChild(defs);

for (const edge of graph.edges) {
  const from = positions.get(edge.from);
  const to = positions.get(edge.to);
  if (!from || !to) continue;
  const line = el("line", {
    x1: from.x + 100, y1: from.y + 24,
    x2: to.x - 100, y2: to.y + 24,
    class: "edge",
    "marker-end": "url(#arrow)"
  });
  line.addEventListener("click", () => setDetails({ type: "edge", ...edge }));
  svg.appendChild(line);

  const label = el("text", { x: (from.x + to.x) / 2, y: (from.y + to.y) / 2 - 6, class: "edge-label", "text-anchor": "middle" });
  label.textContent = edge.label;
  label.addEventListener("click", () => setDetails({ type: "edge", ...edge }));
  svg.appendChild(label);
}

for (const node of graph.nodes) {
  const p = positions.get(node.id);
  if (!p) continue;
  const g = el("g", { class: "node", transform: `translate(${p.x - 98}, ${p.y})` });
  g.addEventListener("click", () => setDetails({ type: "node", ...node }));

  const rect = el("rect", { width: 196, height: 54 });
  g.appendChild(rect);

  const text = el("text", { x: 10, y: 18 });
  const lines = String(node.label).split("\n").slice(0, 2);
  lines.forEach((line, i) => {
    const tspan = el("tspan", { x: 10, dy: i === 0 ? 0 : 14 });
    tspan.textContent = line;
    text.appendChild(tspan);
  });
  g.appendChild(text);
  svg.appendChild(g);
}

setDetails({
  type: "graph",
  graph_type: graph.graph_type,
  source_file: graph.source_file,
  source_file_count: Array.isArray(graph.source_files) ? graph.source_files.length : 0,
  node_count: graph.nodes.length,
  edge_count: graph.edges.length
});
renderSourceRegistry(graph);
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

    for body in &analysis.bodies {
        let body_name = context
            .and_then(|ctx| ctx.body_name(body.id))
            .unwrap_or_else(|| format!("body-{}", body.id.0));
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
        let source_note = context
            .and_then(|ctx| ctx.expr_short_label(&source_expr))
            .unwrap_or_else(|| format!("B{}:E{}", source_expr.body.0, source_expr.expr.0));
        let mut metadata = vec![
            ("runtime_id".to_owned(), runtime_id.to_string()),
            ("runtime_kind".to_owned(), boundary.to_owned()),
        ];
        append_source_expr_metadata(&mut metadata, &source_expr, context);
        finalize_metadata(&mut metadata);

        nodes.push(GraphNode {
            id: runtime_node_id(runtime_id),
            kind: GraphNodeKind::RuntimeBoundary,
            label: format!("Runtime {} R{}\n{}", boundary, runtime_id, source_note),
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
    let normalized = slice.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        None
    } else {
        let max = 96;
        if normalized.len() <= max {
            Some(normalized)
        } else {
            Some(format!("{}…", &normalized[..max]))
        }
    }
}
