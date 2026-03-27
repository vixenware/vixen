use crate::{
    BodyId, ExtractedNodeReason, Observation, RuntimeBoundaryKind, RuntimeNode, SingleSourceBridge,
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
    assert_eq!(analysis_a.bodies[0].id, BodyId(0));
    assert_eq!(analysis_a.bodies[1].id, BodyId(1));

    let main_body = analysis_a
        .bodies
        .iter()
        .find(|body| body.id == BodyId(1))
        .expect("main body should exist");
    assert!(
        main_body
            .extracted_nodes
            .iter()
            .any(|node| { matches!(node.reason, ExtractedNodeReason::SharedAcrossUses) })
    );
}

#[test]
fn bridge_to_analysis_marks_exec_and_discover_boundaries_for_build_shaped_source() {
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

    let has_exec = analysis
        .runtime_nodes
        .iter()
        .any(|node| matches!(node, RuntimeNode::Exec { .. }));
    let has_discover = analysis
        .runtime_nodes
        .iter()
        .any(|node| matches!(node, RuntimeNode::Discover { .. }));
    assert!(
        has_exec,
        "exec call should become an exec runtime boundary node"
    );
    assert!(
        has_discover,
        "discover call should become a discover runtime boundary node"
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
fn bridge_to_analysis_classifies_stabilize_as_shared_keyed_boundary() {
    let src = r#"
fn main() -> String (
  stabilize(() => "w", (w) => w)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    analysis
        .validate()
        .expect("stabilize-shaped minimal analysis should validate");

    assert!(
        analysis
            .runtime_nodes
            .iter()
            .any(|node| matches!(node, RuntimeNode::SharedKeyed { .. })),
        "stabilize call should become a shared-keyed runtime boundary node"
    );

    let body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(0))
        .expect("main should be the first top-level body");
    assert!(body.extracted_nodes.iter().any(|node| {
        matches!(
            node.reason,
            crate::ExtractedNodeReason::RuntimeBoundary {
                kind: RuntimeBoundaryKind::SharedKeyed
            }
        )
    }));

    let callable_count = body
        .use_observations
        .values()
        .filter(|obs| **obs == Observation::NeedCallable)
        .count();
    assert!(
        callable_count >= 1,
        "stabilize should seed NeedCallable observations via boundary classification"
    );
}

#[test]
fn bridge_to_analysis_does_not_classify_local_exec_as_runtime_boundary() {
    let src = r#"
fn exec(x: U64) -> U64 ( x + 1 )

fn main() -> U64 (
  exec(41)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    analysis
        .validate()
        .expect("local-exec minimal analysis should validate");

    assert!(
        analysis.runtime_nodes.is_empty(),
        "local non-stdlib exec should not become a runtime boundary node"
    );

    let body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(1))
        .expect("main should be the second top-level body");
    let callable_count = body
        .use_observations
        .values()
        .filter(|obs| **obs == Observation::NeedCallable)
        .count();
    assert!(
        callable_count >= 1,
        "local non-stdlib exec should still seed callable demand as a direct call"
    );
}

#[test]
fn bridge_to_analysis_does_not_seed_shared_node_for_single_use_local_binding() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )

fn main(n: U64) -> U64 (
  let x = plus_one(n);
  x + 1
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);

    let main_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(1))
        .expect("main body should exist");
    assert!(
        !main_body
            .extracted_nodes
            .iter()
            .any(|node| { matches!(node.reason, ExtractedNodeReason::SharedAcrossUses) }),
        "single-use local binding should not seed shared extraction"
    );
}

#[test]
fn bridge_to_analysis_sharing_seed_is_deterministic_across_runs() {
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

    assert_eq!(
        analysis_a, analysis_b,
        "sharing seed should be deterministic"
    );
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
fn bridge_to_analysis_seeds_lazy_product_field_for_program_a_like_tuple_projection() {
    let src = r#"
fn bomb() -> U64 ( fail "bomb" )

fn main() -> U64 (
  let pair = (41 + 1, bomb());
  pair._0
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);

    let main_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(1))
        .expect("main body should exist");
    assert!(
        main_body.extracted_nodes.iter().any(|node| {
            matches!(
                node.reason,
                ExtractedNodeReason::LazyProductField {
                    field: crate::FieldKey::Index(1)
                }
            )
        }),
        "projecting _0 should allow sibling _1 to remain lazy"
    );
}

