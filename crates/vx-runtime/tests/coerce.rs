//! `#[coerce]` end-to-end: a plain `String` literal passed where a `Path` is
//! expected (a checking-mode position) is implicitly coerced by the type-checker
//! — it inserts an `ExprKind::Coerce` node around the literal — and the evaluator
//! runs the coercion, which for `String -> Path` is identity (paths are
//! `Value::Str` at runtime).
//!
//! `Path` and its `#[coerce] fn Path(s: String) -> Self` constructor live in
//! `std/core.vx`; `compile_source` pulls the stdlib in, so the coercion table is
//! built from the real declaration. A successful compile is itself the proof that
//! type-checking inserted the coercion: without it, `f("out.txt")` would raise
//! `ArgTypeMismatch` (String vs Path) and the front-end would error out.

use vx_ir::compile::compile_source;
use vx_ir::expr::ExprKind;
use vx_ir::item::ItemKind;
use vx_runtime::eval::{Outcome, Value, run};

/// A plain string literal in a `Path` argument position type-checks (the coercion
/// is inserted, no `ArgTypeMismatch`) and evaluates as identity (the path is the
/// same string at runtime).
#[test]
fn string_literal_coerces_to_path_end_to_end() {
    // `f` wants a `Path`; `main` passes it a bare `String` literal. The argument is
    // a checking-mode position (the parameter type is known), so the checker
    // coerces `"out.txt"` through `Path`'s `#[coerce]` constructor.
    let src = "fn f(p: Path) -> Path ( p )\n\
               fn main() -> Path ( f(\"out.txt\") )";

    // (a) Type-checks: a successful front-end run means no ArgTypeMismatch was
    // raised — the coercion was inserted instead.
    let fe = compile_source(src, "main").expect("front-end compiles: coercion inserted");

    // The checker must have materialized a `Coerce` node somewhere (around the
    // `"out.txt"` literal). Find it and confirm its child is the string literal.
    let arena = &fe.program.arena;
    let coerce = arena
        .exprs
        .iter()
        .find_map(|e| match &e.kind {
            ExprKind::Coerce(c) => Some(c.clone()),
            _ => None,
        })
        .expect("a Coerce node was inserted at the String→Path argument site");
    assert!(
        matches!(&arena[coerce.expr].kind, ExprKind::Literal(_)),
        "the coerced child is the original string literal, got {:?}",
        arena[coerce.expr].kind
    );
    // The coercion function it points at is `Path`'s constructor.
    match &arena[coerce.coerce_fn].kind {
        ItemKind::Function(f) => assert_eq!(f.name, "Path", "coerce_fn is Path's constructor"),
        other => panic!("coerce_fn should be a function, got {other:?}"),
    }

    // (b) Evaluates correctly: identity — the Path is the same string at runtime.
    let outcome = run(src, "main").expect("evaluation runs");
    match outcome {
        Outcome::Clean(Value::Str(s)) => {
            assert_eq!(
                &*s, "out.txt",
                "String→Path coercion is identity at runtime"
            )
        }
        other => panic!("expected the coerced path string `out.txt`, got {other:?}"),
    }
}

/// An array literal in a checking position coerces element-wise: `["a", "b"]`
/// passed where `Array[Path]` is expected has each `String` element coerced
/// through `Path`'s `#[coerce]` constructor. Type-checks (no ArgTypeMismatch on
/// the array) and evaluates to the same strings (the coercion is identity).
#[test]
fn string_array_coerces_to_path_array_end_to_end() {
    // `f` wants an `Array[Path]`; `main` passes a `["a", "b"]` array of bare
    // `String` literals. The argument is a checking-mode position, so the checker
    // coerces each element structurally.
    let src = "fn f(ps: Array[Path]) -> Array[Path] ( ps )\n\
               fn main() -> Array[Path] ( f([\"a\", \"b\"]) )";

    // (a) Type-checks: a successful front-end run means no ArgTypeMismatch was
    // raised on the array — the element-wise coercion was inserted instead.
    let fe = compile_source(src, "main").expect("front-end compiles: array coercion inserted");

    // Each string element must have been wrapped in a `Coerce` node pointing at
    // `Path`'s constructor.
    let arena = &fe.program.arena;
    let coerces: Vec<_> = arena
        .exprs
        .iter()
        .filter_map(|e| match &e.kind {
            ExprKind::Coerce(c) => Some(c.clone()),
            _ => None,
        })
        .collect();
    assert!(
        coerces.len() >= 2,
        "expected at least two Coerce nodes (one per array element), got {}",
        coerces.len()
    );
    for coerce in &coerces {
        assert!(
            matches!(&arena[coerce.expr].kind, ExprKind::Literal(_)),
            "each coerced child is an original string literal, got {:?}",
            arena[coerce.expr].kind
        );
        match &arena[coerce.coerce_fn].kind {
            ItemKind::Function(f) => {
                assert_eq!(f.name, "Path", "coerce_fn is Path's constructor")
            }
            other => panic!("coerce_fn should be a function, got {other:?}"),
        }
    }

    // (b) Evaluates correctly: the array value comes back clean (the coercion is
    // identity, so the two paths are the same strings). The elements are lazy
    // thunks; the front-end already proved each was coerced, so here we just
    // confirm the array evaluates without poison and has the right arity.
    let outcome = run(src, "main").expect("evaluation runs");
    match outcome {
        Outcome::Clean(Value::Array(items)) => {
            assert_eq!(
                items.len(),
                2,
                "the coerced `Array[Path]` has both elements"
            );
        }
        other => panic!("expected a clean array of two coerced paths, got {other:?}"),
    }
}
