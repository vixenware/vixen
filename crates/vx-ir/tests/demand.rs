//! Tests for VIR demand analysis.

use vx_ir::BodyId;
use vx_ir::build::{VirBuilder, VirProgram};
use vx_ir::demand::{Observation, VirCallTarget, VirDemandResult, analyze_demand};
use vx_ir::expr::FieldKey;
use vx_ir::resolve::resolve_names;
use vx_parser::{SourceId, parse_source_file};

/// Helper: build VIR program, resolve names, run demand analysis.
fn analyze_source(source: &str) -> (VirProgram, VirDemandResult) {
    let parsed = parse_source_file(source, SourceId(0));
    let file = parsed.tree();
    let mut program = VirBuilder::build_program(&[(SourceId(0), file)]);
    let _errors = resolve_names(&mut program.arena);
    let result = analyze_demand(&program);
    (program, result)
}

/// Find a function body by name.
fn fn_body(program: &VirProgram, name: &str) -> Option<BodyId> {
    for (i, body) in program.arena.bodies.iter().enumerate() {
        if let vx_ir::body::BodyOwner::Function { item } = body.owner
            && let vx_ir::item::ItemKind::Function(f) = &program.arena[item].kind
            && f.name == name
        {
            return Some(BodyId(i as u32));
        }
    }
    None
}

/// Get observation for a body.
fn body_obs(result: &VirDemandResult, body_id: BodyId) -> &Observation {
    &result
        .bodies
        .iter()
        .find(|b| b.body_id == body_id)
        .unwrap()
        .result_observation
}

// =========================================================================
// 1. Single function, no calls
// =========================================================================

#[test]
fn single_function_no_calls() {
    let (program, result) = analyze_source("fn main() -> U64 ( 42 )");
    let main_id = fn_body(&program, "main").unwrap();
    assert_eq!(result.entry_points, vec![main_id]);
    assert!(result.reachable.contains(&main_id));
    assert_eq!(body_obs(&result, main_id), &Observation::NeedValue);
}

// =========================================================================
// 2. Direct call chain
// =========================================================================

#[test]
fn direct_call_chain() {
    let (program, result) =
        analyze_source("fn helper() -> U64 ( 1 ); fn main() -> U64 ( helper() )");
    let main_id = fn_body(&program, "main").unwrap();
    let helper_id = fn_body(&program, "helper").unwrap();
    assert_eq!(result.entry_points, vec![main_id]);
    assert!(result.reachable.contains(&main_id));
    assert!(result.reachable.contains(&helper_id));
    assert_eq!(body_obs(&result, helper_id), &Observation::NeedValue);
}

// =========================================================================
// 3. Dead function
// =========================================================================

#[test]
fn dead_function() {
    let (program, result) = analyze_source("fn dead() -> U64 ( 1 ); fn main() -> U64 ( 42 )");
    let main_id = fn_body(&program, "main").unwrap();
    let dead_id = fn_body(&program, "dead").unwrap();
    assert!(result.reachable.contains(&main_id));
    assert!(!result.reachable.contains(&dead_id));
    assert_eq!(body_obs(&result, dead_id), &Observation::NoObs);
}

// =========================================================================
// 4. Closure call
// =========================================================================

#[test]
fn closure_call() {
    let (program, result) = analyze_source("fn main() -> U64 ( let f = () => 42; f() )");
    let main_id = fn_body(&program, "main").unwrap();
    assert!(result.reachable.contains(&main_id));

    // Find the closure body.
    let closure_bodies: Vec<BodyId> = program
        .arena
        .bodies
        .iter()
        .enumerate()
        .filter(|(_, b)| matches!(b.owner, vx_ir::body::BodyOwner::Closure))
        .map(|(i, _)| BodyId(i as u32))
        .collect();
    assert_eq!(closure_bodies.len(), 1);
    assert!(
        result.reachable.contains(&closure_bodies[0]),
        "closure body should be reachable"
    );

    // Check call edge exists with Closure target.
    let main_analyzed = result.bodies.iter().find(|b| b.body_id == main_id).unwrap();
    assert!(
        main_analyzed
            .call_edges
            .iter()
            .any(|e| e.callee == VirCallTarget::Closure(closure_bodies[0])),
        "expected Closure call edge, got {:?}",
        main_analyzed.call_edges
    );
}

