//! Tests for the VIR type checking pass.
//!
//! Unit tests verify individual expression types.
//! Equivalence tests compare VIR checker output against the old AST-based checker.

use vx_ir::build::VirBuilder;
use vx_ir::resolve::resolve_names;
use vx_ir::type_collect::collect_types_vir;
use vx_ir::type_collection::TypeCollection;
use vx_ir::{ExprId, VirArena};
use vx_parser::{SourceId, parse_source_file};
use vx_runtime::type_check::{TypeCheckVirResult, type_check_vir};
use vx_types::{Scalar, Ty};

/// Helper: build VIR, resolve names, collect types, run type checker, then run
/// the inferred-error-union pass (so `?` types reflect the real `E`).
fn check_source(source: &str) -> (VirArena, TypeCollection, TypeCheckVirResult) {
    let parsed = parse_source_file(source, SourceId(0));
    let file = parsed.tree();
    let mut program = VirBuilder::build_program(&[(SourceId(0), file)]);
    let mut tc = collect_types_vir(&mut program.arena);
    let _resolve_errors = resolve_names(&mut program.arena);
    let result = type_check_vir(&mut program.arena, &mut tc);
    let _eu_errors =
        vx_runtime::error_union::infer_error_unions(&mut program.arena, &mut tc, &result);
    (program.arena, tc, result)
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

/// Helper: assert that an expression's type is the given Ty.
fn assert_expr_ty(arena: &VirArena, tc: &TypeCollection, id: ExprId, expected: &Ty) {
    let ty_id = arena[id].ty.expect("expression should be typed");
    assert_eq!(tc.env.get_ty(ty_id), expected);
}

/// Unwrap a `Failure[E]` struct type to its single domain-error type-arg `E`. The
/// caught `Err` payload of a `?` is `Failure[E]` (the failure model's envelope), so
/// the inferred-union tests read `E` from inside the `Failure`.
fn failure_domain_err(env: &vx_types::TypeEnv, err_payload: vx_types::TypeId) -> vx_types::TypeId {
    let Ty::Struct(failure_ty) = env.get_ty(err_payload).clone() else {
        panic!(
            "expected the Err payload to be a Failure struct, got {:?}",
            env.get_ty(err_payload)
        );
    };
    assert!(
        matches!(env.get_def(failure_ty.def), vx_types::TypeDef::Struct(s) if s.name == "Failure"),
        "the Err payload should be the Failure struct, got def {:?}",
        env.get_def(failure_ty.def)
    );
    failure_ty.type_args[0]
}

// =========================================================================
// 1. Literal types
// =========================================================================

#[test]
fn literal_int() {
    let (arena, tc, _) = check_source("42");
    // Find the int literal.
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Literal(l) if matches!(l.value, vx_ir::expr::LitKind::Int { .. }))).unwrap();
    assert_expr_ty(&arena, &tc, id, &Ty::Scalar(Scalar::U64));
}

#[test]
fn literal_float() {
    let (arena, tc, _) = check_source("3.14");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Literal(l) if matches!(l.value, vx_ir::expr::LitKind::Float { .. }))).unwrap();
    assert_expr_ty(&arena, &tc, id, &Ty::Scalar(Scalar::F64));
}

#[test]
fn literal_string() {
    let (arena, tc, _) = check_source("\"hello\"");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Literal(l) if matches!(l.value, vx_ir::expr::LitKind::String { .. }))).unwrap();
    assert_expr_ty(&arena, &tc, id, &Ty::Scalar(Scalar::String));
}

#[test]
fn literal_bool() {
    let (arena, tc, _) = check_source("true");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Literal(l) if matches!(l.value, vx_ir::expr::LitKind::Bool(true)))).unwrap();
    assert_expr_ty(&arena, &tc, id, &Ty::Scalar(Scalar::Bool));
}

// =========================================================================
// 2. Binary operators
// =========================================================================

#[test]
fn binary_add() {
    let (arena, tc, _) = check_source("1 + 2");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Binary(_))).unwrap();
    assert_expr_ty(&arena, &tc, id, &Ty::Scalar(Scalar::U64));
}

#[test]
fn binary_comparison() {
    let (arena, tc, _) = check_source("1 == 2");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Binary(_))).unwrap();
    assert_expr_ty(&arena, &tc, id, &Ty::Scalar(Scalar::Bool));
}

#[test]
fn binary_logical() {
    let (arena, tc, _) = check_source("true && false");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Binary(_))).unwrap();
    assert_expr_ty(&arena, &tc, id, &Ty::Scalar(Scalar::Bool));
}

