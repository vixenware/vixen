# The vix lowering — machine slice 3+ (the arc's constitution)

*How vix lowers onto weavy::task. Governed by A5/A6 and the tooth-2
ABI; written before implementation so the shape can't wobble. The
acceptance is already pinned: merge-demand.vix's absolute corpus
contract — the machine runs it, left.o is ABSENT from the trace, both
lanes. Amos's line, never re-blurred: demand is vix's; fable is
imperative; never-started belongs to the demand graph, started-but-
blocked belongs to weavy::task.*

## The three spawn-suppression mechanisms (stacked, in order)

1. **Memo hit** — computed before: demand reaches a Call node, the
   closure-hash × args key hits the memo store, the node resolves with
   NO task, NO frame, nothing. (Warm daemon: most of the graph.)
2. **Undemanded** — never asked: a node no selection reaches is pure
   data forever. No task exists to suppress.
3. **Parked** — asked and waiting: only THIS mechanism costs a frame,
   and the frame chain in the arena IS the whole cost.

## Node → task

- A vix Call node (memo boundary) lowers to ONE weavy::task Fn: frame
  layout = declared record of (args, locals, spills) via mem::declared;
  body = the fn's typed ops (instruction selection from the checked
  types — vix's checker work rides the same discipline as fable's).
- The lowering is CACHED content-addressed (canonical hash → compiled
  Fn + frame layout), per the lowering-cache-is-primary ruling. The
  machine driver instantiates tasks from cached lowerings.
- Spawn happens ONLY when demand reaches the node AND the memo misses.
  Spawn = allocate entry frame, write args frame-direct (arg values
  are handles/scalars — see Values below), enter via the driver.

## Demand edge → await input

- A node body that needs another node's value awaits an INPUT SLOT;
  the machine driver owns per-task readiness/value arrays (exactly
  TaskExec's shape — the Advance seam holds either lane).
- When node B completes, the driver fills B's slot in every parked
  task awaiting it and wakes them. One completion, many resumptions —
  the demand graph's sharing, unchanged.
- JOINT NEED at memo boundaries is a SCHEDULER policy: when a body's
  input set is statically known, the driver demands the batch before
  entering, so bodies don't start-park-start-park. Policy about when
  to begin; never spawning undemanded work.
- Exec/fetch/solve runs stay DRIVER-SIDE primitives (runs registry,
  journal, two-tier cache unchanged); a run's completion feeds await
  slots exactly like a node's — the primitive reentrancy rule (V2)
  holds: primitives speak to the machine only through demanded slots.

## Values across node boundaries

- Inside a frame: untagged bytes per the tooth-2 ABI.
- ACROSS nodes: results are content-addressed store entries. A
  completed task's Ret bytes become the node's value under the node's
  result layout; an await delivers either an unboxed scalar (one word)
  or a HANDLE to a store entry (also one word). Composites never copy
  across node boundaries by value — sharing is identity in the store,
  which is also what memo/hashing/shipping already key on.
- TotalF64 canonicalization is applied at the language boundary (value
  creation), never inside machine arithmetic (IEEE there, bitwise-
  pinned across lanes).

## Trace (the unified stream)

- The vix lowering EMITS instrumentation ops carrying the three-moment
  vocabulary: node-demanded, memo-hit/miss, spawn, park, resume,
  finish — static ids mapping to nodes/spans via the lowering's
  tables. Innards mode = full story (tests, wtf, playground lanes);
  Production strips to structural suspension events. Tests are trace
  assertions; ABSENCE is the laziness proof.

## The driver (the demand scheduler)

- Owns: the demand worklist, the memo store, the journal, the task
  set (running/parked), the readiness plumbing, the store.
- Single-threaded first, written work-queue-shaped: workers/stealing
  arrive by sharing the queue (legal: canonical order makes schedules
  unobservable). Persistence only at content-addressed boundaries:
  memo, journal, lowerings. Arena and worklists die with the eval.

## Order of implementation

1. Driver skeleton + node vocabulary over cached lowerings; scalars
   only. DONE (vix::machine::driver, d47d31cf5): the three
   suppression mechanisms trace-asserted; INVOKE protocol (memo
   boundary = HostCall+Await; hit = sync fill, miss = spawn+park).
   CORRECTION from recon: eval.vix is NOT an arithmetic core — it is
   the self-hosting shape (recursive enums, strings, Map, match) and
   belongs to slice 2/3. Slice 1's corpus program is a NEW minimal
   sample (scalar fns, lets, arithmetic, recursive memo-boundary
   calls — fib in vix) with its own pinned contract; the AST lowering
   (machine/lower.rs) targets it first.
2. Store + handles; composite results; types.vix.
3. Exec seam as await-fed runs; merge-demand.vix — THE MILESTONE:
   left.o absent, both lanes, trace-asserted.
4. Corpus parity → delete the frozen evaluators (A5's funeral clause).
