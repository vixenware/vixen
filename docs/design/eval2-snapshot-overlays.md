# Eval2 snapshot overlays and centralized commit

## Status

This note records the next scheduler/runtime direction for eval2.

It follows from two things we now know concretely:

- the evaluator wants one unified execution model for serial and parallel work
- the current `Arc<BTreeMap<...>>` worker path is paying the wrong costs

Recent profiling showed tiny authored computations spending most of their time
in scheduler bookkeeping:

- `Arc::make_mut`
- `BTreeMap::clone_subtree`
- corresponding drop/free churn

That is a design problem, not a semver problem.

## Problem

Today prepared work is conceptually "snapshot-like", but the implementation is
still half mutable:

1. the session clones an `EvalScheduler`
2. workers share scheduler state through `Arc<...>`
3. worker-side mutations fall back to `Arc::make_mut`
4. the worker clone is dropped after producing a completion

In the hot path this is not acting like cheap copy-on-write. It is acting like:

1. share large maps
2. clone the large maps on first mutation
3. drop the clones
4. repeat per work item

That defeats the point of the scheduler's delta/lease architecture.

## Goal

Eval2 should use one execution model:

1. prepared work runs against an immutable base snapshot
2. worker-local mutations go to overlays / deltas
3. only the owning session commits deltas into the base state
4. serial execution is just the one-worker case of that model

This keeps:

- provenance and graph snapshots coherent
- completion ordering centralized
- parallel execution possible
- serial execution cheap

without maintaining separate serial and parallel engines.

## Core model

### Base snapshot

The session owns the current scheduler base state.

Workers may read from that base state, but they do not mutate it directly.

### Worker overlay

Each prepared work item gets an initially empty overlay that records:

- new node records
- node-state updates
- new dependency edges
- queued work
- pending registrations
- keyed-node changes
- retained expression tasks
- learned no-goods

The exact storage can vary. The important part is that these are worker-owned
changes, not worker-owned replacements for the whole scheduler.

### Completion

When work finishes, the worker returns:

- outcome
- trace/events
- call-profile data
- scheduler delta

If work poisons or is otherwise rejected, the delta is discarded.

### Commit

The session remains the single writer for base scheduler state.

Committing a completion means:

1. validate ordering / lease conditions
2. merge the delta into the base scheduler
3. update queues, completed-work records, and current resolution state

## Serial and parallel are the same model

This note explicitly rejects a separate serial execution path.

Instead:

- serial execution prepares one work item
- that work item still receives a base snapshot plus empty overlay
- its completion still commits through the same owner-side merge path

The one-worker case should differ only in:

- no concurrent sibling workers
- ordinary node/work IDs can stay unscoped

not in evaluator semantics or scheduler mutation rules.

## Immediate implementation target

The first slice does not need to rebuild the whole scheduler.

It should:

1. make every prepared work use a lease namespace, including the one-worker case
2. preserve ordinary node/work IDs for the serial one-worker case
3. route hot worker mutations away from `Arc::make_mut` on shared maps

The hot fields proven by profiling are:

- node records
- node states
- dependency edges

Those should move to overlay writes first.

## Data-structure direction

Longer term, scheduler storage should be chosen by access pattern, not by
convenience.

### Likely good fits

- node records / node states:
  - slab or indexed-vector style storage
- dependency edges:
  - append-only `Vec`
- environments/scopes:
  - persistent map semantics are useful here

### `im`

`im` is a plausible fit for language environments, where cheap copies and local
shadowing are the point.

It is not, by itself, the scheduler solution.

The scheduler problem is not just "use a more persistent map". The scheduler
needs:

- immutable base snapshots
- worker-local overlays
- centralized commit
- rejection without publishing worker-local mutation

Those are architectural properties, not a crate choice.

## Longer-term ideal

The ideal eval2 scheduler would make the model explicit:

1. snapshot id / generation
2. immutable base scheduler state
3. worker-local delta
4. centralized validation + commit
5. serial as the one-worker case

That is the architecture we should now grow into, instead of extending the
current worker-side `Arc::make_mut` compromise.