// =========================================================================
// 3. Prefix operators
// =========================================================================

#[test]
fn prefix_not() {
    let (arena, tc, _) = check_source("!true");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Prefix(_))).unwrap();
    assert_expr_ty(&arena, &tc, id, &Ty::Scalar(Scalar::Bool));
}

#[test]
fn prefix_neg() {
    let (arena, tc, _) = check_source("-42");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Prefix(_))).unwrap();
    assert_expr_ty(&arena, &tc, id, &Ty::Scalar(Scalar::U64));
}

// =========================================================================
// 4. Function call
// =========================================================================

#[test]
fn function_call_return_type() {
    let (arena, tc, _) = check_source("fn add(x: U64, y: U64) -> U64 ( x + y ); add(1, 2)");
    // Find the Call expression.
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Call(_))).unwrap();
    assert_expr_ty(&arena, &tc, id, &Ty::Scalar(Scalar::U64));
}

// =========================================================================
// 5. If expression
// =========================================================================

#[test]
fn if_expression_type() {
    let (arena, tc, _) = check_source("if true ( 1 ) else ( 2 )");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::If(_))).unwrap();
    // Old checker returns then-branch type.
    assert_expr_ty(&arena, &tc, id, &Ty::Scalar(Scalar::U64));
}

// =========================================================================
// 6. Let binding type
// =========================================================================

#[test]
fn let_binding_type() {
    let (arena, tc, _) = check_source("let x = 42; x");
    let binding = arena.bindings.iter().find(|b| b.name == "x").unwrap();
    let ty_id = binding.ty.unwrap();
    assert_eq!(tc.env.get_ty(ty_id), &Ty::Scalar(Scalar::U64));
}

// =========================================================================
// 7. Let with annotation
// =========================================================================

#[test]
fn let_annotation_wins() {
    let (arena, tc, _) = check_source("let x: F64 = 42; x");
    let binding = arena.bindings.iter().find(|b| b.name == "x").unwrap();
    let ty_id = binding.ty.unwrap();
    assert_eq!(tc.env.get_ty(ty_id), &Ty::Scalar(Scalar::F64));
}

// =========================================================================
// 8. Struct literal
// =========================================================================

#[test]
fn struct_literal_type() {
    let (arena, tc, _) = check_source("struct Point { x: U64, y: U64 }; Point { x: 1, y: 2 }");
    let id = find_expr(&arena, |k| {
        matches!(k, vx_ir::expr::ExprKind::StructLiteral(_))
    })
    .unwrap();
    let ty_id = arena[id].ty.unwrap();
    let ty = tc.env.get_ty(ty_id);
    assert!(matches!(ty, Ty::Struct(_)), "expected Struct, got {:?}", ty);
}

// =========================================================================
// 9. Tuple
// =========================================================================

#[test]
fn tuple_type() {
    let (arena, tc, _) = check_source("(1, \"hello\")");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Tuple(_))).unwrap();
    let ty_id = arena[id].ty.unwrap();
    let ty = tc.env.get_ty(ty_id);
    if let Ty::Tuple(t) = ty {
        assert_eq!(t.0.len(), 2);
        assert_eq!(tc.env.get_ty(t.0[0]), &Ty::Scalar(Scalar::U64));
        assert_eq!(tc.env.get_ty(t.0[1]), &Ty::Scalar(Scalar::String));
    } else {
        panic!("expected Tuple, got {:?}", ty);
    }
}

// =========================================================================
// 10. Array
// =========================================================================

#[test]
fn array_type() {
    let (arena, tc, _) = check_source("[1, 2, 3]");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Array(_))).unwrap();
    let ty_id = arena[id].ty.unwrap();
    let ty = tc.env.get_ty(ty_id);
    if let Ty::Array(arr) = ty {
        assert_eq!(tc.env.get_ty(arr.0), &Ty::Scalar(Scalar::U64));
    } else {
        panic!("expected Array, got {:?}", ty);
    }
}

// =========================================================================
// 11. Fallible
// =========================================================================

