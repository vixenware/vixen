//! Tests for the VIR expression type population pass.

use vx_parser::parse_source_file;
use vx_resolve::{SourceId, TypeCheckResult, collect_types, resolve, type_check};
use vx_types::{Scalar, Ty};
use vx_vir::arena::VirArena;
use vx_vir::build::VirBuilder;
use vx_vir::expr::ExprKind;
use vx_vir::item::ItemKind;
use vx_vir::type_populate::populate_expr_types;

/// Build VIR and populate expression types from the existing type checker.
fn build_and_type(source: &str) -> VirArena {
    let parsed = parse_source_file(source);
    let file = parsed.tree();
    let resolution = resolve(&file);
    let types = collect_types(&file);
    let tc = type_check(&file, &resolution, &types);
    let mut program = VirBuilder::build_program(&[(SourceId(0), file)]);
    populate_expr_types(&mut program.arena, &tc);
    program.arena
}

/// Helper: get the tail expression of a named function's root block.
fn fn_tail_expr(arena: &VirArena, su_id: vx_vir::SourceUnitId, name: &str) -> vx_vir::ExprId {
    for &item_id in &arena[su_id].items {
        if let ItemKind::Function(f) = &arena[item_id].kind {
            if f.name == name {
                let root = &arena[arena[f.body].root_expr];
                if let ExprKind::Block(b) = &root.kind {
                    return b.tail.expect("function should have tail expression");
                }
                panic!("function root is not a block");
            }
        }
    }
    panic!("function '{}' not found", name);
}

// =========================================================================
// 1. Simple typed expression
// =========================================================================

#[test]
fn simple_typed_expression() {
    let arena = build_and_type("fn add(x: U64) -> U64 ( x + 1 )");
    let su_id = vx_vir::SourceUnitId::from_index(0);
    let tail_id = fn_tail_expr(&arena, su_id, "add");
    let tail = &arena[tail_id];

    assert_eq!(
        tail.ty,
        Some(Ty::Scalar(Scalar::U64)),
        "tail expression x + 1 should be typed as U64"
    );

    // The name ref 'x' in the binary should also be typed.
    if let ExprKind::Binary(bin) = &tail.kind {
        let lhs = &arena[bin.lhs];
        assert_eq!(
            lhs.ty,
            Some(Ty::Scalar(Scalar::U64)),
            "name ref x should be typed as U64"
        );
    }
}

// =========================================================================
// 2. Paren elimination — inner expr still gets typed
// =========================================================================

#[test]
fn paren_elimination_typed() {
    let arena = build_and_type("fn main() -> U64 ( (1 + 2) )");
    let su_id = vx_vir::SourceUnitId::from_index(0);
    let tail_id = fn_tail_expr(&arena, su_id, "main");

    // VIR eliminates parens — the tail is the inner binary expr directly.
    assert!(
        matches!(arena[tail_id].kind, ExprKind::Binary(_)),
        "tail should be Binary after paren elimination, got {:?}",
        arena[tail_id].kind
    );
    assert_eq!(
        arena[tail_id].ty,
        Some(Ty::Scalar(Scalar::U64)),
        "inner expr should still be typed after paren elimination"
    );
}

// =========================================================================
// 3. Synthetic block stays None
// =========================================================================

#[test]
fn synthetic_block_stays_none() {
    let arena = build_and_type("fn main() -> U64 ( 1 )");
    let su_id = vx_vir::SourceUnitId::from_index(0);

    // The top-level body's root expr is the synthetic block.
    let body = &arena[arena[su_id].top_level_body];
    let root = &arena[body.root_expr];
    if let ExprKind::Block(b) = &root.kind {
        assert!(b.synthetic, "top-level block should be synthetic");
    }
    assert_eq!(root.ty, None, "synthetic block should have ty = None");
}

// =========================================================================
// 4. Error node stays None
// =========================================================================

#[test]
fn error_node_stays_none() {
    // Broken source — missing RHS produces an ExprKind::Error node.
    let arena = build_and_type("fn main() ( 1 + )");

    // Find any ExprKind::Error nodes and verify they have no type.
    let error_count = arena
        .exprs
        .iter()
        .filter(|e| matches!(e.kind, ExprKind::Error))
        .count();
    assert!(error_count > 0, "broken source should produce error exprs");

    for expr in &arena.exprs {
        if matches!(expr.kind, ExprKind::Error) {
            assert_eq!(expr.ty, None, "error node should have ty = None");
        }
    }
}

// =========================================================================
// 5. Rerun clears stale types
// =========================================================================

#[test]
fn rerun_clears_stale_types() {
    let parsed = parse_source_file("fn add(x: U64) -> U64 ( x + 1 )");
    let file = parsed.tree();
    let resolution = resolve(&file);
    let types = collect_types(&file);
    let tc = type_check(&file, &resolution, &types);

    let mut program = VirBuilder::build_program(&[(SourceId(0), file)]);

    // First run: types are populated.
    populate_expr_types(&mut program.arena, &tc);
    let has_some = program.arena.exprs.iter().any(|e| e.ty.is_some());
    assert!(has_some, "first run should populate some types");

    // Second run with empty TypeCheckResult: all types should be cleared.
    let empty_tc = TypeCheckResult {
        expr_types: Default::default(),
        member_defs: Default::default(),
        closure_param_types: Default::default(),
        closure_param_docs: Default::default(),
        binding_types: Default::default(),
        errors: Default::default(),
    };
    populate_expr_types(&mut program.arena, &empty_tc);

    for expr in &program.arena.exprs {
        assert_eq!(
            expr.ty, None,
            "rerun with empty checker should clear all types"
        );
    }
}

// =========================================================================
// 6. Multi-source isolation
// =========================================================================

#[test]
fn multi_source_isolation() {
    let source_a = "fn a() -> U64 ( 1 )";
    let source_b = "fn b() -> U64 ( 2 )";

    let parsed_a = parse_source_file(source_a);
    let parsed_b = parse_source_file(source_b);
    let file_a = parsed_a.tree();
    let file_b = parsed_b.tree();

    // Only type-check source A.
    let resolution = resolve(&file_a);
    let types = collect_types(&file_a);
    let tc = type_check(&file_a, &resolution, &types);

    // Build VIR for both sources.
    let mut program = VirBuilder::build_program(&[(SourceId(0), file_a), (SourceId(1), file_b)]);
    populate_expr_types(&mut program.arena, &tc);

    // Source A exprs should have types; source B exprs should not.
    let source_b_typed = program
        .arena
        .exprs
        .iter()
        .filter(|e| e.span.source == SourceId(1))
        .any(|e| e.ty.is_some());
    assert!(
        !source_b_typed,
        "source B exprs should not get types from source A's checker"
    );
}
