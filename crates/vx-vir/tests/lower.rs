//! End-to-end eval tests for VIR lowering (010-0: core non-capturing).
//!
//! Each test: source → VIR → demand analysis → lower → vx-rt eval.

use vx_parser::parse_source_file;
use vx_resolve::SourceId;
use vx_rt::{RtValue, eval_program};
use vx_vir::build::VirBuilder;
use vx_vir::demand::analyze_demand;
use vx_vir::lower::lower_vir_program;
use vx_vir::resolve::resolve_names;

/// Build VIR, resolve, analyze demand, lower, eval.
fn eval_vir(source: &str, args: &[RtValue]) -> Result<RtValue, String> {
    let parsed = parse_source_file(source);
    let file = parsed.tree();
    let mut program = VirBuilder::build_program(&[(SourceId(0), file)]);
    let _errors = resolve_names(&mut program.arena);
    let demand = analyze_demand(&program);
    let executable = lower_vir_program(&program, &demand).map_err(|e| format!("lower: {e:?}"))?;
    eval_program(&executable, args).map_err(|e| format!("eval: {e:?}"))
}

fn expect_u64(source: &str, args: &[RtValue], expected: u64) {
    match eval_vir(source, args) {
        Ok(RtValue::U64(n)) => assert_eq!(n, expected, "source:\n{source}"),
        Ok(other) => panic!("expected U64({expected}), got {other:?}\nsource:\n{source}"),
        Err(e) => panic!("error: {e}\nsource:\n{source}"),
    }
}

fn expect_string(source: &str, args: &[RtValue], expected: &str) {
    match eval_vir(source, args) {
        Ok(RtValue::String(s)) => assert_eq!(s, expected, "source:\n{source}"),
        Ok(other) => panic!("expected String({expected:?}), got {other:?}\nsource:\n{source}"),
        Err(e) => panic!("error: {e}\nsource:\n{source}"),
    }
}

fn expect_failure(source: &str) {
    match eval_vir(source, &[]) {
        Err(e) if e.contains("Failure") => {} // ok
        other => panic!("expected failure, got {other:?}\nsource:\n{source}"),
    }
}

// =========================================================================
// 1. Constant return
// =========================================================================

#[test]
fn constant_return() {
    expect_u64("fn main() -> U64 ( 42 )", &[], 42);
}

// =========================================================================
// 2. Param passthrough
// =========================================================================

#[test]
fn param_passthrough() {
    expect_u64("fn main(n: U64) -> U64 ( n )", &[RtValue::U64(42)], 42);
}

// =========================================================================
// 3. Addition
// =========================================================================

#[test]
fn addition() {
    expect_u64("fn main() -> U64 ( 20 + 22 )", &[], 42);
}

// =========================================================================
// 4. Subtraction
// =========================================================================

#[test]
fn subtraction() {
    expect_u64("fn main(n: U64) -> U64 ( n - 1 )", &[RtValue::U64(43)], 42);
}

// =========================================================================
// 5. Let binding
// =========================================================================

#[test]
fn let_binding() {
    expect_u64("fn main() -> U64 ( let x = 40; x + 2 )", &[], 42);
}

// =========================================================================
// 6. Chained lets
// =========================================================================

#[test]
fn chained_lets() {
    expect_u64(
        "fn main() -> U64 ( let a = 10; let b = a + 20; b + 12 )",
        &[],
        42,
    );
}

// =========================================================================
// 7. If/select true
// =========================================================================

#[test]
fn if_select_true() {
    expect_u64("fn main() -> U64 ( if true ( 42 ) else ( 0 ) )", &[], 42);
}

// =========================================================================
// 8. If/select false
// =========================================================================

#[test]
fn if_select_false() {
    expect_u64("fn main() -> U64 ( if false ( 0 ) else ( 42 ) )", &[], 42);
}

// =========================================================================
// 9. Equality
// =========================================================================

#[test]
fn equality() {
    expect_u64("fn main() -> U64 ( if 1 == 1 ( 42 ) else ( 0 ) )", &[], 42);
}

// =========================================================================
// 10. Not-equal
// =========================================================================

#[test]
fn not_equal() {
    expect_u64("fn main() -> U64 ( if 1 != 2 ( 42 ) else ( 0 ) )", &[], 42);
}

// =========================================================================
// 11. Direct call
// =========================================================================

#[test]
fn direct_call() {
    expect_u64(
        "fn add_one(n: U64) -> U64 ( n + 1 )\nfn main() -> U64 ( add_one(41) )",
        &[],
        42,
    );
}

// =========================================================================
// 12. Call chain
// =========================================================================

#[test]
fn call_chain() {
    expect_u64(
        r#"
fn f(n: U64) -> U64 ( n + 1 )
fn g(n: U64) -> U64 ( f(n) + 1 )
fn main() -> U64 ( g(40) )
"#,
        &[],
        42,
    );
}

// =========================================================================
// 13. Fail
// =========================================================================

#[test]
fn fail_expr() {
    expect_failure("fn main() -> U64 ( fail(\"boom\") )");
}

// =========================================================================
// 14. String concat (interpolated string)
// =========================================================================

#[test]
fn string_concat() {
    expect_string(
        "fn main() -> String ( \"hello\" ++ \" world\" )",
        &[],
        "hello world",
    );
}

// =========================================================================
// 15. Unused binding (never materialized)
// =========================================================================

#[test]
fn unused_binding() {
    expect_u64("fn main() -> U64 ( let x = 99; 42 )", &[], 42);
}

// =========================================================================
// 16. Bool literal in condition
// =========================================================================

#[test]
fn bool_literal_condition() {
    expect_u64(
        "fn main() -> U64 ( let b = true; if b ( 42 ) else ( 0 ) )",
        &[],
        42,
    );
}

// =========================================================================
// 17. Nested if
// =========================================================================

#[test]
fn nested_if() {
    expect_u64(
        "fn main() -> U64 ( if true ( if false ( 0 ) else ( 42 ) ) else ( 99 ) )",
        &[],
        42,
    );
}

// =========================================================================
// 18. Multi-arg function call
// =========================================================================

#[test]
fn multi_arg_call() {
    expect_u64(
        "fn add(a: U64, b: U64) -> U64 ( a + b )\nfn main() -> U64 ( add(20, 22) )",
        &[],
        42,
    );
}