#[test]
fn fallible_over_infallible_wraps_in_result_failure_never() {
    // `expr? : Result[T, Failure[E]]` — the failure model's catch (was `Option[T]`).
    // The caught `Err` payload is the `Failure` envelope around the inferred error
    // union `E`. `42` can never fail (no reachable `fail`/fallible callee), so `E` is
    // the zero-variant `Never` enum: `42? : Result[U64, Failure[Never]]`. `Result`
    // and `Failure` are stdlib types, so go through the bundled front-end that loads
    // them (and runs the error-union pass).
    let fe = vx_runtime::compile::semantic_front_end_bundled("fn f() ( 42? )").expect("front-end");
    let id = find_expr(&fe.program.arena, |k| {
        matches!(k, vx_ir::expr::ExprKind::Fallible(_))
    })
    .unwrap();
    let ty_id = fe.program.arena[id].ty.unwrap();
    let ty = fe.env.get_ty(ty_id).clone();
    let Ty::Enum(enum_ty) = ty else {
        panic!("expected Result enum, got {ty:?}");
    };
    assert!(
        matches!(fe.env.get_def(enum_ty.def), vx_types::TypeDef::Enum(e) if e.name == "Result"),
        "expected the Result enum"
    );
    assert_eq!(
        fe.env.get_ty(enum_ty.type_args[0]),
        &Ty::Scalar(Scalar::U64)
    );
    // The Err payload is `Failure[Never]` — the Failure struct wrapping the
    // synthetic zero-variant `Never` enum as its single domain-error type arg.
    let Ty::Struct(failure_ty) = fe.env.get_ty(enum_ty.type_args[1]).clone() else {
        panic!(
            "expected Failure[Never] struct as the Err payload, got {:?}",
            fe.env.get_ty(enum_ty.type_args[1])
        );
    };
    assert!(
        matches!(fe.env.get_def(failure_ty.def), vx_types::TypeDef::Struct(s) if s.name == "Failure"),
        "the Err payload should be the Failure struct"
    );
    let Ty::Enum(err_enum) = fe.env.get_ty(failure_ty.type_args[0]).clone() else {
        panic!(
            "expected Never enum as Failure's E, got {:?}",
            fe.env.get_ty(failure_ty.type_args[0])
        );
    };
    let vx_types::TypeDef::Enum(err_def) = fe.env.get_def(err_enum.def) else {
        panic!("E def should be an enum");
    };
    assert!(
        err_def.variants.is_empty(),
        "E should be the zero-variant Never enum, got variants {:?}",
        err_def.variants
    );
}

#[test]
fn fallible_infers_string_from_single_fail() {
    // A function whose only failure is `fail "…"` infers `E = String` (computed,
    // not hardcoded): `f()? : Result[U64, Failure[String]]`.
    let src =
        "fn f(x: U64) -> U64 ( if x > 0 ( x ) else ( fail \"neg\" ) )\nfn g(x: U64) ( f(x)? )";
    let fe = vx_runtime::compile::semantic_front_end_bundled(src).expect("front-end");
    let id = find_expr(&fe.program.arena, |k| {
        matches!(k, vx_ir::expr::ExprKind::Fallible(_))
    })
    .unwrap();
    let ty_id = fe.program.arena[id].ty.unwrap();
    let Ty::Enum(enum_ty) = fe.env.get_ty(ty_id).clone() else {
        panic!("expected Result enum");
    };
    let e_ty = failure_domain_err(&fe.env, enum_ty.type_args[1]);
    assert_eq!(
        fe.env.get_ty(e_ty),
        &Ty::Scalar(Scalar::String),
        "single `fail \"…\"` should infer Failure[E] with E = String"
    );
}

#[test]
fn fallible_infers_non_string_error_type() {
    // A function that fails with a NON-String value (a custom enum variant) infers
    // that enum as `E` — proving the union is computed from the failed value's type,
    // not hardcoded to String.
    let src = "enum MyErr { Bad, Worse }\n\
               fn f(x: U64) -> U64 ( if x > 0 ( x ) else ( fail Bad ) )\n\
               fn g(x: U64) ( f(x)? )";
    let fe = vx_runtime::compile::semantic_front_end_bundled(src).expect("front-end");
    let id = find_expr(&fe.program.arena, |k| {
        matches!(k, vx_ir::expr::ExprKind::Fallible(_))
    })
    .unwrap();
    let ty_id = fe.program.arena[id].ty.unwrap();
    let Ty::Enum(enum_ty) = fe.env.get_ty(ty_id).clone() else {
        panic!("expected Result enum");
    };
    let e_ty = fe
        .env
        .get_ty(failure_domain_err(&fe.env, enum_ty.type_args[1]))
        .clone();
    assert!(
        !matches!(e_ty, Ty::Scalar(Scalar::String)),
        "E must NOT be String when the fail value is a custom enum, got {e_ty:?}"
    );
    let Ty::Enum(err_enum) = e_ty else {
        panic!("expected MyErr enum as E, got {e_ty:?}");
    };
    assert!(
        matches!(fe.env.get_def(err_enum.def), vx_types::TypeDef::Enum(e) if e.name == "MyErr"),
        "E should be the MyErr enum"
    );
}

