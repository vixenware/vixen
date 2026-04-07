//! Tests for the VIR capture analysis pass.

use vx_parser::parse_source_file;
use vx_resolve::SourceId;
use vx_vir::body::BodyOwner;
use vx_vir::build::VirBuilder;
use vx_vir::capture::analyze_captures;
use vx_vir::resolve::resolve_names;
use vx_vir::{BodyId, VirArena};

/// Helper: build VIR, resolve names, run capture analysis.
fn analyze_source(source: &str) -> VirArena {
    let parsed = parse_source_file(source);
    let file = parsed.tree();
    let mut program = VirBuilder::build_program(&[(SourceId(0), file)]);
    let _errors = resolve_names(&mut program.arena);
    analyze_captures(&mut program.arena);
    program.arena
}

/// Find a closure body by searching for BodyOwner::Closure bodies.
/// Returns all closure bodies in order.
fn closure_bodies(arena: &VirArena) -> Vec<BodyId> {
    arena
        .bodies
        .iter()
        .enumerate()
        .filter(|(_, b)| matches!(b.owner, BodyOwner::Closure))
        .map(|(i, _)| BodyId(i as u32))
        .collect()
}

/// Find a function body by name.
fn fn_body(arena: &VirArena, name: &str) -> Option<BodyId> {
    for (i, body) in arena.bodies.iter().enumerate() {
        if let BodyOwner::Function { item } = body.owner {
            if let vx_vir::item::ItemKind::Function(f) = &arena[item].kind {
                if f.name == name {
                    return Some(BodyId(i as u32));
                }
            }
        }
    }
    None
}

/// Get capture binding names for a body.
fn capture_names(arena: &VirArena, body_id: BodyId) -> Vec<String> {
    arena[body_id]
        .captures
        .iter()
        .map(|&bid| arena[bid].name.clone())
        .collect()
}

// =========================================================================
// 1. No captures (top-level fn)
// =========================================================================

#[test]
fn no_captures_top_level_fn() {
    let arena = analyze_source("fn f() ( 1 )");
    let body = fn_body(&arena, "f").unwrap();
    assert!(arena[body].captures.is_empty());
}

// =========================================================================
// 2. Simple closure capture
// =========================================================================

#[test]
fn simple_closure_capture() {
    let arena = analyze_source("let x = 1; let f = () => x");
    let closures = closure_bodies(&arena);
    assert_eq!(closures.len(), 1);
    let names = capture_names(&arena, closures[0]);
    assert_eq!(names, vec!["x"]);
}

// =========================================================================
// 3. No capture for own binding
// =========================================================================

#[test]
fn no_capture_for_own_param() {
    let arena = analyze_source("let f = (x) => x");
    let closures = closure_bodies(&arena);
    assert_eq!(closures.len(), 1);
    assert!(arena[closures[0]].captures.is_empty());
}

// =========================================================================
// 4. Multiple captures
// =========================================================================

#[test]
fn multiple_captures() {
    let arena = analyze_source("let x = 1; let y = 2; let f = () => x + y");
    let closures = closure_bodies(&arena);
    assert_eq!(closures.len(), 1);
    let mut names = capture_names(&arena, closures[0]);
    names.sort();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"x".to_string()));
    assert!(names.contains(&"y".to_string()));
}

// =========================================================================
// 5. Nested closure — propagated captures
// =========================================================================

#[test]
fn nested_closure_propagated() {
    let arena = analyze_source("let x = 1; let f = () => (() => x)");
    let closures = closure_bodies(&arena);
    assert_eq!(closures.len(), 2);

    // Both closures should capture x.
    // Inner closure captures x directly.
    // Outer closure captures x via propagation from inner.
    let inner_names = capture_names(&arena, closures[0]);
    let outer_names = capture_names(&arena, closures[1]);

    // One of them is the inner, one is the outer. Check both have x.
    assert!(
        inner_names.contains(&"x".to_string()) && outer_names.contains(&"x".to_string()),
        "inner: {:?}, outer: {:?}",
        inner_names,
        outer_names
    );
}

// =========================================================================
// 6. Shadowing — no capture
// =========================================================================

#[test]
fn shadowing_no_capture() {
    let arena = analyze_source("let x = 1; let f = (x) => x");
    let closures = closure_bodies(&arena);
    assert_eq!(closures.len(), 1);
    assert!(
        arena[closures[0]].captures.is_empty(),
        "expected no captures, got {:?}",
        capture_names(&arena, closures[0])
    );
}

// =========================================================================
// 7. Local named function captures
// =========================================================================

#[test]
fn local_fn_captures() {
    let arena = analyze_source("fn main() ( let x = 1; fn f() ( x ) )");
    let body = fn_body(&arena, "f").unwrap();
    let names = capture_names(&arena, body);
    assert_eq!(names, vec!["x"]);
}

// =========================================================================
// 8. Local named function cutoff — unresolved doesn't capture
// =========================================================================

#[test]
fn local_fn_cutoff_no_capture() {
    let arena = analyze_source("fn main() ( fn f() ( b ); let b = 2 )");
    let body = fn_body(&arena, "f").unwrap();
    // b is unresolved (cutoff), so no capture.
    assert!(
        arena[body].captures.is_empty(),
        "expected no captures, got {:?}",
        capture_names(&arena, body)
    );
}

// =========================================================================
// 9. Capture from match arm pattern
// =========================================================================

#[test]
fn capture_from_match_pattern() {
    let arena = analyze_source("match 1 { x => (() => x) }");
    let closures = closure_bodies(&arena);
    assert_eq!(closures.len(), 1);
    let names = capture_names(&arena, closures[0]);
    assert_eq!(names, vec!["x"]);
}

// =========================================================================
// 10. Nested body boundary — inner captures y, outer captures x
// =========================================================================

#[test]
fn nested_body_boundary() {
    let arena = analyze_source("fn main() ( let x = 1; let f = () => ( let y = 2; (() => y) ) )");
    let closures = closure_bodies(&arena);
    // Two closures: outer f and inner.
    assert_eq!(closures.len(), 2);

    // Inner closure captures y (from outer closure's body).
    // Outer closure does NOT capture x — nothing in its tree references x.
    // (The inner closure only captures y, so no propagation of x occurs.)
    let mut found_inner = false;
    for &cid in &closures {
        let names = capture_names(&arena, cid);
        if names.contains(&"y".to_string()) {
            found_inner = true;
            assert!(
                !names.contains(&"x".to_string()),
                "inner should not capture x"
            );
        }
        assert!(
            !names.contains(&"x".to_string()),
            "neither closure should capture x, got {:?}",
            names
        );
    }
    assert!(found_inner, "expected inner closure capturing y");
}

// =========================================================================
// 11. Propagation through local function boundary
// =========================================================================

#[test]
fn propagation_through_local_fn() {
    let arena = analyze_source("fn main() ( let x = 1; let f = () => ( fn g() ( x ); g() ) )");
    // g captures x (direct).
    // f captures x (propagated from g).
    let g_body = fn_body(&arena, "g").unwrap();
    let g_names = capture_names(&arena, g_body);
    assert_eq!(g_names, vec!["x"], "g should capture x");

    let closures = closure_bodies(&arena);
    assert_eq!(closures.len(), 1);
    let f_names = capture_names(&arena, closures[0]);
    assert!(
        f_names.contains(&"x".to_string()),
        "f should capture x via propagation from g, got {:?}",
        f_names
    );
}
