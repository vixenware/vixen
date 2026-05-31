//! The demand-driven strand scheduler (`runtime-semantics.md` §2, invariants
//! I1–I7) — **our own**, not tokio tasks. A tokio task races forward; a strand
//! runs only because something pulled on it, holds a **permit only while actively
//! running**, releases it the instant it parks, and never parks without a **typed
//! reason**. From the demand edges + park reasons the scheduler can always say
//! *why* a build isn't progressing (the wait-for tree), which tokio cannot.
//!
//! Concurrency is **real, on a worker thread pool**, kept low-risk by a
//! boss/workers split: the `run()` thread (the boss) owns all scheduler state and
//! is the only one that touches it — it admits ready strands up to each resource's
//! permit count and hands their futures to a pool of workers, which poll them **in
//! parallel** and report back. The only shared state is the work queue, the result
//! channel, and the `Signal`s (an `Arc<Mutex>`); the scheduler's `Inner` never
//! needs a lock. (Per-run thread spawn for now; a persistent pool is a grow-step.)
//!
//! Every transition is an [`Event`]; the invariant tests assert over that firehose
//! — now under genuine parallelism. Emit **only what is real** (§8).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use std::thread::JoinHandle;

/// A strand's work: a `Send` future (it crosses to a worker thread and back).
type Fut = Pin<Box<dyn Future<Output = Outcome> + Send>>;

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
    /// A strand acquired a permit and is now actively running (on a worker).
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

/// A signal a strand can await (the minimal "part"): pending until `fire`d, then it
/// wakes whatever parked on it. Shared across threads (a worker parks on it, the
/// boss fires it), so it's an `Arc<Mutex>`.
#[derive(Clone, Default)]
pub struct Signal(Arc<Mutex<SignalState>>);

#[derive(Default)]
struct SignalState {
    fired: bool,
    waiters: Vec<StrandId>,
}

impl Signal {
    pub fn new() -> Signal {
        Signal::default()
    }
}

/// What a strand produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Clean,
    Poisoned(String),
}

struct Strand {
    /// Taken out while a worker polls it (so the boss holds no reference during the
    /// parallel poll), re-stashed if it parks, left `None` once finished.
    fut: Option<Fut>,
    resource: ResourceId,
    demanded: bool,
    /// `None` while runnable/parked; `Some` once finished (memoised result).
    done: Option<Outcome>,
    /// Set when parked; cleared on admit. The live half of the wait-for tree.
    parked: Option<ParkReason>,
    /// Currently holding a permit (admitted to a worker, not yet returned).
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
    /// Strands admitted to workers and not yet reported back.
    in_flight: u32,
}

/// What a worker reports after polling one strand.
enum WorkerResult {
    Done(Outcome),
    /// Parked: the reason it recorded, and its future handed back to re-stash.
    Parked {
        reason: Option<ParkReason>,
        fut: Fut,
    },
}

/// A pending pop-able queue of work for the pool, with a close flag.
struct WorkQueue {
    items: VecDeque<(StrandId, Fut)>,
    closed: bool,
}

/// The scheduler: the boss owns `Inner` (single-threaded); workers only touch the
/// work queue, the result channel, and `Signal`s. The worker `Pool` is spawned
/// once and reused by every `run()`.
#[derive(Clone)]
pub struct Scheduler(Rc<RefCell<Inner>>, Rc<Pool>);

/// A persistent worker pool: spawned at [`Scheduler::new`], reused by every
/// `run()`, and stopped (queue closed + joined) when the last `Scheduler` handle
/// drops. Workers idle on the work-queue condvar between runs.
struct Pool {
    work: Arc<(Mutex<WorkQueue>, Condvar)>,
    result_rx: RefCell<mpsc::Receiver<(StrandId, WorkerResult)>>,
    handles: RefCell<Vec<JoinHandle<()>>>,
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.work.0.lock().unwrap().closed = true;
        self.work.1.notify_all();
        for h in self.handles.borrow_mut().drain(..) {
            let _ = h.join();
        }
    }
}

