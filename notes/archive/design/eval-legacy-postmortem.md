# Legacy evaluator postmortem

This note is historical. It is not normative.

Normative authority for current semantics remains:
- `/Users/amos/bearcove/vixen/docs/content/spec/language/core.md`
- `/Users/amos/bearcove/vixen/docs/content/spec/language/build.md`
- `/Users/amos/bearcove/vixen/docs/content/spec/stdlib/build.md`
- `/Users/amos/bearcove/vixen/docs/content/spec/internals.md`

The purpose of this note is to capture what the older evaluators were trying to do,
what they got right, what they got wrong, and which lessons eval3 must preserve.

## Executive summary

There were two major historical evaluator lines worth preserving lessons from:

1. `vx-eval`
- Semantically ambitious and domain-rich.
- It understood shipped closures, host observations, carried obligations, process
  execution, trees, blobs, paths, and structured poison.
- Its core problem was layering. Too much runtime, transport, lowering, host
  semantics, and worker orchestration lived in one tangled system.

2. `vx-eval2`
- Much more disciplined about representation.
- It separated payload from metadata, readiness from value, pending from ready,
  provenance from payload, and execution bookkeeping from semantic values.
- Its core problem was operational granularity. The engine became too complex,
  too node-sized, and too heavy to serve as a practical long-term evaluator.

The main eval3 danger is regressing on the representation lessons while trying
not to repeat eval2's execution-grain problem.

## `vx-eval`

### Crate-level approach

Relevant files:
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/lib.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/lower.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/instr.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/node.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/scheduler.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/worker/mod.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/worker/eval.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/value.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/poison.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/exec.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/transport.rs`

`vx-eval` described itself as a demand-driven dataflow evaluator. That was not
marketing fluff. The design really was centered around lazy nodes, pending
references, recursive demand, scheduler execution, and parked unresolved work.

The pipeline was roughly:
- parser CST -> lowering into owned `instr` trees
- `instr` trees -> lazy `NodeJob` graph
- scheduler demands nodes and executes ready batches
- worker engine performs builtins, runtime calls, host checks, exec/discover,
  tree/blob/path operations, and observer execution
- transport layer serializes shipped functions/modules/values for executor-side
  execution

This evaluator was semantically serious. It wanted to be a real build evaluator,
not just a calculator.

### `instr.rs`: lowered owned tree

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/instr.rs`

This file is the "clean lowered AST" layer.

What it contains:
- `Module`, `FnDef`, `StructDecl`
- `Block`, `Stmt`, `Expr`
- source provenance types
- explicit expression forms like `Closure`, `Call`, `Field`, `Fail`, `Tuple`,
  `Array`, `Map`, `StructLit`, `Import`, `If`

What it was trying to do:
- strip away rowan/parser ownership issues
- create a sendable, owned representation with source provenance attached
- keep the evaluation structure close to authored syntax

What it got right:
- provenance was already present here
- it kept a straightforward, understandable semantic surface
- it was explicit about closures, imports, template literals, failure, and tuples

What it got wrong:
- it remained too syntax-shaped to be a stable long-term execution IR
- names were still plain strings
- there was no hard separation yet between source identity, semantic identity,
  and execution shape

This was a good lowering substrate, but not enough of an architecture boundary.

### `lower.rs`: CST to owned semantic tree

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/lower.rs`

This file was the bridge from parser CST to the owned `instr` tree.

What it contains:
- `LowerCtx`
- `LowerError`
- source-file lowering entrypoints
- tree walks that convert parser nodes into `instr::*`

What it was trying to do:
- centralize source lowering in one place
- normalize syntax into evaluator-owned forms
- attach source ids and spans early

What it got right:
- the evaluator did not directly depend on the rowan tree everywhere
- source spans/provenance were created early enough to matter

What it got wrong:
- lowering still fed directly into a tightly coupled runtime stack
- there was not a clean intermediate architecture boundary like today's desired
  bridge -> analysis -> plan split

The lesson is not that this file was bad. The lesson is that it was too close to
both the parser and the runtime at once.

### `node.rs`: lazy node graph and suspension model

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/node.rs`

This file defines the lazy dataflow backbone.

