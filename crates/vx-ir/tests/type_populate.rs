//! Tests for the VIR expression type population pass.

use vx_parser::parse_source_file;
use vx_resolve::{SourceId, TypeCheckResult, collect_types, resolve, type_check};
use vx_types::{Scalar, Ty};
use vx_ir::arena::VirArena;
use vx_ir::binding::BindingKind;
use vx_ir::build::VirBuilder;
use vx_ir::expr::ExprKind;
use vx_ir::item::ItemKind;
use vx_ir::type_populate::{body_result_ty, populate_binding_types, populate_expr_types};

/// Build VIR and populate expression + binding types from the existing type checker.
fn build_and_type(source: &str) -> VirArena {
    let parsed = parse_source_file(source);
    let file = parsed.tree();
    let resolution = resolve(&file);
    let types = collect_types(&file);
    let tc = type_check(&file, &resolution, &types);
    let mut program = VirBuilder::build_program(&[(SourceId(0), file)]);
    populate_expr_types(&mut program.arena, &tc);
    populate_binding_types(&mut program.arena, &tc);
    program.arena
}

/// Helper: get the tail expression of a named function's root block.
fn fn_tail_expr(arena: &VirArena, su_id: vx_ir::SourceUnitId, name: &str) -> vx_ir::ExprId {
    for &item_id in &arena[su_id].items {
        if let ItemKind::Function(f) = &arena[item_id].kind
            && f.name == name
        {
            let root = &arena[arena[f.body].root_expr];
            if let ExprKind::Block(b) = &root.kind {
                return b.tail.expect("function should have tail expression");
            }
            panic!("function root is not a block");
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
    let su_id = vx_ir::SourceUnitId::from_index(0);
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
    let su_id = vx_ir::SourceUnitId::from_index(0);
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
    let su_id = vx_ir::SourceUnitId::from_index(0);

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

// =========================================================================
// 7. Let binding gets init type
// =========================================================================

#[test]
fn let_binding_gets_init_type() {
    let arena = build_and_type("fn main() -> U64 ( let x = 1; x )");
    let binding = arena
        .bindings
        .iter()
        .find(|b| b.name == "x" && b.kind == BindingKind::Let)
        .expect("should find let binding x");
    assert_eq!(
        binding.ty,
        Some(Ty::Scalar(Scalar::U64)),
        "let binding x should get init expr type"
    );
}

// =========================================================================
// 8. Param binding stays None (param types come from FnSig, not binding_types)
// =========================================================================

#[test]
fn param_binding_stays_none() {
    // Parameter types are resolved via FnSig in the type checker, not via
    // binding_types. So populate_binding_types can't fill them in yet.
    let arena = build_and_type("fn add(x: U64) -> U64 ( x + 1 )");
    let binding = arena
        .bindings
        .iter()
        .find(|b| b.name == "x" && b.kind == BindingKind::Parameter)
        .expect("should find param binding x");
    assert_eq!(
        binding.ty, None,
        "param binding type is not yet derived (requires FnSig integration)"
    );
}

// =========================================================================
// 9. Destructuring let gets types from binding_types
// =========================================================================

#[test]
fn destructuring_let_gets_types() {
    // The type checker's type_pattern recursion populates binding_types for
    // tuple-destructured bindings too, so they get types via pass 1.
    let arena = build_and_type("fn main() ( let (a, b) = (1, 2) )");
    let a = arena
        .bindings
        .iter()
        .find(|b| b.name == "a" && b.kind == BindingKind::Let);
    let b = arena
        .bindings
        .iter()
        .find(|b| b.name == "b" && b.kind == BindingKind::Let);
    // Both should have types from the checker's type_pattern walk.
    assert!(
        a.map(|b| b.ty.is_some()).unwrap_or(false),
        "destructured binding a should have type from checker"
    );
    assert!(
        b.map(|b| b.ty.is_some()).unwrap_or(false),
        "destructured binding b should have type from checker"
    );
}

// =========================================================================
// 10. Let with untyped init stays None
// =========================================================================

#[test]
fn let_with_untyped_init_stays_none() {
    // If the init expr has no type (e.g. because the type checker didn't
    // produce one), the binding should also stay None. No panic.
    let parsed = parse_source_file("fn main() ( let x = 1; x )");
    let file = parsed.tree();
    let mut program = VirBuilder::build_program(&[(SourceId(0), file)]);

    // Populate bindings with an empty TypeCheckResult — no expr types, no binding types.
    let empty_tc = TypeCheckResult {
        expr_types: Default::default(),
        member_defs: Default::default(),
        closure_param_types: Default::default(),
        closure_param_docs: Default::default(),
        binding_types: Default::default(),
        errors: Default::default(),
    };
    populate_expr_types(&mut program.arena, &empty_tc);
    populate_binding_types(&mut program.arena, &empty_tc);

    let binding = program
        .arena
        .bindings
        .iter()
        .find(|b| b.name == "x")
        .expect("should find binding x");
    assert_eq!(
        binding.ty, None,
        "binding should stay None when init has no type"
    );
}

// =========================================================================
// 11. Rerun clears stale binding types
// =========================================================================

#[test]
fn rerun_clears_stale_binding_types() {
    let parsed = parse_source_file("fn main() -> U64 ( let x = 1; x )");
    let file = parsed.tree();
    let resolution = resolve(&file);
    let types = collect_types(&file);
    let tc = type_check(&file, &resolution, &types);

    let mut program = VirBuilder::build_program(&[(SourceId(0), file)]);

    // First run: types populated.
    populate_expr_types(&mut program.arena, &tc);
    populate_binding_types(&mut program.arena, &tc);
    let has_typed = program.arena.bindings.iter().any(|b| b.ty.is_some());
    assert!(has_typed, "first run should populate some binding types");

    // Second run with empty checker: all binding types cleared.
    let empty_tc = TypeCheckResult {
        expr_types: Default::default(),
        member_defs: Default::default(),
        closure_param_types: Default::default(),
        closure_param_docs: Default::default(),
        binding_types: Default::default(),
        errors: Default::default(),
    };
    populate_expr_types(&mut program.arena, &empty_tc);
    populate_binding_types(&mut program.arena, &empty_tc);

    for binding in &program.arena.bindings {
        assert_eq!(binding.ty, None, "rerun should clear all binding types");
    }
}

// =========================================================================
// 12. Body result type readable from root expr
// =========================================================================

#[test]
fn body_result_type_from_root_expr() {
    let arena = build_and_type("fn add(x: U64) -> U64 ( x + 1 )");
    let su_id = vx_ir::SourceUnitId::from_index(0);

    for &item_id in &arena[su_id].items {
        if let ItemKind::Function(f) = &arena[item_id].kind
            && f.name == "add"
        {
            let result_ty = body_result_ty(&arena, f.body);
            assert_eq!(
                result_ty,
                Some(&Ty::Scalar(Scalar::U64)),
                "body result type should be U64"
            );
            return;
        }
    }
    panic!("function 'add' not found");
}
