# testing strategy

Status: active design note.

This note defines what the evaluator must prove, how those proofs should be
tested, and how the test layers fit together.

The goal is not "the JIT returns the same scalar as the interpreter on a toy
input".

The goal is to prove that Vixen's demand-driven semantics survive through one
runtime model, with JIT-compiled strict fragments as an execution strategy
inside that model.

## What the evaluator must prove

Every evaluator change should make these statements clearer, not weaker:

- authored Vixen remains demand-driven
- observation drives execution
- dead branches stay dead
- local sharing reuses one lazy identity instead of recomputing silently
- closures, captures, and higher-order code are ordinary semantics
- build-shaped boundaries like `discover`, `exec`, observers, sandboxing, and
  verification remain explicit semantic boundaries
- JIT execution is faithful to the same semantics as the non-JIT path

That means the test regimen has to check more than final output.

## Primary proof obligations

The evaluator should be tested against five kinds of truth:

1. Final result
2. Entered and unentered work
3. Observation propagation
4. Sharing and reuse
5. Boundary behavior

In plain language:

- did the program produce the right result
- did demand reach only the intended cone
- did sibling lazy work stay latent
- did shared work enter once and then reuse
- did host/build boundaries keep the right obligations and checks attached

## Test layers

The regimen should be layered on purpose.

### 1. Reference-harness semantics

The reference harness is the semantic reference for small canonical examples.

Its job is to check:

- final result
- demanded observations
- node enters and non-enters
- sharing and reuse behavior
- semantic trace events

It is not the compiler frontend and it is not the target runtime object model.

### 2. Analysis-structure tests

Before lowering, the evaluator needs structural tests over the analysis result.

These tests should check things like:

- which observations are demanded at each site
- which regions stay local strict work
- which expressions become runtime nodes
- why each extracted node exists
- closure/environment layouts
- strict-fragment entrypoints

### 3. Runtime semantic tests

The demand-driven runtime should be tested directly for:

- local thunk entry/update/reuse
- lazy product behavior
- closure/environment behavior
- suspension and resumption, when present
- explicit boundary ownership for `discover`, `exec`, observers, and related
  runtime services

### 4. JIT parity tests

The JIT path should be tested against the same evaluator semantics, not as a
separate language implementation.

These tests should check:

- same final result as the non-JIT path
- same entered vs unentered work on named semantic nodes
- same sharing/reuse story
- same explicit failures for unsupported constructs

Byte-for-byte trace identity is not required, but semantic equivalence is.

### 5. Real zoo program tests

The long-term target is not Programs A-D forever. It is real authored `zoo`
code, especially build-shaped programs.

That layer should eventually include:

- one honest build-shaped `zoo` program
- explicit `discover` / `exec` / observer pressure
- sandboxing and verification behavior
- observed-value and proof/obligation flow

This is the layer that proves the architecture against the language we actually
intend to ship.

## Expected semantic event vocabulary

The exact trace format can evolve, but the semantic event vocabulary should stay
recognizable.

Useful events include:

- node allocated
- node entered
- node reused
- observation widened
- product field projected
- environment allocated
- closure allocated
- boundary entered
- trap entered

The important thing is that tests can state both events and non-events.

## Current state

Today the repo already has parts of this regimen, but not the whole shape in one
place.

What exists:

- reference-harness planning for result + trace checks
- canonical slice examples
- runtime tests
- JIT lowering/codegen tests
- interpreter-vs-JIT parity tests

What is still missing or incomplete:

- broader structural tests over analysis output
- one clear semantic-trace contract carried across runtime and JIT paths
- build-shaped `zoo` targets that exercise boundary semantics honestly

## Review rule

When reviewing evaluator work, ask:

- what semantic claim does this test prove
- is it checking more than final output
- does it prove demand-driven behavior or only eager equivalence
- does it exercise one runtime model, or accidentally reward a split path
- does it move us closer to real `zoo` coverage

If a test only proves "the fast path returned the same scalar", it is useful but
not sufficient.
