# Eval2 Roadmap: Real Program Evaluator First, Parallel Solver Later

This note exists to keep the current direction coherent.

There are three pressures on `eval2` at the same time:

1. we do not want evaluator number three later
2. we need useful results sooner rather than later
3. Cargo-class resolution must eventually live in Vixen code, not in Rust policy

So the roadmap cannot be:

1. keep rewriting the scheduler forever
2. ship build features by baking more domain semantics into Rust
3. postpone the real program-evaluator work until after all build plumbing exists

The goal is to freeze the right kernel shape, then spend effort on semantics
that unlock real builds while keeping a clean path to the Cargo-class parallel
solver described in
[parallel-solver.md](/Users/amos/bearcove/vixen/docs/design/parallel-solver.md).

## North Star

The end-state is still:

1. a generic justified demand-driven runtime
2. deterministic parallel search
3. domain logic written in Vixen, not in Rust
4. Cargo-like package resolution as one important instance

But the near-term business goal is not "Cargo solver first". The near-term goal
is:

1. build and execute real things correctly
2. with distributed evaluation
3. with reuse, verification, and debuggability
4. on a kernel that will not need to be thrown away when solver work starts

## Where Eval2 Stands Now

`eval2` is no longer just a recursive evaluator with a runtime hook.

It now has:

1. a session-owned scheduler
2. stable node IDs and work IDs
3. queued, in-flight, buffered, and completed work
4. dependency edges
5. resumable pending work
6. checkpoints and step-back
7. a compiled program object shared by session and prepared work
8. a built-in bounded parallel driver
9. transactional completion merge
10. structured metadata on values:
    - provenance
    - claims
    - reuse class
    - crossing policy
11. an inert branch-context seam reserved for later solver work
12. slot substrate experiments with stable slot identity and refresh intent

That is enough substrate to stop pretending the evaluator is still shape-less.

It is not yet the full solver runtime, but it is finally close to a kernel we
can build on.

## Phase 0: Finish And Freeze The Scheduler

This is the last phase where scheduler churn should dominate.

The purpose is not "perfect parallel runtime". The purpose is:

1. no major re-think of node/work/session shape later
2. parallel-ready execution is real, not theoretical
3. debugger and "why?" tooling have honest state to inspect

Remaining scheduler work should stay bounded to:

1. fix the remaining RPC/slot correctness gaps around reuse and verification
2. close any observability holes that still hide live work state
3. document the settled scheduler contract clearly enough that semantic work can stop depending on scheduler rewrites

We should stop infrastructure churn as soon as the following statement is true:

> ordinary evaluation, suspension, local resume, prepared work, buffered
> completion, bounded parallel execution, and slot-backed reuse all fit one
> documented session model, and new semantics no longer require changing that
> model's shape.

That is the point where `eval2` stops being "still under construction" and
starts being "the evaluator".

## Phase 1: Make Eval2 A Real Program Evaluator

This is now the most important generic language cut.

The current blocker is not parsing or lowering. `eval2` already parses authored
Vixen source. The blocker is that ordinary call expressions still route directly
into runtime hooks, which keeps too much semantic policy in Rust.

The next milestone is:

1. ordinary calls resolve to user-defined functions before falling back to runtime hooks
2. declarations without bodies remain runtime/builtin-backed
3. helper logic can start moving out of Rust builtins and into real Vixen code

### 1.1 Direct User-Defined Calls

Start with the smallest meaningful slice:

1. top-level functions defined in the same source file are callable from other functions
2. entry-function setup becomes the ordinary function-call path, not a special one-off mechanism
3. runtime hooks are only the fallback path for body-less declarations or unsupported builtins

Why first:

1. it removes one of the biggest excuses for stuffing policy into Rust
2. it gives `eval2` the basic shape of a real language evaluator again
3. it lets slot and resolver behavior migrate into authored Vixen code incrementally

### 1.2 Function Values And Closures

The old evaluator already had real function values, nested named functions,
closures with captures, and partial application.

`eval2` should recover that capability, not treat it as optional future sugar.

The first pass does not need the final execution IR, but the responsibility is
clear:

1. user-defined functions are semantic values
2. closures and captures belong in `eval2`, not in ad hoc runtime behavior
3. builtins remain a distinct path for declarations without bodies

### 1.3 Resolve Integration

`vx-resolve` already exists and already powers the LSP.

`eval2` should grow toward being a real program evaluator over the same language
model, not stay a one-off single-source interpreter forever.

The likely order is:

1. same-source direct calls first
2. then real integration with `vx-resolve` for callable definitions across the program graph
3. then stdlib execution through the same model, with builtin fallback only for declarations without bodies

