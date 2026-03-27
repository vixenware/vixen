# implementation plan

Status: active restart note.

This note exists so `eval3` work survives context loss and does not drift back
into toy-lowering work.

## Plain-language problem

`eval3` is not mainly a codegen problem.

`eval3` is mainly an analysis problem:

- start from real Vixen frontend output
- decide what stays lazy runtime state
- decide what can be compiled as a straight-line kernel
- only then lower the chosen kernels toward Kajit

The key decision for every authored expression is:

- does this become part of a compiled kernel, or
- does this become a runtime node with lazy identity?

That decision depends on:

- demanded observation
- strictness under that observation
- sharing
- escape
- closure capture and environment layout
- host/runtime boundaries
- later, solver/shared-node boundaries

If the work is not attacking those decisions, it is probably not moving `eval3`
forward.

## Current diagnosis

Two things are true at once:

1. Removing canonical-pattern matching from `vx-eval3-core` was locally correct.
2. Building lowering around `vx-eval3-oracle::Program` was the wrong seam.

The existing `vx-eval3-core <- vx-eval3-oracle::Program` path is prototype code.
It is not the architecture we should keep building around.

Why:

- the oracle/reference harness is a tiny semantic model for slice-0 examples
- it is not the real Vixen frontend
- the hard part of `eval3` is analysis on real authored Vixen
- lowering the oracle model skips that hard part

## Terms

Use these terms consistently.

### Frontend

The real parser / resolver / type information already in-tree.

This is the input side that `eval3` must reuse.

### Reference harness

The thing currently called the "oracle".

Its job is:

- run tiny canonical examples
- produce result + trace
- check semantic invariants

Its job is not:

- be the frontend for lowering
- become a second language implementation we optimize

### Analysis plan

The first real `eval3` artifact we still need to define.

It should answer, for a fixed frontend-derived program slice:

- what is demanded
- what is local strict work
- what is shared
- what escapes
- what becomes a runtime node
- what kernel entrypoints must exist
- what closure/env/frame layouts are needed

### Lazy core

An internal IR after analysis decisions have already been made.

It is not where we discover laziness.
It is where we encode already-decided laziness explicitly enough to lower to
Kajit.

## What to keep

Keep these:

- the active design note set under `docs/design/`
- the four canonical slice-0 examples
- the semantic expectations attached to those examples

That last item means:

- expected result
- expected enters / non-enters
- expected sharing / reuse behavior

It does not mean we need to preserve the current harness or trace implementation.

## What to stop doing

Stop doing these:

- extending lowering from `vx-eval3-oracle::Program`
- treating the reference harness as the source language for the compiler
- spending effort on `vx-eval3-core` lowering coverage before the frontend
  analysis boundary is defined
- arguing about Kajit lowering details before the analysis result is explicit
- trying to preserve `vx-eval3-core` as the implementation substrate just
  because it already exists

## Working assumption

Assume `vx-eval3-core` is disposable.

The restart should keep:

- the canonical programs
- their semantic expectations as tests
- any tiny utilities that are genuinely reusable in isolation

But it should not optimize for continuity with the current core/lowering path.

The new implementation should start from:

- real frontend input
- explicit analysis output
- a fresh kernel/runtime split

## Four canonical examples

These are still worth keeping. They prove:

1. Program A: lazy field selection
   - observing `pair.0` does not enter `bomb()`
2. Program B: closure capture
   - closures have explicit environment identity
   - unused siblings stay unentered
3. Program C: local sharing
   - one shared local thunk enters once and is reused
4. Program D: higher-order local helper
   - calling through a closure value stays in the compiled model

These examples should survive as real fixtures and semantic trace checks, even
if the current oracle crate is later replaced or reduced.

## Immediate implementation direction

The next work should happen in this order.

### 1. Lock the testing story

Define the test contract in plain language:

- input fixture
- demanded observation
- expected result
- expected semantic events / non-events

The current useful event vocabulary is:

- node allocated
- node entered
- node reused
- observation widened
- product field projected
- env allocated
- closure allocated
- trap entered

The exact harness implementation does not need to survive. Only the fixtures and
their semantic expectations do.

### 2. Define the real frontend bridge

Write down the exact input artifact `eval3` analysis consumes from the existing
frontend.

This bridge must come from real Vixen frontend structures, not the oracle AST.

The bridge only needs enough information for slice-0 analysis:

- functions / closures
- bindings / local uses
- call sites
- projection / product construction
- static type information
- capture sets or enough info to derive them

### 3. Define the analysis output

Before more lowering code, define one explicit analysis data structure.

It should include at least:

- demanded observation per use site
- extracted node set
- compiled kernel set
- kernel parameters / result observation
- closure environment layouts
- thunk/frame layouts
- runtime-boundary markers

This is the real missing centerpiece.

### 4. Lower the analysis result, not raw expressions

Only after steps 1-3 should we lower into:

- slice-0 lazy core
- Kajit-facing HIR shape

At that point, lowering should be mostly mechanical.

