use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    GraphEdgeKind, GraphNodeKind, export_graph_html_from_file,
    fixtures::canonical_program_b_fixture, graph_from_file, project_analysis_graph,
};

#[test]
fn canonical_fixture_graph_has_expected_nodes_edges_and_mermaid_text() {
    let analysis = canonical_program_b_fixture();
    let graph = project_analysis_graph(&analysis);

    assert!(
        graph
            .nodes
            .iter()
            .any(|node| node.kind == GraphNodeKind::Body && node.id == "body_b1")
    );
    assert!(
        graph
            .nodes
            .iter()
            .any(|node| node.kind == GraphNodeKind::Kernel && node.id == "kernel_k10")
    );
    assert!(
        graph
            .nodes
            .iter()
            .any(|node| node.kind == GraphNodeKind::ClosureLayout && node.id == "layout_l10")
    );

    assert!(graph.edges.iter().any(|edge| {
        edge.kind == GraphEdgeKind::BodyToKernel
            && edge.from == "body_b1"
            && edge.to == "kernel_k10"
    }));

    let mermaid = graph.to_mermaid_flowchart();
    assert!(mermaid.starts_with("flowchart TD\n"));
    assert!(mermaid.contains("body_b1"));
    assert!(mermaid.contains("kernel_k10"));
    assert!(mermaid.contains("layout_l10"));

    let json = graph.to_json_pretty();
    assert!(json.contains("\"graph_type\": \"structural_analysis_graph\""));
    assert!(json.contains("\"kind\": \"body\""));
    assert!(json.contains("\"kind\": \"kernel\""));
    assert!(json.contains("\"kind\": \"closure_layout\""));
}

#[test]
fn zoo_cc_version_graph_smoke() {
    let zoo_file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cc-version.vx");
    assert!(
        zoo_file.exists(),
        "expected zoo file to exist at {}",
        zoo_file.display()
    );

    let graph = graph_from_file(&zoo_file).expect("zoo graph projection should succeed");
    assert!(
        !graph.nodes.is_empty(),
        "zoo graph projection should produce at least one node"
    );

    let mermaid = graph.to_mermaid_flowchart();
    assert!(mermaid.starts_with("flowchart TD\n"));
    assert!(mermaid.contains("boundary owns closure arg"));

    let html = graph.to_standalone_html();
    assert!(html.contains("Vixen Structural Analysis Graph"));
    assert!(html.contains("structural analysis graph"));
    assert!(html.contains("cc-version.vx"));

    let output = unique_temp_path("cc-version-graph.html");
    export_graph_html_from_file(&zoo_file, &output).expect("html export should succeed");
    let written =
        std::fs::read_to_string(&output).expect("written html artifact should be readable");
    assert!(written.contains("Vixen Structural Analysis Graph"));
    assert!(written.contains("cc-version.vx"));
    let _ = std::fs::remove_file(&output);
}

#[test]
fn zoo_cc_version_includes_runtime_dependency_and_boundary_to_layout_edges() {
    let zoo_file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cc-version.vx");
    let graph = graph_from_file(&zoo_file).expect("zoo graph projection should succeed");

    let has_discover_to_exec = graph.edges.iter().any(|edge| {
        edge.kind == GraphEdgeKind::RuntimeValueDependency
            && edge
                .metadata
                .iter()
                .any(|(k, v)| k == "binding_name" && v == "cc")
    });
    assert!(
        has_discover_to_exec,
        "cc-version should include discover->exec runtime dependency via `cc` binding"
    );

    let boundary_to_layout_count = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == GraphEdgeKind::RuntimeBoundaryToClosureLayout)
        .count();
    assert!(
        boundary_to_layout_count >= 2,
        "cc-version should include boundary->layout edges for host lambda and observer lambda"
    );
}

#[test]
fn zoo_graph_projection_and_export_are_deterministic_across_repeated_runs() {
    let zoo_file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cc-version.vx");
    let graph_a = graph_from_file(&zoo_file).expect("first zoo projection should succeed");
    let graph_b = graph_from_file(&zoo_file).expect("second zoo projection should succeed");

    assert_eq!(graph_a, graph_b);
    assert_eq!(
        graph_a.to_mermaid_flowchart(),
        graph_b.to_mermaid_flowchart()
    );
    assert_eq!(graph_a.to_json_pretty(), graph_b.to_json_pretty());
    assert_eq!(graph_a.to_standalone_html(), graph_b.to_standalone_html());
}

#[test]
fn zoo_tri_platform_png_project_import_graph_smoke() {
    let zoo_file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/tri-platform-png.vx");
    assert!(
        zoo_file.exists(),
        "expected zoo file to exist at {}",
        zoo_file.display()
    );

    let graph = graph_from_file(&zoo_file).expect("project graph projection should succeed");
    assert!(
        !graph.nodes.is_empty(),
        "graph should contain structural nodes"
    );

    let json = graph.to_json_pretty();
    assert!(json.contains("tri-platform-png.vx"));
    assert!(json.contains("zlib-lib.vx"));
    assert!(json.contains("libpng-lib.vx"));
    assert!(json.contains("pnggen-app.vx"));

    let html = graph.to_standalone_html();
    assert!(html.contains("tri-platform-png.vx"));
    assert!(html.contains("structural analysis graph"));
}

fn unique_temp_path(filename: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be monotonic enough for temp name")
        .as_nanos();
    std::env::temp_dir().join(format!("vx-eval3-analysis-{nanos}-{filename}"))
}