This is not separate from semantic work. It is the generic substrate that keeps
semantic work from collapsing back into Rust builtins.

## Phase 2: Slots As Real Semantic Primitive

Slots remain the first persistence/reuse primitive, but the boundary is now
explicit:

1. Rust/runtime owns slot persistence and substrate
2. Vixen code owns domain-specific resolution behavior

A slot gives:

1. one stable slot identity
2. one current stored value
3. one witness for invalidation
4. optional claims / verifier material
5. one action:
   - `Current`
   - `Refresh(intent)`

The important rule is:

1. refresh intent is slot-domain-specific
2. refresh rewrites the same slot; it does not fork lineage
3. slots are generic substrate, not a place to hide domain policy in Rust

Why now:

1. it gives explicit persisted reuse
2. it turns "we can cache expressions" into something users can inspect and control
3. it is needed for lockfile-shaped workflows
4. it is needed for discovered toolchains and similar expensive facts
5. it gives the UI something concrete to show and invalidate

Deliverable:

1. explicit slot identity and stored-value model
2. witness invalidation
3. refresh/recompute action without changing slot identity
4. persistent reuse with revalidation where required
5. trace/explanation of populate / reuse / rewrite reasons

## Phase 3: Observation, Verification, And Exec Discipline

This is the money path for real builds.

### 3.1 Observation / Claims / Verification

Finish the observed-value story:

1. host observations produce values with claims
2. those claims propagate through pure computation
3. verification happens at reuse or use-site boundaries

Why:

1. it is required for trustworthy distributed builds
2. it is required for toolchain discovery
3. it is required for "same machine, same facts, reuse safely"

### 3.2 Exec Discipline And Grants

Make `exec` stop relying on ambient trust.

The important rule is:

1. anything available to the action is either:
   - through tracked/opaque VFS mounts
   - or through verified host grants
2. nothing else is reachable

Why:

1. it turns the current runtime into something principled
2. it gives a real answer to "why is this cacheable / repeatable?"
3. it is directly useful for shipping hermetic or semi-hermetic builds

### 3.3 Crossing Semantics

Make by-value / by-proxy / blocked a real part of the V2 model, not a pile of
exceptions.

Why:

1. distributed evaluation is already in scope
2. remote builds are part of the product path
3. this is foundational for later solver distribution too

## Phase 4: Prove That Resolver Policy Can Live In Vixen

Before touching real Cargo semantics, prove the direction on smaller authored
Vixen programs.

The point of this phase is not to build a Rust toy resolver. The point is to
prove that:

1. slot-backed current vs refresh behavior can be expressed in Vixen
2. helper logic can live in ordinary user-defined functions
3. runtime/builtin support stays generic and substrate-level

Good proving grounds are:

1. a pure slot consumer written in Vixen
   - witness changes invalidate
   - refresh forces recompute
2. an observed slot consumer written in Vixen
   - current reuses if claims verify
   - refresh forces rediscovery
3. a tiny authored Vixen resolver sketch
   - current build reuses the current chosen graph
   - refresh with a typed intent rewrites that same slot
   - no crates.io protocol and no full Cargo semantics yet

This phase exists specifically to avoid mistaking fixture metadata or Rust
helpers for real semantic progress.

## Phase 5: Solver-Compatible Semantics Primitives

This is where we stop just "doing builds" and make sure the language surface is
ready for the solver notes.

We do not implement Cargo resolution yet.
We do implement the primitives the solver note assumes.

### 5.1 Keyed Nodes

We need a language-level way to state:

1. this node has a canonical key
2. repeated demand for the same key joins into one node

This corresponds to `#[key]` in
[parallel-solver.md](/Users/amos/bearcove/vixen/docs/design/parallel-solver.md).

Eval2 now has a first provisional seam here:

1. `keyed(key, joined, resolver)` creates a canonical keyed node
2. repeated demand converges on that node inside one session
3. keyed nodes are visible in graph snapshots/debug state

That is useful, but it is not the end-state yet. In particular, dependent
invalidation after a keyed result has already been consumed still needs to be
made honest.

### 5.2 Joinable Inputs

We need monotone widening, not imperative fixpoint loops.

The rule from the solver note should hold:

1. join semantics belong to the value type
2. the runtime only needs to know which inputs are joinable

This corresponds to `#[join]` in
[parallel-solver.md](/Users/amos/bearcove/vixen/docs/design/parallel-solver.md).

The first implementation can be small:

1. keyed memoization
2. join-on-collision
3. re-evaluate affected nodes when joined inputs widen

