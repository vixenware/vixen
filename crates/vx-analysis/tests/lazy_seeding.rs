use vx_analysis::{
    BodyId, ExtractedNodeReason, FieldKey, SingleSourceBridge, build_minimal_analysis,
    lower_executable_program_for_entry_body,
};

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

    // There should be a LazyProductField for field _1 (the dead sibling)
    let lazy_fields: Vec<_> = main_body
        .extracted_nodes
        .iter()
        .filter_map(|en| match &en.reason {
            ExtractedNodeReason::LazyProductField { field } => Some(field.clone()),
            _ => None,
        })
        .collect();

    assert!(
        lazy_fields.contains(&FieldKey::Index(1)),
        "Expected LazyProductField for _1, got: {lazy_fields:?}"
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
    let lazy_index_1_count = lazy_fields
        .iter()
        .filter(|(_, f)| *f == FieldKey::Index(1))
        .count();

    assert!(
        lazy_index_1_count >= 3,
        "Expected LazyProductField for _1 at 3 nesting levels (outer, middle, inner), \
         got {lazy_index_1_count}: {lazy_fields:?}"
    );
}

/// Verify that lowering emits AllocThunk for dead siblings when the product
/// IS constructed (not inlined away). We check the entry body's ops.
#[test]
fn lowering_emits_alloc_thunk_for_dead_sibling() {
    use vx_analysis::KernelOp;

    let source = r#"
fn bomb() -> U64 ( trap("boom") )
fn main() -> U64 (
  let p = (42, bomb());
  p._0
)
"#;
    let (program, _bridge, _analysis, _body_id) = lower_entry_ops(source);

    // Collect all ops across all bodies
    let all_ops: Vec<_> = program
        .bodies
        .values()
        .flat_map(|body| body.ops.iter())
        .collect();

    // The entry body should have either:
    // (a) No MakePair at all (fully inlined — acceptable), OR
    // (b) A MakePair where the dead field is an AllocThunk result
    let has_make_pair = all_ops
        .iter()
        .any(|op| matches!(op, KernelOp::MakePair { .. }));

    if has_make_pair {
        // If we're constructing the pair, there MUST be an AllocThunk for the dead field
        let has_alloc_thunk = all_ops
            .iter()
            .any(|op| matches!(op, KernelOp::AllocThunk { .. }));
        assert!(
            has_alloc_thunk,
            "MakePair emitted but no AllocThunk for dead sibling! Ops: {all_ops:#?}"
        );
    }
    // If no MakePair, the projection was fully inlined — also acceptable
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
