# Legacy evaluator postmortem

This note is historical. It is not normative.

Normative authority for current semantics remains:
- `/Users/amos/bearcove/vixen/docs/content/spec/language/core.md`
- `/Users/amos/bearcove/vixen/docs/content/spec/language/build.md`
- `/Users/amos/bearcove/vixen/docs/content/spec/stdlib/build.md`
- `/Users/amos/bearcove/vixen/docs/content/spec/internals.md`

The purpose of this note is not to praise or preserve the old evaluators as
architectures. The purpose is to record what they were actually doing, what they
proved, what they failed to prove, and which lessons eval3 must keep.

## Executive summary

There were two major legacy evaluator lines.

1. `vx-eval`
- It had many of the right build/runtime nouns.
- It knew about trees, blobs, exec paths, discovered values, host observations,
  verification obligations, shipped closures, and structured poison.
- Its failure was not merely layering. It was also not honestly lazy enough, not
  shaped for future lowering, and not shaped for the kind of parallel runtime
  Vixen actually wants.

2. `vx-eval2`
- It reacted against eval1 by making runtime state and value representation much
  more explicit.
- It separated payload from metadata and readiness from value.
- Its failure was not semantics but execution grain and machinery volume. It
  became too operationally elaborate and too expensive in its core shape.

The correct eval3 synthesis is not:
- "take eval1 semantics"
- "take eval2 daemon discipline"

That framing is wrong.

The actual target is:
- keep important build/runtime nouns from eval1
- keep representation discipline from eval2
- reject eval1's forcing behavior and monolithic engine
- reject eval2's per-node / per-step machine

## Important correction

A previous version of this note over-credited both legacy evaluators.

Specifically:
- it took eval1 crate docs too literally when they described the evaluator as
  lazy and demand-driven
- it gave eval2 credit for daemon/protocol/application integration that was
  actually implemented in surrounding `vx` code, such as
  `/Users/amos/bearcove/vixen/crates/vx/src/eval_runtime.rs`

This rewrite corrects that.

## What `vx-eval` actually was

Relevant code:
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/value.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/poison.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/exec.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/transport.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/node.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/scheduler.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/worker/mod.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/worker/eval.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/worker/exec_local.rs`

### The good part: it had real build/runtime nouns

Eval1's strongest contribution was not its architecture. It was that it already
knew Vixen evaluation cannot be expressed honestly with only numbers, strings,
and generic tuples.

The value model had real domain concepts:
- `TreeValue`
- `BlobValue`
- `PathValue`
- `ExecPathValue`
- `HostValue`
- `DiscoveredValue`
- explicit host observations like `Which`, `Read`, `Hash`, `Os`, `Arch`

The boundary model also had real concepts:
- `ExecRequest`
- `DiscoverRequest`
- observer closures
- verification obligations
- shipped closures and transported captures

The failure model was also serious:
- `PoisonError`
- `PoisonKind`
- `DemandChain`
- nested causes and observation context

This is the main thing eval1 got right:

> Vixen build evaluation needs domain-specific runtime concepts, not a flattened
> generic value soup.

### The bad part: it was not honestly lazy enough

Eval1 described itself as lazy and demand-driven, but the implementation forced
far more eagerly than the language model wants.

Important evidence:
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/node.rs:108`
  `NodeJob::should_preforce_inputs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/worker/eval.rs:39`
  `force_value(...)` recursively forces tuples, arrays, maps, trees, and structs
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/worker/exec_local.rs:185`
  and following force exec args, env, mounts, observer, writable paths, and so on

So the honest statement is:
- eval1 had a lazy graph vocabulary
- but not a sufficiently lazy runtime discipline

That matters because it means eval1 cannot simply be remembered as "the lazy
one." One of the major discoveries after it was exactly that it was not actually
preserving laziness well enough.

### It was also the wrong parallel shape

Eval1 did contain real parallel execution machinery.

Evidence:
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/scheduler.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/worker/eval.rs:129`
  `force_many(...)` uses `tokio::task::JoinSet`

But that should not be mistaken for having the right parallel architecture.

The actual problem was:
- too much work lived in one giant engine object
- too much runtime identity was implicit in that engine and its node graph
- execution and host/boundary policy were too tightly fused
- the model was not a good fit for a daemon-owned, resumable, distributed future

So the correct judgment is:
- eval1 had some local concurrency
- eval1 did not have the right parallel runtime shape

### It was not shaped for lowering/JIT

Eval1 had lowering, but not the kind of split that makes later compiled kernels
feel natural.

What it had:
- parser CST lowered into owned `instr` trees
- lazy node jobs built on top of that

What it did not have:
- a clean static executable plan separate from runtime state
- a stable kernel/body/runtime split
- a shape where you could replace kernel execution with a future JIT without
  dragging the entire worker engine with it

The runtime object model was too entangled with evaluation semantics and host
behavior.

So eval1 should not be remembered as "the one with the right semantics, just a
bit messy." It had important nouns, but it was still the wrong machine.

### Its biggest architectural failure: one giant evaluator brain

This is where eval1 really collapsed.

The load-bearing runtime logic is concentrated in:
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/worker/mod.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/worker/eval.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval/src/worker/exec_local.rs`

That engine ended up owning too much at once:
- evaluation
- materialization
- boundary parsing
- host obligation gathering
- verification
- tree operations
- local exec behavior
- module/scope state
- error aggregation

So eval1's real architectural lesson is not just "don't tangle things." It is:

> do not let the evaluator become a giant runtime-policy object that knows too
> much about everything.

### Synthetic judgment on eval1

Eval1 was:
- semantically ambitious
- build-domain aware
- operationally too eager
- architecturally too centralized
- not a credible long-term substrate for honest laziness, lowering, or daemon
  execution

The parts worth preserving are its nouns, not its machine.

## What `vx-eval2` actually was

Relevant code:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/lib.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/value.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/result.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/program.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/graph.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/session.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/work.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/eval/mod.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/eval/resume.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/wire.rs`