impl Scheduler {
    /// A scheduler with the given per-resource capacities (`capacities[i]` is the
    /// permit count of `ResourceId(i)`). Spawns the worker pool.
    pub fn new(capacities: &[u32]) -> Scheduler {
        let inner = Rc::new(RefCell::new(Inner {
            resources: capacities.iter().map(|&c| (c, 0)).collect(),
            peak: vec![0; capacities.len()],
            strands: Vec::new(),
            ready: VecDeque::new(),
            events: Vec::new(),
            in_flight: 0,
        }));
        let n_workers = std::thread::available_parallelism()
            .map_or(4, |n| n.get())
            .clamp(1, 16);
        let work: Arc<(Mutex<WorkQueue>, Condvar)> = Arc::new((
            Mutex::new(WorkQueue {
                items: VecDeque::new(),
                closed: false,
            }),
            Condvar::new(),
        ));
        let (result_tx, result_rx) = mpsc::channel::<(StrandId, WorkerResult)>();
        let handles: Vec<_> = (0..n_workers)
            .map(|_| {
                let work = Arc::clone(&work);
                let result_tx = result_tx.clone();
                std::thread::spawn(move || worker_loop(&work, &result_tx))
            })
            .collect();
        drop(result_tx); // only the workers' clones remain
        Scheduler(
            inner,
            Rc::new(Pool {
                work,
                result_rx: RefCell::new(result_rx),
                handles: RefCell::new(handles),
            }),
        )
    }

    /// Register a strand on `resource` running `fut`. It does **not** run until
    /// [`demand`](Self::demand)ed (I1). The future must be `Send` — it is polled on
    /// a worker thread.
    pub fn spawn<F>(&self, resource: ResourceId, fut: F) -> StrandId
    where
        F: Future<Output = Outcome> + Send + 'static,
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

    /// A signal a strand can `park_on` and the boss can `fire`.
    pub fn signal(&self) -> Signal {
        Signal::new()
    }

    /// Fire a signal: mark it fired and re-queue every strand parked on it (each
    /// must still re-acquire a permit to resume — parking released it). Called on
    /// the boss thread, between or after `run()`s.
    pub fn fire(&self, sig: &Signal) {
        let waiters = {
            let mut st = sig.0.lock().unwrap();
            st.fired = true;
            std::mem::take(&mut st.waiters)
        };
        let mut inner = self.0.borrow_mut();
        for w in waiters {
            inner.events.push(Event::Woken(w));
            inner.strands[w.0 as usize].parked = None;
            inner.ready.push_back(w);
        }
    }

    /// Drive demanded strands to quiescence on the persistent worker pool: admit
    /// ready strands up to each resource's capacity, hand their futures to workers
    /// that poll them in parallel, release permits as they park/finish. Returns when
    /// nothing more can make progress (all done or all parked); the workers stay
    /// alive (idle on the queue condvar) for the next `run()`.
    pub fn run(&self) {
        let pool = &self.1;
        let result_rx = pool.result_rx.borrow();
        loop {
            // Hand every admittable strand to the pool (permit-gated).
            while let Some(item) = self.admit_one() {
                pool.work.0.lock().unwrap().items.push_back(item);
                pool.work.1.notify_one();
            }
            // Quiescent when nothing is running and nothing more is admittable.
            if self.0.borrow().in_flight == 0 {
                break;
            }
            // A worker will report (something is in flight); record + free its permit.
            let (id, res) = result_rx
                .recv()
                .expect("a result while strands are in flight");
            self.complete(id, res);
        }
    }

    /// Admit one ready strand whose resource has a free permit: take the permit,
    /// record peak concurrency, and hand its future out. `None` if nothing is
    /// admittable right now (ready empty, or all ready strands blocked on permits).
    fn admit_one(&self) -> Option<(StrandId, Fut)> {
        let mut inner = self.0.borrow_mut();
        for _ in 0..inner.ready.len() {
            let id = inner.ready.pop_front().unwrap();
            let r = inner.strands[id.0 as usize].resource;
            let (cap, used) = inner.resources[r.0 as usize];
            if used >= cap {
                inner.ready.push_back(id); // no permit yet — leave it queued
                continue;
            }
            inner.resources[r.0 as usize].1 += 1;
            let now = inner.resources[r.0 as usize].1;
            if now > inner.peak[r.0 as usize] {
                inner.peak[r.0 as usize] = now;
            }
            inner.in_flight += 1;
            inner.strands[id.0 as usize].admitted = true;
            inner.strands[id.0 as usize].parked = None;
            inner.events.push(Event::Admitted {
                strand: id,
                resource: r,
            });
            let fut = inner.strands[id.0 as usize]
                .fut
                .take()
                .expect("an admitted strand has a future");
            return Some((id, fut));
        }
        None
    }

