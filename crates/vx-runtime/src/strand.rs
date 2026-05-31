//! The demand-driven strand scheduler (`runtime-semantics.md` §2, invariants
//! I1–I7) — **our own**, not tokio tasks. A tokio task races forward; a strand
//! runs only because something pulled on it, holds a **permit only while actively
//! running**, releases it the instant it parks, and never parks without a **typed
//! reason**. From the demand edges + park reasons the scheduler can always say
//! *why* a build isn't progressing (the wait-for tree), which tokio cannot.
//!
//! **Strands are stackful coroutines, not futures.** Each runs on its own real
//! stack (corosensei): we can capture that stack on a park (the stall report shows
//! *where* a strand blocked, not just that it did), step through it in a debugger,
//! and attribute its off-CPU time — none of which a shredded async state machine
//! gives us. It also means eval stays plain synchronous Rust: it `park`s deep in a
//! call tree without every caller becoming `async`.
//!
//! Concurrency is **real, on a worker thread pool**, kept low-risk by a
//! boss/workers split: the `run()` thread (the boss) owns all scheduler state and
//! is the only one that touches it. Because a coroutine holding eval's `Rc` state
//! is `!Send`, **a worker owns its coroutines and they never migrate** — the boss
//! and workers exchange only `StrandId`s and `Send` payloads (`Command` /
//! `WorkerMsg`), never coroutines. The boss admits ready strands up to each
//! resource's permit count, routing a new strand to the least-loaded worker and a
//! resumed one back to its home worker; the worker builds/resumes the coroutine in
//! place and reports done-or-parked.
//!
//! Every transition is an [`Event`]; the invariant tests assert over that firehose
//! — under genuine parallelism. Emit **only what is real** (§8).

use std::backtrace::Backtrace;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use corosensei::{Coroutine, CoroutineResult, Yielder};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// What a strand yields when it parks: the typed reason **plus** a backtrace
/// captured on the strand's own (deep) stack at the park point — the stall report's
/// evidence of *where* it blocked.
type Yielded = (ParkReason, Backtrace);

/// A strand's coroutine: resumed with `()`, [`Yielded`] each time it parks, returns
/// its [`Outcome`] when done. `!Send` (it may hold eval's `Rc` state) — so it lives
/// and dies on one worker and is never put in a message.
type Coro = Coroutine<(), Yielded, Outcome>;

/// A strand body: the synchronous work. It parks via [`park_on`] (which reaches the
/// coroutine's yielder through a thread-local), so it takes no yielder argument and
/// stays uncoloured. `Send` so the boss can hand it to a worker to build the coro.
type Body = Box<dyn FnOnce() -> Outcome + Send>;

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

/// One leaf of the live wait-for tree: a demanded, unfinished, parked strand — its
/// typed reason and the stack where it blocked (I7, with the coroutine payoff).
#[derive(Debug, Clone)]
pub struct Stalled {
    pub strand: StrandId,
    pub reason: ParkReason,
    /// The strand's stack at the park point, rendered on demand. `None` only if the
    /// park carried no captured stack.
    pub backtrace: Option<String>,
}

struct Strand {
    /// The body, taken out and shipped to a worker on first admit (`Start`). `None`
    /// after — the worker owns the coroutine from then on.
    body: Option<Body>,
    resource: ResourceId,
    demanded: bool,
    /// The worker that owns this strand's coroutine, set on first admit. Every later
    /// admit `Resume`s on this same worker (the coroutine never migrates).
    home: Option<usize>,
    /// `None` while runnable/parked; `Some` once finished (memoised result).
    done: Option<Outcome>,
    /// Set when parked; cleared on admit. The live half of the wait-for tree.
    parked: Option<ParkReason>,
    /// The stack captured at the most recent park (for the stall report).
    park_bt: Option<Backtrace>,
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
    /// Per-worker outstanding-command count, for least-loaded routing of new strands.
    worker_load: Vec<u32>,
}

/// A boss→worker command on a worker's private channel. Carries only `Send` data —
/// never a coroutine (those stay worker-local, enforced by `Coro: !Send`).
enum Command {
    /// Build this strand's coroutine here and run it for the first time.
    Start { id: StrandId, body: Body },
    /// Resume the coroutine this worker already owns.
    Resume { id: StrandId },
    /// Drain and exit (pool shutdown).
    Shutdown,
}

