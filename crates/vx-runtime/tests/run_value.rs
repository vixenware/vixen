//! The Run value model's `.ok` gate, against a synthetic process: exit 0 → `Exit`
//! (clean), nonzero → poison. The §7 success-gate, exercised in isolation before a
//! real subprocess (#2) feeds the model.

use vx_ir::item::ItemKind;
use vx_runtime::compile::compile_source;
use vx_runtime::eval::{KernelMode, Outcome, Value, eval_body};
use vx_runtime::run_value::{ProcOutcome, build};

/// Evaluate `(run) => run.ok` against a synthetic process with the given exit code.
fn observe_ok(exit_code: i32) -> Outcome {
    let fe = compile_source("fn observe(run: Run) -> Exit ( run.ok )", "observe")
        .expect("front-end compiles");
    let entry = fe
        .program
        .arena
        .items
        .iter()
        .find_map(|it| match &it.kind {
            ItemKind::Function(f) if f.name == "observe" => Some(f.body),
            _ => None,
        })
        .expect("observe fn");
    let run = build(ProcOutcome {
        exit_code,
        stdout: b"hi".to_vec(),
        stderr: Vec::new(),
    });
    eval_body(&fe.program, &fe.env, entry, vec![run], KernelMode::Off)
}

#[test]
fn run_ok_yields_exit_on_success() {
    assert!(
        matches!(observe_ok(0), Outcome::Clean(Value::Struct(_))),
        "exit 0 → run.ok forces to an Exit"
    );
}

#[test]
fn run_ok_poisons_on_failure() {
    let o = observe_ok(1);
    assert!(
        matches!(o, Outcome::Poisoned { .. }),
        "nonzero exit → run.ok poisons (the .ok success gate), got {o:?}"
    );
}
