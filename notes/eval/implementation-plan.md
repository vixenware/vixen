# implementation plan

Status: active roadmap note.

This note exists so evaluator work survives context loss and does not drift back
into toy lowering or backend-first thinking.

## Plain-language problem

The evaluator is not mainly a codegen problem.

It is mainly an analysis problem:

- start from real Vixen frontend output
- decide what stays runtime-owned demand-driven state
- decide what becomes one strict local region
- only then lower those strict fragments toward JIT execution

The key decision for every authored expression is:

- does this become part of a strict fragment, or
- does this become a runtime node or boundary with explicit identity?

That decision depends on:

- demanded observation
- strictness under that observation
- sharing
- escape
- closure capture and environment layout
- host/runtime boundaries
- later, keyed/shared-search boundaries

If the work is not attacking those decisions, it is probably not moving the
evaluator forward.

## Current diagnosis

Two things are true at once:

1. Throwing away old prototype seams was correct.
2. The real missing centerpiece is still analysis over real authored Vixen.

The reference harness is useful, but it is not the frontend seam and it is not
the implementation substrate to keep growing forever.

Why:

- the reference harness is a tiny semantic model for canonical examples
- it is not the real frontend
- the hard part is analysis over real authored Vixen
- lowering the harness model skips that hard part

## Terms

Use these terms consistently.

### Frontend

The real parser, resolver, and type information already in-tree.

This is the input side the evaluator must reuse.

### Reference harness

The small semantic model for canonical examples.

Its job is:

- run tiny fixtures
- produce result + trace
- check semantic invariants

Its job is not:

- be the frontend for lowering
- become a second language implementation we optimize

### Analysis plan

The first explicit evaluator artifact that matters.

It should answer, for a fixed frontend-derived program slice:

- what is demanded
- what is local strict work
- what is shared
- what escapes
- what becomes a runtime node
- what strict-fragment entrypoints must exist
- what closure/environment/frame layouts are needed

### Internal execution layer

The notes currently call this "lazy core".

Whether that name survives or not, the idea is:

- analysis decisions have already been made
- the strict-fragment/runtime split is explicit
- lowering should not need to rediscover semantics

## What to keep

Keep these:

- the evaluator note set under `notes/eval/`
- the canonical small examples
- the semantic expectations attached to those examples
- the demand-driven runtime model
- JIT as an execution strategy for strict fragments

Those semantic expectations mean:

- expected result
- expected enters / non-enters
- expected sharing / reuse behavior
- expected boundary behavior where relevant

It does not mean every old prototype crate or trace format has to survive.

## What to stop doing

Stop doing these:

- treating the reference harness as the source language for the compiler
- backend-first design that skips analysis
- preserving old prototype substrate just because it exists
- narrowing success to scalar examples that avoid real boundary semantics
- speaking as if JIT is a separate evaluator instead of one execution mode

## Testing story

The testing regimen is documented in
[`testing-strategy.md`](testing-strategy.md).

That document is part of the plan, not optional supporting material.

The short rule is:

- structural tests for analysis
- semantic result + trace tests through the reference harness
- direct runtime tests
- JIT parity tests
- real `zoo` targets that eventually cover boundary semantics honestly

## Implementation order

The next work should happen in this order.

### 1. Keep the testing contract explicit

Every implementation slice should state:

- input fixture
- demanded observation
- expected result
- expected semantic events and non-events
- expected runtime boundaries

The exact harness implementation can change. The semantic contract cannot.

### 2. Define the real frontend bridge

Write down the exact input artifact analysis consumes from the existing
frontend.

This bridge must come from real Vixen frontend structures, not the reference
harness AST.

For the first small slice, the bridge only needs enough information for:

- functions and closures
- bindings and uses
- call sites
- projection and product construction
- static types
- capture-set derivation

### 3. Define the analysis output

Before more lowering code, define one explicit analysis data structure.

It should include at least:

- demanded observation per use site
- extracted node set
- strict-fragment set
- strict-fragment parameters and result observation
- closure environment layouts
- thunk/frame layouts
- runtime-boundary markers

This is the missing centerpiece.

### 4. Lower the analysis result, not raw expressions

Only after the earlier steps should we lower into:

- the internal execution layer
- a JIT-facing strict-fragment IR

At that point, lowering should be mostly mechanical.

## First executable slice

The first end-to-end implementation target should prove one thing:

- analysis can carve a real authored body into local strict fragments plus
  explicit runtime-managed boundaries

That slice should stay small, but it needs:

- real frontend input
- real analysis output
- real strict-fragment execution without tiny-step runtime round-trips
- explicit boundaries for the things that must stay in the runtime

## Backend rule

The backend should not dictate the semantics.

The point of the slice is to prove the split:

- local regions run in one shot
- the runtime is not re-entered after every ordinary expression
- boundaries still return control explicitly

JIT execution belongs on top of that split, not in place of it.

## First slice contents

The first executable slice should cover:

1. lazy field selection
2. closure capture
3. local sharing
4. higher-order local helper call
5. one build-shaped "prepare args then `exec`" example

That last example matters because real `zoo` code does a lot of path
construction, array building, branching, and command assembly before reaching a
boundary.

## First slice deliverables

1. Frontend bridge for the required subset
2. Analysis result structs in Rust
3. Structural tests for the analysis result on the canonical examples
4. A strict-fragment IR for straight-line local regions
5. A runtime path for explicit boundary nodes
6. End-to-end tests that check both result and semantic trace

## Minimum runtime-owned set

For the first slice, the runtime should still own:

- shared lazy thunks
- closures and their environments
- lazy products where not all fields are demanded
- `exec`
- `discover`

Everything else should be pushed into strict fragments whenever analysis can
prove it is local and non-escaping.

## Next required slice: shared search

The first slice is deliberately small, but it must not paint us into a corner.

The next required workload after "local strict fragments plus build boundaries"
is shared search:

- keyed shared nodes
- join-on-collision for monotone requests
- deterministic search order
- structured conflicts
- reusable discovered facts

This is not a separate future project. It is the next semantic pressure on the
same architecture.

The implementation still must not hard-code Cargo-or-Rust semantics. It should
only know generic machinery for shared search.
