# reference harness

Status: planning draft.

This note describes the reference oracle for
[`slice0.md`](/Users/amos/bearcove/vixen/docs/design/canonical-examples.md),
[`slice0-abi.md`](/Users/amos/bearcove/vixen/docs/design/runtime-abi.md),
[`slice0-observations.md`](/Users/amos/bearcove/vixen/docs/design/observations.md),
and
[`slice0-lowering.md`](/Users/amos/bearcove/vixen/docs/design/compiler-pipeline.md).

In implementation discussion, "reference harness" is the clearer name.

Whatever we call it, this component is for semantic result + trace checks.
It is not the frontend boundary for lowering.

The oracle is not `eval2` in miniature.

It is a tiny semantic reference model for the slice-0 subset.

## Purpose

The oracle exists to answer two questions:

1. What is the correct slice-0 result?
2. What semantic work was or was not demanded to get that result?

So the oracle should validate:

- final result
- observations applied to each node
- which nodes were entered
- which nodes were not entered
- whether sharing reused one node instead of recomputing it

That is more useful than "did the program eventually produce the same number?"

## What the oracle is not

The oracle should not be:

- based on `eval2` internals
- built on snapshot/delta owner/worker machinery
- used as a performance baseline
- treated as the final runtime object model
- treated as the compiler input language for `eval3` lowering

It is a correctness model for one tiny semantic slice.

## Why not eval2

`eval2` already contains the exact baggage slice 0 is trying to avoid:

- universal-ish runtime values
- heavy runtime objects
- scheduler/admin machinery
- bytecode-era execution assumptions

If the oracle reuses those semantics directly, it becomes much harder to tell
whether the compiled slice-0 path is actually faithful to the new model or just
"close enough to eval2".

The oracle should instead directly implement the slice-0 notes.

## Recommended implementation form

The oracle should be executable Rust code, but intentionally simple.

The right shape is probably:

- one small Rust module or crate dedicated to reference-harness execution
- handwritten AST for the slice-0 subset
- handwritten typed observation model
- handwritten node store / environment store
- deterministic trace output for tests

That is acceptable only as reference-harness machinery.
The real `eval3` compiler path should still start from the actual Vixen
frontend.

It is fine if the oracle internally uses Rust enums and maps.

That is not a contradiction.
The oracle is a reference model, not the target runtime representation.

Its job is to model semantics clearly, not prove low-level layout choices.

## Semantic subset

The oracle should implement exactly the subset frozen in
[`slice0.md`](/Users/amos/bearcove/vixen/docs/design/canonical-examples.md#L47):

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

Nothing else.

If a later note wants arrays, host resources, or keyed nodes, that is a later
oracle extension or a different oracle.

## Suggested data model

The oracle should separate:

- source expressions
- runtime refs
- runtime nodes
- observations
- trace events

### Expressions

Use a tiny AST for the slice-0 subset.

Enough variants for:

- literal
- local
- tuple / struct
- projection
- `if`
- lambda
- call
- `let`
- primitive add
- primitive trap

This AST is not the compiled form.
It is just the easiest way to state the slice-0 programs.

### Runtime refs

The oracle should model the semantics of `Ref<T>` even if it uses erased Rust
internals to do so.

Conceptually, refs should distinguish:

- ready scalar
- node ref
- product ref
- callable ref

It is acceptable for the oracle to represent these as a small Rust enum.

### Runtime nodes

The oracle needs only a tiny node set:

- thunk node
- product object
- closure object
- environment object

Each thunk node should record at least:

- source/strict-fragment identity
- current state
- strongest satisfied observation so far
- cached result
- enter count

### Observations

Use the exact slice-0 observation model from
[`slice0-observations.md`](/Users/amos/bearcove/vixen/docs/design/observations.md):

- `NoObs`
- `NeedValue`
- `NeedCallable`
- `NeedProduct { field -> child_obs }`

The oracle should actually implement observation join and comparison, not just
simulate them informally.

### Trace events

This is the whole point.

The oracle trace should record events like:

- node allocated
- node entered under observation `o`
- node reused under observation `o`
- observation widened from `o1` to `o2`
- product field selected
- closure allocated
- trap entered

If the trace is poor, the oracle is much less useful.

## Execution model

The oracle should evaluate by observation, not by eager whole-value reduction.

Core operation:

- `observe(ref, obs) -> observed_result`

Where `observed_result` is enough to continue evaluation under the requested
observation.

### Scalar behavior

- ready scalar under `NeedValue` returns immediately
- scalar thunk under `NeedValue` enters once, caches, and reuses

### Product behavior

- product shell can exist without entering all fields
- selecting one field only observes that field's ref
- sibling fields remain untouched unless observed

### Callable behavior

- observing a function item or closure under `NeedCallable` produces callable
  identity
- calling it does not eagerly force all arguments
- arguments stay lazy refs inside the call body

### Sharing behavior

If one binding is used twice, the oracle should use one shared node identity for
that binding when the slice-0 rules say it is shared.

The trace should make this visible:

- one node id
- one enter
- later reuse

## Success criteria for the oracle itself

The oracle is useful if it can run the four canonical slice-0 programs from
[`slice0.md`](/Users/amos/bearcove/vixen/docs/design/canonical-examples.md#L295) and report:

### Program A

- final result is `42`
- `bomb()` node exists but is never entered

### Program B

- final result is `n + 2`
- closure env is allocated
- only the demanded sibling call result is entered

### Program C

- final result is `(n + 1) + (n + 1)`
- the shared `plus_one(n)` node is entered once
- second use is recorded as reuse

### Program D

- final result is `n + 41`
- callable observation and higher-order call both work
- no generic "eval everything" fallback path appears in the trace

## Suggested API

The oracle does not need a large API.

A minimal useful API would be:

- construct one canonical program AST
- run `observe_main(obs)`
- return:
  - final observed result
  - trace
  - node stats

Conceptually:

```text
run(program, obs) -> OracleRun {
  result,
  trace,
  stats,
}
```

Where `stats` can include:

- node enter counts
- node reuse counts
- maximum observation reached per node

## Relationship to the compiled path

Later, when the first slice-0 compiled path exists, the oracle becomes the
differential reference.

That comparison should check at least:

- same final result
- same demanded observations on the named semantic nodes
- same entered vs unentered node behavior
- same sharing/reuse story

The compiled path does not need to produce byte-for-byte identical trace
formatting, but it should expose enough semantic events to compare.

## What the oracle deliberately does not prove

The oracle does not prove:

- the final object layouts
- the final `Ref<T>` bit encoding
- the final ABI calling convention
- Kajit lowering correctness
- runtime performance

Those belong to other artifacts.

The oracle proves:

- the slice-0 semantics themselves are pinned down enough to test

## Deliverable recommendation

The implementation should probably come in two pieces:

1. This note.
2. One tiny executable Rust reference model with unit tests for the four
   canonical programs.

That is enough to make slice 0 falsifiable instead of aspirational.
