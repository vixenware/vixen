//! The demand-driven strand scheduler (`runtime-semantics.md` §2, invariants
//! I1–I7) — **our own**, not tokio tasks. A tokio task races forward; a strand
//! runs only because something pulled on it, holds a **permit only while actively
//! running**, releases it the instant it parks, and never parks without a **typed
//! reason**. From the demand edges + park reasons the scheduler can always say
//! *why* a build isn't progressing (the wait-for tree), which tokio cannot.
//!
//! This is the minimal core: a single-threaded, cooperative, demand-driven
//! executor (concurrency via interleaving at await points; thread-pool parallelism
//! is a later grow-step *under the same seven invariants*). It owns the permit
//! pools and the observability firehose; every transition is an [`Event`], and the
//! invariant tests assert over that firehose — max-concurrency, ran-once,
//! undemanded-never-ran, the stall report. Emit **only what is real** (§8).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

/// A permit-bearing resource: a local-pool slot, a remote executor's capacity, a
/// host's concurrent-request limit — all the same mechanism (§2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResourceId(pub u32);

/// A scheduled unit of demand-driven work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StrandId(pub u32);

/// Why a strand is parked — never absent (I7: no silent blocking). The scheduler
/// derives the wait-for tree from these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParkReason {
    /// Runnable, but waiting for a permit on a saturated resource.
    AwaitingPermit { resource: ResourceId },
    /// Blocked on a part another strand (a producer) has not yet yielded.
    AwaitingPart { producer: StrandId, part: String },
}

/// A firehose event — the scheduler's history. Tests assert over the stream; a
/// renderer turns it into the playground picture. Only-what-is-real (§8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A strand became (transitively) demanded.
    Demanded(StrandId),
    /// A strand acquired a permit and is now actively running.
    Admitted {
        strand: StrandId,
        resource: ResourceId,
    },
    /// A running strand parked, releasing its permit, with the reason why.
    Parked {
        strand: StrandId,
        reason: ParkReason,
    },
    /// A parked strand was woken and re-queued.
    Woken(StrandId),
    /// A strand ran to completion (clean or poisoned).
    Finished { strand: StrandId, poisoned: bool },
}

/// A signal a strand can await (the minimal "part"): pending until `fire`d, then
/// it wakes whatever parked on it. Stands in for a producer yielding a part.
#[derive(Clone, Default)]
pub struct Signal(Rc<RefCell<SignalState>>);

#[derive(Default)]
struct SignalState {
    fired: bool,
    waiters: Vec<StrandId>,
}

impl Signal {
    pub fn new() -> Signal {
        Signal(Rc::new(RefCell::new(SignalState::default())))
    }
    fn is_fired(&self) -> bool {
        self.0.borrow().fired
    }
}

/// What a strand produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Clean,
    Poisoned(String),
}

struct Strand {
    /// Taken out while being polled (so polling holds no scheduler borrow — the
    /// future calls back in), re-stashed if it parks, left `None` once finished.
    fut: Option<Pin<Box<dyn Future<Output = Outcome>>>>,
    resource: ResourceId,
    demanded: bool,
    /// `None` while runnable/parked; `Some` once finished (memoised result).
    done: Option<Outcome>,
    /// Set when parked; cleared on admit. The live half of the wait-for tree.
    parked: Option<ParkReason>,
    /// Currently holding a permit (admitted, not yet parked/finished).
    admitted: bool,
}

struct Inner {
    /// Per-resource `(capacity, in_use)`.
    resources: Vec<(u32, u32)>,
    /// Observed peak `in_use` per resource (I2 evidence).
    peak: Vec<u32>,
    strands: Vec<Strand>,
    ready: VecDeque<StrandId>,
    events: Vec<Event>,
}

/// The scheduler: owns permits, strands, the ready queue, and the firehose.
#[derive(Clone)]
pub struct Scheduler(Rc<RefCell<Inner>>);

impl Scheduler {
    /// A scheduler with the given per-resource capacities (`capacities[i]` is the
    /// permit count of `ResourceId(i)`).
    pub fn new(capacities: &[u32]) -> Scheduler {
        Scheduler(Rc::new(RefCell::new(Inner {
            resources: capacities.iter().map(|&c| (c, 0)).collect(),
            peak: vec![0; capacities.len()],
            strands: Vec::new(),
            ready: VecDeque::new(),
            events: Vec::new(),
        })))
    }