What it contains:
- `NodeId`
- `NodeKey`
- `NodeJob`
- `Outcome`
- `Waiter`
- `NodeState`
- `Node`

What it was trying to do:
- treat evaluation as a graph of lazy jobs
- memoize work by node identity
- support parking and wakeup through waiter lists
- capture demand recursion and dependency edges explicitly

What it got right:
- pending work was explicit
- memoization was built into the semantic model
- waiter lists and node states matched the demand-driven architecture
- this is the ancestor of the good eval3 instinct that suspension belongs in
  runtime cells/frames, not on every raw AST node

What it got wrong:
- jobs and graph shape were still too entangled with evaluator internals
- there was no clear "static plan" vs "dynamic state" split
- node identities and jobs were not well-isolated from the worker/runtime layer

This file had the right semantic instinct, but not the right long-term boundary.

### `scheduler.rs`: real demand-driven execution and batching

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/scheduler.rs`

This file is the old evaluator's execution heart.

What it contains:
- dependency and reverse-dependency tracking
- demanded/completed sets
- demand paths
- ready queue batching
- parallel batch execution via `tokio::task::JoinSet`
- stalled/cycle/depth detection

What it was trying to do:
- recursively demand nodes
- execute only what was ready
- batch independent ready nodes
- preserve demand-path context for errors

What it got right:
- it was actually parallel-friendly in a meaningful way
- demand paths were not an afterthought
- cycle and depth failures were modeled directly as evaluator failures
- the scheduler knew enough about causal structure to explain stalls

What it got wrong:
- it was too tightly coupled to the engine's notion of node execution
- the scheduler was not cleanly separated from runtime/worker/node implementation
- it operated over a graph shape that was still too evaluator-private

This was good machinery buried inside a bad architectural package.

### `value.rs`: semantically rich runtime value model

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/value.rs`

This is one of the strongest files in the old evaluator.

What it contains:
- `Value`
- domain-specific payloads like `PathValue`, `ExecPathValue`, `BlobValue`,
  `TreeValue`, `StructValue`, `FunctionValue`, `BuiltinValue`
- `HostObservation`
- `DiscoveredValue`
- executor selection and transport-facing values

What it was trying to do:
- make build/runtime semantics first-class in the evaluator
- represent domain values honestly instead of flattening them into bags or strings
- carry host observations and discovered obligations with values

What it got right:
- `DiscoveredValue { value, obligations }` is exactly the kind of semantic noun
  eval3 must not forget
- host observations were explicit and typed:
  - `Os`
  - `Arch`
  - `Which { name, expected_path }`
  - `Read { path, content_hash }`
  - `Hash { path, content_hash }`
- build values were not treated as generic maps or opaque externals
- paths, trees, blobs, exec paths, hosts, executors, partial applications, and
  builtins each had dedicated runtime meaning

What it got wrong:
- the value layer was too closely entangled with worker/runtime mechanics
- metadata, readiness, and provenance were not separated as cleanly as eval2 later did

The core lesson is that eval1 had the right semantic nouns, even when it did not
have the right architecture.

### `poison.rs`: structured failure, not stringly errors

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/poison.rs`

This is the other very strong file in eval1.

What it contains:
- `PoisonError`
- `PoisonKind`
- `DemandChain`
- structured context, observations, nested causes, and formatting

What it was trying to do:
- make lazy failure a structured semantic object
- preserve causal context through demand chains and nested failures
- avoid collapsing failure into a single string too early

What it got right:
- failure had kind, structure, causal stack, and context
- poison was part of the evaluator model, not just a formatting concern
- demand-driven failure explanation was treated as a first-class feature

What it got wrong:
- some of this structure was still too tightly specific to the old engine
- it did not sufficiently distinguish representation payload from scheduler/runtime
  bookkeeping the way eval2 later tried to

Still, current eval3's `LazyPoison(String)` is a regression relative to this file.

### `exec.rs`: serious boundary payloads

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/exec.rs`

This file represented the old evaluator's boundary seriousness.

What it contains:
- async boundary futures
- `ExecArg`, `ExecMount`, `ExecWritable`, `ExecCreateDir`
- `ExecRequest`
- `DiscoverRequest`
- `RuntimeHooks`
- runtime-ready payloads for exec/discover completion

