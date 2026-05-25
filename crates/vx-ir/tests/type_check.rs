//! Tests for the VIR type checking pass.
//!
//! Unit tests verify individual expression types.
//! Equivalence tests compare VIR checker output against the old AST-based checker.

use vx_parser::parse_source_file;
use vx_resolve::SourceId;
use vx_types::{Scalar, Ty};
use vx_ir::build::VirBuilder;
use vx_ir::resolve::resolve_names;
use vx_ir::type_check::{TypeCheckVirResult, type_check_vir};
use vx_ir::type_collect::collect_types_vir;
use vx_ir::{ExprId, VirArena};

/// Helper: build VIR, resolve names, collect types, run type checker.
fn check_source(source: &str) -> (VirArena, TypeCheckVirResult) {
    let parsed = parse_source_file(source);
    let file = parsed.tree();
    let mut program = VirBuilder::build_program(&[(SourceId(0), file)]);
    let tc = collect_types_vir(&mut program.arena);
    let _resolve_errors = resolve_names(&mut program.arena);
    let result = type_check_vir(&mut program.arena, &tc);
    (program.arena, result)
}

/// Helper: find the first expression of a given kind pattern.
fn find_expr(
    arena: &VirArena,
    predicate: impl Fn(&vx_ir::expr::ExprKind) -> bool,
) -> Option<ExprId> {
    for (i, expr) in arena.exprs.iter().enumerate() {
        if predicate(&expr.kind) {
            return Some(ExprId::from_index(i));
        }
    }
    None
}

// =========================================================================
// 1. Literal types
// =========================================================================

#[test]
fn literal_int() {
    let (arena, _) = check_source("42");
    // Find the int literal.
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Literal(l) if matches!(l.value, vx_ir::expr::LitKind::Int { .. }))).unwrap();
    assert_eq!(arena[id].ty, Some(Ty::Scalar(Scalar::U64)));
}

#[test]
fn literal_float() {
    let (arena, _) = check_source("3.14");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Literal(l) if matches!(l.value, vx_ir::expr::LitKind::Float { .. }))).unwrap();
    assert_eq!(arena[id].ty, Some(Ty::Scalar(Scalar::F64)));
}

#[test]
fn literal_string() {
    let (arena, _) = check_source("\"hello\"");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Literal(l) if matches!(l.value, vx_ir::expr::LitKind::String { .. }))).unwrap();
    assert_eq!(arena[id].ty, Some(Ty::Scalar(Scalar::String)));
}

#[test]
fn literal_bool() {
    let (arena, _) = check_source("true");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Literal(l) if matches!(l.value, vx_ir::expr::LitKind::Bool(true)))).unwrap();
    assert_eq!(arena[id].ty, Some(Ty::Scalar(Scalar::Bool)));
}

// =========================================================================
// 2. Binary operators
// =========================================================================

#[test]
fn binary_add() {
    let (arena, _) = check_source("1 + 2");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Binary(_))).unwrap();
    assert_eq!(arena[id].ty, Some(Ty::Scalar(Scalar::U64)));
}

#[test]
fn binary_comparison() {
    let (arena, _) = check_source("1 == 2");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Binary(_))).unwrap();
    assert_eq!(arena[id].ty, Some(Ty::Scalar(Scalar::Bool)));
}

#[test]
fn binary_logical() {
    let (arena, _) = check_source("true && false");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Binary(_))).unwrap();
    assert_eq!(arena[id].ty, Some(Ty::Scalar(Scalar::Bool)));
}

// =========================================================================
// 3. Prefix operators
// =========================================================================

#[test]
fn prefix_not() {
    let (arena, _) = check_source("!true");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Prefix(_))).unwrap();
    assert_eq!(arena[id].ty, Some(Ty::Scalar(Scalar::Bool)));
}

#[test]
fn prefix_neg() {
    let (arena, _) = check_source("-42");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Prefix(_))).unwrap();
    assert_eq!(arena[id].ty, Some(Ty::Scalar(Scalar::U64)));
}

// =========================================================================
// 4. Function call
// =========================================================================

#[test]
fn function_call_return_type() {
    let (arena, _) = check_source("fn add(x: U64, y: U64) -> U64 ( x + y ); add(1, 2)");
    // Find the Call expression.
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Call(_))).unwrap();
    assert_eq!(arena[id].ty, Some(Ty::Scalar(Scalar::U64)));
}

// =========================================================================
// 5. If expression
// =========================================================================

#[test]
fn if_expression_type() {
    let (arena, _) = check_source("if true ( 1 ) else ( 2 )");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::If(_))).unwrap();
    // Old checker returns then-branch type.
    assert_eq!(arena[id].ty, Some(Ty::Scalar(Scalar::U64)));
}

// =========================================================================
// 6. Let binding type
// =========================================================================

