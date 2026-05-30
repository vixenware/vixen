+++
title = "Evaluation"
insert_anchor_links = "heading"
weight = 30
+++

# Vixen Evaluation Specification

This chapter covers evaluation semantics that apply to Vixen programs
independently of any particular standard-library surface.

Build entry points and runtime-backed surfaces such as `main`, `exec`,
`Blob`, `Tree`, `Path`, and `acquire` are specified in the
[Build and Runtime Standard Library](../stdlib/build.md).

## Evaluation model

### Demand-driven dataflow

> r[eval.demand]
>
> Vixen uses demand-driven dataflow evaluation. The runtime calls the entry
> point and needs its return value — that is the root demand; all evaluation
> flows backward from there. Constructing the program's dataflow/demand graph
> from analyzed source is ordinary compilation and is expected. What is absent
> is a separate *build-planning* phase in the Buck/Bazel sense: the set of
> boundary actions (`exec`, `acquire`, …) is **not** enumerated ahead of
> evaluation. Which boundaries run, and with what inputs, is discovered
> dynamically as demand reaches them — because those inputs depend on values
> only evaluation produces (`discover` results, file contents, resolved
> capabilities). The graph of *pure dataflow* may be analyzed statically; the
> graph of *effects* is demand-discovered.

### Concurrency

> r[eval.concurrency]
>
> There are no concurrency primitives. No `spawn`, no `async`, no `await`,
> no channels. Concurrency comes from the structure of expressions — any
> expression with multiple independent sub-expressions evaluates them
> concurrently. Function arguments, array elements, struct fields, tuple
> elements, and map values all evaluate concurrently when independent.

### Pending values

> r[eval.pending]
>
> A value is either resolved or pending. Any value of any type can be
> pending. A struct can have some fields resolved and some pending.
> Pending values can be freely passed to functions, stored in fields, put
> in arrays, and returned. None of these operations wait.

### Forcing

> r[eval.forcing]
>
> Inside the orchestrator, **nothing forces a pending value**. No
> language construct waits. `a + b` where `a` is pending produces a
> pending result. `if cond ( x ) else ( y )` where `cond` is pending
> produces a pending value. `match`, `is`, indexing, `as`, overload
> dispatch — all build nodes in the dataflow graph. None of them block.
> The language never waits.

> r[eval.forcing.boundary]
>
> Values are forced only when they cross a **boundary** between
> environments:
>
> 1. **Orchestrator → Executor** — when calling `exec`, the arguments,
>    mounts, serialized observer payload, and any attached discovery
>    obligations must be fully resolved before they can be shipped to
>    the executor process. This is a serialization boundary.
> 2. **Orchestrator → External caller** — the external environment (CLI,
>    API, or other driver) demands the final result. This is where the
>    output tree must be fully materialized.
>
> These are the only two forcing points. They are both environment
> boundaries — the moment data leaves the orchestrator. Everything
> inside the orchestrator is lazy dataflow. The runtime resolves the
> graph as dependencies become available, but no language-level
> operation ever blocks waiting for a value.

### Control flow demand propagation

> r[eval.control-demand]
>
> `if`, `match`, and `is` do not force pending values. They propagate
> demand to the scrutinee/condition. Demand propagates to a branch only
> after that branch is selected by the resolved scrutinee. Branches not
> taken receive no demand — no work is done for dead branches.

### The evaluator enters functions

> r[eval.enter-functions]
>
> The evaluator does not treat functions as opaque nodes. It enters
> function bodies, discovers their dataflow graph, and schedules internal
> computations the same way it schedules top-level ones. Demand propagates
> all the way down.

### Demand depth limit

> r[eval.demand-depth]
>
> The evaluator tracks the depth of the demand chain. Two distinct
> failure modes exist:
>
> - **Circular dependency** — the evaluator detects that a value
>   transitively demands itself. This is a logic error in the program
>   and poisons with a circular dependency diagnostic.
> - **Depth exhaustion** — the demand chain exceeds a maximum depth
>   without cycling. This is a resource limit and poisons with a
>   recursion depth diagnostic. The limit is configurable.

### Memoization

> r[eval.memoization]
>
> The runtime memoizes function calls transparently. The cache key is
> **(code identity, arguments)** — the function's transitive code
> identity (see [fn.identity]) plus its resolved argument values.
> Because all values are hashable and comparable, any function call can
> be memoized. This is what makes incremental builds work — and because
> code identity is stable across comment/formatting edits, the cache
> survives source changes that do not affect behavior. In every case the key
> is computed **before** the call runs, from inputs the runtime already holds;
> it never includes a call's outputs, nor which of its inputs the call turns
> out to read (see `r[eval.readset]`).
>
> `exec` cache keys are **(observer code identity, observer captures,
> args, env, mounts)**, where the args carry the resolved ability values
> (e.g. a concrete `RustToolchain`) that flow into them. There is no
> "executor class" in the key: routing is type-driven, so an `exec`'s reuse
> is determined by its content-addressed inputs — including the resolved
> toolchain identities — not by a stringly-typed executor selector. Because
> the observer's code identity is transitive, changing a helper function
> that the observer calls invalidates the `exec` cache — even though the
> observer's own AST did not change.

> r[eval.memoization.pending]
>
> Only fully resolved values participate in memoization. Pending values
> are not hashable for cache-key purposes and do not participate in
> memo table lookup. A function call is memoized only once all its
> arguments have resolved.

> r[eval.readset]
>
> A command's **read-set** — the inputs it actually consults — is *discovered
> by running it* and recorded as provenance. Because the cache key
> (`r[eval.memoization]`) must be computable *before* the command runs, the
> read-set is **never part of the key**: keying on it would require running the
> command to decide whether to run it. Instead the read-set governs **rerun
> avoidance** — when a change to *declared* inputs produces a key miss, a prior
> result whose recorded read-set is byte-for-byte unchanged under the new
> inputs remains valid and may be reused without re-execution. This is sound
> only when the command is a deterministic function of the bytes it reads;
> non-determinism is flagged separately (impurity) and forfeits the reuse. The
> storage and lookup that map a command and its declared inputs to candidate
> prior results and their read-sets are not yet specified.