What it was trying to do:
- make `exec` and `discover` structured runtime boundaries
- preserve observer closures and verification obligations through execution
- route runtime-specific work through hooks instead of burying it in pure evaluation

What it got right:
- the request payloads were not toy shapes
- exec knew about args, env, mounts, writable/create_dirs, executor selection,
  observer closures, provenance, and verification obligations
- discover knew it was a different kind of boundary from exec

What it got wrong:
- the hook boundary was not isolated enough from the rest of the evaluator
- transport/runtime concerns leaked widely across the crate

The lesson is to keep the semantic seriousness, not the crate entanglement.

### `transport.rs`: shipped closure and executor-side transport

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/transport.rs`

This file is critical for understanding what eval1 already knew.

What it contains:
- transport forms of modules, blocks, statements, and expressions
- `ShippedFunction`
- transport values for trees, blobs, structs, builtins, discovered values, and
  functions
- bidirectional conversion between transport forms and local `instr`/`Value`

What it was trying to do:
- serialize evaluator payloads so executor-side evaluation could happen with real
  shipped code and captures
- preserve more than raw values by transporting code identity, captures,
  provenance, and builtins

What it got right:
- shipped closures were not a vague future idea; they were already real
- transport was explicit about captures and code identity
- discovered values crossed the boundary with obligations attached

What it got wrong:
- transport lived in the same crate and conceptual layer as everything else
- conversion code became sprawling and tightly coupled to runtime value forms
- some constructs were already being encoded through evaluator-private enums,
  making cleanup difficult

The lesson is that shipped closure semantics are real and old. Eval3 should not
act like this is a new idea.

### `worker/mod.rs` and `worker/eval.rs`: where eval1 lost the plot

Relevant files:
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/worker/mod.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/worker/eval.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/worker/identity.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/worker/exec_local.rs`

This is where the evaluator became too tangled to age well.

What it contains:
- `Engine`
- scheduler-visible node execution state
- builtins and method dispatch
- host obligation verification
- shallow/deep materialization
- exec request parsing
- local exec handling
- module loading and scope state
- tree/path/blob manipulation
- demand stack tracing and error aggregation

What it was trying to do:
- be the one place where evaluation actually happens
- centralize the operational semantics

What it got right:
- it knew about the real semantic surfaces
- it knew obligations had to be gathered from values and rechecked
- it treated discovered content and opaque mounts differently
- it did serious value inspection and materialization work

What it got wrong:
- far too much was centralized in one runtime object
- policy, execution, evaluation, transport, and materialization were all mixed
- local exec and worker-specific concerns were not isolated enough
- the code became hard to reason about and hard to refactor safely

This is the central eval1 postmortem point:

Eval1 had many correct semantic nouns and many correct semantic instincts, but it
failed at architecture. It did too much in too few layers.

### Overall verdict on `vx-eval`

What eval1 got right:
- domain-rich value model
- structured poison/failure
- carried host observations / obligations
- serious exec/discover boundaries
- shipped closures and transported evaluator payloads
- actually demand-driven scheduler semantics

What eval1 got wrong:
- layering
- runtime/worker sprawl
- insufficient separation between lowering, semantics, transport, host hooks,
  and execution policy

What eval3 must keep from eval1:
- domain-shaped runtime values
- discover obligations attached to values
- serious process/tree/blob/path/host semantics
- structured poison and provenance-rich failure
- shipped closures as real runtime payloads

What eval3 must reject from eval1:
- giant central engine object
- host/platform policy smeared through evaluator code
- transport/runtime/lowering all in one conceptual bucket

## `vx-eval2`

### Crate-level approach

Relevant files:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/lib.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/program.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/value.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/result.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/slot.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/trace.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/mod.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/graph.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/session.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/work.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/eval/mod.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/eval/builtins.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/eval/resume.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/wire.rs`

The explicit goal of eval2 was to fix eval1's muddled representation and runtime
boundary discipline.

Its core thesis was:
- payload is not metadata
- readiness is not payload
- pending is not ready
- provenance should stay attached without corrupting payload shape
- runtime edges, claims, and crossing state should be first-class

This was the right reaction to eval1.

### `lib.rs`: the architectural manifesto

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/lib.rs`