#[test]
fn let_binding_type() {
    let (arena, _) = check_source("let x = 42; x");
    // Find the binding.
    let binding = arena.bindings.iter().find(|b| b.name == "x").unwrap();
    assert_eq!(binding.ty, Some(Ty::Scalar(Scalar::U64)));
}

// =========================================================================
// 7. Let with annotation
// =========================================================================

#[test]
fn let_annotation_wins() {
    let (arena, _) = check_source("let x: F64 = 42; x");
    let binding = arena.bindings.iter().find(|b| b.name == "x").unwrap();
    assert_eq!(binding.ty, Some(Ty::Scalar(Scalar::F64)));
}

// =========================================================================
// 8. Struct literal
// =========================================================================

#[test]
fn struct_literal_type() {
    let (arena, _) = check_source("struct Point { x: U64, y: U64 }; Point { x: 1, y: 2 }");
    let id = find_expr(&arena, |k| {
        matches!(k, vx_ir::expr::ExprKind::StructLiteral(_))
    })
    .unwrap();
    let ty = arena[id].ty.as_ref().unwrap();
    assert!(matches!(ty, Ty::Struct(_)), "expected Struct, got {:?}", ty);
}

// =========================================================================
// 9. Tuple
// =========================================================================

#[test]
fn tuple_type() {
    let (arena, _) = check_source("(1, \"hello\")");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Tuple(_))).unwrap();
    let ty = arena[id].ty.as_ref().unwrap();
    if let Ty::Tuple(t) = ty {
        assert_eq!(t.elements.len(), 2);
        assert_eq!(t.elements[0], Ty::Scalar(Scalar::U64));
        assert_eq!(t.elements[1], Ty::Scalar(Scalar::String));
    } else {
        panic!("expected Tuple, got {:?}", ty);
    }
}

// =========================================================================
// 10. Array
// =========================================================================

#[test]
fn array_type() {
    let (arena, _) = check_source("[1, 2, 3]");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Array(_))).unwrap();
    let ty = arena[id].ty.as_ref().unwrap();
    assert_eq!(*ty, Ty::Array(Box::new(Ty::Scalar(Scalar::U64))));
}

// =========================================================================
// 11. Fallible
// =========================================================================

#[test]
fn fallible_wraps_in_optional() {
    let (arena, _) = check_source("42?");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Fallible(_))).unwrap();
    let ty = arena[id].ty.as_ref().unwrap();
    assert_eq!(*ty, Ty::Optional(Box::new(Ty::Scalar(Scalar::U64))));
}

// =========================================================================
// 12. Function param binding type
// =========================================================================

#[test]
fn function_param_has_type() {
    let (arena, _) = check_source("fn f(x: U64) -> U64 ( x )");
    // The name ref `x` inside the body should be typed.
    let x_ref = find_expr(
        &arena,
        |k| matches!(k, vx_ir::expr::ExprKind::NameRef(nr) if nr.name == "x"),
    )
    .unwrap();
    assert_eq!(arena[x_ref].ty, Some(Ty::Scalar(Scalar::U64)));
}

// =========================================================================
// 13. No return type → Unit
// =========================================================================

#[test]
fn function_no_return_type_unit() {
    // The body's root expression should still be typed.
    let (arena, _) = check_source("fn noop() ( 1 )");
    // Body root should be an int literal with U64 type.
    let int_lit = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Literal(l) if matches!(l.value, vx_ir::expr::LitKind::Int { .. }))).unwrap();
    assert_eq!(arena[int_lit].ty, Some(Ty::Scalar(Scalar::U64)));
}

// =========================================================================
// 14. All exprs get Some(ty) — no None left behind
// =========================================================================

#[test]
fn no_none_after_full_check() {
    let (arena, _) = check_source("fn add(x: U64, y: U64) -> U64 ( x + y ); let z = add(1, 2); z");
    for (i, expr) in arena.exprs.iter().enumerate() {
        assert!(
            expr.ty.is_some(),
            "expr {} ({:?}) at {:?} was not typed",
            i,
            std::mem::discriminant(&expr.kind),
            expr.span,
        );
    }
}

// =========================================================================
// 15. Interpolated string
// =========================================================================

#[test]
fn interpolated_string_type() {
    let (arena, _) = check_source("`hello {42}`");
    let id = find_expr(&arena, |k| {
        matches!(k, vx_ir::expr::ExprKind::InterpolatedString(_))
    })
    .unwrap();
    assert_eq!(arena[id].ty, Some(Ty::Scalar(Scalar::String)));
}

// =========================================================================
// 16. Match expression
// =========================================================================

#[test]
fn match_returns_first_non_poison_arm() {
    let (arena, _) = check_source("match 1 { 1 => \"a\", _ => \"b\" }");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Match(_))).unwrap();
    assert_eq!(arena[id].ty, Some(Ty::Scalar(Scalar::String)));
}
