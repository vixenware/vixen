//! The observe dispatch over strands: a strand dispatches an `observe` RPC, **parks**
//! (`AwaitingObserve`, freeing its worker), the boss drives the vox call on its
//! runtime, and the strand **resumes** with the result. This is `force_remote`
//! generalized to "force an observe" — the eval↔async-observe bridge for
//! `exec(cmd) with { observer }`, over a real vox connection (`local_observer`).

use vx_ir::item::ItemKind;
use vx_ir::reachable;
use vx_runtime::compile::compile_source;
use vx_runtime::observe::{ExecCommand, ObserveRequest, local_observer};
use vx_runtime::strand::{ObserveOutcome, Outcome, ResourceId, Scheduler, dispatch_observe};

/// Bundle `(run) => run.ok` — clean on exit 0, poisons on nonzero.
fn observer_bundle() -> vx_ir::Bundle {
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

fn request(program: &str) -> ObserveRequest {
    ObserveRequest {
        observer: Some(observer_bundle()),
        command: ExecCommand {
            program: program.to_string(),
            args: vec![],
        },
    }
}

/// `true` exits 0 → the observer's `run.ok` is clean. The strand parks on the observe,
/// the boss drives it over vox, and the strand resumes Clean.
#[test]
fn a_strand_dispatches_an_observe_and_resumes_clean() {
    let s = Scheduler::new(&[4]);
    // Establish the observe client on the scheduler's runtime so the boss drives it.
    let client = s.block_on(local_observer());
    let cid = s.register_observe_client(client);

    let req = request("true");
    let strand = s.spawn(ResourceId(0), move || {
        let observed = dispatch_observe(cid, req);
        match &*observed {
            ObserveOutcome::Poisoned(why) => Outcome::Poisoned(format!("observe poisoned: {why}")),
            // A crossed value (or tree handle) means the observer ran and `run.ok` was clean.
            _ => Outcome::Clean,
        }
    });
    s.demand(strand);
    s.run();
    assert_eq!(s.outcome(strand), Some(Outcome::Clean));
}

/// `false` exits 1 → the observer's `run.ok` poisons. The poison crosses back as
/// `Observed::Poisoned`, and the strand detonates with it (I6 across the boundary).
#[test]
fn a_strand_observe_carries_back_the_processs_failure() {
    let s = Scheduler::new(&[4]);
    let client = s.block_on(local_observer());
    let cid = s.register_observe_client(client);

    let req = request("false");
    let strand = s.spawn(ResourceId(0), move || {
        let observed = dispatch_observe(cid, req);
        match &*observed {
            ObserveOutcome::Poisoned(_) => Outcome::Poisoned("run.ok poisoned".into()),
            other => Outcome::Poisoned(format!("expected a poison, got {other:?}")),
        }
    });
    s.demand(strand);
    s.run();
    match s.outcome(strand) {
        Some(Outcome::Poisoned(why)) => assert_eq!(why, "run.ok poisoned"),
        other => panic!("expected the failure to cross back, got {other:?}"),
    }
}