This file is unusually revealing because the crate docs explain the intent.

What it says, in effect:
- eval1 smeared payload and metadata together
- eval2 wants `ValueRep` plus `ValueMeta`
- eval1 confused readiness with values like `Pending(...)`
- eval2 wants `Resolution` for ready/pending/poison/waiting
- claims and provenance should stay structured and visible

This is the main representation lesson eval3 should remember.

### `value.rs`: disciplined value payload plus metadata

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/value.rs`

What it contains:
- `Value { rep, meta }`
- `ValueRep`
- `ValueMeta`
- crossing/reuse/provenance/claims-related structures
- function references and capture structures
- comparison helpers

What it was trying to do:
- separate semantic payload from attached metadata
- preserve provenance and crossing semantics without turning the runtime value
  itself into a mess

What it got right:
- this is a better representation discipline than eval1
- it makes it possible to reason about payload separately from transport/provenance
- it avoids putting pending/poison directly in the value payload

What it got wrong:
- the surrounding evaluator became so mechanically elaborate that this good model
  was buried under execution complexity
- some domain richness from eval1 became less central or less ergonomic to work with

The key lesson is that eval2's representation discipline was right even if the
whole evaluator was too heavy.

### `result.rs`: readiness as a separate concept

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/result.rs`

What it contains:
- `Resolution`
- `EvalError`
- structured error kind/message/detail/demand chain

What it was trying to do:
- cleanly separate ready values from waiting/pending/poison
- make failure structured and compatible with the new value/meta split

What it got right:
- `Resolution::Ready(Value)` vs `Pending(...)` vs `Waiting` vs `Poison(EvalError)`
  is the right kind of separation
- current eval3 should learn directly from this instead of putting lazy poison and
  pending-like ideas inside a giant value enum

What it got wrong:
- the clarity of this type did not save the engine from becoming too operationally complex

### `slot.rs`: stateful verification and refresh model

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/slot.rs`

What it contains:
- slot ids and witnesses
- slot intents/actions
- slot records and store structures
- refresh/current semantics

What it was trying to do:
- model stateful refreshed data in a principled way
- make witness-based state changes explicit instead of magical

What it got right:
- stateful runtime interactions were treated as real semantic structures
- it did not flatten stateful observations into untracked ad hoc cache state

What it got wrong:
- the machinery is substantial and adds to the evaluator's sense of being a full
  operating system rather than a crisp evaluator core

The lesson is to keep explicitness, but watch complexity budget carefully.

### `trace.rs`: tracing as a first-class runtime product

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/trace.rs`

What it contains:
- trace event structures
- causal event recording
- session/work/node event indexing

What it was trying to do:
- preserve full runtime traceability
- make the evaluator explainable, replayable, and introspectable

What it got right:
- provenance and runtime causality were not treated as optional debug extras
- this aligns with the current daemon/provenance goals for eval3

What it got wrong:
- tracing was tied to an already overcomplicated execution engine
- it added even more surface area to a system that was already too hard to hold in mind

The lesson is that traces matter, but they need a simpler runtime spine than eval2 had.

### `program.rs`: compiled plan explosion

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/program.rs`

This file is a major source of eval2's complexity.

What it contains:
- source/import program wrapper
- many typed ids for plan tables
- expression plan tables for every syntax form
- body ids for block and bytecode bodies
- lookup tables for expressions and rewritten execution refs
- `BytecodeBody` and `BytecodeOp`
- compiled program construction from parser/resolve/type information

What it was trying to do:
- make every evaluable shape explicit and indexed
- create a stable compiled program with typed ids everywhere
- bridge authored syntax into both direct expression plans and bytecode bodies

What it got right:
- typed identity everywhere
- serious source-to-plan compilation discipline
- explicit bytecode bodies for lowerable parts
- it knew that stable ids matter

What it got wrong:
- the amount of plan surface exploded dramatically
- there were too many parallel representations at once:
  expression refs, plan ids, block bodies, bytecode bodies, rewrites, execution kinds
- this made the evaluator hard to change and hard to reason about

This is the eval2 representation paradox:
- it learned the right lesson about typed identity
- but implemented it at too fine a granularity

### `engine/graph.rs`: scheduler state turned into an ecosystem

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/graph.rs`

