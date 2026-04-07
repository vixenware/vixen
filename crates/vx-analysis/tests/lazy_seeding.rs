use vx_analysis::{
    BodyId, ExtractedNodeReason, FieldKey, SingleSourceBridge, build_minimal_analysis,
    lower_executable_program_for_entry_body,
};

fn product_exprs_for_body(
    bridge: &SingleSourceBridge,
    body: BodyId,
) -> std::collections::HashSet<u32> {
    bridge
        .products
        .iter()
        .filter(|p| p.body == body)
        .map(|p| p.expr.0)
        .collect()
}

/// Helper: lower a program and return the KernelOps for the entry body.
fn lower_entry_ops(
    source: &str,
) -> (
    vx_analysis::ExecutableProgram,
    SingleSourceBridge,
    vx_analysis::AnalysisResult,
    BodyId,
) {
    let bridge = SingleSourceBridge::from_source_text(source);
    assert!(
        bridge.parse_errors.is_empty(),
        "parse errors: {:?}",
        bridge.parse_errors
    );
    assert!(
        bridge.resolve_errors.is_empty(),
        "resolve errors: {:?}",
        bridge.resolve_errors
    );
    let body_id = bridge
        .bodies
        .iter()
        .find(|b| b.name == "main")
        .expect("no main function")
        .id;
    let analysis = build_minimal_analysis(&bridge);
    let program = lower_executable_program_for_entry_body(&analysis, &bridge, body_id)
        .expect("lowering failed");
    (program, bridge, analysis, body_id)
}

/// Verify that seed_lazy_product_fields produces LazyProductField extracted nodes
/// for dead siblings in a simple `let p = (42, bomb()); p._0` pattern.
/// This tests the analysis seeding, not just runtime behavior.
#[test]
fn lazy_seeding_simple_tuple() {
    let source = r#"
fn bomb() -> U64 ( trap("boom") )
fn main() -> U64 (
  let p = (42, bomb());
  p._0
)
"#;
    let bridge = SingleSourceBridge::from_source_text(source);
    let analysis = build_minimal_analysis(&bridge);
    let body_id = bridge.bodies.iter().find(|b| b.name == "main").unwrap().id;
    let main_body = analysis.bodies.iter().find(|b| b.id == body_id).unwrap();

    let product_exprs = product_exprs_for_body(&bridge, body_id);

    // There should be a LazyProductField for field _1 (the dead sibling)
    let lazy_nodes: Vec<_> = main_body
        .extracted_nodes
        .iter()
        .filter(|en| matches!(en.reason, ExtractedNodeReason::LazyProductField { .. }))
        .collect();

    assert!(
        lazy_nodes.iter().any(|en| matches!(
            &en.reason,
            ExtractedNodeReason::LazyProductField { field } if *field == FieldKey::Index(1)
        )),
        "Expected LazyProductField for _1, got: {lazy_nodes:?}"
    );

    // Non-dodgeable requirement: the seed must be keyed by the *product construction*
    // ExprId, not the projection receiver ExprId.
    assert!(
        lazy_nodes.iter().any(|en| {
            matches!(
                &en.reason,
                ExtractedNodeReason::LazyProductField { field } if *field == FieldKey::Index(1)
            ) && product_exprs.contains(&en.source.expr.0)
        }),
        "Expected LazyProductField(_1) keyed by a BridgeProduct.expr. \
         Lazy nodes: {lazy_nodes:?}, product exprs: {product_exprs:?}"
    );
}

/// Verify that for a deep chain `outer._0._0._0`, the analysis seeds
/// LazyProductField for dead siblings at ALL levels of nesting.
#[test]
fn lazy_seeding_deep_tuple_chain() {
    let source = r#"
fn bomb() -> U64 ( trap("boom") )
fn main() -> U64 (
  let outer = (((7, bomb()), bomb()), bomb());
  outer._0._0._0
)
"#;
    let bridge = SingleSourceBridge::from_source_text(source);
    let analysis = build_minimal_analysis(&bridge);
    let body_id = bridge.bodies.iter().find(|b| b.name == "main").unwrap().id;
    let main_body = analysis.bodies.iter().find(|b| b.id == body_id).unwrap();

    let product_exprs = product_exprs_for_body(&bridge, body_id);

    let lazy_fields: Vec<_> = main_body
        .extracted_nodes
        .iter()
        .filter_map(|en| match &en.reason {
            ExtractedNodeReason::LazyProductField { field } => {
                Some((en.source.expr, field.clone()))
            }
            _ => None,
        })
        .collect();

    // There should be lazy fields seeded at multiple product construction sites
    // (one for each level of nesting that has a dead sibling)
    let lazy_index_1_exprs: std::collections::HashSet<_> = lazy_fields
        .iter()
        .filter_map(|(expr, f)| (*f == FieldKey::Index(1)).then_some(expr.0))
        .collect();

    assert!(
        lazy_index_1_exprs.len() >= 3,
        "Expected LazyProductField(_1) at 3 nesting levels, got {}: {lazy_fields:?}",
        lazy_index_1_exprs.len()
    );

    assert!(
        lazy_index_1_exprs
            .iter()
            .all(|expr| product_exprs.contains(expr)),
        "Expected LazyProductField(_1) exprs to all be BridgeProduct.expr ids. \
         got exprs {lazy_index_1_exprs:?}, product exprs: {product_exprs:?}, all lazy: {lazy_fields:?}"
    );
}

