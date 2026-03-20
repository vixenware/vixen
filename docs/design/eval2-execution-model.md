# Eval2 execution model: node IDs and step events

## Status

This is a design note for evaluator v2.

It records the next infrastructure direction for eval2 while the surrounding
language and runtime model are still moving:

- keep the first slice small
- improve the evaluator substrate before building debugger UI
- make future parallel execution and explanation tooling possible without
  rewriting the evaluator again

This document does not claim that eval2 already has a scheduler, a reversible
machine, or a full dependency graph. It records the minimum execution-model
shape we want to grow into.

## Problem

Today eval2 is still a direct recursive evaluator over the parsed AST.

That keeps the early fixture work simple, but it leaves four important
capabilities underpowered:

1. semantic stepping
2. post-hoc "why did this happen?" explanations
3. time-travel / replay-oriented debugging
4. internal parallel scheduling

A native Rust debugger can attach to eval2, but that is not the same as a
Vixen-aware debugger. The current engine does not expose:

- stable node IDs
- step boundaries
- explicit parent/child execution relationships
- a retained execution event log

If we keep adding semantics directly to the recursive evaluator without these
pieces, the later scheduler/debugger refactor gets more expensive.

## Goal

The immediate goal is not "build the debugger".

The immediate goal is to add enough execution-model structure that the same
substrate can later support:

- a debugger
- trace replay
- parallel scheduling
- explanation tooling
- solver / fixpoint style evaluation

The minimum useful slice is:

1. stable node IDs
2. emitted step events
3. explicit parent-child relationships in those events
4. trace preservation even when evaluation ends in poison
5. thread-ready interfaces for runtime calls and event delivery
6. explicit dependency edges and pending-node registration

## First slice

The first slice adds execution trace events to eval2.

### Stable node IDs

Every expression evaluation site gets a stable node ID for the duration of one
evaluation. This is enough to:

- correlate enter/exit pairs
- correlate runtime call phases
- attach later explanation data without relying on stack addresses or ad hoc
  logging

These IDs are currently in-memory only. Within one session they stay stable
across suspension and local resumption, but they are not yet durable cross-run
identities in the cache/debugger sense.

### Event stream

The engine emits structured events for:

- argument binding
- entering an expression
- exiting an expression with a ready value
- exiting an expression with poison
- starting a runtime call
- finishing a runtime call with a ready value
- finishing a runtime call with poison

Each event carries:

- work ID, if the event came from scheduler-owned session work
- node ID
- parent node ID, if any
- place
- optional source span

This is intentionally small. It is enough to observe the evaluator's current
shape without prematurely freezing a full scheduler protocol.

### Poison keeps its trace

Traced entrypoints must preserve the trace even when evaluation ends in poison.

That matters because "what just happened before this failed?" is one of the
main reasons to capture a semantic trace in the first place. If the trace is
dropped on failure, the interface is much less useful for debugging and
explanation.

### Thread-ready boundaries

Even before the scheduler is parallel, eval2 should avoid baking in
single-threaded interface assumptions.

So this slice also treats two boundaries as thread-ready from the start:

- runtime hooks are `Send + Sync`
- event sinks are `Send`

That does not make the evaluator internally parallel yet. It does mean:

- values, results, and traces must stay movable across threads
- runtime implementations must not rely on thread-local non-`Send` state
- debugger/trace tooling can consume events through message passing instead of
  sharing one in-thread buffer

The first concrete mechanism here is a channel-backed event sink. That gives us
one real message-passing path immediately, instead of postponing concurrency
pressure until after the evaluator API is already set in stone.

### Dependency edges

Parent-child nesting in the trace is useful, but it is not enough on its own to
answer "why did this parent block or rerun?"

So eval2 should also emit explicit dependency-edge events when one expression
demands another child expression. This is deliberately redundant with the raw
 call tree:

- the child node still emits its own enter/exit events
- the parent also emits `depend_on(child_id)`

That redundancy is useful for later explanation tooling, because "this node
depends on that node" should be queryable without reconstructing it from stack
shape alone.

### Pending nodes

`Resolution::Pending` already existed as a public result kind, but the engine
did not actually produce it for ordinary evaluation.

The next substrate step is to let runtime calls return:

- unsupported
- ready value
- pending description

The evaluator then:

- allocates the pending reference itself
- registers that pending node in the trace
- returns `Resolution::Pending(...)`

The next step beyond that is a resumable session with scheduler-owned state:

- the session owns pending completions
- the session owns the current node table
- the scheduler owns a ready queue, an in-flight table, and a blocked-pending
  table
- the ready queue can drive both a fresh entry evaluation and resumed work
- the session can expose one scheduler step at a time instead of only `run()`
- start-entry setup can queue ordinary expression work instead of evaluating the
  whole entry inline
- the session can retain queued, in-flight, and completed work records under
  stable work IDs
- resumed work records can point back to the expression node that is runnable
- node state can distinguish "blocked on pending" from "ready to resume"
- ordinary child expressions can also become queued work in session mode, with
  parent nodes blocked on child-node completion instead of always recursing
  inline
- some expression forms can now expose multiple runnable sibling children at
  once:
  - array items
  - call arguments
  - struct fields
  This is still driven by a single-threaded scheduler today, but it means the
  evaluator can already surface a real runnable frontier instead of discovering
  siblings only one at a time
- the session can retain node records and expose a graph snapshot over:
  - node record metadata
  - current node state
  - dependency edges
  - queued, in-flight, and completed work
  - current pending tokens
- the session can expose explicit checkpoints so an external debugger can own
  time-travel state instead of relying only on the session's private history
