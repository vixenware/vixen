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
    assert!(html.contains("Source Snippet"));
    assert!(html.contains("detailRow(\"Source\""));
    assert!(html.contains("detailRow(\"Location\""));
    assert!(!html.contains("detailRow(\"Source ID\""));
    assert!(!html.contains("detailRow(\"Source Path\""));
    assert!(html.contains("Raw JSON"));
    assert!(html.contains("Lane Summary"));
    assert!(html.contains("fold stdlib lanes"));
    assert!(html.contains("unfold all"));
    assert!(html.contains("data-lane-fold-source-id"));
    assert!(html.contains("data-lane-focus-source-id"));
    assert!(html.contains("laneHintFor("));
    assert!(html.contains("applyLaneFolding("));
    assert!(html.contains("foldedLaneSourceIds"));
    assert!(html.contains("params.get(\"fold\")"));
    assert!(html.contains("params.set(\"fold\""));
    assert!(html.contains("search-strip"));
    assert!(html.contains("search-query"));
    assert!(html.contains("search-results"));
    assert!(html.contains("search-clear"));
    assert!(html.contains("buildSearchEntries("));
    assert!(html.contains("renderSearchResults("));
    assert!(html.contains("activateSearchResult("));
    assert!(html.contains("params.get(\"q\")"));
    assert!(html.contains("params.set(\"q\""));
    assert!(html.contains("compact-source-row"));
    assert!(!html.contains("Source Strip"));
    assert!(html.contains("source-inspector"));
    assert!(html.contains("source_contents"));
    assert!(html.contains("line_start_offsets"));
    assert!(html.contains("setInspectorSelection("));
    assert!(html.contains("renderSourceInspector("));
    assert!(html.contains("inspector-line"));
    assert!(html.contains("params.get(\"isrc\")"));
    assert!(html.contains("params.set(\"isrc\""));
    assert!(html.contains("params.get(\"ix\")"));
    assert!(html.contains("params.set(\"ix\""));
    assert!(!html.contains("inspector-marker"));
    assert!(html.contains("sourceGraphMappings("));
    assert!(html.contains("activateInspectorMapping("));
    assert!(html.contains("data-inspector-line"));
    assert!(html.contains("data-inspector-select-key"));
    assert!(html.contains("inspectorChoiceLine"));
    assert!(html.contains("clickable-range"));
    assert!(html.contains("data-inspector-range-key"));
    assert!(html.contains("mappingHintText("));
    assert!(html.contains("choice-hint"));
    assert!(html.contains("inspector-choice-pop"));
    assert!(!html.contains(" pick"));
    assert!(html.contains("params.get(\"icl\")"));
    assert!(html.contains("params.set(\"icl\""));
    assert!(html.contains("keepInspectorChoice"));
    assert!(html.contains("keepChoice: true"));
    assert!(html.contains("button type=\"button\" class=\"code clickable-range\""));
    assert!(html.contains("source_snippet"));
    assert!(html.contains("host.which(\\\"cc\\\")"));
    assert!(html.contains("color-scheme: dark"));
    assert!(html.contains(".source-registry li.active"));
    assert!(html.contains("snippet-line"));
    assert!(html.contains("source_snippet_highlighted_html"));
    assert!(html.contains("arb-code"));
    assert!(html.contains("data-arb-theme=\"dark\""));
    assert!(html.contains(".arb-code[data-arb-theme=\"light\"]"));
    assert!(html.contains(".arb-code[data-arb-theme=\"dark\"]"));
    assert!(html.contains("a-k {"));
    assert!(html.contains("applySourceFilter("));
    assert!(html.contains("topbar"));
    assert!(html.contains("graph-first-layout"));
    assert!(html.contains("graph-stage"));
    assert!(html.contains("graph-toolbar"));
    assert!(html.contains("search-strip"));
    assert!(html.contains("semantic-aux"));
    assert!(html.contains("semantic-filter-chips"));
    assert!(html.contains("compact-source-row { position: sticky;"));
    assert!(html.contains("minmax(220px, 38vh)"));
    assert!(html.contains("laneIsInViewport("));
    assert!(html.contains("viewportLaneOrder"));
    assert!(html.contains("source-first-row"));
    assert!(html.contains("utility-strip"));
    assert!(html.contains("aux-pop"));
    assert!(html.contains("legend-help"));
    assert!(html.contains("sources-aux"));
    assert!(html.contains("lanes-aux"));
    assert!(html.contains("Graph Legend"));
    assert!(html.contains("node-kind-body"));
    assert!(html.contains("edge-kind-runtime_value_dependency"));
    assert!(html.contains("source-lane-band"));
    assert!(html.contains("source-lane-label"));
    assert!(html.contains("barycenterForNode"));
    assert!(html.contains("laneOrder"));
    assert!(html.contains("graph viewport controls"));
    assert!(html.contains("zoom-in"));
    assert!(html.contains("zoom-out"));
    assert!(html.contains("zoom-reset"));
    assert!(html.contains("setViewportScale("));
    assert!(html.contains("focusSourceLane("));
    assert!(html.contains("selectGraphItem("));
    assert!(html.contains(".selected"));
    assert!(html.contains("node-demand-demanded"));
    assert!(html.contains("node-sharing-shared"));
    assert!(html.contains("edge-role-value_flow"));
    assert!(html.contains("edgeSemanticLabel("));
    assert!(html.contains("edgeDisplayLabel("));
    assert!(html.contains("shouldRenderEdgeLabel("));
    assert!(html.contains("hidden-edge-label"));
    assert!(html.contains("nodeDisplayLines("));
    assert!(html.contains("\"Closure Layout\""));
    assert!(html.contains("captured value"));
    assert!(html.contains("detailRow(\"Source\""));
    assert!(html.contains("detailRow(\"Location\""));
    assert!(!html.contains("detailRow(\"Source ID\""));
    assert!(!html.contains("detailRow(\"Source Path\""));
    assert!(html.contains("\"Closure Layout\""));
    assert!(html.contains("captured value"));
    assert!(html.contains("semantic-summary"));
    assert!(html.contains("Semantic Filters"));
    assert!(html.contains("filter-demand"));
    assert!(html.contains("filter-runtime-role"));
    assert!(html.contains("filter-sharing"));
    assert!(html.contains("applySemanticFilters("));
    assert!(html.contains("matchesNodeSemanticFilters("));
    assert!(html.contains("semanticNarrative("));
    assert!(html.contains("Why It Matters"));
    assert!(html.contains("Connection Focus"));
    assert!(html.contains("focus-neighborhood-toggle"));
    assert!(html.contains("hide-stdlib-noise-toggle"));
    assert!(html.contains("applyDependencyFocus("));
    assert!(html.contains("selectedNeighborhoodFromKey("));
    assert!(html.contains("applyNoiseReduction("));
    assert!(html.contains("isStdlibSourceId("));
    assert!(html.contains("focus-primary"));
    assert!(html.contains("focus-neighbor"));
    assert!(html.contains("dim-focus"));
    assert!(html.contains("dependencySummaryHtml("));
    assert!(html.contains("Dependency Summary"));
    assert!(html.contains("What this node depends on"));
    assert!(html.contains("What depends on this node"));
    assert!(html.contains("selectedTraceFromNode("));
    assert!(html.contains("focus-path-mode"));
    assert!(html.contains("upstream path"));
    assert!(html.contains("downstream path"));
    assert!(html.contains("Why This Edge Exists"));
    assert!(html.contains("data-select-node-id"));
    assert!(html.contains("data-select-edge-key"));
    assert!(html.contains("wireDetailTraversal("));
    assert!(html.contains("selectByGraphKey("));
    assert!(html.contains("selection-history-back"));
    assert!(html.contains("selection-history-forward"));
    assert!(html.contains("updateSelectionHistoryButtons("));
    assert!(html.contains("pushSelectionHistory("));
    assert!(html.contains("copy-share-link"));
    assert!(html.contains("copyShareLink("));
    assert!(html.contains("share-link-status"));
    assert!(html.contains("navigator.clipboard.writeText"));
    assert!(html.contains("moveDependencyFocus("));
    assert!(html.contains("keydown"));
    assert!(html.contains("ArrowLeft"));
    assert!(html.contains("ArrowRight"));
    assert!(html.contains("readViewerStateFromUrl("));
    assert!(html.contains("writeViewerStateToUrl("));
    assert!(html.contains("persistViewerState("));
    assert!(html.contains("applyViewerStateFromUrl("));
    assert!(html.contains("popstate"));
    assert!(html.contains("restoredFromUrl"));
    assert!(html.contains("params.set(\"v\", \"1\")"));
    assert!(html.contains("runtime_role_summary"));
    let source_pos = html
        .find("<section class=\"source-first-row")
        .expect("source strip section marker should exist");
    let graph_pos = html
        .find("<section class=\"graph-stage\"")
        .expect("graph stage section marker should exist");
    assert!(
        source_pos < graph_pos,
        "source strip should appear before graph viewport in sticky source-first layout"
    );

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
    assert!(
        graph
            .nodes
            .iter()
            .filter(|node| node.kind == GraphNodeKind::Kernel)
            .all(|node| node.metadata.iter().any(|(k, _)| k == "span_source_id")),
        "project-backed kernel nodes should carry source provenance"
    );

    let json = graph.to_json_pretty();
    assert!(json.contains("tri-platform-png.vx"));
    assert!(json.contains("zlib-lib.vx"));
    assert!(json.contains("libpng-lib.vx"));
    assert!(json.contains("pnggen-app.vx"));

    let html = graph.to_standalone_html();
    assert!(html.contains("tri-platform-png.vx"));
    assert!(html.contains("structural analysis graph"));
    assert!(html.contains("Source Registry"));
    assert!(html.contains("This graph includes"));
    assert!(html.contains("zlib-lib.vx"));
    assert!(html.contains("libpng-lib.vx"));
    assert!(html.contains("pnggen-app.vx"));
    assert!(html.contains("data-source-id"));
    assert!(html.contains("sourceFilterSourceId"));
    assert!(html.contains("focused"));
    assert!(html.contains("topbar"));
    assert!(html.contains("graph-first-layout"));
    assert!(html.contains("graph-stage"));
    assert!(html.contains("graph-toolbar"));
    assert!(html.contains("search-strip"));
    assert!(html.contains("semantic-aux"));
    assert!(html.contains("semantic-filter-chips"));
    assert!(html.contains("compact-source-row { position: sticky;"));
    assert!(html.contains("minmax(220px, 38vh)"));
    assert!(html.contains("laneIsInViewport("));
    assert!(html.contains("viewportLaneOrder"));
    assert!(html.contains("source-first-row"));
    assert!(html.contains("utility-strip"));
    assert!(html.contains("aux-pop"));
    assert!(html.contains("legend-help"));
    assert!(html.contains("sources-aux"));
    assert!(html.contains("lanes-aux"));
    assert!(html.contains("Graph Legend"));
    assert!(html.contains("node-kind-runtime_boundary"));
    assert!(html.contains("edge-kind-body_to_runtime_boundary"));
    assert!(html.contains("source-lane-band"));
    assert!(html.contains("source lanes"));
    assert!(html.contains("source-lane-label"));
    assert!(html.contains("graph-viewport"));
    assert!(html.contains("Source Registry"));
    assert!(html.contains("focusSourceLane("));
    assert!(html.contains("node-badge"));
    assert!(html.contains("sharing_role"));
    assert!(html.contains("Runtime Role"));
    assert!(html.contains("hidden-semantic"));
    assert!(html.contains("semantic-controls"));
    assert!(html.contains("dim stdlib-heavy sources"));
    assert!(html.contains("dependency neighborhood"));
    assert!(html.contains("noise-dim"));
    assert!(html.contains("focus-path-mode"));
    assert!(html.contains("Connection Focus"));
    assert!(html.contains("open node"));
    assert!(html.contains("open edge"));
    assert!(html.contains("selection-nav"));
    assert!(html.contains("copy link"));
    assert!(html.contains("link copied"));
    assert!(html.contains("moveDependencyFocus("));
    assert!(html.contains("sf"));
    assert!(html.contains("trace"));
    assert!(html.contains("noise"));
    assert!(html.contains("Lane Summary"));
    assert!(html.contains("fold stdlib lanes"));
    assert!(html.contains("hidden-folded-lane"));
    assert!(html.contains("folded-lane"));
    assert!(html.contains("setLaneFoldState("));
    assert!(html.contains("foldStdlibLanes("));
    assert!(html.contains("clearFoldedLanes("));
    assert!(html.contains("applyLaneFolding("));
    assert!(html.contains("computePrimaryProjectLane("));
    assert!(html.contains("primaryProjectLane"));
    assert!(!html.contains("path.endsWith(\"/tri-platform-png.vx\")"));
    assert!(!html.contains("path.endsWith(\"\\\\tri-platform-png.vx\")"));
    assert!(html.contains("const stackDepth = Math.max(1, maxNodesInKind);"));
    assert!(!html.contains("Math.max(maxNodesInKind, stats.totalNodes)"));
    assert!(html.contains("params.get(\"fold\")"));
    assert!(html.contains("params.set(\"fold\""));
    assert!(html.contains("search-query"));
    assert!(html.contains("search-results"));
    assert!(html.contains("Search graph"));
    assert!(html.contains("setSearchQuery("));
    assert!(html.contains("activateSearchResult("));
    assert!(html.contains("ArrowDown"));
    assert!(html.contains("ArrowUp"));
    assert!(html.contains("params.get(\"q\")"));
    assert!(html.contains("params.set(\"q\""));
    assert!(html.contains("compact-source-row"));
    assert!(!html.contains("Source Strip"));
    assert!(html.contains("source_contents"));
    assert!(html.contains("inspector-lines"));
    assert!(html.contains("lineFromByteOffset("));
    assert!(html.contains("inspectorSpanStart"));
    assert!(html.contains("inspectorSpanEnd"));
    assert!(html.contains("source-inspector-toggle"));
    assert!(html.contains("params.get(\"isrc\")"));
    assert!(html.contains("params.set(\"isrc\""));
    assert!(html.contains("params.get(\"ix\")"));
    assert!(html.contains("params.set(\"ix\""));
    assert!(!html.contains("inspector-marker"));
    assert!(html.contains("inspector-choice-pop"));
    assert!(!html.contains(" pick"));
    assert!(html.contains("sourceGraphMappings("));
    assert!(html.contains("activateInspectorMapping("));
    assert!(html.contains("data-inspector-line"));
    assert!(html.contains("data-inspector-select-key"));
    assert!(html.contains("data-inspector-range-key"));
    assert!(html.contains("clickable-range"));
    assert!(html.contains("choice-hint"));
    assert!(html.contains("params.get(\"icl\")"));
    assert!(html.contains("params.set(\"icl\""));
    assert!(html.contains("keepInspectorChoice"));
    assert!(html.contains("button type=\"button\" class=\"code clickable-range\""));
    assert!(html.contains("edgeDisplayLabel("));
    assert!(html.contains("shouldRenderEdgeLabel("));
    assert!(html.contains("hidden-edge-label"));
    assert!(html.contains("nodeDisplayLines("));
    let source_pos = html
        .find("<section class=\"source-first-row")
        .expect("source strip section marker should exist");
    let graph_pos = html
        .find("<section class=\"graph-stage\"")
        .expect("graph stage section marker should exist");
    assert!(
        source_pos < graph_pos,
        "source strip should appear before graph viewport in sticky source-first layout"
    );
}

#[test]
fn zoo_tri_platform_png_projection_and_export_are_deterministic_across_repeated_runs() {
    let zoo_file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/tri-platform-png.vx");
    let graph_a =
        graph_from_file(&zoo_file).expect("first project-backed zoo projection should succeed");
    let graph_b =
        graph_from_file(&zoo_file).expect("second project-backed zoo projection should succeed");

    assert_eq!(graph_a, graph_b);
    assert_eq!(
        graph_a.to_mermaid_flowchart(),
        graph_b.to_mermaid_flowchart()
    );
    assert_eq!(graph_a.to_json_pretty(), graph_b.to_json_pretty());
    assert_eq!(graph_a.to_standalone_html(), graph_b.to_standalone_html());
}

fn unique_temp_path(filename: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be monotonic enough for temp name")
        .as_nanos();
    std::env::temp_dir().join(format!("vx-eval3-analysis-{nanos}-{filename}"))
}