    /// Register a strand on `resource` running `fut`. It does **not** run until
    /// [`demand`](Self::demand)ed (I1).
    pub fn spawn<F>(&self, resource: ResourceId, fut: F) -> StrandId
    where
        F: Future<Output = Outcome> + 'static,
    {
        let mut inner = self.0.borrow_mut();
        let id = StrandId(inner.strands.len() as u32);
        inner.strands.push(Strand {
            fut: Some(Box::pin(fut)),
            resource,
            demanded: false,
            done: None,
            parked: None,
            admitted: false,
        });
        id
    }

    /// Mark a strand demanded and queue it. Idempotent.
    pub fn demand(&self, id: StrandId) {
        let mut inner = self.0.borrow_mut();
        let s = &mut inner.strands[id.0 as usize];
        if s.demanded || s.done.is_some() {
            return;
        }
        s.demanded = true;
        inner.events.push(Event::Demanded(id));
        inner.ready.push_back(id);
    }

    /// A signal a strand can `park_on` and the scheduler can `fire`.
    pub fn signal(&self) -> Signal {
        Signal::new()
    }

    /// Fire a signal: mark it fired and re-queue every strand parked on it (each
    /// must still re-acquire a permit to resume — parking released it).
    pub fn fire(&self, sig: &Signal) {
        let waiters = {
            let mut st = sig.0.borrow_mut();
            st.fired = true;
            std::mem::take(&mut st.waiters)
        };
        let mut inner = self.0.borrow_mut();
        for w in waiters {
            inner.events.push(Event::Woken(w));
            inner.ready.push_back(w);
        }
    }

    /// Drive demanded strands to quiescence: admit ready strands up to each
    /// resource's capacity, poll them, release permits on park/finish. Returns
    /// when nothing more can make progress (all done or all parked).
    pub fn run(&self) {
        loop {
            // Pick a ready strand whose resource has a free permit.
            let next = {
                let mut inner = self.0.borrow_mut();
                let mut picked = None;
                for _ in 0..inner.ready.len() {
                    let id = inner.ready.pop_front().unwrap();
                    let r = inner.strands[id.0 as usize].resource;
                    let (cap, used) = inner.resources[r.0 as usize];
                    if used < cap {
                        picked = Some(id);
                        break;
                    }
                    inner.ready.push_back(id); // no permit yet — leave it queued
                }
                picked
            };
            let Some(id) = next else { return };
            self.admit_and_poll(id);
        }
    }

    /// Admit one strand (take a permit), poll it once, and release the permit if it
    /// parked or finished.
    fn admit_and_poll(&self, id: StrandId) {
        // Admit: take a permit, record peak concurrency, and take the future out so
        // polling holds no scheduler borrow (the future calls back in).
        let mut fut = {
            let mut inner = self.0.borrow_mut();
            let r = inner.strands[id.0 as usize].resource;
            inner.resources[r.0 as usize].1 += 1;
            let now = inner.resources[r.0 as usize].1;
            if now > inner.peak[r.0 as usize] {
                inner.peak[r.0 as usize] = now;
            }
            inner.strands[id.0 as usize].admitted = true;
            inner.strands[id.0 as usize].parked = None;
            inner.events.push(Event::Admitted {
                strand: id,
                resource: r,
            });
            inner.strands[id.0 as usize]
                .fut
                .take()
                .expect("an admitted strand has a future")
        };

        CURRENT.with(|c| *c.borrow_mut() = Some((self.clone(), id)));
        let waker = strand_waker();
        let mut cx = Context::from_waker(&waker);
        let poll = fut.as_mut().poll(&mut cx);
        CURRENT.with(|c| *c.borrow_mut() = None);

        // Release the permit; record the outcome or re-stash the parked future.
        let mut inner = self.0.borrow_mut();
        let r = inner.strands[id.0 as usize].resource;
        inner.resources[r.0 as usize].1 -= 1;
        inner.strands[id.0 as usize].admitted = false;
        match poll {
            Poll::Ready(outcome) => {
                let poisoned = matches!(outcome, Outcome::Poisoned(_));
                inner.strands[id.0 as usize].done = Some(outcome);
                inner.events.push(Event::Finished {
                    strand: id,
                    poisoned,
                });
            }
            Poll::Pending => {
                inner.strands[id.0 as usize].fut = Some(fut);
                // The await-primitive recorded the park reason before returning
                // Pending; surface it (I7 — never park silently).
                let reason = inner.strands[id.0 as usize]
                    .parked
                    .clone()
                    .unwrap_or(ParkReason::AwaitingPermit { resource: r });
                inner.events.push(Event::Parked { strand: id, reason });
            }
        }
    }

