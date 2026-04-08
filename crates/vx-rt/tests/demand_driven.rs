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

/// Pair must be constructed (passed to a function), but dead sibling is lowerable.
/// This catches "only thunk on UnsupportedShape" — bomb() lowers fine but must not run.
#[test]
fn pair_passed_to_fn_dead_sibling() {
    expect_u64(
        r#"
fn bomb() -> U64 ( fail("boom") )
fn pick_first(p: (U64, U64)) -> U64 ( p._0 )

fn main() -> U64 (
  let p = (42, bomb());
  pick_first(p)
)
"#,
        &[],
        42,
    );
}

/// Chained projection without intermediate let: outer._0._0
#[test]
fn chained_projection_no_let() {
    expect_u64(
        r#"
fn bomb() -> U64 ( fail("boom") )

fn main() -> U64 (
  let outer = ((7, bomb()), (bomb(), bomb()));
  outer._0._0
)
"#,
        &[],
        7,
    );
}

/// Let-bound struct with named field projection
#[test]
fn let_bound_struct_named_field() {
    expect_u64(
        r#"
struct Pair { a: U64, b: U64 }
fn bomb() -> U64 ( fail("boom") )

fn main() -> U64 (
  let s = Pair { a: 42, b: bomb() };
  s.a
)
"#,
        &[],
        42,
    );
}

/// Deep tuple projection chain (no intermediate lets): outer._0._0._0
#[test]
fn deep_tuple_projection_chain() {
    expect_u64(
        r#"
fn bomb() -> U64 ( fail("boom") )

fn main() -> U64 (
  let outer = (((7, bomb()), bomb()), bomb());
  outer._0._0._0
)
"#,
        &[],
        7,
    );
}

/// Deep named-field projection chain: s.a.a
#[test]
fn deep_named_field_projection_chain() {
    expect_u64(
        r#"
struct Pair { a: U64, b: U64 }
struct Outer { a: Pair, b: U64 }
fn bomb() -> U64 ( fail("boom") )

fn main() -> U64 (
  let s = Outer { a: Pair { a: 42, b: bomb() }, b: bomb() };
  s.a.a
)
"#,
        &[],
        42,
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
