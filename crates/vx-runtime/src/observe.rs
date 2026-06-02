//! The `observe` RPC — the orchestrator ships an observer bundle **and a command**
//! to an executor (a fresh **mirror** runtime) over **vox**; the executor runs the
//! command, builds a `Run` from the result, evaluates the observer against it, and
//! returns the result. In-process uses a `transport-local` memory link; a remote
//! executor is a different transport, the *same* service. Vox owns the serialization
//! — this code never names a codec.
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

/// What the executor needs to observe: the observer bundle + the command to run.
#[derive(facet::Facet, Debug, Clone)]
pub struct ObserveRequest {
    pub bundle: Bundle,
    pub command: ExecCommand,
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

/// The runner-side service: ship a bundle + command, get the observer's result.
#[vox::service]
pub trait Observe {
    async fn observe(&self, request: ObserveRequest) -> Observed;
}

/// The executor side — a fresh mirror per call: run the command, build the `Run`,
/// install the bundle, evaluate the observer against the `Run`. Never compiles,
/// never allocates an id.
#[derive(Clone, Default)]
pub struct ObserveService;

impl Observe for ObserveService {
    async fn observe(&self, request: ObserveRequest) -> Observed {
        // Run the actual command — a real subprocess.
        let proc = match tokio::process::Command::new(&request.command.program)
            .args(&request.command.args)
            .output()
            .await
        {
            Ok(out) => ProcOutcome {
                exit_code: out.status.code().unwrap_or(-1),
                stdout: out.stdout,
                stderr: out.stderr,
            },
            Err(e) => return Observed::Poisoned(format!("spawn {}: {e}", request.command.program)),
        };

        // Build the `Run` (its `.ok` gate forces on the real exit code) and evaluate
        // the observer against it on a fresh mirror, deep-forcing its result into a
        // `WireValue` to cross back. The whole value crosses — not a lossy projection.
        let run = run_value::build(proc);
        let (program, env) = request.bundle.install();
        match eval_body_wire(
            &program,
            &env,
            request.bundle.entry,
            vec![run],
            KernelMode::Off,
        ) {
            Ok(value) => Observed::Value(value),
            Err(reason) => Observed::Poisoned(reason),
        }
    }
}

/// Stand up an [`ObserveService`] over an in-process vox memory link and return a
/// client. The bundle + command cross as vox bytes — colocated today, a remote
/// executor tomorrow, the same service either way.
pub async fn local_observer() -> ObserveClient {
    let (client_link, server_link) = vox::memory_link_pair(64);
    tokio::spawn(async move {
        let acceptor = vox::acceptor_fn(
            |request: &vox::ConnectionRequest, connection: vox::PendingConnection| match request
                .service()
            {
                "Observe" => {
                    connection.handle_with(ObserveDispatcher::new(ObserveService));
                    Ok(())
                }
                _ => Err(vec![]),
            },
        );
        if let Ok(client) = vox::acceptor_on(server_link)
            .on_connection(acceptor)
            .establish::<vox::NoopClient>()
            .await
        {
            client.caller.closed().await;
        }
    });
    vox::initiator_on(client_link, vox::TransportMode::Bare)
        .establish::<ObserveClient>()
        .await
        .expect("establish in-process Observe client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vx_ir::compile::compile_source;
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

    fn cmd(program: &str) -> ExecCommand {
        ExecCommand {
            program: program.to_string(),
            args: vec![],
        }
    }

    /// The first real exec over vox: ship the observer + a command, the executor runs
    /// the real process, builds a `Run`, `run.ok` gates on its actual exit code, and
    /// the observer's value — the whole `Exit` struct — crosses back as data.
    #[tokio::test]
    async fn observe_runs_a_real_command_and_returns_the_observer_value() {
        let bundle = observer_bundle();
        let client = local_observer().await;

        // `true` exits 0 → run.ok forces to an `Exit` → the struct crosses back
        // verbatim (empty stdout/stderr for `true`), not a "clean" placeholder.
        let ok = client
            .observe(ObserveRequest {
                bundle: bundle.clone(),
                command: cmd("true"),
            })
            .await
            .expect("observe over vox");
        assert_eq!(
            ok,
            Observed::Value(WireValue::Struct(vec![
                ("stdout".to_string(), WireValue::Str(String::new())),
                ("stderr".to_string(), WireValue::Str(String::new())),
            ])),
            "exit 0 → the whole Exit value crosses back, got {ok:?}"
        );

        // `false` exits 1 → run.ok poisons.
        let bad = client
            .observe(ObserveRequest {
                bundle,
                command: cmd("false"),
            })
            .await
            .expect("observe over vox");
        assert!(
            matches!(bad, Observed::Poisoned(_)),
            "exit 1 → run.ok poisons, got {bad:?}"
        );
    }

    /// An observer that returns the producing tree (`run.stream.out`) crosses back as
    /// a tree **handle marker** — the genuinely-remote leaf does not travel inline.
    /// (Turning the marker into a forceable handle — the orchestrator opens a
    /// `Producing` virtual connection + `register_handle` — is the next piece.)
    #[tokio::test]
    async fn observe_crosses_the_output_tree_as_a_handle_marker() {
        let fe = compile_source("fn observe(run: Run) -> Tree ( run.stream.out )", "observe")
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
        let bundle = reachable(&fe.program.arena, &fe.env, entry);

        let client = local_observer().await;
        let observed = client
            .observe(ObserveRequest {
                bundle,
                command: cmd("true"),
            })
            .await
            .expect("observe over vox");
        assert_eq!(
            observed,
            Observed::Value(WireValue::Tree),
            "the output tree crosses back as a handle marker, got {observed:?}"
        );
    }
}