/// A worker→boss report. `worker` lets the boss release that worker's load + permit.
enum WorkerMsg {
    Done {
        id: StrandId,
        worker: usize,
        outcome: Outcome,
    },
    Parked {
        id: StrandId,
        worker: usize,
        reason: ParkReason,
        backtrace: Backtrace,
    },
}

/// What [`Scheduler::admit_one`] decided: ship a new strand's body to `worker`
/// (`Start`), or resume the coroutine `worker` already owns (`Resume`).
enum Dispatch {
    Start {
        worker: usize,
        id: StrandId,
        body: Body,
    },
    Resume {
        worker: usize,
        id: StrandId,
    },
}

/// The scheduler: the boss owns `Inner` (single-threaded); workers own their
/// coroutines and touch only the channels + `Signal`s. The worker `Pool` is spawned
/// once and reused by every `run()`.
#[derive(Clone)]
pub struct Scheduler(Rc<RefCell<Inner>>, Rc<Pool>);

/// A persistent worker pool: spawned at [`Scheduler::new`], reused by every
/// `run()`, and stopped (a `Shutdown` per worker, then joined) when the last
/// `Scheduler` handle drops. Each worker idles on its private command channel
/// between runs, holding its parked coroutines.
struct Pool {
    /// One private command channel per worker (a `Resume` must reach the worker that
    /// owns the coroutine; a new strand goes to the least-loaded worker). Std mpsc —
    /// workers block-recv on it.
    cmd_tx: Vec<mpsc::Sender<Command>>,
    /// Worker→boss reports. A *tokio* channel so the boss can `await` it inside the
    /// async `run` loop, alongside driving in-flight remote forces. `Option` so the
    /// boss can own the receiver across the loop's awaits (rather than hold a
    /// `RefCell` borrow across them) and restore it when quiescent.
    result_rx: RefCell<Option<UnboundedReceiver<WorkerMsg>>>,
    handles: RefCell<Vec<JoinHandle<()>>>,
    /// The boss's current-thread runtime: it drives the remote-part vox `force()`
    /// calls (and owns the vox clients) interleaved with scheduling. `run()` blocks
    /// on it.
    runtime: tokio::runtime::Runtime,
}

impl Drop for Pool {
    fn drop(&mut self) {
        for tx in &self.cmd_tx {
            let _ = tx.send(Command::Shutdown);
        }
        for h in self.handles.borrow_mut().drain(..) {
            let _ = h.join();
        }
    }
}

impl Scheduler {
    /// A scheduler with the given per-resource capacities (`capacities[i]` is the
    /// permit count of `ResourceId(i)`). Spawns the worker pool.
    pub fn new(capacities: &[u32]) -> Scheduler {
        let n_workers = std::thread::available_parallelism()
            .map_or(4, |n| n.get())
            .clamp(1, 16);
        let (result_tx, result_rx) = tokio::sync::mpsc::unbounded_channel::<WorkerMsg>();
        let mut cmd_tx = Vec::with_capacity(n_workers);
        let mut handles = Vec::with_capacity(n_workers);
        for idx in 0..n_workers {
            let (tx, rx) = mpsc::channel::<Command>();
            cmd_tx.push(tx);
            let result_tx = result_tx.clone();
            handles.push(std::thread::spawn(move || {
                worker_loop(idx, &rx, &result_tx)
            }));
        }
        drop(result_tx); // only the workers' clones remain
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build the boss's current-thread runtime");
        let inner = Rc::new(RefCell::new(Inner {
            resources: capacities.iter().map(|&c| (c, 0)).collect(),
            peak: vec![0; capacities.len()],
            strands: Vec::new(),
            ready: VecDeque::new(),
            events: Vec::new(),
            in_flight: 0,
            worker_load: vec![0; n_workers],
        }));
        Scheduler(
            inner,
            Rc::new(Pool {
                cmd_tx,
                result_rx: RefCell::new(Some(result_rx)),
                handles: RefCell::new(handles),
                runtime,
            }),
        )
    }