### The good part: representation discipline

Eval2's best idea was that evaluator state needs cleaner representation than
what eval1 had.

The strongest separations it introduced were:
- payload is not metadata
- readiness is not payload
- a value should not itself be "pending"
- protocol envelopes are not evaluator-local values
- runtime/session state should be explicit rather than hidden inside ad hoc
  evaluator objects

That shows up clearly in:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/value.rs`
  `Value { rep, meta }`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/result.rs`
  `Resolution::{Ready, Pending, Waiting, Poison}`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/wire.rs`
  refusing to serialize pending/waiting/function values across the boundary

This is eval2's most important contribution:

> readiness, provenance, crossing policy, and claims must not be smashed into
> the semantic value payload.

### The good part: explicit resumable state

Eval2 also took suspension and resumption seriously.

This shows up in:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/session.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/work.rs`
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/eval/resume.rs`

It did understand that:
- evaluation sessions can wait
- pending runtime calls need resumable references
- continuations should be explicit
- bytecode bodies are a meaningful execution unit

That is real progress over eval1.

### But it was not "the daemon/protocol evaluator"

This needs to be stated plainly because it is easy to over-credit eval2 once it
is integrated into the surrounding app.

Eval2 does contain:
- protocol conversion machinery in `wire.rs`
- runtime hook types
- session and waiting state

But the surrounding application/runtime integration is not "an eval2 invention."
The actual CLI/daemon runtime adaptation lives in places like:
- `/Users/amos/bearcove/vixen/crates/vx/src/eval_runtime.rs`

So eval2 should get credit for:
- representation and session discipline
- protocol boundary cleanliness

It should not get broad credit for the surrounding daemon/application model.

### Its core failure: too much machine

The decisive problem with eval2 is not that it was wrong about representation.
It was right about representation.

The problem is that it took explicitness too far into operational grain.

The engine accumulated too much machinery:
- large compiled-plan tables in `program.rs`
- extensive scheduler state in `engine/graph.rs`
- session/wave/lease/checkpoint machinery in `engine/session.rs`
- worker/task/lease/pending bookkeeping in `engine/work.rs`
- too many continuation cases in `engine/eval/resume.rs`

The result is a system where there is too much distance between:
- "this thing is demanded"
- and "compute it"

That is why eval2 feels like a scheduler kernel more than a practical evaluator.

### Bytecode was not enough to save it

Eval2 did have one promising direction:
- resumable bytecode bodies
- explicit bytecode locals
- explicit prepared body refs

Relevant file:
- `/Users/amos/bearcove/vixen/crates/vx-eval2/src/engine/eval/mod.rs`

That is closer to a useful kernel idea.

But the surrounding machine was still too elaborate.
The existence of bytecode did not fix the runtime shape because:
- runtime identities were still too large
- work/session machinery was still too heavy
- too much evaluator-private apparatus remained in the hot path

So eval2 should not be remembered as "the lowering/JIT-friendly one" either.
It moved in that direction, but it did not arrive.

### Synthetic judgment on eval2

Eval2 was:
- representationally disciplined
- explicit about runtime/session state
- more honest about readiness and suspension
- operationally overbuilt
- too fine-grained
- still not the right long-term runtime shape

The parts worth preserving are its separations, not its machine.

## What eval3 should actually inherit

### From eval1

Keep:
- real build/runtime nouns
- discovered values carrying obligations
- explicit host observations
- structured failure with provenance and causal context
- serious boundary payloads
- shipped closures as real runtime payloads

Do not keep:
- aggressive forcing behavior
- giant evaluator engine
- host/platform policy mixed into evaluator internals
- the implicit idea that local async batching is the same thing as having the
  right parallel runtime architecture

### From eval2

Keep:
- payload vs metadata separation
- readiness vs value separation
- explicit resumable runtime state
- typed ids and typed execution references
- clear daemon/protocol boundary discipline

Do not keep:
- plan-table explosion
- per-node/per-step execution machinery
- giant scheduler/work/session surface area
- the tendency to build a scheduler kernel before settling the evaluator shape

## What eval3 must not regress on

This is the most important practical takeaway for current work.

Eval3 is at risk of avoiding eval1's tangling and eval2's machinery by becoming
representation-sloppy again.

That would be the wrong trade.

Concrete regressions to avoid:
- catch-all runtime value enums that carry too many unrelated semantic roles
- generic pair/tuple fallback where the language/runtime wants richer product
  meaning
- stringly poison or failure values
- mixing readiness/pending state back into the semantic value payload

If eval3 avoids eval2's complexity by forgetting eval2's representation lesson,
it will have learned the wrong lesson.

If eval3 avoids eval1's tangling by forgetting eval1's domain nouns, it will
also have learned the wrong lesson.

## Bottom line

Eval1 proved that Vixen needs rich build/runtime concepts.

Eval2 proved that evaluator state needs stronger separation discipline.

Neither gave the right final machine.

Eval3 should therefore aim for:
- eval1's important nouns
- eval2's representation discipline
- without eval1's eagerness and monolith
- without eval2's machine explosion

That is the real inheritance line.