    /// Record a worker's report: release the permit and either memoise the result
    /// or re-stash the parked future with its typed reason.
    fn complete(&self, id: StrandId, res: WorkerResult) {
        let mut inner = self.0.borrow_mut();
        let r = inner.strands[id.0 as usize].resource;
        inner.resources[r.0 as usize].1 -= 1;
        inner.in_flight -= 1;
        inner.strands[id.0 as usize].admitted = false;
        match res {
            WorkerResult::Done(outcome) => {
                let poisoned = matches!(outcome, Outcome::Poisoned(_));
                inner.strands[id.0 as usize].done = Some(outcome);
                inner.events.push(Event::Finished {
                    strand: id,
                    poisoned,
                });
            }
            WorkerResult::Parked { reason, fut } => {
                inner.strands[id.0 as usize].fut = Some(fut);
                let reason = reason.unwrap_or(ParkReason::AwaitingPermit { resource: r });
                inner.strands[id.0 as usize].parked = Some(reason.clone());
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

/// A worker: pop a strand's future, poll it (in parallel with its peers), report
/// the outcome (or the parked future + reason) to the boss. Exits when the queue
/// is closed and drained.
fn worker_loop(
    work: &(Mutex<WorkQueue>, Condvar),
    result_tx: &mpsc::Sender<(StrandId, WorkerResult)>,
) {
    let (lock, cv) = work;
    loop {
        let item = {
            let mut q = lock.lock().unwrap();
            loop {
                if let Some(item) = q.items.pop_front() {
                    break Some(item);
                }
                if q.closed {
                    break None;
                }
                q = cv.wait(q).unwrap();
            }
        };
        let Some((id, mut fut)) = item else { return };

        CURRENT.with(|c| *c.borrow_mut() = Some(WorkerCtx { id, park: None }));
        let waker = noop_waker();
        let poll = fut.as_mut().poll(&mut Context::from_waker(&waker));
        let park = CURRENT.with(|c| c.borrow_mut().take().and_then(|ctx| ctx.park));

        let res = match poll {
            Poll::Ready(outcome) => WorkerResult::Done(outcome),
            Poll::Pending => WorkerResult::Parked { reason: park, fut },
        };
        let _ = result_tx.send((id, res));
    }
}

thread_local! {
    /// The strand the current worker is polling, so an await-primitive can record
    /// its park reason (read back by the worker after poll).
    static CURRENT: RefCell<Option<WorkerCtx>> = const { RefCell::new(None) };
}

struct WorkerCtx {
    id: StrandId,
    park: Option<ParkReason>,
}

/// Await a signal: if already fired, resume; else register as a waiter and record
/// the park reason (`AwaitingPart`), then park (the worker releases the permit on
/// Pending). The fired-check and waiter-registration are atomic under the signal
/// lock, so a concurrent `fire` either drains this waiter or is seen as fired.
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
        let mut st = self.sig.0.lock().unwrap();
        if st.fired {
            return Poll::Ready(());
        }
        CURRENT.with(|c| {
            if let Some(ctx) = &mut *c.borrow_mut() {
                st.waiters.push(ctx.id);
                ctx.park = Some(ParkReason::AwaitingPart {
                    producer: self.producer,
                    part: self.part.clone(),
                });
            }
        });
        Poll::Pending
    }
}

/// A no-op waker: wakeups are driven by `fire` re-queueing (the boss re-polls from
/// the ready queue), so the std waker is unused — but `Context` needs a valid one.
fn noop_waker() -> Waker {
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
    /// concurrency stayed 2 — now under real parallelism.
    #[test]
    fn i2_i5_capacity_and_progress() {
        let s = Scheduler::new(&[2]);
        let never = s.signal(); // never fired
        let mut stuck = Vec::new();
        let mut runnable = Vec::new();
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
    /// capacities (and thus different parallelism), yield identical per-strand
    /// outcomes. Scheduling can't change the answer.
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

    /// Real parallelism: `par` strands at capacity `par`, each blocking on a
    /// `Barrier(par)` — which releases only if all `par` are polled *simultaneously*
    /// on distinct worker threads. Run on a side thread with a generous timeout, so
    /// a failure to parallelize surfaces as a failed test, not a hang. `par` adapts
    /// to the machine (so a 1-core box trivially passes).
    #[test]
    fn strands_run_in_parallel_on_the_pool() {
        use std::sync::Barrier;
        use std::time::Duration;

        let par = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(4);
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let s = Scheduler::new(&[par as u32]);
            let barrier = Arc::new(Barrier::new(par));
            let ids: Vec<_> = (0..par)
                .map(|_| {
                    let b = Arc::clone(&barrier);
                    s.spawn(R0, async move {
                        b.wait(); // releases only when all `par` are here at once
                        Outcome::Clean
                    })
                })
                .collect();
            for &id in &ids {
                s.demand(id);
            }
            s.run();
            let all_clean = ids.iter().all(|&id| s.outcome(id) == Some(Outcome::Clean));
            let _ = done_tx.send(all_clean);
        });
        match done_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(true) => {}
            Ok(false) => panic!("strands did not all complete"),
            Err(_) => {
                panic!("timed out — strands did not run in parallel (barrier never released)")
            }
        }
    }
}
