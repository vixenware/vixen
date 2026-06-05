//! The `observe` RPC — the orchestrator ships an observer bundle **and a command**
//! to an executor (a fresh **mirror** runtime) over **vox**; the executor runs the
//! command, builds a `Run` from the result, evaluates the observer against it, and
//! returns the result. A local runner may use a transport-local memory link; a
//! remote executor is a different transport, the *same* service. Vox owns the
//! serialization — this code never names a codec.
//!
//! The `Run` is built and consumed *here*, on the executor: `run.ok` is a co-located
//! force, not a remote one. The genuinely remote values are the **outputs** the
//! observer returns (streams, the producing tree) — those cross back as handles, and
//! that join (`force_remote`) lands with the streaming work.

use vx_ir::Bundle;

use crate::eval::{KernelMode, eval_body_wire};
use crate::run_value::{self, ProcOutcome};

/// A command to run — minimal for now (program + args). The full `Command` value
/// with its grammar-classified inputs/outputs, and running it under the VFS sandbox
/// via `run_exec`, land as the input/output handling grows.
#[derive(facet::Facet, Debug, Clone)]
pub struct ExecCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// What the executor needs to observe: the command to run plus the observer to
/// evaluate against its `Run`. `observer` is `None` for the default observer
/// `run.ok.out` (`r[exec.observer.simple]`) — applied natively on the executor
/// (gate on success, yield the output tree) so a bare invocation ships no closure.
#[derive(facet::Facet, Debug, Clone)]
pub struct ObserveRequest {
    pub observer: Option<Bundle>,
    pub command: ExecCommand,
    pub outputs: Vec<String>,
}

/// A runtime value crossing back from the executor to the orchestrator over vox —
/// the observer's result, fully forced into data. This is the wire form of
/// `eval::Value`'s data subset: scalars and the compound shapes (tuple/array/struct/
/// variant), with every field already forced (no thunks cross). Code values
/// (closures), capability values (commands/grammars/host), and the opaque failure
/// envelope are not data and never cross here; the genuinely-remote leaves
/// (`Stream`/`Tree`) cross as handles, not inline — both are their own pieces.
///
/// Facet-derived: it *is* the vox contract for the `observe` result. Vox owns the
/// serialization; this code never names a codec.
#[derive(facet::Facet, Debug, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum WireValue {
    U64(u64),
    Bool(bool),
    Str(String),
    Unit,
    Tuple(Vec<WireValue>),
    Array(Vec<WireValue>),
    /// A **producing tree** the observer returned (`run.ok.out`/`run.stream.out`) —
    /// a genuinely-remote leaf, *not* inline data. It crosses as a handle: the
    /// orchestrator opens a `Producing` virtual connection to the executor and forces
    /// its paths on demand (`force_remote`). Parameterless for now (one output tree
    /// per run); it carries a tree id once a run exposes several.
    Tree,
    /// A struct's named fields, in construction order.
    Struct(Vec<(String, WireValue)>),
    /// An enum variant: its `tag` plus its (named or positional-`"0"`/`"1"`/…) fields.
    Variant {
        tag: String,
        fields: Vec<(String, WireValue)>,
    },
}

/// The result of observing: the observer's value crossed back as data, or a poison
/// (a failed `run.ok`, or a run that couldn't start). The poison still crosses as a
/// bare message — the *structured* failure crossing (the domain error value + the
/// failure context) is its own piece.
#[derive(facet::Facet, Debug, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum Observed {
    /// The observer returned cleanly; here is its value.
    Value(WireValue),
    /// The observer poisoned (e.g. `run.ok` on a failed process), or the run couldn't start.
    Poisoned(String),
}

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
            match eval_body_wire(&program, &env, bundle.entry, vec![run], KernelMode::Off) {
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

/// The runner-side service: ship a bundle + command, get the observer's result.
#[vox::service]
pub trait Observe {
    async fn observe(&self, request: ObserveRequest) -> Observed;
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
