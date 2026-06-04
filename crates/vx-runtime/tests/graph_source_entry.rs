use vx_runtime::eval::{KernelMode, Outcome, Value, run_in_mode};

#[test]
fn source_entry_drives_graph_root_constant() {
    let (outcome, dispatches) =
        run_in_mode("fn main() -> U64 ( 42 )", "main", KernelMode::Off).expect("graph run");
    assert_eq!(dispatches, 0);
    match outcome {
        Outcome::Clean(Value::U64(42)) => {}
        other => panic!("expected graph root constant, got {other:?}"),
    }
}

#[test]
fn source_entry_drives_graph_projection_without_forcing_dead_slot() {
    let source = r#"
fn main() -> U64 (
    let pair = (42, 💣);
    pair[0]
)
"#;
    let (outcome, _) = run_in_mode(source, "main", KernelMode::Off).expect("graph run");
    match outcome {
        Outcome::Clean(Value::U64(42)) => {}
        other => panic!("expected selected graph field, got {other:?}"),
    }
}

#[test]
fn source_entry_reports_unsupported_graph_shape() {
    let err = run_in_mode(
        "fn main() -> Bool ( assert_eq(1, 1) )",
        "main",
        KernelMode::Off,
    )
    .expect_err("assert_eq is still an unsupported opaque graph call");
    assert!(
        err.contains("graph unsupported"),
        "unexpected graph error: {err}"
    );
}
