//! Emit a self-contained HTML view of a [`DemandGraph`] — the inside-out turn,
//! made visible. The point of the playground (`runtime-semantics.md` §2/§8): you
//! *see* what became a kernel, what became a referenced lazy-data node, where the
//! boundaries fell — and which nodes demand never reaches (the bomb sits dark).
//!
//! ```text
//! cargo run -p vx-runtime --bin graph-viz > crates/vx-runtime/playground/bomb-pair.html
//! ```
//! then open the file in a browser. (Today it renders the hand-built
//! `example_bomb_pair`; once the VIR→graph analysis lands, this same viewer
//! renders arbitrary zoo programs.)

use facet::Facet;
use vx_runtime::analyze::analyze_source;
use vx_runtime::graph::{DemandGraph, NodeKind};

/// A viewer-friendly projection of a node: `kind` as a plain string and a
/// precomputed `reached` flag, so the page needs no knowledge of the Rust enum
/// encoding and no reachability logic of its own.
#[derive(Facet)]
struct ViewNode {
    id: u32,
    label: String,
    kind: String,
    reached: bool,
    inputs: Vec<u32>,
}

fn kind_str(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Const => "Const",
        NodeKind::Param => "Param",
        NodeKind::Kernel => "Kernel",
        NodeKind::LazyData => "LazyData",
        NodeKind::Project { .. } => "Project",
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
            reached: reached[i],
            inputs: n.inputs.iter().map(|d| d.0).collect(),
        })
        .collect()
}

fn main() {
    // `graph-viz <file.vx> <entry-fn>` analyses a real program; with no args it
    // analyses a built-in sample. Either way the graph comes from the **real**
    // front-end + VIR walk (analyze.rs), not a hand-built fixture.
    let args: Vec<String> = std::env::args().collect();
    let (g, title) = if args.len() >= 3 {
        let src = std::fs::read_to_string(&args[1]).expect("read source file");
        match analyze_source(&src, &args[2]) {
            Ok(g) => (g, format!("{} :: {}", args[1], args[2])),
            Err(e) => {
                eprintln!("analysis failed: {e}");
                std::process::exit(1);
            }
        }
    } else {
        let src = "fn demo() -> U64 ( let pair = (42, 💣); pair[0] )";
        let g = analyze_source(src, "demo").expect("sample analyses");
        (
            g,
            "demo() = { let pair = (42, 💣); pair[0] }  ·  real analysis".to_string(),
        )
    };
    let nodes = view_nodes(&g);
    let json = facet_json::to_string(&nodes).expect("view nodes serialise");
    print!("{}", render_html(&json, g.root.0, &title));
}

fn render_html(nodes_json: &str, root: u32, title: &str) -> String {
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>vixen demand graph — {title}</title>
<script src="https://unpkg.com/cytoscape@3.30.2/dist/cytoscape.min.js"></script>
<style>
  body {{ margin: 0; font: 14px ui-monospace, monospace; background: #11141a; color: #cdd3de; }}
  header {{ padding: 10px 16px; border-bottom: 1px solid #2a2f3a; }}
  header b {{ color: #fff; }}
  #cy {{ width: 100vw; height: calc(100vh - 96px); }}
  .legend {{ padding: 6px 16px; display: flex; gap: 14px; flex-wrap: wrap; border-bottom: 1px solid #2a2f3a; }}
  .legend span {{ display: inline-flex; align-items: center; gap: 5px; }}
  .dot {{ width: 11px; height: 11px; border-radius: 3px; display: inline-block; }}
</style>
</head>
<body>
<header>vixen — the inside-out turn: <b>{title}</b> &nbsp;·&nbsp; dim = unreached by demand (the road not taken)</header>
<div class="legend" id="legend"></div>
<div id="cy"></div>
<script>
const NODES = {nodes_json};
const ROOT = "{root}";
const COLORS = {{
  Const:"#9e9e9e", Param:"#42a5f5", Kernel:"#66bb6a", LazyData:"#ab47bc",
  Project:"#ffa726", Boundary:"#ef5350", Bomb:"#161616", Fail:"#b71c1c", Unresolved:"#fdd835"
}};
const LEGEND = {{ Kernel:"eager / JIT", LazyData:"lifted structure", Project:"selects a slot",
  Boundary:"effect (exec/intrinsic)", Bomb:"forcing poisons", Fail:"explicit fail", Const:"literal", Param:"input", Unresolved:"dynamic frontier" }};

const legend = document.getElementById("legend");
for (const [k, desc] of Object.entries(LEGEND)) {{
  const s = document.createElement("span");
  s.innerHTML = `<span class="dot" style="background:${{COLORS[k]}}"></span>${{k}} — ${{desc}}`;
  legend.appendChild(s);
}}

const elements = [];
for (const n of NODES) {{
  elements.push({{ data: {{ id: String(n.id), label: `${{n.label}}\n#${{n.id}} ${{n.kind}}`, kind: n.kind, reached: n.reached }} }});
}}
for (const n of NODES) {{
  for (const dep of n.inputs) {{
    elements.push({{ data: {{ source: String(n.id), target: String(dep) }} }});
  }}
}}

cytoscape({{
  container: document.getElementById("cy"),
  elements,
  style: [
    {{ selector: "node", style: {{
      "background-color": (e) => COLORS[e.data("kind")] || "#888",
      "label": "data(label)", "color": "#e7ecf3", "font-size": 11,
      "text-wrap": "wrap", "text-valign": "center", "text-halign": "center",
      "text-outline-color": "#11141a", "text-outline-width": 2,
      "width": 64, "height": 64, "shape": "round-rectangle",
      "opacity": (e) => e.data("reached") ? 1.0 : 0.22,
      "border-width": (e) => e.data("kind") === "Bomb" ? 2 : 0,
      "border-style": "dashed", "border-color": "#ef5350"
    }} }},
    {{ selector: "edge", style: {{
      "width": 2, "line-color": "#3a4150", "target-arrow-color": "#3a4150",
      "target-arrow-shape": "triangle", "curve-style": "bezier"
    }} }},
  ],
  layout: {{ name: "breadthfirst", directed: true, roots: "#" + ROOT, padding: 30, spacingFactor: 1.3 }},
}});
</script>
</body>
</html>
"##
    )
}
