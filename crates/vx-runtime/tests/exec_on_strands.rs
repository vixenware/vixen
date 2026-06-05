//! Cold-demand exec compatibility coverage.
//!
//! Production-path value crossing and cache-key proof targets live in `vx-runner`
//! runner-backed tests. This file keeps the non-demand case that must stay cold
//! even when the registered observe client id would be invalid if reached.

use vx_ir::item::ItemKind;
use vx_runtime::compile::compile_source;
use vx_runtime::eval::{
    KernelMode, Outcome as EvalOutcome, Value, eval_body_with_exec, tool_value,
};
use vx_runtime::strand::{Outcome, ResourceId, Scheduler};

fn cold_exec_outcome(binary: &str) -> Option<Outcome> {
    let fe = compile_source(
        "fn t(cmd: Command) -> U64 ( \
           let pair = ( `$cmd` with { observer: (run) => run.ok }, 42 ); \
           pair[1] )",
        "t",
    )
    .expect("exec demand program compiles");
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
    let cid = 0;

    let program = fe.program;
    let env = fe.env;
    let binary = binary.to_string();
    let strand = s.spawn(ResourceId(0), move || {
        let cmd = tool_value("@vixen/grammar-cc", "cc", &binary);
        match eval_body_with_exec(&program, &env, entry, vec![cmd], KernelMode::GraphOnly, cid) {
            EvalOutcome::Clean(Value::U64(42)) => Outcome::Clean,
            EvalOutcome::Clean(other) => {
                Outcome::Poisoned(format!("expected the demanded sibling 42, got {other:?}"))
            }
            EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
        }
    });
    s.demand(strand);
    s.run();
    s.outcome(strand)
}

/// The cold tuple field is a real backtick exec whose observer would poison for
/// `false`, but the caller demands only the second field. A recursive eager walk
/// runs the command and fails; graph demand leaves it dark.
#[test]
fn undemanded_exec_in_tuple_does_not_dispatch() {
    assert_eq!(
        cold_exec_outcome("false"),
        Some(Outcome::Clean),
        "only pair[1] is demanded, so the failing exec in pair[0] must stay cold"
    );
}
