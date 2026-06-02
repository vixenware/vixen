//! The exec **mirror** round-trip: compile a function (orchestrator side), bundle
//! its body, install the bundle into a *fresh* program in the original id space, and
//! evaluate it there — yielding the same value as evaluating it locally, with zero
//! re-compilation and zero id allocation on the mirror.
//!
//! This is the spine in miniature: closure-walk → install → eval. Argument binding
//! (the observer's `Run`) and shipping over vox land on top of it.

use vx_ir::compile::compile_source;
use vx_ir::item::ItemKind;
use vx_ir::reachable;
use vx_runtime::eval::{KernelMode, Outcome, Value, eval_body};

#[test]
fn mirror_installs_and_evaluates_a_body() {
    let fe =
        compile_source("fn answer() -> U64 ( 21 + 21 )", "answer").expect("front-end compiles");
    let entry = fe
        .program
        .arena
        .items
        .iter()
        .find_map(|it| match &it.kind {
            ItemKind::Function(f) if f.name == "answer" => Some(f.body),
            _ => None,
        })
        .expect("answer fn");

    // Oracle: evaluate the body in the original program.
    let local = eval_body(&fe.program, &fe.env, entry, KernelMode::Off);

    // Mirror: bundle the body → install into a fresh program → evaluate there.
    let bundle = reachable(&fe.program.arena, &fe.env, entry);
    let (mprog, menv) = bundle.install();
    let mirror = eval_body(&mprog, &menv, entry, KernelMode::Off);

    assert!(
        matches!(local, Outcome::Clean(Value::U64(42))),
        "local oracle = {local:?}"
    );
    assert!(
        matches!(mirror, Outcome::Clean(Value::U64(42))),
        "mirror eval of the installed bundle = {mirror:?}"
    );
}