#[test]
fn bridge_to_analysis_does_not_seed_lazy_product_field_without_projection() {
    let src = r#"
fn main() -> U64 (
  let pair = (1, 2);
  41
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);

    let main_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(0))
        .expect("main body should exist");
    assert!(
        !main_body
            .extracted_nodes
            .iter()
            .any(|node| { matches!(node.reason, ExtractedNodeReason::LazyProductField { .. }) }),
        "non-projection product should not seed lazy product field extraction"
    );
}

#[test]
fn bridge_to_analysis_product_projection_seed_is_deterministic_across_runs() {
    let src = r#"
fn main() -> U64 (
  let pair = (1, 2);
  pair._0
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis_a = build_minimal_analysis(&bridge);
    let analysis_b = build_minimal_analysis(&bridge);

    assert_eq!(
        analysis_a.bodies[0].extracted_nodes, analysis_b.bodies[0].extracted_nodes,
        "product/projection extraction seeding must be deterministic"
    );
    assert_eq!(
        analysis_a.runtime_nodes, analysis_b.runtime_nodes,
        "runtime node seeding from product/projection should be deterministic"
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

#[test]
fn bridge_to_analysis_seeds_need_callable_for_direct_named_function_call() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 (
  plus_one(n)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    let main_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(1))
        .expect("main body should exist");

    assert!(
        main_body
            .use_observations
            .values()
            .any(|obs| *obs == Observation::NeedCallable),
        "direct named function call should seed NeedCallable"
    );
}

#[test]
fn bridge_to_analysis_seeds_need_callable_for_local_callable_binding_call() {
    let src = r#"
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  add_n(1)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    let main_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(0))
        .expect("main body should exist");

    assert!(
        main_body
            .use_observations
            .values()
            .any(|obs| *obs == Observation::NeedCallable),
        "local callable binding call should seed NeedCallable"
    );
}

#[test]
fn bridge_to_analysis_callable_seeding_from_call_sites_is_deterministic() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  plus_one(n) + add_n(1)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis_a = build_minimal_analysis(&bridge);
    let analysis_b = build_minimal_analysis(&bridge);

    assert_eq!(
        analysis_a.bodies[1].use_observations, analysis_b.bodies[1].use_observations,
        "call-site callable seeding should be deterministic"
    );
}

#[test]
fn bridge_to_analysis_carries_direct_call_target_into_analysis_body() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 (
  plus_one(n)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    let expected_target = bridge
        .call_sites
        .iter()
        .find(|site| site.kind == crate::BridgeCallSiteKind::DirectNamedFunction)
        .and_then(|site| site.target)
        .expect("bridge direct call site should carry a target");

    let main_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(1))
        .expect("main body should exist");
    assert!(
        main_body
            .callee_targets
            .values()
            .any(|target| *target == expected_target),
        "direct call target should survive into analysis output"
    );
}

#[test]
fn bridge_to_analysis_carries_local_callable_target_into_analysis_body() {
    let src = r#"
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  add_n(1)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    let expected_target = bridge
        .call_sites
        .iter()
        .find(|site| site.kind == crate::BridgeCallSiteKind::LocalCallableBinding)
        .and_then(|site| site.target)
        .expect("bridge local callable call site should carry a target");

    let main_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(0))
        .expect("main body should exist");
    assert!(
        main_body
            .callee_targets
            .values()
            .any(|target| *target == expected_target),
        "local callable target should survive into analysis output"
    );
}

#[test]
fn bridge_to_analysis_callee_target_links_are_deterministic() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  plus_one(n) + add_n(1)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis_a = build_minimal_analysis(&bridge);
    let analysis_b = build_minimal_analysis(&bridge);

    assert_eq!(
        analysis_a.bodies[1].callee_targets, analysis_b.bodies[1].callee_targets,
        "callee target links should be deterministic across repeated runs"
    );
}

#[test]
fn bridge_to_analysis_indexes_direct_call_source_local_origin() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 (
  plus_one(n)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    let main_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(1))
        .expect("main body should exist");

    assert!(
        main_body
            .callee_local_origins
            .values()
            .any(|origin| { *origin == crate::SourceLocalCalleeOrigin::TopLevelBody(BodyId(0)) }),
        "direct call should index top-level callee body origin"
    );
}

