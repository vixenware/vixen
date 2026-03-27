# execution model

Status: foundation draft.

Current implementation status lives in
[`implementation-plan.md`](/Users/amos/bearcove/vixen/docs/design/implementation-plan.md).

`eval3` is not "eval2, but faster".

It is the next attempt at an execution model for Vixen that is actually aligned
with the language as it is written today in [`zoo/`](/Users/amos/bearcove/vixen/zoo).

That means starting from the real surface area:

- everything is lazy
- demand is structural and typed
- closures, `where` capture, and higher-order combinators are ordinary code
- `Tree`, `Blob`, `Path`, `exec`, `discover`, and observers are ordinary code
- keyed canonical nodes, joined inputs, branch-local assumptions, and conflicts
  are ordinary code
- parallel search is part of the model, not an add-on

The goal of `eval3` is to make those semantics cheap enough that Vixen can run
mostly as native code without lying about what the language is.

This README is a foundation note. It is not a full design, but it is meant to
pin down the shape of the machine before implementation runs away again.

## Document map

- [`implementation-plan.md`](/Users/amos/bearcove/vixen/docs/design/implementation-plan.md):
  current implementation order, code-status note, and next-task order
- [`frontend-bridge.md`](/Users/amos/bearcove/vixen/docs/design/frontend-bridge.md):
  the intended input seam from the real Vixen frontend into `eval3` analysis
- [`analysis-plan.md`](/Users/amos/bearcove/vixen/docs/design/analysis-plan.md): the
  missing centerpiece between frontend input and lowering
- [`canonical-examples.md`](/Users/amos/bearcove/vixen/docs/design/canonical-examples.md):
  the frozen slice-0 semantic subset and the four canonical examples
- [`observations.md`](/Users/amos/bearcove/vixen/docs/design/observations.md):
  typed observation model for slice 0
- [`runtime-abi.md`](/Users/amos/bearcove/vixen/docs/design/runtime-abi.md): object and
  calling-model direction for refs, thunks, closures, and products
- [`compiler-pipeline.md`](/Users/amos/bearcove/vixen/docs/design/compiler-pipeline.md):
  required compiler pipeline and analysis-before-lowering direction
- [`lazy-core.md`](/Users/amos/bearcove/vixen/docs/design/lazy-core.md):
  role of the internal lazy core layer
- [`lazy-core-ir.md`](/Users/amos/bearcove/vixen/docs/design/lazy-core-ir.md):
  a concrete slice-0 lazy-core IR sketch
- [`reference-harness.md`](/Users/amos/bearcove/vixen/docs/design/reference-harness.md):
  reference-harness note for result + trace checks on the canonical examples

## What eval2 taught

`eval2` proved important things:

- Vixen really is a lazy demand-driven graph language
- keyed evaluation belongs in the runtime model
- joined monotone inputs are the right shape for shared solver state
- branch-local search assumptions and replayable conflicts are real semantics
- parallel execution must exist in the same model as single-worker execution

It also made the architectural failures obvious:

- runtime identities were too large
- `Value` carried too much structure
- function "refs" conflated definition, closure environment, and activation data
- owner/worker handoff moved large snapshots and deltas instead of cheap tokens
- too much local churn was promoted into globally coordinated state
- bytecode existed, but it did not simplify the runtime enough to matter

The problem was not just implementation quality.

The runtime object model and the execution protocol were wrong for the
language.

## What Vixen actually is

Vixen is not an eager imperative language with some lazy patches.

It is a statically typed, lazy, demand-driven language whose ordinary authored
style already looks like this:

- closures passed to `map`, `find`, `fold`, `merge`, and `discover`
- local helper functions with captured `where` environments
- partially applied host operations like `partial exec`
- tree and collection builders like `Tree %{ ... }`, `.map(...).merge()`, and
  `.flatten().to_array()`
- host/runtime boundaries like `exec`, `discover`, `acquire`, filesystems, and
  observer callbacks
- solver boundaries like `keyed(...)`, joined requests, `search_assumption`,
  `branch_assumption`, and structured `Conflict`

`eval3` must be designed for that language, not for a toy subset that only has
arithmetic and tuple projection.

## Core thesis

Vixen remains lazy.

The implementation must not become eager just because eager code is easier to
compile.

But laziness does not require "one heap thunk per expression" or "call back into
the runtime for every force".

The central bet of `eval3` is:

- authored Vixen stays lazy
- the compiler statically extracts strict kernels from that lazy source
- the runtime only owns the parts that are genuinely global, shared, or
  effectful

So the execution model is a mix of:

- native kernels
- explicit runtime nodes

The important boundary is:

- runtime = global graph services
- compiled code = local demand propagation, local reduction, and ordinary domain
  logic