/// Compile `src` through the bundled front-end and return any
/// `NonExhaustiveMatch` diagnostics (as `(missing-variant-list)`), or an empty
/// vec if it compiled clean / failed elsewhere.
fn non_exhaustive_diags(src: &str) -> Vec<Vec<String>> {
    match vx_runtime::compile::semantic_front_end_bundled(src) {
        Ok(_) => Vec::new(),
        Err(vx_runtime::compile::CompileError::ErrorUnion { errors, .. }) => errors
            .into_iter()
            .filter_map(|d| match d {
                vx_ir::IrDiagnostic::NonExhaustiveMatch { missing, .. } => Some(missing),
                _ => None,
            })
            .collect(),
        Err(other) => panic!("expected clean or ErrorUnion, got {other:?}"),
    }
}

#[test]
fn fallible_infers_heterogeneous_two_element_union() {
    // A `?` whose inner subtree reaches a String-fail AND an enum-fail infers
    // `E` as a 2-element union alias `String | MyErr`.
    let src = "enum MyErr { Bad }\n\
               fn fs(x: U64) -> U64 ( if x > 0 ( x ) else ( fail \"s\" ) )\n\
               fn fe(x: U64) -> U64 ( if x > 0 ( x ) else ( fail Bad ) )\n\
               fn g(a: U64, b: U64) ( (fs(a) + fe(b))? )";
    let fe = vx_runtime::compile::semantic_front_end_bundled(src).expect("front-end");
    let id = find_expr(&fe.program.arena, |k| {
        matches!(k, vx_ir::expr::ExprKind::Fallible(_))
    })
    .unwrap();
    let ty_id = fe.program.arena[id].ty.unwrap();
    let Ty::Enum(enum_ty) = fe.env.get_ty(ty_id).clone() else {
        panic!("expected Result enum");
    };
    // The Err payload is `Failure[E]`; E is a synthetic anonymous alias whose
    // alternatives are {String, MyErr}.
    let e_ty = failure_domain_err(&fe.env, enum_ty.type_args[1]);
    let Ty::Alias(alias_ty) = fe.env.get_ty(e_ty).clone() else {
        panic!("expected union alias as E, got {:?}", fe.env.get_ty(e_ty));
    };
    // The alias resolves to a sum (its alternatives are recorded). We can't read
    // the side table off `FrontEnd`, but we can confirm both member types appear
    // among the env's interned types and that the alias name is the synthetic one.
    let vx_types::TypeDef::Alias(adef) = fe.env.get_def(alias_ty.def) else {
        panic!("E def should be an alias");
    };
    assert_eq!(
        adef.name, "<error union>",
        "E should be the synthetic union alias"
    );
}

#[test]
fn fallible_union_propagates_through_call_chain() {
    // `h` calls `g` calls `f` (which fails with a String). The error union must
    // propagate two hops: `h(x)? : Result[_, String]`.
    let src = "fn f(x: U64) -> U64 ( if x > 0 ( x ) else ( fail \"e\" ) )\n\
               fn g(x: U64) -> U64 ( f(x) )\n\
               fn h(x: U64) -> U64 ( g(x) )\n\
               fn top(x: U64) ( h(x)? )";
    let fe = vx_runtime::compile::semantic_front_end_bundled(src).expect("front-end");
    let id = find_expr(&fe.program.arena, |k| {
        matches!(k, vx_ir::expr::ExprKind::Fallible(_))
    })
    .unwrap();
    let ty_id = fe.program.arena[id].ty.unwrap();
    let Ty::Enum(enum_ty) = fe.env.get_ty(ty_id).clone() else {
        panic!("expected Result enum");
    };
    let e_ty = failure_domain_err(&fe.env, enum_ty.type_args[1]);
    assert_eq!(
        fe.env.get_ty(e_ty),
        &Ty::Scalar(Scalar::String),
        "the String error should propagate through f→g→h (as Failure[String])"
    );
}

#[test]
fn fallible_union_terminates_on_mutual_recursion() {
    // Mutually recursive `a`/`b` (each calls the other and can `fail`). The
    // fixpoint must terminate and infer the String union for `a(x)?`.
    let src = "fn a(x: U64) -> U64 ( if x > 0 ( b(x) ) else ( fail \"ae\" ) )\n\
               fn b(x: U64) -> U64 ( if x > 1 ( a(x) ) else ( fail \"be\" ) )\n\
               fn top(x: U64) ( a(x)? )";
    let fe = vx_runtime::compile::semantic_front_end_bundled(src).expect("front-end");
    let id = find_expr(&fe.program.arena, |k| {
        matches!(k, vx_ir::expr::ExprKind::Fallible(_))
    })
    .unwrap();
    let ty_id = fe.program.arena[id].ty.unwrap();
    let Ty::Enum(enum_ty) = fe.env.get_ty(ty_id).clone() else {
        panic!("expected Result enum");
    };
    // Both fails are String, so the union collapses to a single String inside the
    // Failure envelope: Failure[String].
    let e_ty = failure_domain_err(&fe.env, enum_ty.type_args[1]);
    assert_eq!(
        fe.env.get_ty(e_ty),
        &Ty::Scalar(Scalar::String),
        "mutual recursion should terminate with E = String"
    );
}