    /// Register a strand on `resource` running `body`. It does **not** run until
    /// [`demand`](Self::demand)ed (I1). The body must be `Send` — it is shipped to a
    /// worker, which builds the coroutine and runs it there.
    pub fn spawn<F>(&self, resource: ResourceId, body: F) -> StrandId
    where
        F: FnOnce() -> Outcome + Send + 'static,
    {
        let mut inner = self.0.borrow_mut();
        let id = StrandId(inner.strands.len() as u32);
        inner.strands.push(Strand {
            body: Some(Box::new(body)),
            resource,
            demanded: false,
            home: None,
            done: None,
            parked: None,
            park_bt: None,
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
            inner.strands[w.0 as usize].park_bt = None;
            inner.ready.push_back(w);
        }
    }

    /// Drive demanded strands to quiescence on the persistent worker pool: admit
    /// ready strands up to each resource's capacity, command their home worker to
    /// start/resume them in parallel, release permits as they park/finish. Returns
    /// when nothing more can make progress (all done or all parked); the workers
    /// stay alive (idle on their channels, holding parked coroutines) for the next
    /// `run()`.
    pub fn run(&self) {
        self.1.runtime.block_on(self.run_async());
    }

    /// The boss loop, async so it can drive in-flight remote forces (the vox
    /// `force()` calls) alongside worker reports. Today's body: dispatch every
    /// admittable strand, then await the next worker report, until quiescent.
    async fn run_async(&self) {
        let pool = &self.1;
        // Own the receiver across the loop (no `RefCell` borrow held over an await);
        // restore it when quiescent so the next `run()` can take it again.
        let mut result_rx = pool
            .result_rx
            .borrow_mut()
            .take()
            .expect("the boss's result receiver is present between runs");
        loop {
            // Dispatch every admittable strand (permit-gated) to its worker. Dispatch
            // *all* of them before blocking on a result, so peers that must run at the
            // same time (a barrier) can.
            while let Some(dispatch) = self.admit_one() {
                match dispatch {
                    Dispatch::Start { worker, id, body } => {
                        let _ = pool.cmd_tx[worker].send(Command::Start { id, body });
                    }
                    Dispatch::Resume { worker, id } => {
                        let _ = pool.cmd_tx[worker].send(Command::Resume { id });
                    }
                }
            }
            // Quiescent when nothing is running and nothing more is admittable.
            if self.0.borrow().in_flight == 0 {
                break;
            }
            // A worker will report (something is in flight); record + free its permit.
            let msg = result_rx
                .recv()
                .await
                .expect("a result while strands are in flight");
            self.complete(msg);
        }
        *pool.result_rx.borrow_mut() = Some(result_rx);
    }

    /// Admit one ready strand whose resource has a free permit: take the permit,
    /// record peak concurrency, choose its worker (home if started, else the
    /// least-loaded — recorded as home), and decide start-vs-resume. `None` if
    /// nothing is admittable right now (ready empty, or all ready strands blocked on
    /// permits).
    fn admit_one(&self) -> Option<Dispatch> {
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
            // Home worker: reuse if started, else route to the least-loaded one.
            let worker = match inner.strands[id.0 as usize].home {
                Some(h) => h,
                None => {
                    let h = least_loaded(&inner.worker_load);
                    inner.strands[id.0 as usize].home = Some(h);
                    h
                }
            };
            inner.worker_load[worker] += 1;
            inner.strands[id.0 as usize].admitted = true;
            inner.strands[id.0 as usize].parked = None;
            inner.events.push(Event::Admitted {
                strand: id,
                resource: r,
            });
            // First admit ships the body (`Start`); later admits resume the coro.
            return Some(match inner.strands[id.0 as usize].body.take() {
                Some(body) => Dispatch::Start { worker, id, body },
                None => Dispatch::Resume { worker, id },
            });
        }
        None
    }

