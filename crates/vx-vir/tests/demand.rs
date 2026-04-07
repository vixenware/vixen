//! Tests for VIR demand analysis.

use std::collections::BTreeSet;

use vx_parser::parse_source_file;
use vx_resolve::SourceId;
use vx_vir::BodyId;
use vx_vir::build::{VirBuilder, VirProgram};
use vx_vir::demand::{Observation, VirCallTarget, VirDemandResult, analyze_demand};
use vx_vir::resolve::resolve_names;

/// Helper: build VIR program, resolve names, run demand analysis.
fn analyze_source(source: &str) -> (VirProgram, VirDemandResult) {
    let parsed = parse_source_file(source);
    let file = parsed.tree();
    let mut program = VirBuilder::build_program(&[(SourceId(0), file)]);
    let _errors = resolve_names(&mut program.arena);
    let result = analyze_demand(&program);
    (program, result)
}

/// Find a function body by name.
fn fn_body(program: &VirProgram, name: &str) -> Option<BodyId> {
    for (i, body) in program.arena.bodies.iter().enumerate() {
        if let vx_vir::body::BodyOwner::Function { item } = body.owner {
            if let vx_vir::item::ItemKind::Function(f) = &program.arena[item].kind {
                if f.name == name {
                    return Some(BodyId(i as u32));
                }
            }
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
        .filter(|(_, b)| matches!(b.owner, vx_vir::body::BodyOwner::Closure))
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
        .filter(|(_, b)| matches!(b.owner, vx_vir::body::BodyOwner::Closure))
        .map(|(i, _)| BodyId(i as u32))
        .collect();
    assert_eq!(closure_bodies.len(), 1);
    assert!(
        !result.reachable.contains(&closure_bodies[0]),
        "destructuring-bound closure is a known limitation — should NOT be reachable"
    );
}
