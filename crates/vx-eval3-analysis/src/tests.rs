use crate::{
    BodyId, ExprId, ExtractedNodeReason, Observation, RuntimeBoundaryKind, RuntimeNode, UseSiteId,
    fixtures::{
        build_shaped_exec_fixture, canonical_program_a_fixture, canonical_program_b_fixture,
        canonical_program_c_fixture, canonical_program_d_fixture,
    },
};

#[test]
fn program_a_represents_projection_demand_without_demanding_bomb_path() {
    let analysis = canonical_program_a_fixture();
    analysis.validate().expect("fixture A should validate");

    let body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(0))
        .expect("main body should exist");

    let pair_obs = body
        .use_observations
        .get(&UseSiteId(0))
        .expect("pair observation should exist");
    assert!(matches!(
        pair_obs,
        Observation::NeedProduct(fields)
            if fields.get(&crate::FieldKey::Index(0)) == Some(&Observation::NeedValue)
    ));

    let bomb_expr = ExprId(4);
    assert!(
        !body
            .extracted_nodes
            .iter()
            .any(|node| node.source.expr == bomb_expr),
        "bomb path should not appear as demanded extracted work"
    );
}

#[test]
fn program_b_represents_closure_layout_selected_sibling_and_callable_demand() {
    let analysis = canonical_program_b_fixture();
    analysis.validate().expect("fixture B should validate");

    assert_eq!(analysis.closure_layouts.len(), 1);
    assert_eq!(analysis.closure_layouts[0].captures.len(), 1);
    assert_eq!(analysis.closure_layouts[0].captures[0].name, "n");

    let body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(1))
        .expect("main body should exist");

    assert_eq!(
        body.use_observations.get(&UseSiteId(10)),
        Some(&Observation::NeedCallable)
    );
    assert!(
        body.use_observations.contains_key(&UseSiteId(12)),
        "selected sibling demand should be represented explicitly"
    );
}

#[test]
fn program_c_represents_single_shared_node_for_plus_one_reuse_pressure() {
    let analysis = canonical_program_c_fixture();
    analysis.validate().expect("fixture C should validate");

    let body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(2))
        .expect("main body should exist");

    let shared_nodes: Vec<_> = body
        .extracted_nodes
        .iter()
        .filter(|node| node.reason == ExtractedNodeReason::SharedAcrossUses)
        .collect();
    assert_eq!(
        shared_nodes.len(),
        1,
        "plus_one should be represented as one shared extracted node"
    );
}

#[test]
fn program_d_keeps_higher_order_callable_path_first_class() {
    let analysis = canonical_program_d_fixture();
    analysis.validate().expect("fixture D should validate");

    let apply_body = analysis
        .bodies
        .iter()
        .find(|body| body.id == BodyId(3))
        .expect("apply body should exist");
    assert_eq!(
        apply_body.use_observations.get(&UseSiteId(30)),
        Some(&Observation::NeedCallable)
    );

    let has_exec_or_discover_boundary = analysis.runtime_nodes.iter().any(|node| {
        matches!(
            node,
            RuntimeNode::Exec { .. } | RuntimeNode::Discover { .. }
        )
    });
    assert!(
        !has_exec_or_discover_boundary,
        "higher-order local call should not collapse into runtime boundary calls"
    );
}

#[test]
fn build_shaped_fixture_has_local_kernel_and_exec_runtime_boundary() {
    let analysis = build_shaped_exec_fixture();
    analysis.validate().expect("build fixture should validate");

    assert_eq!(analysis.kernels.len(), 1);
    assert_eq!(analysis.kernels[0].id.0, 50);

    let boundary_reason =
        analysis.bodies[0]
            .extracted_nodes
            .iter()
            .find_map(|node| match node.reason {
                ExtractedNodeReason::RuntimeBoundary { kind } => Some(kind),
                _ => None,
            });
    assert_eq!(boundary_reason, Some(RuntimeBoundaryKind::Exec));

    assert!(
        analysis
            .runtime_nodes
            .iter()
            .any(|node| matches!(node, RuntimeNode::Exec { .. })),
        "fixture should include one exec-tagged runtime boundary"
    );
}
