use vx_analysis::{
    SingleSourceBridge, build_minimal_analysis, lower_executable_program_for_entry_body,
};
use vx_rt::{RtValue, eval_program};

fn compile_and_run(source: &str, function_name: &str, args: &[RtValue]) -> RtValue {
    let bridge = SingleSourceBridge::from_source_text(source);
    assert!(
        bridge.parse_errors.is_empty(),
        "parse errors: {:?}",
        bridge.parse_errors
    );
    assert!(
        bridge.resolve_errors.is_empty(),
        "resolve errors: {:?}",
        bridge.resolve_errors
    );
    assert!(
        bridge.type_errors.is_empty(),
        "type errors: {:?}",
        bridge.type_errors
    );
    assert!(
        bridge.cap_errors.is_empty(),
        "cap errors: {:?}",
        bridge.cap_errors
    );

    let body_id = bridge
        .bodies
        .iter()
        .find(|b| b.name == function_name)
        .unwrap_or_else(|| panic!("function `{function_name}` not found"))
        .id;
    let analysis = build_minimal_analysis(&bridge);
    analysis.validate().expect("analysis validation failed");
    let program = lower_executable_program_for_entry_body(&analysis, &bridge, body_id)
        .expect("lowering failed");

    eval_program(&program, args).expect("eval failed")
}

/// Program A: lazy field selection
/// `(41 + 1, bomb()); pair._0` must return 42 with bomb() never entered.
/// bomb() recurses infinitely — if forced, we'd stack overflow.
#[test]
fn program_a_lazy_field_selection() {
    let source = r#"
fn bomb() -> U64 ( bomb() )

fn main() -> U64 (
  let pair = (41 + 1, bomb());
  pair._0
)
"#;
    let result = compile_and_run(source, "main", &[]);
    match result {
        RtValue::U64(42) => {} // success — bomb was never forced
        other => panic!("expected U64(42), got {:?}", other),
    }
}

/// Program C: local sharing
/// `plus_one(n) + plus_one(n)` where sharing means plus_one entered once.
#[test]
fn program_c_local_sharing() {
    let source = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )

fn main(n: U64) -> U64 (
  let x = plus_one(n);
  x + x
)
"#;
    let result = compile_and_run(source, "main", &[RtValue::U64(10)]);
    match result {
        RtValue::U64(22) => {}
        other => panic!("expected U64(22), got {:?}", other),
    }
}

/// Simple constant
#[test]
fn constant_return() {
    let source = r#"
fn main() -> U64 ( 42 )
"#;
    let result = compile_and_run(source, "main", &[]);
    match result {
        RtValue::U64(42) => {}
        other => panic!("expected U64(42), got {:?}", other),
    }
}

/// Program B: closure capture
/// `add_n = (x) => x + n; (add_n(1), add_n(2))._1` returns n+2
#[test]
fn program_b_closure_capture() {
    let source = r#"
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  let pair = (add_n(1), add_n(2));
  pair._1
)
"#;
    let result = compile_and_run(source, "main", &[RtValue::U64(10)]);
    match result {
        RtValue::U64(12) => {}
        other => panic!("expected U64(12), got {:?}", other),
    }
}

/// Program D: higher-order helper
/// `apply(f, x)` with 2 args — tests multi-arg callable calls
#[test]
fn program_d_higher_order() {
    let source = r#"
fn apply(f: Fn(U64) -> U64, x: U64) -> U64 ( f(x) )

fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  apply(add_n, 41)
)
"#;
    let result = compile_and_run(source, "main", &[RtValue::U64(1)]);
    match result {
        RtValue::U64(42) => {}
        other => panic!("expected U64(42), got {:?}", other),
    }
}

/// Addition with parameter
#[test]
fn add_with_param() {
    let source = r#"
fn main(n: U64) -> U64 ( n + 1 )
"#;
    let result = compile_and_run(source, "main", &[RtValue::U64(41)]);
    match result {
        RtValue::U64(42) => {}
        other => panic!("expected U64(42), got {:?}", other),
    }
}