    // --- observation (the firehose + the live wait-for snapshot) ---

    /// The event firehose (history).
    pub fn events(&self) -> Vec<Event> {
        self.0.borrow().events.clone()
    }

    /// Peak observed concurrency on a resource (I2 evidence).
    pub fn peak_concurrency(&self, r: ResourceId) -> u32 {
        self.0.borrow().peak[r.0 as usize]
    }

    /// Whether a strand has finished, and its outcome.
    pub fn outcome(&self, id: StrandId) -> Option<Outcome> {
        self.0.borrow().strands[id.0 as usize].done.clone()
    }

    /// The live **stall report** (I7): every demanded, unfinished strand and the
    /// typed reason it is parked — the leaves of the wait-for tree.
    pub fn stall_report(&self) -> Vec<(StrandId, ParkReason)> {
        let inner = self.0.borrow();
        inner
            .strands
            .iter()
            .enumerate()
            .filter(|(_, s)| s.demanded && s.done.is_none())
            .filter_map(|(i, s)| s.parked.clone().map(|r| (StrandId(i as u32), r)))
            .collect()
    }
}

thread_local! {
    /// The strand currently being polled, so an await-primitive can find its
    /// scheduler + id to record a park reason.
    static CURRENT: RefCell<Option<(Scheduler, StrandId)>> = const { RefCell::new(None) };
}

/// Await a signal: if already fired, resume; else record the park reason
/// (`AwaitingPart`) and park (the permit is released by the executor on Pending).
pub async fn park_on(sig: Signal, producer: StrandId, part: &str) {
    ParkOn {
        sig,
        producer,
        part: part.to_string(),
    }
    .await
}

struct ParkOn {
    sig: Signal,
    producer: StrandId,
    part: String,
}

impl Future for ParkOn {
    type Output = ();
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.sig.is_fired() {
            return Poll::Ready(());
        }
        // Record this strand as a waiter and its typed park reason.
        CURRENT.with(|c| {
            if let Some((sched, id)) = &*c.borrow() {
                self.sig.0.borrow_mut().waiters.push(*id);
                sched.0.borrow_mut().strands[id.0 as usize].parked =
                    Some(ParkReason::AwaitingPart {
                        producer: self.producer,
                        part: self.part.clone(),
                    });
            }
        });
        Poll::Pending
    }
}