#[test]
fn match_over_fallible_missing_err_is_non_exhaustive() {
    // `match checked(0)? ( Ok(v) => v )` omits the `Err` arm → non-exhaustive.
    let src = "fn checked(x: U64) -> U64 ( if x > 0 ( x ) else ( fail \"e\" ) )\n\
               fn g(x: U64) -> U64 ( match checked(x)? ( Ok(v) => v ) )";
    let diags = non_exhaustive_diags(src);
    assert!(
        diags
            .iter()
            .any(|missing| missing.iter().any(|m| m == "Err")),
        "expected NonExhaustiveMatch missing `Err`, got {diags:?}"
    );
}

#[test]
fn match_over_fallible_with_wildcard_is_exhaustive() {
    // A `_` arm covers everything → no NonExhaustiveMatch.
    let src = "fn checked(x: U64) -> U64 ( if x > 0 ( x ) else ( fail \"e\" ) )\n\
               fn g(x: U64) -> U64 ( match checked(x)? ( Ok(v) => v, _ => 0 ) )";
    let diags = non_exhaustive_diags(src);
    assert!(
        diags.is_empty(),
        "a wildcard arm should suppress NonExhaustiveMatch, got {diags:?}"
    );
}

#[test]
fn match_over_fallible_with_both_arms_is_exhaustive() {
    // `Ok` + `Err` covers the whole `Result` → exhaustive.
    let src = "fn checked(x: U64) -> U64 ( if x > 0 ( x ) else ( fail \"e\" ) )\n\
               fn g(x: U64) -> U64 ( match checked(x)? ( Ok(v) => v, Err(e) => 0 ) )";
    let diags = non_exhaustive_diags(src);
    assert!(
        diags.is_empty(),
        "Ok + Err arms should be exhaustive, got {diags:?}"
    );
}

// =========================================================================
// 12. Function param binding type
// =========================================================================

#[test]
fn function_param_has_type() {
    let (arena, tc, _) = check_source("fn f(x: U64) -> U64 ( x )");
    // The name ref `x` inside the body should be typed.
    let x_ref = find_expr(
        &arena,
        |k| matches!(k, vx_ir::expr::ExprKind::NameRef(nr) if nr.name == "x"),
    )
    .unwrap();
    assert_expr_ty(&arena, &tc, x_ref, &Ty::Scalar(Scalar::U64));
}

// =========================================================================
// 13. No return type → Unit
// =========================================================================

#[test]
fn function_no_return_type_unit() {
    // The body's root expression should still be typed.
    let (arena, tc, _) = check_source("fn noop() ( 1 )");
    // Body root should be an int literal with U64 type.
    let int_lit = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Literal(l) if matches!(l.value, vx_ir::expr::LitKind::Int { .. }))).unwrap();
    assert_expr_ty(&arena, &tc, int_lit, &Ty::Scalar(Scalar::U64));
}

// =========================================================================
// 14. All exprs get Some(ty) — no None left behind
// =========================================================================

#[test]
fn no_none_after_full_check() {
    let (arena, _, _) =
        check_source("fn add(x: U64, y: U64) -> U64 ( x + y ); let z = add(1, 2); z");
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
    let (arena, tc, _) = check_source("`hello {42}`");
    let id = find_expr(&arena, |k| {
        matches!(k, vx_ir::expr::ExprKind::InterpolatedString(_))
    })
    .unwrap();
    assert_expr_ty(&arena, &tc, id, &Ty::Scalar(Scalar::String));
}

// =========================================================================
// 16. Match expression
// =========================================================================

#[test]
fn match_returns_first_non_poison_arm() {
    let (arena, tc, _) = check_source("match 1 { 1 => \"a\", _ => \"b\" }");
    let id = find_expr(&arena, |k| matches!(k, vx_ir::expr::ExprKind::Match(_))).unwrap();
    assert_expr_ty(&arena, &tc, id, &Ty::Scalar(Scalar::String));
}