#[test]
fn bridge_to_analysis_indexes_local_callable_source_local_origin_when_recoverable() {
    let src = r#"
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  add_n(1)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let expected_closure_source = bridge
        .local_closure_inits
        .first()
        .map(|init| init.source.clone())
        .expect("local callable binding should have direct closure init source");
    let analysis = build_minimal_analysis(&bridge);
    let main_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(0))
        .expect("main body should exist");

    assert!(
        main_body.callee_local_origins.values().any(|origin| {
            *origin == crate::SourceLocalCalleeOrigin::LocalClosure(expected_closure_source.clone())
        }),
        "local callable call should index recoverable closure-body origin"
    );
}

#[test]
fn bridge_to_analysis_keeps_nonrecoverable_local_callable_on_generic_fallback_origin() {
    let src = r#"
fn apply(f: Fn(U64) -> U64, n: U64) -> U64 (
  f(n)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let expected_local_target = bridge
        .call_sites
        .iter()
        .find(|site| site.kind == crate::BridgeCallSiteKind::LocalCallableBinding)
        .and_then(|site| site.target)
        .expect("local callable call site should have a resolved target");
    let analysis = build_minimal_analysis(&bridge);
    let apply_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(0))
        .expect("apply body should exist");

    assert!(
        apply_body.callee_local_origins.values().any(|origin| {
            *origin == crate::SourceLocalCalleeOrigin::LocalDef(expected_local_target)
        }),
        "non-recoverable local callable path should stay on generic LocalDef fallback"
    );
}

#[test]
fn bridge_to_analysis_callee_local_origin_links_are_deterministic() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  plus_one(n) + add_n(1)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis_a = build_minimal_analysis(&bridge);
    let analysis_b = build_minimal_analysis(&bridge);

    assert_eq!(
        analysis_a.bodies[1].callee_local_origins, analysis_b.bodies[1].callee_local_origins,
        "callee local-origin links should be deterministic across repeated runs"
    );
}

#[test]
fn bridge_to_analysis_indexes_direct_named_source_local_call_edge() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 (
  plus_one(n)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    let main_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(1))
        .expect("main body should exist");

    assert!(
        main_body
            .source_local_call_edges
            .iter()
            .any(|edge| { edge.callee == crate::SourceLocalCalleeOrigin::TopLevelBody(BodyId(0)) })
    );
}

#[test]
fn bridge_to_analysis_indexes_local_callable_source_local_call_edge_when_recoverable() {
    let src = r#"
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  add_n(1)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let expected_closure_source = bridge
        .local_closure_inits
        .first()
        .map(|init| init.source.clone())
        .expect("local callable binding should have direct closure init source");
    let analysis = build_minimal_analysis(&bridge);
    let main_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(0))
        .expect("main body should exist");

    assert!(main_body.source_local_call_edges.iter().any(|edge| {
        edge.callee == crate::SourceLocalCalleeOrigin::LocalClosure(expected_closure_source.clone())
    }));
}

#[test]
fn bridge_to_analysis_source_local_call_edges_are_deterministic() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  plus_one(n) + add_n(1)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis_a = build_minimal_analysis(&bridge);
    let analysis_b = build_minimal_analysis(&bridge);

    assert_eq!(
        analysis_a.bodies[1].source_local_call_edges, analysis_b.bodies[1].source_local_call_edges,
        "source-local call edges should be deterministic across repeated runs"
    );
}

#[test]
fn bridge_to_analysis_links_recovered_local_closure_origin_to_concrete_layout() {
    let src = r#"
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  add_n(1)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    let main_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(0))
        .expect("main body should exist");

    let has_link = main_body
        .callee_closure_layouts
        .iter()
        .any(|(call_expr, layout_id)| {
            let Some(origin) = main_body.callee_local_origins.get(call_expr) else {
                return false;
            };
            let Some(layout) = analysis.closure_layouts.iter().find(|l| l.id == *layout_id) else {
                return false;
            };
            *origin == crate::SourceLocalCalleeOrigin::LocalClosure(layout.source.clone())
        });
    assert!(
        has_link,
        "recoverable local callable origin should link to a concrete closure layout"
    );
}