// A no-op-ish waker: waking is driven by `fire` re-queueing, so the waker itself
// need not do anything (the executor re-polls from the ready queue). We still need
// a valid Waker for `Context`.
fn strand_waker() -> Waker {
    fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    fn noop(_: *const ()) {}
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R0: ResourceId = ResourceId(0);

    fn max_concurrency(s: &Scheduler) -> u32 {
        s.peak_concurrency(R0)
    }

    /// I1 — Demand-driven: an undemanded strand never runs (the bomb, lifted to
    /// strands). The strand would poison if run; we never demand it, so the run
    /// stays clean.
    #[test]
    fn i1_undemanded_never_runs() {
        let s = Scheduler::new(&[4]);
        let bomb = s.spawn(R0, async { Outcome::Poisoned("💣 forced".into()) });
        let live = s.spawn(R0, async { Outcome::Clean });
        s.demand(live);
        s.run();
        assert_eq!(s.outcome(live), Some(Outcome::Clean));
        assert_eq!(s.outcome(bomb), None, "undemanded bomb must never run");
        assert!(
            !s.events()
                .iter()
                .any(|e| matches!(e, Event::Admitted { strand, .. } if *strand == bomb)),
            "undemanded strand must never be admitted"
        );
    }

    /// I3 — Single execution: demanding the same strand twice runs it once.
    #[test]
    fn i3_single_execution() {
        let s = Scheduler::new(&[4]);
        let id = s.spawn(R0, async { Outcome::Clean });
        s.demand(id);
        s.demand(id); // idempotent
        s.run();
        let admits = s
            .events()
            .iter()
            .filter(|e| matches!(e, Event::Admitted { strand, .. } if *strand == id))
            .count();
        assert_eq!(admits, 1, "one identity → one run");
    }

    /// I2 + I5 — capacity respected, and parked work never starves runnable work.
    /// 16 strands, capacity 2; the first 8 park forever awaiting a never-fired
    /// signal (parked → permit released); assert the other 8 finish and peak
    /// concurrency stayed 2.
    #[test]
    fn i2_i5_capacity_and_progress() {
        let s = Scheduler::new(&[2]);
        let never = s.signal(); // never fired
        let mut stuck = Vec::new();
        let mut runnable = Vec::new();
        // Dispatch the 8 stuck first (they're "ahead in the queue").
        for _ in 0..8 {
            let sig = never.clone();
            let id = s.spawn(R0, async move {
                park_on(sig, StrandId(u32::MAX), "never").await;
                Outcome::Clean
            });
            stuck.push(id);
        }
        for _ in 0..8 {
            let id = s.spawn(R0, async { Outcome::Clean });
            runnable.push(id);
        }
        for &id in stuck.iter().chain(runnable.iter()) {
            s.demand(id);
        }
        s.run();

        for &id in &runnable {
            assert_eq!(
                s.outcome(id),
                Some(Outcome::Clean),
                "runnable strand must finish"
            );
        }
        for &id in &stuck {
            assert_eq!(s.outcome(id), None, "stuck strand stays parked forever");
        }
        assert!(
            max_concurrency(&s) <= 2,
            "capacity 2 must never be exceeded"
        );
    }

    /// I4 — Schedule-independent results: the same strands, run under different
    /// capacities (strictly serial at 1, interleaved at 8), yield identical
    /// per-strand outcomes. Scheduling can't change the answer.
    #[test]
    fn i4_results_independent_of_capacity() {
        fn build_and_run(capacity: u32) -> Vec<Option<Outcome>> {
            let s = Scheduler::new(&[capacity]);
            let ids: Vec<_> = (0..8u32)
                .map(|i| {
                    s.spawn(R0, async move {
                        if i % 3 == 0 {
                            Outcome::Poisoned(format!("p{i}"))
                        } else {
                            Outcome::Clean
                        }
                    })
                })
                .collect();
            for &id in &ids {
                s.demand(id);
            }
            s.run();
            ids.iter().map(|&id| s.outcome(id)).collect()
        }
        assert_eq!(
            build_and_run(1),
            build_and_run(8),
            "per-strand outcome must not depend on capacity / admission order"
        );
    }

    /// I7 — No silent blocking: every parked strand has a typed reason, and the
    /// stall report names them.
    #[test]
    fn i7_stall_report_explains_the_block() {
        let s = Scheduler::new(&[2]);
        let never = s.signal();
        let stuck: Vec<_> = (0..3)
            .map(|i| {
                let sig = never.clone();
                let part = format!("part{i}");
                s.spawn(R0, async move {
                    park_on(sig, StrandId(99), &part).await;
                    Outcome::Clean
                })
            })
            .collect();
        for &id in &stuck {
            s.demand(id);
        }
        s.run();
        let report = s.stall_report();
        // Two parked (capacity 2 admitted them); all reasons are typed AwaitingPart.
        assert!(!report.is_empty(), "a stalled run must explain itself");
        assert!(
            report
                .iter()
                .all(|(_, r)| matches!(r, ParkReason::AwaitingPart { .. })),
            "no strand parks without a typed reason: {report:?}"
        );
    }

    /// A parked strand resumes when its signal fires (and re-acquires a permit).
    #[test]
    fn parked_strand_resumes_on_fire() {
        let s = Scheduler::new(&[1]);
        let sig = s.signal();
        let id = {
            let sig = sig.clone();
            s.spawn(R0, async move {
                park_on(sig, StrandId(0), "rmeta").await;
                Outcome::Clean
            })
        };
        s.demand(id);
        s.run();
        assert_eq!(s.outcome(id), None, "parked until fired");
        s.fire(&sig);
        s.run();
        assert_eq!(s.outcome(id), Some(Outcome::Clean), "resumes after fire");
    }
}