/// Verify that lowering emits AllocThunk for dead siblings when the product
/// IS constructed (not inlined away). We check the entry body's ops.
#[test]
fn lowering_emits_alloc_thunk_for_dead_sibling() {
    use vx_analysis::KernelOp;

    let source = r#"
fn bomb() -> U64 ( trap("boom") )
fn main() -> (U64, U64) (
  let p = (42, bomb());
  let x = p._0;
  p
)
"#;
    let (program, _bridge, _analysis, body_id) = lower_entry_ops(source);

    let entry_ops = &program
        .bodies
        .get(&body_id)
        .expect("missing entry body")
        .ops;

    let mut producer_kind_by_slot = std::collections::HashMap::new();
    for op in entry_ops {
        match op {
            KernelOp::AllocThunk { dst, .. } => {
                producer_kind_by_slot.insert(*dst, "AllocThunk");
            }
            KernelOp::CallBody { dst, .. } => {
                producer_kind_by_slot.insert(*dst, "CallBody");
            }
            KernelOp::MakePair { dst, .. } => {
                producer_kind_by_slot.insert(*dst, "MakePair");
            }
            _ => {}
        }
    }

    assert!(
        entry_ops
            .iter()
            .any(|op| matches!(op, KernelOp::MakePair { .. })),
        "Expected MakePair to be emitted (main returns the pair, so it must be constructed). Ops: {entry_ops:#?}"
    );

    // For the MakePair that constructs `p`, the dead sibling is index 1, so `second`
    // must come from an AllocThunk.
    let mut found = false;
    for op in entry_ops {
        let KernelOp::MakePair { second, .. } = op else {
            continue;
        };
        let vx_analysis::Operand::Slot(slot) = second else {
            continue;
        };
        if producer_kind_by_slot.get(slot).copied() == Some("AllocThunk") {
            found = true;
            break;
        }
    }
    assert!(
        found,
        "Expected a MakePair whose `second` operand is produced by AllocThunk (dead sibling thunking). Ops: {entry_ops:#?}"
    );

    // Extra guard: the dead sibling must not be directly computed in the entry body.
    // If we see a CallBody in the entry ops, it's almost certainly an eager `bomb()`.
    assert!(
        !entry_ops
            .iter()
            .any(|op| matches!(op, KernelOp::CallBody { .. })),
        "Expected no CallBody in entry ops; bomb() should live in the thunk body. Ops: {entry_ops:#?}"
    );
}

/// For deep chains, if any intermediate product is constructed, its dead
/// siblings must be thunked.
#[test]
fn lowering_deep_chain_thunks_if_constructed() {
    use vx_analysis::KernelOp;

    let source = r#"
fn bomb() -> U64 ( trap("boom") )
fn main() -> U64 (
  let outer = (((7, bomb()), bomb()), bomb());
  outer._0._0._0
)
"#;
    let (program, _bridge, _analysis, _body_id) = lower_entry_ops(source);

    let all_ops: Vec<_> = program
        .bodies
        .values()
        .flat_map(|body| body.ops.iter())
        .collect();

    let make_pair_count = all_ops
        .iter()
        .filter(|op| matches!(op, KernelOp::MakePair { .. }))
        .count();
    let alloc_thunk_count = all_ops
        .iter()
        .filter(|op| matches!(op, KernelOp::AllocThunk { .. }))
        .count();

    if make_pair_count > 0 {
        assert!(
            alloc_thunk_count >= make_pair_count,
            "Each MakePair should have at least one AllocThunk for dead sibling. \
             MakePairs: {make_pair_count}, AllocThunks: {alloc_thunk_count}. Ops: {all_ops:#?}"
        );
    }
}
