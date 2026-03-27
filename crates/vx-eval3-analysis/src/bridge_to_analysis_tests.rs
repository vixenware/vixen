use crate::{
    BodyId, Observation, RuntimeBoundaryKind, RuntimeNode, SingleSourceBridge,
    build_minimal_analysis,
};

#[test]
fn bridge_to_analysis_is_deterministic_for_canonical_like_source() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )

fn main(n: U64) -> U64 (
  let x = plus_one(n);
  x + x
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis_a = build_minimal_analysis(&bridge);
    let analysis_b = build_minimal_analysis(&bridge);

    assert_eq!(analysis_a, analysis_b, "adapter should be deterministic");
    analysis_a
        .validate()
        .expect("minimal analysis should satisfy core invariants");

    assert_eq!(analysis_a.bodies.len(), 2);
    assert_eq!(analysis_a.kernels.len(), 2);
    assert!(analysis_a.runtime_nodes.is_empty());
    assert_eq!(analysis_a.bodies[0].id, BodyId(0));
    assert_eq!(analysis_a.bodies[1].id, BodyId(1));
}

#[test]
fn bridge_to_analysis_marks_exec_boundary_for_build_shaped_source() {
    let src = r#"
fn build(root: Path, args: Array[String], env: Map[String, String]) #fs #discover -> Tree (
  let class = executor_class("default");
  let cc = discover(class, (host) => host.which("cc"));
  exec [cc, "-c", @root/main.c, "-o", @out/main.o] where {
    observer: (proc, tree) => tree,
  }
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    analysis
        .validate()
        .expect("build-shaped minimal analysis should validate");

    assert!(
        analysis
            .runtime_nodes
            .iter()
            .any(|node| matches!(node, RuntimeNode::Exec { .. })),
        "exec call should become an exec runtime boundary node"
    );

    let build_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(0))
        .expect("build should be the first top-level body");

    assert!(build_body.extracted_nodes.iter().any(|node| {
        matches!(
            node.reason,
            crate::ExtractedNodeReason::RuntimeBoundary {
                kind: RuntimeBoundaryKind::Exec
            }
        )
    }));
}

#[test]
fn bridge_to_analysis_seeds_need_value_for_field_projection() {
    let src = r#"
fn main() -> U64 (
  let pair = (1, 2);
  pair._0
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);

    let body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(0))
        .expect("main body should exist");

    assert!(
        body.use_observations
            .values()
            .any(|obs| *obs == Observation::NeedValue),
        "field projection should seed NeedValue use observation"
    );
}

#[test]
fn bridge_to_analysis_seeds_need_callable_for_exec_and_discover_deterministically() {
    let src = r#"
fn build(root: Path, args: Array[String], env: Map[String, String]) #fs #discover -> Tree (
  let class = executor_class("default");
  let cc = discover(class, (host) => host.which("cc"));
  exec [cc, "-c", @root/main.c, "-o", @out/main.o] where {
    observer: (proc, tree) => tree,
  }
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis_a = build_minimal_analysis(&bridge);
    let analysis_b = build_minimal_analysis(&bridge);

    assert_eq!(
        analysis_a.bodies[0].use_observations, analysis_b.bodies[0].use_observations,
        "use observation seeding must be deterministic"
    );

    let callable_count = analysis_a.bodies[0]
        .use_observations
        .values()
        .filter(|obs| **obs == Observation::NeedCallable)
        .count();
    assert!(
        callable_count >= 2,
        "exec and discover should seed NeedCallable observations"
    );
}
