//! Forcing an invocation runs it (`r[exec.invocation]`) and the tree output crosses
//! back, end to end. Three shapes share one strand harness:
//!
//!  * an explicit `with { observer: (run) => run.stream.out }` returns the live
//!    output tree;
//!  * a **bare** invocation uses the default observer `run.ok.out`
//!    (`r[exec.observer.simple]`) — applied natively on the executor (gate on
//!    success, yield the tree) so no closure ships;
//!  * a bare invocation whose process fails poisons at that gate.
//!
//! The spine, on one strand: forcing the invocation dispatches the observe (parks);
//! the boss drives it — the executor runs the command (writing a file), captures its
//! outputs, and serves them on a `Producing` virtual connection; the boss opens that
//! vconn + `register_handle` and hands eval a `Value::Tree(HandleId)`; then
//! `out.file("out.txt")` `force_remote`s that path (parks again), the boss pulls it
//! over the vconn, and eval resumes with the file's bytes. Demand-pull across
//! executors, in the language.

use std::rc::Rc;

use vx_ir::item::ItemKind;
use vx_runtime::compile::compile_source;
use vx_runtime::eval::{
    KernelMode, Outcome as EvalOutcome, Value, eval_body_with_exec, tool_value,
};
use vx_runtime::observe::local_observer;
use vx_runtime::strand::{Outcome, ResourceId, Scheduler};

/// Compile `src` (entry `t(cmd: Command, script: String)`), run it on a strand with a
/// `sh` tool and `script` bound, and interpret the worker-side `EvalOutcome` with
/// `check` — a `Value` is `!Send`, so the interpretation must happen on the strand.
fn run_t(
    src: &str,
    script: &str,
    check: impl FnOnce(EvalOutcome) -> Outcome + Send + 'static,
) -> Option<Outcome> {
    let fe = compile_source(src, "t").expect("program compiles");
    let entry = fe
        .program
        .arena
        .items
        .iter()
        .find_map(|it| match &it.kind {
            ItemKind::Function(f) if f.name == "t" => Some(f.body),
            _ => None,
        })
        .expect("fn t");

    let s = Scheduler::new(&[4]);
    let client = s.block_on(local_observer());
    let cid = s.register_observe_client(client);

    let program = fe.program;
    let env = fe.env;
    let script = script.to_string();
    let strand = s.spawn(ResourceId(0), move || {
        // A `sh` tool whose command coordinate rides along for shape; the invocation
        // supplies the args via a `${script}` hole (crosses as one argv element, so no
        // command-arg quoting is needed).
        let cmd_value = tool_value("@vixen/grammar-cc", "cc", "sh");
        let script = Value::Str(Rc::from(script.as_str()));
        check(eval_body_with_exec(
            &program,
            &env,
            entry,
            vec![cmd_value, script],
            KernelMode::Off,
            cid,
        ))
    });
    s.demand(strand);
    s.run();
    s.outcome(strand)
}

/// The forced bytes equal `want` (rendered lossily as a `Str` for now), else a poison
/// describing the mismatch.
fn expect_bytes(want: &'static str) -> impl FnOnce(EvalOutcome) -> Outcome + Send + 'static {
    move |outcome| match outcome {
        EvalOutcome::Clean(Value::Str(s)) if &*s == want => Outcome::Clean,
        EvalOutcome::Clean(v) => {
            Outcome::Poisoned(format!("returned {v:?}, expected the file bytes {want:?}"))
        }
        EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
    }
}

/// An explicit `(run) => run.stream.out` observer returns the live output tree;
/// forcing `out.txt` out of it pulls the bytes over vox.
#[test]
fn explicit_stream_observer_returns_a_forceable_output_tree() {
    let outcome = run_t(
        "fn t(cmd: Command, script: String) -> Blob ( \
           let out = `$cmd -c ${script}` with { observer: (run) => run.stream.out }; \
           out.file(\"out.txt\") )",
        "printf CONTENT > out.txt",
        expect_bytes("CONTENT"),
    );
    assert_eq!(
        outcome,
        Some(Outcome::Clean),
        "the explicit stream observer returns the tree; out.txt forces to its bytes"
    );
}

/// A **bare** invocation uses the default observer `run.ok.out` — the executor gates
/// on success and yields the output tree natively, with no observer closure shipped.
/// Forcing `out.txt` out of it pulls the bytes over vox, same as the explicit case.
#[test]
fn bare_invocation_uses_the_default_observer() {
    let outcome = run_t(
        "fn t(cmd: Command, script: String) -> Blob ( \
           let out = `$cmd -c ${script}`; \
           out.file(\"out.txt\") )",
        "printf CONTENT > out.txt",
        expect_bytes("CONTENT"),
    );
    assert_eq!(
        outcome,
        Some(Outcome::Clean),
        "a bare invocation defaults to run.ok.out → the output tree"
    );
}

/// The default observer is success-gated: a bare invocation whose process exits
/// nonzero poisons at the gate, so forcing it (and anything downstream) detonates.
#[test]
fn bare_invocation_poisons_when_the_process_fails() {
    let outcome = run_t(
        "fn t(cmd: Command, script: String) -> Blob ( \
           let out = `$cmd -c ${script}`; \
           out.file(\"out.txt\") )",
        "exit 7",
        |outcome| match outcome {
            EvalOutcome::Poisoned { reason, .. } => {
                assert!(!reason.is_empty(), "the gate failure carries a reason");
                Outcome::Clean
            }
            EvalOutcome::Clean(v) => {
                Outcome::Poisoned(format!("expected a gate poison, got {v:?}"))
            }
        },
    );
    assert_eq!(
        outcome,
        Some(Outcome::Clean),
        "exit 7 → the default run.ok gate poisons → the invocation poisons"
    );
}
