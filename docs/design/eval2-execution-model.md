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

These IDs are currently per-evaluation and in-memory only. They are not yet
cross-run identities.

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

This is still not a resumable scheduler. It is the smallest "scheduler-owned
pending node" shape that keeps the allocator and identity source inside the
evaluator rather than inside the runtime hook.

## What this does not do yet

This slice does not yet provide:

- a retained dependency graph
- explicit pending-node scheduling
- resumable continuations
- parallel work stealing
- reversible execution
- a stable on-disk trace format
- a debugger UI

It is still a recursive single-threaded evaluator with a structured event sink.
Some runtime calls can now surface `Pending`, but there is not yet a scheduler
that can later resume those pending nodes.

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

## Next slices

The likely follow-up steps are:

1. add explicit dependency-edge events
2. separate "runtime call requested" from "runtime result delivered"
3. introduce scheduler-owned pending nodes, even if the first scheduler stays
   single-threaded
4. define a replayable trace format for debugger/time-travel work

Only after those are in place should we spend time on debugger UX.
