use vx_rt::{RtValue, eval_source};

fn expect_u64(source: &str, args: &[RtValue], expected: u64) {
    let result = eval_source(source, "main", args).expect("eval failed");
    match result {
        RtValue::U64(n) => assert_eq!(n, expected, "source:\n{source}"),
        other => panic!("expected U64({expected}), got {other:?}"),
    }
}

#[test]
fn constant_return() {
    expect_u64("fn main() -> U64 ( 42 )", &[], 42);
}

#[test]
fn add_with_param() {
    expect_u64("fn main(n: U64) -> U64 ( n + 1 )", &[RtValue::U64(41)], 42);
}

/// Program A: lazy field selection — bomb() infinite recursion never entered.
#[test]
fn program_a_lazy_field_selection() {
    expect_u64(
        r#"
fn bomb() -> U64 ( bomb() )

fn main() -> U64 (
  let pair = (41 + 1, bomb());
  pair._0
)
"#,
        &[],
        42,
    );
}

/// Program B: closure capture
#[test]
fn program_b_closure_capture() {
    expect_u64(
        r#"
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  let pair = (add_n(1), add_n(2));
  pair._1
)
"#,
        &[RtValue::U64(10)],
        12,
    );
}

/// Program C: local sharing
#[test]
fn program_c_local_sharing() {
    expect_u64(
        r#"
fn plus_one(n: U64) -> U64 ( n + 1 )

fn main(n: U64) -> U64 (
  let x = plus_one(n);
  x + x
)
"#,
        &[RtValue::U64(10)],
        22,
    );
}

/// Program D: higher-order helper
#[test]
fn program_d_higher_order() {
    expect_u64(
        r#"
fn apply(f: Fn(U64) -> U64, x: U64) -> U64 ( f(x) )

fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  apply(add_n, 41)
)
"#,
        &[RtValue::U64(1)],
        42,
    );
}