If compiled code has to ask the runtime to evaluate every ordinary expression,
the design has failed.

## The language model eval3 must honor

The primitive is not:

- "force this thing to a whole value"

It is closer to:

- "propagate a typed observation / demand through the graph"

Examples:

```vx
let arr = [f(), g()];
```

does not mean "run `f()` and `g()` now".

And:

```vx
a() + b()
```

does not mean "demand the full result of `a()` and `b()` right now" just
because `+` appears syntactically. The semantics are still lazy. Demand reaches
that addition only if something observes the result.

Likewise, demanding a function call does not imply "run the whole function body
to a final concrete value". A function can return a structured lazy result, and
only part of that result may ever be observed.

So the right model is:

- calls produce lazy results
- observations are typed and structural
- demand propagates through the graph
- computation only happens for the demanded cone

The exact observation lattice is still open, but `eval3` must be built around
typed observation, not a universal "give me the full value" primitive.

## Real authored pressure from zoo

The right foundation has to explain the code we already write in
[`zoo/`](/Users/amos/bearcove/vixen/zoo), especially:

- build-style programs dominated by `Tree`, `Blob`, `Path`, `exec`, `discover`,
  and observer closures
- higher-order collection code dominated by `.map(...).merge()`, `.to_map()`,
  `array_fold`, and similar combinators
- solver code dominated by `keyed(...)`, joined inputs, structured conflicts,
  and branch-local search

This means a valid `eval3` foundation must account for three kinds of work from
day one:

1. Local lazy reduction
2. Host/runtime boundary work
3. Shared solver work

If the model only talks clearly about the first one, it is incomplete.

## Compiler strategy

The compiler should statically rewrite lazy source into a mix of:

- strict kernels for the demanded local cone
- runtime nodes for the parts that must remain latent, shared, or effectful

This implies a few core analyses:

- observation analysis
- strictness-under-observation analysis
- sharing/escape analysis
- closure/environment analysis
- runtime-boundary insertion
- node extraction for shared solver semantics

In plain English:

- if the compiler can prove that some demanded cone is local and straight-line,
  it should emit native code for that cone
- if a value escapes, is shared, survives suspension, is keyed, crosses a host
  boundary, or is demanded in a way that cannot be cheaply specialized, it
  becomes a real runtime node

### Kernels are not just selector kernels

The first draft over-centered tuple projection.

Real Vixen kernels will often be:

- arithmetic kernels
- control-flow kernels
- builder kernels for arrays, maps, and trees
- higher-order combinator kernels like `map`/`fold`/`merge`
- closure bodies
- observer callbacks
- projection/selector kernels

That matters because real zoo programs are often more builder-shaped than
selector-shaped.

## Node / kernel split

This split is the center of `eval3`.

A kernel is:

- local
- compiled by Kajit to native code
- allowed to chase local demand without generic runtime round-trips
- typed
- often specialized to a particular observation shape

A node is:

- a semantic laziness boundary or sharing boundary
- runtime-tracked and identity-bearing
- something that may be memoized, widened, suspended, shared, stolen, or
  revisited under stronger demand

Expected node boundaries include:

- sharing
- escape
- keyed canonicalization
- joined-input widening
- branch-local search splits
- external pending/suspension
- host boundaries whose effects or identities must be runtime-tracked
- unknown or highly dynamic observation shape

Everything else should prefer kernels.

## Runtime responsibilities

The runtime is still substantial.

It is not an interpreter for ordinary local code. It is the global control plane
for the parts of the machine that are actually global.

The runtime should own:

- keyed node canonicalization and memoization
- join-on-collision for monotone keyed inputs
- branch contexts, search volumes, and frontier scheduling
- worker ownership, stealing, and deterministic publication of shared facts
- structured conflict capture and replayable provenance
- external pending and resumption
- heap / object / node allocation
- runtime identity for host-backed resources where identity matters
- debug/profiling/inspection hooks

This is the same direction as
[`docs/design/parallel-solver.md`](/Users/amos/bearcove/vixen/docs/design/parallel-solver.md),
but made more explicit as an execution boundary.

The runtime must not own:

- semver logic
- package policy
- C toolchain flag selection
- ordinary control flow
- local collection construction when it stays inside one kernel
- local closure invocation when it stays inside one kernel

## Kajit responsibilities

Kajit is not just a backend helper.

The intended end state is that Kajit compiles authored Vixen logic to machine
code.

That still leaves a runtime, but the runtime is a service layer, not the place
where ordinary Vixen semantics are secretly interpreted.

For `eval3`, Kajit should ultimately compile:

- function bodies
- closure bodies
- builder/combinator kernels
- observation-specialized entrypoints
- continuation/resume kernels
- local enter/update logic for non-shared nodes
- observer code that runs at host boundaries