The current MVP join surface is intentionally narrower than the target:

1. array inputs widen by ordered dedup-union
2. scalar inputs currently require equality
3. user-defined join semantics per value type are still ahead

That is already useful beyond Cargo:

1. minimal version selection style problems
2. feature unification
3. NixOS-style option merging

### 5.3 Structured Conflict Values

We need conflict as data, not just `fail "no"` as text.

Why:

1. explanations
2. replayable exclusions later
3. learning / widening later

This should arrive before real search.

### 5.4 Branch-Local Context

We need a way to evaluate the same keyed graph under branch-local assumptions
without turning those assumptions into global joined facts.

This is the most important solver primitive that is not already implied by the
current build/distributed work.

This likely corresponds to the "branch-local assumptions" section in
[parallel-solver.md](/Users/amos/bearcove/vixen/docs/design/parallel-solver.md).

`#[choose]` may still be sugar later, but branch context is the real primitive.

## Phase 6: Cargo Resolver In Vixen

Only after the primitives above exist should we attempt a Cargo-shaped
resolver.

The key requirement is now explicit:

1. Cargo resolution policy lives in authored Vixen code
2. Rust/runtime may provide generic substrate and leaf capabilities
3. Rust/runtime must not own Cargo-specific selection or update policy

At this phase, the goal is still not the full parallel solver from
[parallel-solver.md](/Users/amos/bearcove/vixen/docs/design/parallel-solver.md).
The goal is:

1. one slot for the current chosen lock / resolved graph
2. current-build action that reuses the slot when still valid
3. explicit refresh/update action that recomputes and replaces that same slot
4. update-one-package behavior modeled without inventing a second persistence primitive
5. enough real Vixen expression power that this logic is not smuggled back into Rust

This is the bridge between small authored resolver sketches and real Cargo
integration.

## Phase 7: Cargo Solver MVP

Only after the primitives above exist should we attempt a Cargo-like solver.

The MVP order should stay aligned with
[parallel-solver.md](/Users/amos/bearcove/vixen/docs/design/parallel-solver.md):

1. convergent demand only
2. structured conflicts
3. branch-local assumptions
4. exact search

This means:

1. start with keyed nodes and joinable facts
2. model conflicts precisely
3. branch on assumptions
4. find correct solutions before worrying about aggressive learning

This is enough to prove that the language/runtime model can host Cargo-like
resolution without hardcoding Cargo semantics in Rust.

## Phase 8: Full Parallel Cargo Solver

Only after the MVP works should we add the more advanced solver machinery:

1. learned point no-goods
2. range widening
3. metadata-aware splitting
4. optimistic bounds
5. incumbent tracking
6. parallel frontier search
7. diagnostic fallback search

That is where the research-heavy parts live.

They should build on the same kernel and the same semantic primitives, not
start a new evaluator.

## What Counts As Success Soon

If money and time pressure matter, the next meaningful wins are not "Cargo
solver complete".

They are:

1. `eval2` can execute real Vixen helper logic, not just runtime hooks
2. slots make expensive choices reusable and inspectable
3. observed toolchain / host facts are trusted and replayable
4. exec uses tracked mounts and verified grants instead of ambient trust
5. distributed evaluation and reuse feel coherent
6. the scheduler stops changing shape underneath that work

If we achieve those six things, Vixen becomes much more useful immediately,
and none of that work is wasted when solver semantics arrive.

## Guardrails

To keep `eval2` from drifting into evaluator number three, we should avoid:

1. adding build semantics that require reshaping node/work/session again
2. implementing Cargo resolution as a Rust-side imperative loop
3. mistaking fixture metadata or generated wrappers for real semantic progress
4. explaining fixpoint as "run until stable"
5. hiding important live scheduler state from observability
6. letting short-term build hacks define the long-term model

## Current Recommended Order

From here, the recommended order is:

1. finish and freeze the scheduler
2. make ordinary user-defined calls work in `eval2`
3. recover real function / closure semantics in `eval2`
4. integrate `vx-resolve` into execution-time callable resolution
5. finish slots as a real semantic primitive
6. finish observed values / claims / verification
7. tighten `exec` discipline and grant semantics
8. finish crossing semantics
9. prove slot-backed current/refresh behavior in authored Vixen code
10. add keyed-node and join primitives
11. add structured conflict values
12. add branch-local assumptions
13. build a Cargo-shaped resolver in Vixen on top of slots
14. build Cargo solver MVP
15. add full parallel solver optimizations

That is the route that best balances:

1. useful results soon
2. coherent design
3. no evaluator number three later
4. real Cargo semantics in Vixen rather than Rust