This file is enormous and tells the real story.

What it contains:
- runtime call request/ready types
- work ids, wave ids, branch contexts
- work causes and node states
- buffered/leased/queued/in-flight work records
- dependency edges, keyed node memoization, no-good learning
- explanations, snapshots, stats, profiles, call trees, profile summaries,
  checkpoints, prepared snapshots, validation structures

What it was trying to do:
- turn the evaluator into a fully inspectable, parallel scheduler with explicit
  graph state, leasing, profiling, keyed memoization, and explanation

What it got right:
- it treated runtime state as something worth modeling explicitly
- it took provenance, explanation, and profiling seriously
- keyed memoization and branch-context indexing were real ideas, not hacks

What it got wrong:
- this is where the evaluator became too much system and not enough engine
- the amount of scheduler metadata exploded
- too many concerns accumulated in one layer: execution state, explanation,
  profiling, validation, search learning, parallel leasing

This file is the clearest example of eval2 overreaching.

### `engine/session.rs`: control loop and parallel orchestration

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/session.rs`

What it contains:
- `EvalSession`
- serial `run` loop
- parallel worker orchestration and wave leasing
- checkpoint/history support
- completion application and prepared-work execution

What it was trying to do:
- encapsulate the lifetime of an evaluation
- support both serial and parallel execution
- support stepping, tracing, waiting, checkpointing, and profile capture

What it got right:
- daemon-like, resumable session thinking was correct
- parallel execution was not bolted on at the last minute
- completion application and waiting states were explicit

What it got wrong:
- too many responsibilities piled into one session object
- wave leasing and parallel checkpoint restoration are a lot of machinery
- the control loop feels like a scheduler kernel, not a pragmatic evaluator runtime

This file shows the right daemon-oriented instinct taken too far into operational complexity.

### `engine/work.rs`: the operational cost center

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/work.rs`

What it contains:
- scheduler tables
- ready/in-flight task records
- lease namespaces and ids
- prepared scheduler state
- pending waiter tracking
- ready-task execution entrypoints
- worker message structures

What it was trying to do:
- manage safe, explicit, parallel work scheduling and result commitment
- make prepared work leasing and execution robust and observable

What it got right:
- it took parallelism and resumption seriously
- it knew that work ownership and task completion need explicit structure

What it got wrong:
- this is where the engine became too heavy to love
- there is just too much machinery between "a thing needs computing" and
  "evaluate the thing"
- it made the evaluator difficult to maintain and difficult to trust casually

This file is the main reason eval2 feels like the wrong operational shape.

### `engine/eval/mod.rs`: bytecode execution core

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/eval/mod.rs`

What it contains:
- prepared body refs and entries
- bytecode local initialization and scope overlay logic
- pattern binding in bytecode locals
- function bytecode entry setup
- helpers for looking up and binding locals/scopes while resuming bytecode

What it was trying to do:
- make evaluation resumable at bytecode granularity
- separate prepared executable bodies from higher-level source plans
- support closures, captured environments, and resumption through bytecode state

What it got right:
- this is closer to the right eval3 kernel idea than most of eval2
- it understood that lowerable straight-line work should be explicit and resumable

What it got wrong:
- it still lived inside the huge evaluator machine
- the surrounding continuation, lease, and suspension scaffolding made this
  bytecode core harder to see than it should have been

This is one of the more reusable conceptual pieces in eval2.

### `engine/eval/builtins.rs`: semantic surface hidden inside a giant engine

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/eval/builtins.rs`

What it contains:
- builtin detection and argument checking
- slot/current/refresh builtins
- keyed and assumption/search builtins
- internal stream and merged-tree helper values
- array/tree/option/stream helper constructions

What it was trying to do:
- keep runtime special forms explicit and validated
- support richer evaluator-specific builtins like slots and assumption search

What it got right:
- builtin-specific semantics were treated explicitly, not accidentally
- it knew that stateful/search-y facilities need dedicated modeling

