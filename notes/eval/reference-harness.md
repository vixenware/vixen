# reference harness

Status: active design note.

This note describes the reference harness for the small canonical evaluator
examples.

Companion notes:

- [`canonical-examples.md`](canonical-examples.md)
- [`observations.md`](observations.md)
- [`runtime-abi.md`](runtime-abi.md)
- [`compiler-pipeline.md`](compiler-pipeline.md)
- [`testing-strategy.md`](testing-strategy.md)

The reference harness exists for semantic result + trace checks.
It is not the frontend boundary for lowering.

It is not a miniature copy of the old evaluator.
It is a small semantic model for the frozen canonical subset.

## Purpose

The reference harness exists to answer two questions:

1. What is the correct result?
2. What semantic work was or was not demanded to get that result?

So the reference harness should validate:

- final result
- observations applied to each node
- which nodes were entered
- which nodes were not entered
- whether sharing reused one node instead of recomputing it

That is more useful than "did the program eventually produce the same number?"

## What the reference harness is not

The reference harness should not be:

- based on old evaluator internals
- used as a performance baseline
- treated as the final runtime object model
- treated as the compiler input language for evaluator lowering

It is a correctness model for one small semantic slice.

## Recommended implementation form

The reference harness should be executable Rust code, but intentionally simple.

The right shape is roughly:

- one small Rust module or crate dedicated to reference-harness execution
- handwritten AST for the supported subset
- handwritten typed observation model
- handwritten node store / environment store
- deterministic trace output for tests

That is acceptable only as harness machinery.
The real compiler path should still start from the actual Vixen frontend.

## Semantic subset

The reference harness should implement exactly the subset frozen in
[`canonical-examples.md`](canonical-examples.md):

- `U64`
- `Bool`
- tuples
- simple structs
- function items
- closures with captured environment
- `let`
- `if`
- call
- projection

If a later note wants arrays, host resources, or keyed nodes, that is a later
reference-harness extension or a different harness.

## Suggested data model

The reference harness should separate:

- source expressions
- runtime refs
- runtime nodes
- observations
- trace events

Conceptually, refs should distinguish:

- ready scalar
- node ref
- product ref
- callable ref

The harness only needs a tiny node set:

- thunk node
- product object
- closure object
- environment object

## Observations

The harness should implement observation by semantics, not by eager whole-value
reduction.

The useful primitive is:

- `observe(ref, obs) -> observed_result`

That should be enough to express:

- ready scalar under `NeedValue` returns immediately
- scalar thunk under `NeedValue` enters once, caches, and reuses
- product shell can exist without entering all fields
- selecting one field only observes that field ref
- calling a function does not eagerly force all arguments

## Trace events

This is the whole point.

The harness trace should record events like:

- node allocated
- node entered under observation `o`
- node reused under observation `o`
- observation widened from `o1` to `o2`
- product field selected
- closure allocated
- environment allocated
- trap entered

If the trace is poor, the harness is much less useful.

## Success criteria

The harness is useful if it can run the canonical examples and report:

- final result
- demanded observations on named semantic nodes
- entered vs unentered behavior
- sharing and reuse behavior

That includes at least:

- Program A: selecting `pair.0` does not enter `bomb()`
- Program B: closure environment allocation is visible and only the demanded
  sibling call enters
- Program C: one shared local thunk enters once and later reuses
- Program D: higher-order local call works without an "evaluate everything"
  fallback

## Relationship to the compiled path

Once the compiled path exists, the reference harness becomes the differential
reference.

That comparison should check at least:

- same final result
- same demanded observations on named semantic nodes
- same entered vs unentered behavior
- same sharing/reuse story

The compiled path does not need byte-for-byte identical trace formatting, but it
should expose enough semantic events to compare.

## What the reference harness deliberately does not prove

The harness does not prove:

- final object layouts
- final `Ref<T>` bit encoding
- final ABI calling convention
- JIT lowering correctness
- runtime performance

It proves the semantic story for a small frozen subset.
