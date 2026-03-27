use std::path::PathBuf;

use crate::{
    GraphEdgeKind, GraphNodeKind, fixtures::canonical_program_b_fixture, graph_from_file,
    project_analysis_graph,
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
}