What it got wrong:
- too much semantic surface was hidden in evaluator-private builtins
- internal helper structures proliferated
- this pushed the evaluator further toward a bespoke VM with too many private tricks

### `engine/eval/resume.rs`: continuation and suspension machinery

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/eval/resume.rs`

What it contains:
- resumption of suspended frames
- bytecode-tail continuation handling
- frame-sequence continuation handling
- runtime-call completion handling
- stream-join continuation handling
- slot continuation handling

What it was trying to do:
- make all suspension explicit and resumable
- support runtime calls, slots, bytecode, and stream joins through a common
  continuation language

What it got right:
- resumption was a real, explicit runtime concern
- runtime boundaries, slot updates, and bytecode resumption were all surfaced
- this is the intellectual ancestor of the eval3 "frames and cells" model

What it got wrong:
- too many things could suspend at too fine a grain
- the number of continuation cases became large and mentally expensive

The lesson is not to abandon continuations. It is to reduce the granularity and
surface area of what can suspend.

### `wire.rs`: daemon boundary discipline

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/wire.rs`

What it contains:
- conversion between evaluator `Value`/`Resolution` and protocol envelopes
- explicit rejection of pending/waiting/function values crossing the boundary

What it was trying to do:
- draw a firm daemon boundary
- keep protocol envelopes separate from local evaluator representation

What it got right:
- daemon boundary discipline was explicit
- it refused to pretend pending values can cross the boundary safely
- protocol conversion was treated as its own layer

What it got wrong:
- the core engine behind it was already too elaborate
- some wire limitations reflected an evaluator that was not yet settled enough
  in its payload model

Still, the conceptual boundary here is good and worth keeping.

### Overall verdict on `vx-eval2`

What eval2 got right:
- representation discipline
- payload vs metadata separation
- readiness vs value separation
- structured protocol boundary
- explicit session/work/suspension/provenance machinery
- typed ids everywhere

What eval2 got wrong:
- over-granular execution model
- too much scheduler/session/work apparatus
- too many internal plan kinds and runtime bookkeeping structures
- too much evaluator-private machinery for too little practical clarity

What eval3 must keep from eval2:
- readiness is not a value
- metadata/provenance/claims are not payload
- typed ids matter
- daemon boundary discipline matters
- explicit resumption state matters

What eval3 must reject from eval2:
- per-expression or near-per-expression operational granularity
- giant scheduler/work/profile/trace apparatus in the hot path
- turning the evaluator into a scheduler kernel before semantics are stable

## Direct lessons for eval3

### 1. Preserve eval1's semantic nouns

Eval3 should preserve explicit domain/value concepts like:
- discovered values carrying obligations
- host observations
- process/tree/blob/path/host/executor runtime values
- shipped closures and captures
- structured lazy failure

### 2. Preserve eval2's representation discipline

Eval3 should preserve structural separations like:
- payload vs metadata
- readiness vs value
- pending/waiting/poison outside the value payload
- typed ids and explicit runtime state

### 3. Reject eval1's layering

Do not recreate:
- giant central worker engine
- transport/runtime/lowering/host policy all in one crate or object
- host/platform policy smeared through evaluator internals

### 4. Reject eval2's execution grain

Do not recreate:
- per-expression suspension everywhere
- giant work leasing/wave/checkpoint apparatus as the core runtime shape
- hundreds of tiny plan tables as the primary execution model

### 5. Current eval3 regression to watch closely

The biggest current representation risk in eval3 is regressing beneath both
legacy evaluators by adopting values that are too generic or too weak.

Examples of the wrong direction include:
- catch-all runtime enums with too many unrelated semantic roles
- generic tuple/pair fallback where the language/runtime wants richer products
- stringly poison/failure values
- attaching pending state directly to semantic payload values

If eval3 avoids eval2's operational complexity by throwing away representation
quality, it will have learned the wrong lesson.

## Bottom line

`vx-eval` proved that Vixen evaluation needs rich semantic nouns.

`vx-eval2` proved that those nouns need disciplined representation and explicit
runtime state.

Eval3's job is to combine those two lessons without inheriting:
- eval1's tangled layering
- eval2's over-granular machine.