    /// Record a worker's report: release the permit + worker load, and either
    /// memoise the result or record the parked strand's typed reason and stack.
    fn complete(&self, msg: WorkerMsg) {
        let mut inner = self.0.borrow_mut();
        let (id, worker) = match &msg {
            WorkerMsg::Done { id, worker, .. } | WorkerMsg::Parked { id, worker, .. } => {
                (*id, *worker)
            }
        };
        let r = inner.strands[id.0 as usize].resource;
        inner.resources[r.0 as usize].1 -= 1;
        inner.in_flight -= 1;
        inner.worker_load[worker] -= 1;
        inner.strands[id.0 as usize].admitted = false;
        match msg {
            WorkerMsg::Done { outcome, .. } => {
                let poisoned = matches!(outcome, Outcome::Poisoned(_));
                inner.strands[id.0 as usize].done = Some(outcome);
                inner.events.push(Event::Finished {
                    strand: id,
                    poisoned,
                });
            }
            WorkerMsg::Parked {
                reason, backtrace, ..
            } => {
                inner.strands[id.0 as usize].parked = Some(reason.clone());
                inner.strands[id.0 as usize].park_bt = Some(backtrace);
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

    /// The live **stall report** (I7): every demanded, unfinished, parked strand,
    /// its typed reason, and the stack where it blocked — the leaves of the wait-for
    /// tree. Backtraces are rendered here (lazily), not at park time.
    pub fn stall_report(&self) -> Vec<Stalled> {
        let inner = self.0.borrow();
        inner
            .strands
            .iter()
            .enumerate()
            .filter(|(_, s)| s.demanded && s.done.is_none())
            .filter_map(|(i, s)| {
                s.parked.clone().map(|reason| Stalled {
                    strand: StrandId(i as u32),
                    reason,
                    backtrace: s.park_bt.as_ref().map(|bt| bt.to_string()),
                })
            })
            .collect()
    }
}

/// The least-loaded worker (fewest outstanding commands); ties go to the lowest
/// index. `worker_load` is never empty (at least one worker is spawned).
fn least_loaded(worker_load: &[u32]) -> usize {
    worker_load
        .iter()
        .enumerate()
        .min_by_key(|&(_, &load)| load)
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// A worker: own a set of coroutines, and on each command build (`Start`) or resume
/// (`Resume`) one, running it on its real stack until it parks or finishes, and
/// report back. The coroutine never leaves this thread (it is `!Send`). Exits on
/// `Shutdown` or when the command channel closes.
fn worker_loop(idx: usize, rx: &mpsc::Receiver<Command>, result_tx: &UnboundedSender<WorkerMsg>) {
    let mut coros: HashMap<StrandId, Coro> = HashMap::new();
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Command::Shutdown => return,
            Command::Start { id, body } => {
                let mut coro = Coroutine::new(move |yielder: &Yielder<(), Yielded>, _: ()| {
                    put_current(id, yielder as *const _);
                    body()
                });
                drive(idx, id, &mut coro, result_tx);
                if !coro.done() {
                    coros.insert(id, coro);
                }
            }
            Command::Resume { id } => {
                let mut coro = coros.remove(&id).expect("Resume of an owned coroutine");
                drive(idx, id, &mut coro, result_tx);
                if !coro.done() {
                    coros.insert(id, coro);
                }
            }
        }
        clear_current();
    }
}

/// Resume one coroutine and report what it did (parked with reason+stack, or done).
fn drive(idx: usize, id: StrandId, coro: &mut Coro, result_tx: &UnboundedSender<WorkerMsg>) {
    let msg = match coro.resume(()) {
        CoroutineResult::Yield((reason, backtrace)) => WorkerMsg::Parked {
            id,
            worker: idx,
            reason,
            backtrace,
        },
        CoroutineResult::Return(outcome) => WorkerMsg::Done {
            id,
            worker: idx,
            outcome,
        },
    };
    let _ = result_tx.send(msg);
}

thread_local! {
    /// The strand the current worker is running, plus a pointer to its coroutine's
    /// yielder, so a park-primitive deep in the body can suspend without the body
    /// having to thread a yielder argument through every call. Valid only while a
    /// coroutine body is executing on this thread.
    static CURRENT: RefCell<Option<CurCtx>> = const { RefCell::new(None) };
}

struct CurCtx {
    id: StrandId,
    yielder: *const Yielder<(), Yielded>,
}

fn put_current(id: StrandId, yielder: *const Yielder<(), Yielded>) {
    CURRENT.with(|c| *c.borrow_mut() = Some(CurCtx { id, yielder }));
}

fn clear_current() {
    CURRENT.with(|c| *c.borrow_mut() = None);
}

/// The current strand's id + yielder pointer. Panics if called outside a strand
/// body (a park must happen on a worker, inside a running coroutine).
fn current() -> (StrandId, *const Yielder<(), Yielded>) {
    CURRENT.with(|c| {
        let b = c.borrow();
        let ctx = b.as_ref().expect("park outside a strand body");
        (ctx.id, ctx.yielder)
    })
}

/// Park the current strand with a typed reason: capture its stack, suspend the
/// coroutine (the worker frees its permit boss-side), and — on resume —
/// re-establish the thread-local so a later park in the same body still finds its
/// yielder. The backtrace is captured frames-only here; symbols resolve lazily in
/// the stall report. (Capture is unconditional by design — observability is the
/// point; gate it if it ever shows up hot.)
fn park(reason: ParkReason) {
    let (id, y) = current();
    let backtrace = Backtrace::force_capture();
    // SAFETY: `y` came from the yielder borrowed by *this* coroutine's body, which
    // is valid for the whole body execution; we deref it only here, on the worker
    // thread that is running this very body, and the coroutine never migrates.
    let yielder = unsafe { &*y };
    yielder.suspend((reason, backtrace));
    // Resumed (same worker). The worker cleared CURRENT after the prior segment, so
    // restore it for any subsequent park in this body.
    put_current(id, y);
}

/// Park the current strand until `sig` fires: if already fired, return immediately;
/// else register as a waiter and record the reason (`AwaitingPart`), then suspend.
/// The fired-check and waiter-registration are atomic under the signal lock, so a
/// concurrent `fire` either drains this waiter or is seen as fired.
pub fn park_on(sig: Signal, producer: StrandId, part: &str) {
    let reason = {
        let mut st = sig.0.lock().unwrap();
        if st.fired {
            return;
        }
        let (id, _) = current();
        st.waiters.push(id);
        ParkReason::AwaitingPart {
            producer,
            part: part.to_string(),
        }
    }; // lock released before suspending
    park(reason);
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
        let bomb = s.spawn(R0, || Outcome::Poisoned("💣 forced".into()));
        let live = s.spawn(R0, || Outcome::Clean);
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
        let id = s.spawn(R0, || Outcome::Clean);
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
    /// concurrency stayed 2 — under real parallelism.
    #[test]
    fn i2_i5_capacity_and_progress() {
        let s = Scheduler::new(&[2]);
        let never = s.signal(); // never fired
        let mut stuck = Vec::new();
        let mut runnable = Vec::new();
        for _ in 0..8 {
            let sig = never.clone();
            let id = s.spawn(R0, move || {
                park_on(sig, StrandId(u32::MAX), "never");
                Outcome::Clean
            });
            stuck.push(id);
        }
        for _ in 0..8 {
            let id = s.spawn(R0, || Outcome::Clean);
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
                    s.spawn(R0, move || {
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

    /// I7 — No silent blocking: every parked strand has a typed reason **and** a
    /// captured stack, and the stall report names them.
    #[test]
    fn i7_stall_report_explains_the_block() {
        let s = Scheduler::new(&[2]);
        let never = s.signal();
        let stuck: Vec<_> = (0..3)
            .map(|i| {
                let sig = never.clone();
                let part = format!("part{i}");
                s.spawn(R0, move || {
                    park_on(sig, StrandId(99), &part);
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
                .all(|st| matches!(st.reason, ParkReason::AwaitingPart { .. })),
            "no strand parks without a typed reason: {report:?}"
        );
        assert!(
            report.iter().all(|st| st.backtrace.is_some()),
            "the stall report carries each parked strand's stack"
        );
    }

    /// A parked strand resumes when its signal fires (and re-acquires a permit) —
    /// the coroutine continues on its home worker from exactly where it suspended.
    #[test]
    fn parked_strand_resumes_on_fire() {
        let s = Scheduler::new(&[1]);
        let sig = s.signal();
        let id = {
            let sig = sig.clone();
            s.spawn(R0, move || {
                park_on(sig, StrandId(0), "rmeta");
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
    /// `Barrier(par)` — which releases only if all `par` are running *simultaneously*
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
                    s.spawn(R0, move || {
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
