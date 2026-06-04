//! The tree output crossing, end to end: `exec(cmd) with { observer: (run) =>
//! run.stream.out }` returns the run's **output tree**, and forcing a file out of it
//! pulls the bytes over vox.
//!
//! The whole spine fires on one strand: eval dispatches the observe (parks); the boss
//! drives it — the executor runs the command (writing a file), captures its outputs,
//! and serves them on a `Producing` virtual connection; the boss opens that vconn +
//! `register_handle` and hands eval a `Value::Tree(HandleId)`; then `out.file("out.txt")`
//! `force_remote`s that path (parks again), the boss pulls it over the vconn, and eval
//! resumes with the file's bytes. Demand-pull across executors, in the language.

use vx_ir::item::ItemKind;
use vx_runtime::compile::compile_source;
use vx_runtime::eval::{
    KernelMode, Outcome as EvalOutcome, Value, command_value, eval_body_with_exec,
};
use vx_runtime::observe::local_observer;
use vx_runtime::strand::{Outcome, ResourceId, Scheduler};

#[test]
fn exec_returns_a_forceable_output_tree_over_vox() {
    let fe = compile_source(
        "fn t(cmd: Command) -> Blob ( \
           let out = exec(cmd) with { observer: (run) => run.stream.out }; \
           out.file(\"out.txt\") )",
        "t",
    )
    .expect("exec/tree program compiles");
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
    let strand = s.spawn(ResourceId(0), move || {
        // A command that writes `out.txt` into its scratch dir. The grammar coordinate
        // rides along for shape; `exec` runs `sh -c …` from the record's binary + args.
        let cmd_value = command_value(
            "@vixen/grammar-cc",
            "cc",
            "sh",
            vec!["-c".to_string(), "printf CONTENT > out.txt".to_string()],
        );

        match eval_body_with_exec(&program, &env, entry, vec![cmd_value], KernelMode::Off, cid) {
            // `out.file("out.txt")` forced the path over the vconn → the file's bytes
            // (rendered lossily as a `Str` for now).
            EvalOutcome::Clean(Value::Str(s)) if &*s == "CONTENT" => Outcome::Clean,
            EvalOutcome::Clean(v) => {
                Outcome::Poisoned(format!("exec returned {v:?}, expected the file's bytes"))
            }
            EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
        }
    });
    s.demand(strand);
    s.run();
    assert_eq!(
        s.outcome(strand),
        Some(Outcome::Clean),
        "exec returns the output tree; forcing out.txt over vox yields its bytes"
    );
}