// =========================================================================
// 5. Transitive call chain
// =========================================================================

#[test]
fn transitive_call_chain() {
    let (program, result) =
        analyze_source("fn a() -> U64 ( 1 ); fn b() -> U64 ( a() ); fn main() -> U64 ( b() )");
    let a_id = fn_body(&program, "a").unwrap();
    let b_id = fn_body(&program, "b").unwrap();
    let main_id = fn_body(&program, "main").unwrap();
    assert!(result.reachable.contains(&a_id));
    assert!(result.reachable.contains(&b_id));
    assert!(result.reachable.contains(&main_id));
}

// =========================================================================
// 6. Build entry
// =========================================================================

#[test]
fn build_entry() {
    let (program, result) = analyze_source("fn build() -> U64 ( 42 )");
    let build_id = fn_body(&program, "build").unwrap();
    assert_eq!(result.entry_points, vec![build_id]);
    assert!(result.reachable.contains(&build_id));
}

// =========================================================================
// 7. No main or build — root body becomes entry
// =========================================================================

#[test]
fn root_body_entry() {
    let (program, result) = analyze_source("fn foo() -> U64 ( 42 )");
    let foo_id = fn_body(&program, "foo").unwrap();
    assert_eq!(result.entry_points, vec![foo_id]);
    assert!(result.reachable.contains(&foo_id));
}

// =========================================================================
// 8. Nested closure is not an entry point
// =========================================================================

#[test]
fn nested_closure_not_entry() {
    let (program, result) = analyze_source("fn main() -> U64 ( let f = () => 42; f() )");
    let main_id = fn_body(&program, "main").unwrap();
    // Only main should be an entry point.
    assert_eq!(result.entry_points, vec![main_id]);
}

// =========================================================================
// 9. Local function call
// =========================================================================

#[test]
fn local_function_call() {
    let (program, result) =
        analyze_source("fn main() -> U64 ( fn helper() -> U64 ( 1 ); helper() )");
    let main_id = fn_body(&program, "main").unwrap();
    let helper_id = fn_body(&program, "helper").unwrap();
    assert!(result.reachable.contains(&main_id));
    assert!(result.reachable.contains(&helper_id));
}

// =========================================================================
// 10. Cycle
// =========================================================================

#[test]
fn cycle_reachability() {
    let (program, result) =
        analyze_source("fn a() -> U64 ( b() ); fn b() -> U64 ( a() ); fn main() -> U64 ( a() )");
    let a_id = fn_body(&program, "a").unwrap();
    let b_id = fn_body(&program, "b").unwrap();
    let main_id = fn_body(&program, "main").unwrap();
    assert!(result.reachable.contains(&a_id));
    assert!(result.reachable.contains(&b_id));
    assert!(result.reachable.contains(&main_id));
}

// =========================================================================
// 11. Top-level thunk not entry
// =========================================================================

#[test]
fn top_level_thunk_not_entry() {
    let (_, result) = analyze_source("let x = 42");
    // No top-level named functions → no entry points.
    assert!(
        result.entry_points.is_empty(),
        "expected no entry points, got {:?}",
        result.entry_points
    );
}

// =========================================================================
// 12. Nested call in callee subtree
// =========================================================================

#[test]
fn nested_call_in_callee() {
    // make_fn() is called in the callee position of the outer call.
    // The outer call resolves to Unknown, but make_fn() edge should still exist.
    let (program, result) =
        analyze_source("fn make_fn() -> U64 ( 42 ); fn main() -> U64 ( (make_fn())() )");
    let make_fn_id = fn_body(&program, "make_fn").unwrap();
    let main_id = fn_body(&program, "main").unwrap();
    assert!(result.reachable.contains(&main_id));
    assert!(
        result.reachable.contains(&make_fn_id),
        "make_fn should be reachable via nested callee"
    );
}

// =========================================================================
// 13. Destructuring limitation (known under-approximation)
// =========================================================================