- the session can lease ready work exactly once from its current state, move it
  into an explicit in-flight state, execute it outside the session, and then
  apply the completion back later
- prepared sibling work items can coexist in-flight at the same time
- completions now merge back as deltas:
  - completed-work outcome
  - node-state updates
  - newly queued work
  - pending registrations
  - trace events
  instead of restoring a full session snapshot
- the session owns a compiled program object:
  - source text and source ID
  - line index
  - one parsed green tree plus handle indexes
  Prepared work and checkpoints share this program instead of cloning raw
  source text or reparsing on handle resolution
- completion ordering is explicit today: multiple sibling leases are allowed,
  out-of-order arrivals are buffered, and committed work still advances in
  ascending lease-sequence order
- lease identity must stay monotonic across `restore()` / `step_back()` so
  rewinding cannot make an old completion look current again
- completions can be rejected as stale if the leased work is no longer
  in-flight
- `apply_completion` is transactional: a failed delta merge must not leave the
  live session partially mutated
- a session with no ready work and no external pending nodes can still be in a
  valid waiting state if leased work remains in-flight, even when later
  completions have already been buffered behind an earlier missing lease
- suspension stores continuation frames for the active evaluation path
- resumption continues locally from those frames instead of rerunning the root

This is still not a full continuation machine or a parallel scheduler. It is
already enough to move pending-node identity, wakeup, and local progress into
the evaluator rather than the runtime hook.

## What this does not do yet

This slice does not yet provide:

- a retained dependency graph
- general runnable-node scheduling beyond continuation resumption
- parked stack frames for arbitrary evaluation sites
- parallel work stealing
- reversible execution
- a stable on-disk trace format
- a debugger UI

It is still a recursive single-threaded evaluator with a structured event sink.
Some runtime calls can now surface `Pending`, and a session can later resume
them through a scheduler-owned ready/pending queue and stored continuation
frames. The first ordinary queued work was the root entry body: `StartEntry`
now parses/binds and enqueues an `EvaluateExpr` task instead of running the
whole body inline. Session mode now also queues ordinary child expressions and
lets parent nodes block on child-node completion. But there is not yet a
general node scheduler that can park and resume arbitrary work units
independently of the recursive evaluator.

Prepared work is now a real lease, not just a read-only peek plus checkpoint
restore. Once leased, a work item leaves the ready queue and is visible as
in-flight until its completion is merged or the session is rewound. The merge
path is intentionally conservative today: deltas are applied back onto live
session state, buffered completions are committed only when they become the
earliest outstanding lease, and commit order remains deterministic. That keeps
sibling fan-out possible without claiming a fully commutative parallel
scheduler yet.

Buffered completions are part of the public session/debugger model now. A work
item can be:

- queued: not leased yet
- in-flight: leased and not yet returned
- buffered: returned but still waiting behind an earlier lease
- completed: committed into session state

That matters for observability. A buffered sibling is no longer an invisible
gap between "in-flight" and "completed"; it shows up in work records, graph
snapshots, and node explanation as explicitly buffered work.

The current frontier is deliberately uneven:

- block/let sequencing is still mostly continuation-driven
- array items can already become multiple queued sibling work items
- call arguments can already become multiple queued sibling work items
- struct fields can already become multiple queued sibling work items

So the scheduler can now observe a little real fan-out, even though the engine
is not yet a general parallel task VM.

## Why this is still useful

Even without a scheduler, this slice gives us three immediate benefits.

### 1. Better tests

Eval2 tests can assert not only final values but also evaluation behavior:

- which expression kinds were entered
- whether runtime calls happened
- whether failures were surfaced as poison at the expected node

### 2. Better explanations

Even before we have a full "why" graph, we can answer basic questions such as:

- which runtime call produced this value?
- where did the poisoned result arise?
- what expression shape did we step through first?

### 3. Cleaner path to scheduling

When we later add:

- pending nodes
- explicit dependency edges
- a scheduler

we can extend the existing event vocabulary instead of inventing a second
observability surface from scratch.

The current local-resume session is also useful in its own right:

- it proves that pending node identities are evaluator-owned
- it proves that results can move from pending to ready or poison
- it proves that earlier ready work is not replayed just because a later node
  suspended
- it proves that leased work is consumed exactly once and remains inspectable
  while it is in flight
- it gives a debugger/replay tool a step-by-step work log without scraping the
  raw trace stream
- it gives the session a small "why am I blocked?" substrate by retaining
  dependency edges and following blocked-on-child chains to their terminal node
- it can expose the current node/work graph directly instead of forcing a
  debugger to reconstruct it from trace events
- it can support backwards stepping by snapshotting scheduler-owned session
  state between steps, without claiming a full durable time-travel format yet
- it can hand those checkpoints to an external debugger, which keeps the
  door open for time-travel tooling outside the evaluator itself
- it can hand a prepared work item to a worker thread and apply the completion
  later without overwriting unrelated live scheduler state, which is the
  missing seam between "single-threaded scheduler today" and "real workers
  later"
- it can accept legitimate out-of-order sibling completions without exposing
  them as committed state before earlier leases land
- it now has a built-in bounded worker loop that can lease ready siblings,
  execute them on worker threads, and still commit their results in lease
  order
- it gives the debugger/explainer story a real notion of resumed execution
  before we commit to a full task VM

## Next slices

The likely follow-up steps are:

1. retain runnable node records instead of only suspension continuations
2. separate "runtime call requested" from "runtime result delivered"
3. make the ready queue handle more than resumed runtime-call suspensions
4. define a replayable trace format for debugger/time-travel work

Only after those are in place should we spend time on debugger UX.
