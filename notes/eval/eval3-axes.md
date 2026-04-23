# evaluator architectural axes

This document is not normative. It is a planning aid for evaluator work.

Normative authority remains:

- [core language semantics](/Users/amos/bearcove/vixen/docs/content/spec/language/core.md)
- [evaluation semantics](/Users/amos/bearcove/vixen/docs/content/spec/language/build.md)
- [internals and shipped-closure model](/Users/amos/bearcove/vixen/docs/content/spec/internals.md)

## Purpose

Evaluator work has many competing pressures:

- make real programs execute again
- keep the architecture lowerable later
- preserve provenance and explainability
- avoid cheating with semantic macros and source-shape recognition

This file names the main axes that need to stay true at the same time.

## Core axes

### 1. Demand-driven semantics

The evaluator is demand-driven, not step-driven.

That implies:

- pending values are first-class
- demand propagates through structure
- dead branches stay dead
- forcing happens only at real environment boundaries
- memoization is part of semantics, not a later optimization

### 2. Parallel-friendly runtime

The evaluator must be scheduler-friendly and resumable.

That implies:

- work lives in frames, cells, and explicit runtime waits
- suspension/resumption does not require re-entering from the top
- body calls and runtime boundaries can block and resume independently
- the architecture must remain compatible with multi-executor scheduling later

### 3. Daemon-owned execution

Execution should be owned by a long-lived daemon/runtime, not by a short-lived
CLI process.

### 4. Stable identity and provenance

The evaluator must preserve enough identity to explain what happened and why.

That implies:

- stable typed ids for bodies, expressions, strict fragments, runtime nodes,
  layouts, and slots
- source provenance and causal provenance both matter
- demand paths, carried obligations, and boundary facts must be attributable

### 5. Strict-fragment execution

Per-expression suspension is too expensive.

The evaluator should instead:

- execute straight-line strict fragments as compiled/interpreted regions
- keep boundaries explicit
- avoid both micro-node explosion and opaque "just run it" blobs

### 6. JIT-friendly plan shape

The evaluator must not block JIT execution later.

That implies:

- explicit static executable plans
- typed ops and typed slots
- suspension/resumption outside strict fragments
- no semantic dependence on interpreter-only shortcuts

JIT eligibility should be decided for strict fragments within one runtime path,
not by routing whole programs into a separate evaluator.

### 7. Honest shipped closures

Closures that cross `discover` or `exec` boundaries are real evaluator payloads.

### 8. Clean language / stdlib / runtime / platform split

These layers need to stay distinct:

- language core
- stdlib/build runtime surface
- evaluator analysis
- evaluator runtime
- platform seam

### 9. Boundary semantics stay explicit

`discover` and `exec` are major semantic boundaries, not conveniences.

That implies:

- `discover` is real host observation
- values flowing out of `discover` carry hidden verification obligations
- `exec` rechecks relevant obligations on the concrete runner
- observer execution is real evaluator work

### 10. Build-shaped values matter

`Tree`, `Blob`, `Bytes`, `Path`, `Process`, and `Host` need real runtime meaning.

### 11. Sandboxing and verification are semantic

Sandbox rules affect correctness, not just security posture.

The direction should be toward request-derived allowlists and verification from
discovered facts, not broad host shortcuts.

### 12. Honest support boundary

The evaluator must prefer honest failure over fake success.

Either a construct lowers and executes generically, or it is reported as
unsupported for now.

## Practical review questions

When reviewing evaluator work, ask:

- does this execute real semantics, or just recognize a known shape?
- does it preserve typed identity?
- does it improve or weaken boundary honesty?
- does it keep the runtime lowerable later?
- does it preserve the language / stdlib / runtime / platform split?
- does it move us toward real authored usefulness, or just make a fixture pass?
- does it keep strict fragments as an execution strategy inside one runtime?

## Near-term trajectory

The current near-term sequence is:

1. one honest build-shaped `zoo` path
2. explicit observer, sandboxing, and verification pressure on that path
3. enough reusable runtime/build value surface to attack larger build-shaped
   examples
4. only after that: broader multi-host / executor-class behavior
