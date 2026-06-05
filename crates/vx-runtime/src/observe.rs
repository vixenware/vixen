//! Executor-side observer evaluation.
//!
//! The `Run` is built and consumed *here*, on the executor: `run.ok` is a co-located
//! force, not a remote one. The genuinely remote values are the **outputs** the
//! observer returns (streams, the producing tree) — those cross back as handles, and
//! that join (`force_remote`) lands with the streaming work.

use vx_exec_protocol::{Observed, WireValue};
use vx_ir::Bundle;

use crate::eval::{KernelMode, eval_body_wire};
use crate::run_value::{self, ProcOutcome};

/// A completed process result supplied by an executor.
///
/// This is the only part of exec observation that belongs in `vx-runtime`: given
/// executor-owned facts about a completed run, evaluate the observer using the
/// graph-backed body path and cross the result to wire. Process spawning, scratch
/// directories, CAS-VFS mounts, sandbox policy, output flushing, and retry logic
/// must stay outside this helper.
#[derive(Debug, Clone)]
pub struct CompletedRun {
    pub proc: ProcOutcome,
}

impl CompletedRun {
    pub fn new(proc: ProcOutcome) -> Self {
        Self { proc }
    }
}

pub fn evaluate_observer(run: CompletedRun, observer: Option<Bundle>) -> Observed {
    match observer {
        Some(bundle) => {
            let run = run_value::build(run.proc);
            let (program, env) = bundle.install();
            match eval_body_wire(
                &program,
                &env,
                bundle.entry,
                vec![run],
                KernelMode::GraphOnly,
            ) {
                Ok(value) => Observed::Value(value),
                Err(reason) => Observed::Poisoned(reason),
            }
        }
        None => match run_value::ok_gate_result(&run.proc) {
            Ok(()) => Observed::Value(WireValue::Tree),
            Err(reason) => Observed::Poisoned(reason),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::compile_source;
    use vx_ir::item::ItemKind;
    use vx_ir::reachable;

    fn observer_bundle() -> Bundle {
        // An observer that forces `run.ok` — clean on exit 0, poisons on nonzero.
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
        reachable(&fe.program.arena, &fe.env, entry)
    }

    #[test]
    fn observer_evaluation_accepts_a_completed_process_without_spawning() {
        let bundle = observer_bundle();
        let ok = evaluate_observer(
            CompletedRun::new(ProcOutcome {
                exit_code: 0,
                stdout: b"OUT".to_vec(),
                stderr: b"ERR".to_vec(),
            }),
            Some(bundle.clone()),
        );
        assert_eq!(
            ok,
            Observed::Value(WireValue::Struct(vec![
                ("stdout".to_string(), WireValue::Str("OUT".to_string())),
                ("stderr".to_string(), WireValue::Str("ERR".to_string())),
            ]))
        );

        let bad = evaluate_observer(
            CompletedRun::new(ProcOutcome {
                exit_code: 7,
                stdout: Vec::new(),
                stderr: Vec::new(),
            }),
            Some(bundle),
        );
        assert!(matches!(bad, Observed::Poisoned(_)));
    }
}
