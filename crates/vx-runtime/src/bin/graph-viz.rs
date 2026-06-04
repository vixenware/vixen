//! Emit a self-contained HTML playground of [`DemandGraph`]s — the inside-out
//! turn, made visible (the demand-driven runtime, wiki). A dropdown lets you click
//! through every function; each renders its demand graph with nodes coloured by
//! kind and **dim where demand never reaches** (a bomb in the road not taken).
//!
//! ```text
//! # all functions in the three lang test files (default):
//! cargo run -p vx-runtime --bin graph-viz > crates/vx-runtime/playground/lang.html
//! # or specific files:
//! cargo run -p vx-runtime --bin graph-viz -- path/to/a.vx path/to/b.vx > out.html
//! ```
//! then serve the dir (`python3 -m http.server`) and open it.

use facet::Facet;
use vx_runtime::analyze::analyze_all;
use vx_runtime::graph::{DemandGraph, NodeKind};

/// Viewer-friendly node: `kind`/`ty` as plain strings, `reached` precomputed, so
/// the page needs no knowledge of the Rust enums or the demand walk.
#[derive(Facet)]
struct ViewNode {
    id: u32,
    label: String,
    kind: String,
    ty: String,
    reached: bool,
    inputs: Vec<u32>,
}

/// One named graph in the picker.
#[derive(Facet)]
struct ViewGraph {
    title: String,
    root: u32,
    nodes: Vec<ViewNode>,
}

fn kind_str(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Const(_) => "Const",
        NodeKind::Param => "Param",
        NodeKind::Kernel(_) => "Kernel",
        NodeKind::LazyData => "LazyData",
        NodeKind::Project { .. } => "Project",
        NodeKind::Variant { .. } => "Variant",
        NodeKind::Closure => "Closure",
        NodeKind::Match { .. } => "Match",
        NodeKind::Fallible { .. } => "Fallible",
        NodeKind::CaughtFailure => "CaughtFailure",
        NodeKind::HostMethod { .. } => "HostMethod",
        NodeKind::Boundary => "Boundary",
        NodeKind::Bomb => "Bomb",
        NodeKind::Fail => "Fail",
        NodeKind::Unresolved => "Unresolved",
    }
}

fn view_nodes(g: &DemandGraph) -> Vec<ViewNode> {
    let reached = g.demand_reachable();
    g.nodes
        .iter()
        .enumerate()
        .map(|(i, n)| ViewNode {
            id: n.id.0,
            label: n.label.clone(),
            kind: kind_str(&n.kind).to_string(),
            ty: n.ty.clone(),
            reached: reached[i],
            inputs: n.inputs.iter().map(|d| d.0).collect(),
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let files: Vec<String> = if args.is_empty() {
        ["demand_basis", "which_field_which_graph", "poisons"]
            .iter()
            .map(|f| format!("crates/vx-rt/tests/lang/{f}.vx"))
            .collect()
    } else {
        args
    };

    let mut graphs: Vec<ViewGraph> = Vec::new();
    for path in &files {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("skip {path}: {e}");
                continue;
            }
        };
        let stem = std::path::Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        match analyze_all(&src) {
            Ok(fns) => {
                for (name, g) in fns {
                    graphs.push(ViewGraph {
                        title: format!("{stem} :: {name}"),
                        root: g.root.0,
                        nodes: view_nodes(&g),
                    });
                }
            }
            Err(e) => eprintln!("skip {path}: {e}"),
        }
    }

    if graphs.is_empty() {
        eprintln!("no graphs produced");
        std::process::exit(1);
    }
    let json = facet_json::to_string(&graphs).expect("graphs serialise");
    print!("{}", render_html(&json));
}