#[test]
fn destructuring_closure_not_resolved() {
    let (program, result) = analyze_source("fn main() -> U64 ( let (f, x) = (() => 1, 2); f() )");
    let main_id = fn_body(&program, "main").unwrap();
    assert!(result.reachable.contains(&main_id));

    // The closure body is NOT reachable — known limitation.
    let closure_bodies: Vec<BodyId> = program
        .arena
        .bodies
        .iter()
        .enumerate()
        .filter(|(_, b)| matches!(b.owner, vx_ir::body::BodyOwner::Closure))
        .map(|(i, _)| BodyId(i as u32))
        .collect();
    assert_eq!(closure_bodies.len(), 1);
    assert!(
        !result.reachable.contains(&closure_bodies[0]),
        "destructuring-bound closure is a known limitation — should NOT be reachable"
    );
}

// =========================================================================
// Lazy field seeding tests
// =========================================================================

/// Helper: check that a specific lazy field exists.
fn has_lazy(
    result: &VirDemandResult,
    product_expr_body_name: &str,
    field: FieldKey,
    program: &VirProgram,
) -> bool {
    // Find product exprs in the named function body.
    let body_id = fn_body(program, product_expr_body_name).unwrap();
    result
        .lazy_fields
        .iter()
        .any(|lf| lf.body == body_id && lf.field == field)
}

// =========================================================================
// 14. Simple tuple dead sibling
// =========================================================================

#[test]
fn lazy_simple_tuple_dead_sibling() {
    let (program, result) =
        analyze_source("fn bomb() -> U64 ( 0 ); fn main() -> U64 ( let p = (42, bomb()); p._0 )");
    assert!(
        has_lazy(&result, "main", FieldKey::Index(1), &program),
        "expected Index(1) lazy, got {:?}",
        result.lazy_fields
    );
    assert!(
        !has_lazy(&result, "main", FieldKey::Index(0), &program),
        "Index(0) should NOT be lazy"
    );
}

// =========================================================================
// 15. All fields projected — no lazy
// =========================================================================

#[test]
fn lazy_all_fields_projected() {
    let (_, result) = analyze_source("fn main() -> U64 ( let p = (1, 2); p._0 + p._1 )");
    assert!(
        result.lazy_fields.is_empty(),
        "expected no lazy fields, got {:?}",
        result.lazy_fields
    );
}

// =========================================================================
// 16. Deep nested tuple chain
// =========================================================================

#[test]
fn lazy_deep_nested_tuple() {
    let (_, result) = analyze_source("fn main() -> U64 ( let p = ((1, 2), 3); p._0._0 )");
    // Outer tuple: _0 projected, _1 (=3) lazy.
    // Inner tuple (1, 2): _0 projected, _1 (=2) lazy.
    let lazy_index_1: Vec<_> = result
        .lazy_fields
        .iter()
        .filter(|lf| lf.field == FieldKey::Index(1))
        .collect();
    assert!(
        lazy_index_1.len() >= 2,
        "expected at least 2 lazy Index(1) fields (outer + inner), got {:?}",
        result.lazy_fields
    );
}

// =========================================================================
// 17. Record field lazy
// =========================================================================

#[test]
fn lazy_record_field() {
    let (_, result) = analyze_source("fn main() -> U64 ( let r = { a: 1, b: 2 }; r.a )");
    assert!(
        result
            .lazy_fields
            .iter()
            .any(|lf| lf.field == FieldKey::Name("b".into())),
        "expected Name(\"b\") lazy, got {:?}",
        result.lazy_fields
    );
}

// =========================================================================
// 18. Cross-function lazy (named fn)
// =========================================================================

#[test]
fn lazy_cross_function_named() {
    let (program, result) = analyze_source(
        "fn pick(p: (U64, U64)) -> U64 ( p._0 ); fn main() -> U64 ( pick((42, 99)) )",
    );
    assert!(
        has_lazy(&result, "main", FieldKey::Index(1), &program),
        "expected Index(1) lazy at caller tuple, got {:?}",
        result.lazy_fields
    );
}

// =========================================================================
// 19. Cross-function lazy (closure)
// =========================================================================

#[test]
fn lazy_cross_function_closure() {
    let (program, result) = analyze_source("fn main() -> U64 ( let f = (p) => p._0; f((42, 99)) )");
    assert!(
        has_lazy(&result, "main", FieldKey::Index(1), &program),
        "expected Index(1) lazy at caller tuple (closure target), got {:?}",
        result.lazy_fields
    );
}

