//! Eval on a strand — the real evaluator running inside a worker-owned coroutine
//! on the strand pool, over the public API. This is the integration the coroutine
//! rework exists for: eval's `Rc`-based state runs fine inside a `!Send` coroutine
//! that the type system pins to its worker. The library only *guarantees* this by
//! construction (`Coro: !Send`); here it actually runs.
//!
//! No remote parts yet — pure local evaluation, so no strand ever parks. Forcing an
//! unresolved cross-executor part (which *does* park) is the next step.

use vx_runtime::eval;
use vx_runtime::strand::{Outcome, ResourceId, Scheduler, StrandId};

/// Spawn `entry` of `src` as a strand: the body captures owned source (`Send`),
/// builds the `Rc` eval state worker-side, and maps eval's result to the
/// scheduler's `Outcome`. The coroutine that holds the eval state never leaves its
/// worker — that's what makes the `Rc` sound.
fn eval_strand(s: &Scheduler, src: &str, entry: &str) -> StrandId {
    let (src, entry) = (src.to_string(), entry.to_string());
    s.spawn(ResourceId(0), move || match eval::run(&src, &entry) {
        Ok(eval::Outcome::Clean(_)) => Outcome::Clean,
        Ok(eval::Outcome::Poisoned { reason, .. }) => Outcome::Poisoned(reason),
        Err(setup) => Outcome::Poisoned(format!("setup: {setup}")),
    })
}

/// A clean program evaluates to `Clean` when run as a strand on the pool — kernels
/// and all (`add` dispatches to a kernel inside the coroutine).
#[test]
fn clean_program_evaluates_to_clean_on_a_strand() {
    let s = Scheduler::new(&[4]);
    let id = eval_strand(
        &s,
        "fn add(a: U64, b: U64) -> U64 ( a + b ) fn main() -> U64 ( add(40, 2) )",
        "main",
    );
    s.demand(id);
    s.run();
    assert_eq!(
        s.outcome(id),
        Some(Outcome::Clean),
        "clean eval finishes clean on the pool"
    );
}

/// A poisoning program finishes `Poisoned`, with eval's reason carried out through
/// the strand boundary (divide-by-zero poisons in the kernel).
#[test]
fn poisoning_program_carries_its_reason_through_the_strand() {
    let s = Scheduler::new(&[4]);
    let id = eval_strand(&s, "fn main() -> U64 ( 1 / 0 )", "main");
    s.demand(id);
    s.run();
    match s.outcome(id) {
        Some(Outcome::Poisoned(reason)) => {
            assert!(
                !reason.is_empty(),
                "the poison reason is carried out: {reason:?}"
            )
        }
        other => panic!("expected poison, got {other:?}"),
    }
}

/// A spread of independent eval strands demanded together: each is its own
/// coroutine on (up to) its own worker, and all finish with the right outcome —
/// real evaluation across the pool, not synthetic strands.
#[test]
fn many_evals_run_across_the_pool() {
    let s = Scheduler::new(&[8]);
    let ids: Vec<_> = (0..16u32)
        .map(|i| {
            let src = format!("fn main() -> U64 ( {i} + {i} )");
            eval_strand(&s, &src, "main")
        })
        .collect();
    for &id in &ids {
        s.demand(id);
    }
    s.run();
    for &id in &ids {
        assert_eq!(
            s.outcome(id),
            Some(Outcome::Clean),
            "every eval strand finishes clean"
        );
    }
}
