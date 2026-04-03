# eval3 architectural axes

This document is not normative. It is a planning aid for eval3 work.

Normative authority remains:
- [core language semantics](/Users/amos/bearcove/vixen/docs/content/spec/language/core.md)
- [evaluation semantics](/Users/amos/bearcove/vixen/docs/content/spec/language/build.md)
- [build/runtime stdlib semantics](/Users/amos/bearcove/vixen/docs/content/spec/stdlib/build.md)
- [internals and shipped-closure model](/Users/amos/bearcove/vixen/docs/content/spec/internals.md)

## Purpose

Eval3 work has many competing pressures:
- make real programs execute again
- keep the architecture lowerable later
- preserve provenance and explainability
- avoid cheating with semantic macros and source-shape recognition

This file names the main axes we are trying to keep true at the same time.

## Core axes

### 1. Demand-driven semantics

Eval3 is demand-driven, not step-driven.

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

Execution should be owned by a long-lived daemon/runtime, not by a short-lived CLI process.

That implies:
- evaluation state should be resumable and observable
- CLI and UI are clients of the same runtime model
- static HTML exports are debugging artifacts, not the primary architecture

### 4. Stable identity and provenance

Eval3 must preserve enough identity to explain what happened and why.

That implies:
- stable typed ids for bodies, expressions, kernels, runtime nodes, layouts, and slots
- source provenance and causal provenance both matter
- demand paths, carried obligations, and boundary facts must be attributable
- semantic identity must not depend on raw names or incidental source text

### 5. Kernelized execution, not eval2 granularity

Eval2 proved that per-expression suspension is too expensive.

Eval3 should instead:
- execute straight-line local work inside kernels
- keep boundaries explicit
- keep enough structure for explanation and later lowering
- avoid both extremes:
  - micro-node explosion
  - opaque “just run it” blobs

### 6. JIT-friendly plan shape

Eval3 does not need Kajit now, but it must not block Kajit later.

That implies:
- explicit static executable plans
- typed ops and typed slots
- suspension/resumption outside kernels
- no semantic dependence on interpreter-only shortcuts
- avoid turning everything into a giant dynamic object bag

### 7. Honest shipped closures

Closures that cross `discover` or `exec` boundaries are real evaluator payloads.

That implies:
- shipped closures carry code identity, lowered body, and resolved captures
- executor-side evaluation uses shared evaluator semantics
- observers and probes are not host callbacks
- observer/probe semantics cannot be replaced by recognized-plan macros

### 8. Clean language / stdlib / runtime / platform split

These layers need to stay distinct.

The intended split is:
- language core: syntax and general evaluation semantics
- stdlib/build runtime surface: `discover`, `exec`, `Tree`, `Blob`, `Path`, `Host`, `Process`
- eval3 analysis: static structure, kernels, boundaries, lowering
- eval3 runtime: frames, cells, suspension, resumption, execution
- platform seam: sandboxing, process launch, host interaction

Eval3 must not tunnel through eval1 to get platform behavior.

### 9. Boundary semantics stay explicit

`discover` and `exec` are not conveniences. They are major semantic boundaries.

That implies:
- `discover` is real host observation on an unsandboxed executor-local `Host`
- values flowing out of `discover` carry hidden verification obligations
- `exec` rechecks relevant obligations on the concrete runner
- observer execution is real evaluator work over executor-local values
- boundary payloads should be typed and inspectable

### 10. Tree / Blob / Path / Process / Host semantics matter

Build-shaped values are not incidental wrappers.

That implies:
- `Tree`, `Blob`, `Bytes`, `Path`, `Process`, and `Host` need real runtime meaning
- materialization rules matter
- executor-local values must not be faked to satisfy arity or make a test pass
- carried obligations and VFS semantics have to remain attached to the right values

### 11. Sandboxing and verification are semantic, not test scaffolding

Sandbox rules affect correctness, not just security posture.

That implies:
- do not loosen sandbox policy to make an example pass
- do not hide host drift by normalizing outputs into success
- the direction should be toward request-derived allowlists and verification from discovered facts
- broad legacy allowlists are a temporary liability, not a target

### 12. Fine mounts vs coarse mounts

Not all sandbox access has the same semantic quality.

#### Fine mounts

Fine mounts are VFS-backed and runtime-visible.

Examples:
- `src` mounted through ordinary `exec` mounts
- `Blob`, `Tree`, and `ExecPath` inputs normalized into VFS content

Properties:
- reads and writes happen through the VFS model
- dependency tracking can see what was touched
- overmounts and related diagnostics are possible
- content identity is tied to store-backed values and explicit mount provenance

Fine mounts are the preferred shape.

#### Coarse mounts

Coarse mounts are host holes punched into the sandbox.

Examples:
- broad toolchain roots like `/Library/Developer/...`
- any host path allowlist that bypasses normal VFS mounting

Properties:
- reads are not fully tracked by the runtime
- dependency precision is weaker
- correctness cannot rely on ordinary VFS provenance alone
- broadening them casually changes semantics in ways the runtime cannot fully explain

Coarse mounts are an escape hatch, not the default model.

### 13. Discover is the guardian of coarse-mount correctness

Coarse mounts are only reasonable when guarded by discover-derived facts.

That means:
- `discover` is the intentional hermeticity compromise
- it probes the unsandboxed executor-local `Host`
- it records the host facts consulted
- values flowing out of `discover` carry those facts as hidden obligations
- later `exec` rechecks the relevant obligations on the concrete runner

So the justification split is:
- fine mounts justify themselves through VFS tracking and provenance
- coarse mounts justify themselves through discovery, carried obligations, and verification

This gives a strong review rule:
- do not broaden coarse mounts just to make an example pass
- do not treat legacy blanket host roots as the target design
- move toward request-derived allowlists from discovered and verified facts

### 14. Honest support boundary

Eval3 must prefer honest failure over fake success.

That implies:
- either a construct lowers and executes generically
- or it is reported as unsupported for now
- no semantic macros
- no source-sniffing shortcuts
- no test harnesses that rewrite runtime truth to hit the happy path

## Practical review questions

When reviewing eval3 work, ask:
- does this execute real semantics, or just recognize a known shape?
- does it preserve typed identity?
- does it improve or weaken boundary honesty?
- does it keep the runtime lowerable later?
- does it preserve the lang/stdlib/runtime/platform split?
- does it move us toward eval1-class usefulness, or just make a fixture pass?

## Near-term trajectory

The current near-term sequence is:
1. honest single-host `cc-version`
2. one honest single-host build-shaped path with observer-produced result
3. enough reusable runtime/build value surface to attack `tri-platform-png`
4. only after that: multi-host / executor-class behavior