// =========================================================================
// 20. Cross-function all fields used — no lazy
// =========================================================================

#[test]
fn lazy_cross_function_all_used() {
    let (_, result) = analyze_source(
        "fn both(p: (U64, U64)) -> U64 ( p._0 + p._1 ); fn main() -> U64 ( both((42, 99)) )",
    );
    assert!(
        result.lazy_fields.is_empty(),
        "expected no lazy fields, got {:?}",
        result.lazy_fields
    );
}

// =========================================================================
// 21. No projection — no lazy
// =========================================================================

#[test]
fn lazy_no_projection() {
    let (_, result) = analyze_source("fn main() -> U64 ( let p = (1, 2); 42 )");
    assert!(
        result.lazy_fields.is_empty(),
        "expected no lazy fields, got {:?}",
        result.lazy_fields
    );
}

// =========================================================================
// 22. Binding chain
// =========================================================================

#[test]
fn lazy_binding_chain() {
    let (program, result) = analyze_source("fn main() -> U64 ( let p = (1, 2); let q = p; q._0 )");
    assert!(
        has_lazy(&result, "main", FieldKey::Index(1), &program),
        "expected Index(1) lazy (traced through binding chain), got {:?}",
        result.lazy_fields
    );
}

// =========================================================================
// 23. Destructuring limitation
// =========================================================================

#[test]
fn lazy_destructuring_limitation() {
    let (_, result) = analyze_source("fn main() -> U64 ( let (p, q) = ((1, 2), 3); p._0 )");
    // p has init_expr: None (destructuring), so no lazy fields detected.
    // The outer tuple might get lazy seeding for its fields from local projection,
    // but p._0 can't trace through to the inner tuple.
    // This is a known limitation.
    let _inner_lazy: Vec<_> = result
        .lazy_fields
        .iter()
        .filter(|lf| {
            // Check if any lazy field is at the inner tuple (1, 2).
            // Since we can't trace through destructuring, there shouldn't be any.
            lf.field == FieldKey::Index(1)
        })
        .collect();
    // The test just documents the limitation — inner tuple laziness is not detected.
    // We can't assert exactly because the outer tuple might have lazy fields.
}

// =========================================================================
// 24. Mixed local + cross-function projections
// =========================================================================

#[test]
fn lazy_mixed_local_and_cross_function() {
    let (program, result) = analyze_source(
        "fn pick0(p: (U64, U64, U64)) -> U64 ( p._0 ); fn main() -> U64 ( let p = (1, 2, 3); pick0(p) + p._1 )",
    );
    // projected = {_0 (cross), _1 (local)}, lazy = {_2}
    assert!(
        has_lazy(&result, "main", FieldKey::Index(2), &program),
        "expected Index(2) lazy, got {:?}",
        result.lazy_fields
    );
    assert!(
        !has_lazy(&result, "main", FieldKey::Index(0), &program),
        "Index(0) should NOT be lazy (projected by callee)"
    );
    assert!(
        !has_lazy(&result, "main", FieldKey::Index(1), &program),
        "Index(1) should NOT be lazy (projected locally)"
    );
}

// =========================================================================
// 25. Multiple cross-function call sites
// =========================================================================

#[test]
fn lazy_multiple_cross_function_sites() {
    let (program, result) = analyze_source(
        "fn pick0(p: (U64, U64, U64)) -> U64 ( p._0 ); fn pick1(p: (U64, U64, U64)) -> U64 ( p._1 ); fn main() -> U64 ( let p = (1, 2, 3); pick0(p) + pick1(p) )",
    );
    // projected = {_0 (pick0), _1 (pick1)}, lazy = {_2}
    assert!(
        has_lazy(&result, "main", FieldKey::Index(2), &program),
        "expected Index(2) lazy, got {:?}",
        result.lazy_fields
    );
    assert!(
        !has_lazy(&result, "main", FieldKey::Index(0), &program),
        "Index(0) should NOT be lazy"
    );
    assert!(
        !has_lazy(&result, "main", FieldKey::Index(1), &program),
        "Index(1) should NOT be lazy"
    );
}