#[test]
fn bridge_to_analysis_nonrecoverable_local_callable_has_no_closure_layout_link() {
    let src = r#"
fn apply(f: Fn(U64) -> U64, n: U64) -> U64 (
  f(n)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    let apply_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(0))
        .expect("apply body should exist");

    assert!(
        apply_body.callee_closure_layouts.is_empty(),
        "non-recoverable local callable path should remain on generic fallback without layout link"
    );
}

#[test]
fn bridge_to_analysis_callee_closure_layout_links_are_deterministic() {
    let src = r#"
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  add_n(1)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis_a = build_minimal_analysis(&bridge);
    let analysis_b = build_minimal_analysis(&bridge);

    assert_eq!(
        analysis_a.bodies[0].callee_closure_layouts, analysis_b.bodies[0].callee_closure_layouts,
        "callee->closure-layout linkage should be deterministic across repeated runs"
    );
}

#[test]
fn bridge_to_analysis_propagates_result_demand_to_direct_named_callee_body() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 (
  plus_one(n)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);

    assert_eq!(
        analysis
            .propagated_result_demands
            .get(&crate::ResultDemandCallee::TopLevelBody(BodyId(0))),
        Some(&Observation::NeedValue),
        "direct named local call should seed one-step callee-body result demand"
    );
}

#[test]
fn bridge_to_analysis_propagates_result_demand_to_recovered_local_closure_layout() {
    let src = r#"
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  add_n(1)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    let linked_layout = analysis.bodies[0]
        .callee_closure_layouts
        .values()
        .next()
        .copied()
        .expect("recoverable closure call should have a layout link");

    assert_eq!(
        analysis
            .propagated_result_demands
            .get(&crate::ResultDemandCallee::ClosureLayout(linked_layout)),
        Some(&Observation::NeedValue),
        "recoverable local closure call should seed one-step closure-layout result demand"
    );
}

#[test]
fn bridge_to_analysis_nonrecoverable_local_callable_does_not_fake_propagation() {
    let src = r#"
fn apply(f: Fn(U64) -> U64, n: U64) -> U64 (
  f(n)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);

    assert!(
        analysis.propagated_result_demands.is_empty(),
        "non-recoverable local callable path should not propagate through unknown callee"
    );
}

#[test]
fn bridge_to_analysis_first_result_propagation_step_is_deterministic() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  plus_one(n) + add_n(1)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis_a = build_minimal_analysis(&bridge);
    let analysis_b = build_minimal_analysis(&bridge);

    assert_eq!(
        analysis_a.propagated_result_demands,
        analysis_b.propagated_result_demands
    );
}

#[test]
fn bridge_to_analysis_leaves_unused_helper_body_nondemanded() {
    let src = r#"
fn helper(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 ( n + 1 )
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);

    let helper_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(0))
        .expect("helper should be first top-level body");
    assert_eq!(
        helper_body.result_observation,
        Observation::NoObs,
        "unused non-entry helper body should stay non-demanded"
    );
}

#[test]
fn bridge_to_analysis_marks_direct_top_level_callee_body_demanded_via_propagation() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 ( plus_one(n) )
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    let plus_one_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(0))
        .expect("plus_one should be first top-level body");

    assert_eq!(
        plus_one_body.result_observation,
        Observation::NeedValue,
        "directly called top-level callee body should be marked demanded via propagation"
    );
}

#[test]
fn bridge_to_analysis_recovered_local_closure_callee_gets_need_value_propagation() {
    let src = r#"
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  add_n(1)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    let linked_layout = analysis.bodies[0]
        .callee_closure_layouts
        .values()
        .next()
        .copied()
        .expect("recoverable closure call should have a layout link");

    assert_eq!(
        analysis
            .propagated_result_demands
            .get(&crate::ResultDemandCallee::ClosureLayout(linked_layout)),
        Some(&Observation::NeedValue),
        "recovered local closure callee should get NeedValue via first propagation step"
    );
}

#[test]
fn bridge_to_analysis_body_result_observations_are_deterministic() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 ( plus_one(n) )
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis_a = build_minimal_analysis(&bridge);
    let analysis_b = build_minimal_analysis(&bridge);

    assert_eq!(
        analysis_a
            .bodies
            .iter()
            .map(|body| (body.id, body.result_observation.clone()))
            .collect::<Vec<_>>(),
        analysis_b
            .bodies
            .iter()
            .map(|body| (body.id, body.result_observation.clone()))
            .collect::<Vec<_>>(),
        "demand-driven non-entry body result observations should be deterministic"
    );
}