Kajit's HIR/IR/MIR/debug pipeline is a promising substrate for this, but only if
`eval3` provides the right frontend semantics and ABI.

## Object model direction

Vixen is statically typed.

So the compiled path should not use one giant Rust `Value` enum for everything.

The default compiled representation should be typed values, typed refs, and
typed handles.

Examples:

- `u64`
- `bool`
- typed handle to a string/blob/tree/path object
- `Ref<T>` for a lazily produced `T`
- closure handle plus closure environment handle when captures exist

There may still be erased forms at specific boundaries, but erased universal
values must not be the default execution representation for compiled code.

### Keep these distinct

`eval3` must keep these concepts separate:

- function definition identity
- closure environment
- activation / continuation state
- runtime object identity
- semantic value
- host resource identity

Static definition metadata belongs in program tables.
Closure environments belong in closure objects only when captures exist.
Continuation state belongs in frame/continuation objects.
Host-backed objects like trees and blobs should have explicit runtime
representations instead of being smuggled through a generic mega-value.

A "function value" must not carry all of these at once.

## Host boundary model

This must be first-class.

Build-style Vixen programs are full of operations like:

- `exec`
- `discover`
- filesystem trees and blobs
- observer callbacks
- executor selection and host inspection

So `eval3` needs an explicit story for:

- how compiled code represents `Tree`, `Blob`, `Path`, `TreeEntry`, and similar
  host/resource objects
- how compiled closures cross into host callbacks
- which host operations become runtime nodes
- which host-adjacent builder logic can still remain inside native kernels

If this is not explicit, the first real build workloads will force the runtime
back toward an erased interpreter boundary.

## Demand and memoization direction

The runtime should memoize satisfied observations, not just "full values".

The exact representation is open, but the important rules are monotonicity and
typed widening:

- stronger demand may widen what is known
- already satisfied observations should be reusable
- re-evaluation should only happen when demand genuinely widens or keyed inputs
  widen
- local already-proven work should stay local when no shared boundary is crossed

This is the same broad shape as joined keyed inputs:

- local reduction where possible
- monotone widening through explicit runtime machinery where necessary

## Parallelism direction

Parallelism is not optional in `eval3`.

But the model should not be "every tiny step is globally coordinated".

The intended shape is:

- workers own local kernels and local node segments
- local demand propagation stays local
- only real shared boundaries publish into runtime coordination
- keyed/shared nodes, search frontier changes, pending boundaries, and final
  published facts are coarse runtime events

`workers=1` must be the degenerate case of the same architecture, not a special
inline evaluator and not a different path.

## Vertical slices

The first draft only had a toy slice. That is not enough.

`eval3` should be proved in stages.

### Slice 0: execution-model proof

Purpose:

- prove that lazy Vixen semantics can reduce mostly through native kernels

Scope:

- literals
- local `let`
- ordinary call
- closures with captured environment
- tuple/struct projection
- simple builder/combinator cases

This slice is allowed to stay below current solver semantics.

### Slice 1: build-graph proof

Purpose:

- prove that real zoo-style build code can run in the model without collapsing
  into erased runtime evaluation

Scope should include:

- `Tree`, `Blob`, `Path`
- `exec`
- `discover`
- observer closures
- local helper functions with `where`
- partial application
- array/map/tree builder kernels

This is the first slice that looks like ordinary Vixen build programs.

### Slice 2: solver-core proof

Purpose:

- prove that the real demand-driven solver semantics fit the same machine

Scope should include:

- `keyed(...)`
- joined monotone inputs
- structured `Conflict`
- branch-local assumptions
- search frontier publication

This is the slice that starts to meet the actual `cargo-resolve` class of
workload.

## Open design questions

Important open questions include:

- what is the first explicit observation lattice?
- which node kinds exist in MVP?
- what does `Ref<T>` look like at the ABI level?
- which host/resource types get dedicated runtime representations first?
- when does a local computation become a real runtime node?
- how are continuations represented for suspension/resume?
- how much specialization is generated ahead of time vs on demand?
- how does keyed memoization record partial observations of structured outputs?
- how do builder/combinator kernels preserve laziness without exploding node
  count?

These are execution-shaping questions, not polish details.

## Non-goals for the first pass

The first `eval3` work should not try to:

- replace all of `eval2` immediately
- solve the full solver in one leap
- pretend build-style zoo code is "just host glue"
- bolt closures and host boundaries on after the object model is chosen
- smuggle eager Rust-like execution in under the name of optimization

The first job is to prove the machine:

- lazy semantics stay intact
- compiled kernels do most local work
- host/runtime boundaries are explicit and typed
- shared solver semantics still fit the same architecture

If that works, the rest of the system can grow on top of a real foundation
instead of another evaluator that needs to be replaced later.