## First executable slice

The first end-to-end implementation target should prove one thing:

- analysis can carve a real authored body into local kernels plus explicit
  runtime-managed boundaries

That slice should stay small.

It does not need:

- full package resolution
- a working implementation of keyed shared solver nodes
- remote execution
- native machine code immediately

It does need:

- real frontend input
- real analysis output
- real kernel execution without tiny-step runtime round-trips
- real explicit boundaries for the things that must stay in the runtime

### Kernel/backend rule

For the first executable slice, the backend does not need to be final.

A compact straight-line bytecode is acceptable as the first kernel format if it
proves the correct split:

- local regions run in one shot
- the runtime is not re-entered after every ordinary expression
- boundaries still return control explicitly

Kajit/native lowering can follow once that split is real.

### First slice contents

The first executable slice should cover:

1. lazy field selection
2. closure capture
3. local sharing
4. higher-order local helper call
5. one build-shaped "prepare args then `exec`" example

That last example matters because it proves the design against the real authored
pressure in `zoo/`: Vixen code does a lot of path construction, array building,
branching, and command assembly before reaching a boundary.

### First slice deliverables

1. Frontend bridge for the required subset
2. Analysis result structs in Rust
3. Structural tests for the analysis result on the canonical examples
4. A tiny kernel IR or bytecode for straight-line local regions
5. A runtime path for explicit boundary nodes
6. End-to-end tests that check both result and semantic trace

### Minimum runtime-owned node set

For the first slice, the runtime should still own:

- shared lazy thunks
- closures and their environments
- lazy products where not all fields are demanded
- `exec`
- `discover`

Everything else should be aggressively pushed into kernels whenever analysis can
prove it is local and non-escaping.

## Next required slice: shared search

The first slice is deliberately small, but it must not paint us into a corner.

The next required workload after "local kernels plus build boundaries" is
Cargo-style search:

- keyed shared nodes
- join-on-collision for monotone requests
- deterministic search order
- structured conflicts
- reusable discovered/package facts

This should be treated as the next semantic pressure on the architecture, not
as a separate future project.

### What stays the same

The core split should remain unchanged:

- local straight-line regions become kernels
- shared/effectful/global things remain runtime nodes

Cargo-style search does not replace that model. It extends it.

### What the runtime must add

For the search slice, the runtime must own a few more global services:

- keyed canonicalization
- joined monotone inputs
- branch/search state
- conflict capture and replay
- deterministic frontier scheduling

Rust still must not know Cargo semantics. It should only know the generic
machinery for shared search.

### What the Vixen program must own

The Vixen side must still define:

- request types
- join semantics
- candidate ordering / preference policy
- conflict shapes
- metadata change points

That is what keeps the runtime generic.

### Planning implication

The analysis artifact for slice 1 does not need to implement shared search yet,
but it must leave room for it explicitly.

In practice that means:

- the analysis model must already admit a distinct class of runtime-owned shared
  keyed nodes, even if slice 1 never constructs one yet
- runtime-boundary markers must be able to represent "shared keyed node", not
  just `exec`/`discover`
- kernel extraction must not assume every non-`exec` node is purely local
- closure/env/frame layout decisions must remain compatible with future keyed
  node bodies

### Search-specific executable target

After the first slice is working, the next executable target should be a
Cargo-lite search fixture that proves:

1. repeated package requests meet at one keyed node
2. colliding requests join instead of duplicating work
3. conflicts are structured values, not ad hoc errors
4. local candidate filtering / activation logic can still run inside kernels
5. the final answer remains deterministic

## Short-term code policy

Until the frontend bridge exists:

- `vx-eval3-oracle` is a reference harness, not the compiler frontend
- `vx-eval3-core` is prototype-only
- no new feature work should be added to the current oracle-AST lowering path
  unless it directly supports extracting the restart boundary

## Concrete next tasks

These are the next tasks that should survive agent/context churn.

1. Replace "oracle as lowering input" with "reference harness only" in docs.
2. Write the frontend-bridge note:
   - what existing frontend artifacts exist
   - what extra derived data `eval3` needs
   - what the bridge output should look like
3. Write the analysis-plan note:
   - inputs
   - outputs
   - invariants
   - how Programs A-D exercise it
4. Add one build-shaped example to the executable target:
   - command/path assembly before `exec`
   - explicit statement of what stays kernel-local vs what becomes a boundary
5. Implement a tiny kernel backend:
   - bytecode is acceptable first
   - no tiny-step runtime round-trips inside one kernel
6. Add the next executable target for shared search:
   - one keyed-node Cargo-lite fixture
   - joined requests
   - deterministic result + trace assertions
7. Start the fresh implementation from the frontend bridge and analysis result,
   reusing only the harness fixtures and any isolated utilities that are still
   clearly useful.

## Decision rule

When evaluating a proposed `eval3` task, ask:

"Does this make the frontend-to-analysis-to-kernel boundary clearer?"

If no, it is probably not the next task.
