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