#[test]
fn bridge_to_analysis_unused_helper_kernel_entry_and_result_stay_nondemanded() {
    let src = r#"
fn helper(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 ( n + 1 )
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);

    let helper_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(0))
        .expect("helper should be first top-level body");
    assert!(
        helper_body
            .kernel_entries
            .iter()
            .all(|entry| entry.demanded_result == Observation::NoObs),
        "unused helper body kernel entries should stay non-demanded"
    );

    assert!(
        analysis
            .kernels
            .iter()
            .filter(|kernel| kernel.body == BodyId(0))
            .all(|kernel| kernel.result_obs == Observation::NoObs),
        "unused helper body kernels should stay non-demanded"
    );
}

#[test]
fn bridge_to_analysis_direct_callee_kernel_entry_and_result_get_need_value() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 ( plus_one(n) )
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    let plus_one_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(0))
        .expect("plus_one should be first top-level body");

    assert!(
        plus_one_body
            .kernel_entries
            .iter()
            .all(|entry| entry.demanded_result == Observation::NeedValue),
        "direct callee body kernel entries should become demanded via propagation"
    );
    assert!(
        analysis
            .kernels
            .iter()
            .filter(|kernel| kernel.body == BodyId(0))
            .all(|kernel| kernel.result_obs == Observation::NeedValue),
        "direct callee kernels should become demanded via propagation"
    );
}

#[test]
fn bridge_to_analysis_kernel_demand_shape_is_deterministic() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 ( plus_one(n) )
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis_a = build_minimal_analysis(&bridge);
    let analysis_b = build_minimal_analysis(&bridge);

    assert_eq!(
        analysis_a
            .bodies
            .iter()
            .map(|body| {
                (
                    body.id,
                    body.kernel_entries
                        .iter()
                        .map(|entry| entry.demanded_result.clone())
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>(),
        analysis_b
            .bodies
            .iter()
            .map(|body| {
                (
                    body.id,
                    body.kernel_entries
                        .iter()
                        .map(|entry| entry.demanded_result.clone())
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>(),
        "kernel entry demand shape should be deterministic"
    );
    assert_eq!(
        analysis_a
            .kernels
            .iter()
            .map(|kernel| (kernel.id, kernel.result_obs.clone()))
            .collect::<Vec<_>>(),
        analysis_b
            .kernels
            .iter()
            .map(|kernel| (kernel.id, kernel.result_obs.clone()))
            .collect::<Vec<_>>(),
        "kernel region result demand shape should be deterministic"
    );
}

#[test]
fn bridge_to_analysis_seeds_closure_capture_for_add_n_style_program() {
    let src = r#"
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  add_n(1)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);

    assert_eq!(analysis.closure_layouts.len(), 1);
    let captures = &analysis.closure_layouts[0].captures;
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].name, "n");
}

#[test]
fn bridge_to_analysis_seeds_empty_capture_for_non_capturing_closure() {
    let src = r#"
fn main(n: U64) -> U64 (
  let id = (x) => x;
  id(1)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis_a = build_minimal_analysis(&bridge);
    let analysis_b = build_minimal_analysis(&bridge);

    assert_eq!(analysis_a.closure_layouts, analysis_b.closure_layouts);
    assert_eq!(analysis_a.closure_layouts.len(), 1);
    assert!(
        analysis_a.closure_layouts[0].captures.is_empty(),
        "non-capturing closure should have empty capture set"
    );
}

#[test]
fn bridge_to_analysis_excludes_nested_closure_refs_from_outer_capture_seed() {
    let src = r#"
fn main(n: U64, m: U64) -> U64 (
  let outer = (x) => (
    let inner = (y) => m + y;
    n + x
  );
  outer(1)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);

    assert_eq!(analysis.closure_layouts.len(), 2);
    let mut capture_name_sets = analysis
        .closure_layouts
        .iter()
        .map(|layout| {
            let mut names = layout
                .captures
                .iter()
                .map(|capture| capture.name.clone())
                .collect::<Vec<_>>();
            names.sort();
            names
        })
        .collect::<Vec<_>>();
    capture_name_sets.sort();

    assert_eq!(
        capture_name_sets,
        vec![vec!["m".to_owned()], vec!["n".to_owned()]],
        "outer closure should capture n only; inner closure should capture m only"
    );
}