fn render_html(graphs_json: &str) -> String {
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>vixen demand graphs</title>
<script src="https://unpkg.com/cytoscape@3.30.2/dist/cytoscape.min.js"></script>
<style>
  body {{ margin: 0; font: 14px ui-monospace, monospace; background: #11141a; color: #cdd3de; }}
  header {{ padding: 10px 16px; border-bottom: 1px solid #2a2f3a; display: flex; gap: 14px; align-items: center; }}
  select {{ background: #1b1f29; color: #e7ecf3; border: 1px solid #2a2f3a; padding: 4px 8px; font: inherit; }}
  #cy {{ width: 100vw; height: calc(100vh - 104px); }}
  .legend {{ padding: 6px 16px; display: flex; gap: 14px; flex-wrap: wrap; border-bottom: 1px solid #2a2f3a; }}
  .legend span {{ display: inline-flex; align-items: center; gap: 5px; }}
  .dot {{ width: 11px; height: 11px; border-radius: 3px; display: inline-block; }}
</style>
</head>
<body>
<header>
  <b>vixen — the inside-out turn</b>
  <select id="picker"></select>
  <span>dim = unreached by demand (the road not taken)</span>
</header>
<div class="legend" id="legend"></div>
<div id="cy"></div>
<script>
const GRAPHS = {graphs_json};
const COLORS = {{
  Const:"#9e9e9e", Param:"#42a5f5", Kernel:"#66bb6a", LazyData:"#ab47bc",
  Project:"#ffa726", Boundary:"#ef5350", Bomb:"#161616", Fail:"#b71c1c", Unresolved:"#fdd835"
}};
const LEGEND = {{ Kernel:"eager / JIT", LazyData:"lifted structure", Project:"selects a slot",
  Boundary:"effect", Bomb:"forcing poisons", Fail:"explicit fail", Const:"literal", Param:"input", Unresolved:"frontier" }};

const legend = document.getElementById("legend");
for (const [k, desc] of Object.entries(LEGEND)) {{
  const s = document.createElement("span");
  s.innerHTML = `<span class="dot" style="background:${{COLORS[k]}}"></span>${{k}} — ${{desc}}`;
  legend.appendChild(s);
}}

const picker = document.getElementById("picker");
GRAPHS.forEach((g, i) => {{
  const o = document.createElement("option");
  o.value = i; o.textContent = g.title;
  picker.appendChild(o);
}});

const STYLE = [
  {{ selector: "node", style: {{
    "background-color": (e) => COLORS[e.data("kind")] || "#888",
    "label": "data(label)", "color": "#e7ecf3", "font-size": 11,
    "text-wrap": "wrap", "text-valign": "center", "text-halign": "center",
    "text-outline-color": "#11141a", "text-outline-width": 2,
    "width": 72, "height": 72, "shape": "round-rectangle",
    "opacity": (e) => e.data("reached") ? 1.0 : 0.22,
    "border-width": (e) => (e.data("kind") === "Bomb" || e.data("kind") === "Fail") ? 2 : 0,
    "border-style": "dashed", "border-color": "#ef5350"
  }} }},
  {{ selector: "edge", style: {{
    "width": 2, "line-color": "#3a4150", "target-arrow-color": "#3a4150",
    "target-arrow-shape": "triangle", "curve-style": "bezier"
  }} }},
];

let cy = null;
function render(i) {{
  const g = GRAPHS[i];
  const elements = [];
  for (const n of g.nodes) {{
    const ty = n.ty && n.ty !== "?" ? ` : ${{n.ty}}` : "";
    elements.push({{ data: {{ id: String(n.id), label: `${{n.label}}${{ty}}\n#${{n.id}} ${{n.kind}}`, kind: n.kind, reached: n.reached }} }});
  }}
  for (const n of g.nodes) {{
    for (const dep of n.inputs) {{
      elements.push({{ data: {{ source: String(n.id), target: String(dep) }} }});
    }}
  }}
  if (cy) cy.destroy();
  cy = cytoscape({{
    container: document.getElementById("cy"),
    elements, style: STYLE,
    layout: {{ name: "breadthfirst", directed: true, roots: "#" + g.root, padding: 30, spacingFactor: 1.3 }},
  }});
}}

picker.addEventListener("change", () => render(Number(picker.value)));
render(0);
</script>
</body>
</html>
"##
    )
}
