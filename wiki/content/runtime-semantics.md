+++
title = "The demand-driven runtime"

[extra]
type = "Doc"
tiers = ["2"]
tags = ["runtime", "evaluation", "demand-driven", "kernels", "trees"]
+++

A lazy **demand sea** with eager pure **kernel islands**. The runtime calls the
entry point, needs its return value (the *root demand*), and everything flows
backward from there. It replaces the eager evaluator (`vx-rt`, the daemon's
`runtime_eval`, the flat `KernelOp` ISA) and is linked by **both** the
orchestrator and the executor, so a shipped closure evaluates the same on either
side. Concurrency lives in [The strand scheduler](/strand-scheduler/); the build
primitive that rides on it is [exec](/exec/).

## Why this exists — the rot it fixes

The runtime was rebuilt several times, and each time an agent (or a human in a
hurry) reached for the **eager, imperative, local** shortcut because the codebase
*afforded* it: a whole second eager evaluator used as the test path (so the
language was validated against an engine that *isn't* the language); `pending`
implemented as a human-readable string doubling as the not-implemented sentinel;
a flat instruction set mixing pure ops with boundary ops in one enum a
tree-walker matches, presupposing an interpreter and erasing the kernel/boundary
partition.

**The lesson: deletion + prose has failed every time** — the rejected concept
regrows under a new name in a new place. Enforcement has to be **types** (make
the wrong thing not compile) and **executable oracles** (make the wrong thing go
red), not documentation an agent skims. Two concrete tripwires:

- the **demand graph as a first-class, inspectable artifact** — you can render
  it; an eager tree-walker has no graph to show;
- the **bomb `💣`** — a node that detonates *only when forced*; an eager engine
  over-forces and lights up a node that should stay dark.

## Vocabulary — related but separate

The terms that were getting fused with each other:

- **root demand** — the entry point's return value; evaluation flows backward
  from it.
- **pending** — a value not yet resolved. A *property of any value of any type*,
  not a variant.
- **resolver** — *how* a pending value resolves: local-kernel, remote-handle,
  stream. The thing a `Pending(String)` label threw away.
- **kernel** — an eager, pure, local fragment that runs to completion once
  entered. An eager island in the lazy sea.
- **strand** — a chain of sequentially-dependent work the scheduler runs as one
  thread of progress; the unit of concurrency ([strand scheduler](/strand-scheduler/)).
- **identity** vs **materialization** — a value's content hash (*what it is*) vs
  where its bytes physically live (*separate from identity*).
- **ingest** (compute identity: walk + hash, eager, no transfer) vs **transfer**
  (move bytes, lazy, demand-driven).
- **remote handle** — a value whose parts are resolved by activity on another
  host, asynchronously and independently.

## Demand-driven, no build-planning phase

Constructing the program's dataflow/demand graph from analyzed source is ordinary
compilation. What is **absent** is a Buck/Bazel-style up-front *build-planning*
phase: the set of boundary actions (`exec`, `acquire`, …) is **not** enumerated
ahead of evaluation. Which boundaries run, and with what inputs, is discovered as
demand reaches them — because those inputs depend on values only evaluation
produces (earlier `exec` outputs, file contents, resolved capabilities). The
graph of *pure dataflow* may be analyzed statically; the graph of *effects* is
demand-discovered.

## Pending by default; forcing is structured

Inside the orchestrator **nothing forces**: `a + b` with `a` pending is itself
pending; control flow propagates demand without blocking. A pending value carries
a **resolver** describing how it resolves — not a string. Forcing is not one
clean "materialize everything"; a remote handle resolves in parts, over time.

## Kernels — eager islands in the lazy sea

A kernel is eager, pure, local, run to completion once its inputs are ready and
its result is demanded. It touches no capability and performs no async operation.
Pure intrinsics (`parse`, `pubgrub_solve`) may be **called from inside** a kernel.
Codegen is copy-and-patch: interpret first, compile when hot.

**Where concurrency comes from:** the user cannot spawn threads. The only
parallelism is the demand graph **separating independent strands** the scheduler
owns. A kernel is a maximal *sequential* segment within a strand; you cannot JIT
across a suspension, and you cannot JIT two strands as one — kernels are jitted
**individually, never fused**. **Pure forks coalesce** into one kernel (`(a*2,
b*3)` — splitting buys nothing); **async forks split** into real strands
(`(http_a(), http_b())` overlap).

**The async criterion (a rule, not a list):** *async ⟺ the result is not a pure
function of its Vixen-value inputs* — it observes or affects the external world.
So `http`/`exec`/`acquire`/host-ops are async; `parse`/`pubgrub_solve` are sync
(pure functions of bytes/constraints). This classifies a *new* intrinsic without
editing a list. Note: there is no `discover` intrinsic — host facts come from the
control plane ([runners and the control plane](/architecture-and-infrastructure/)).

**Two orthogonal axes** (previously fused as "intrinsic"): **async**
(external/non-deterministic — defines the kernel boundary and strand splits) and
**persist-cache** (expensive-enough-to-memoize — a policy). `parse`/`pubgrub` are
the proof they differ: persist-cached but synchronous. The partition rides on the
**async** axis alone.

## Values are node references (the inside-out turn)

A value is **not** "resolved or pending" — it is a **reference to a node** in the
dataflow graph. "Resolved" is not a property of a value; it is a per-**node**
runtime fact: does the node have a memoised output yet. A constant is a trivial
node; a struct is a node whose slots reference its field nodes; `💣` is a node
*referenced but never evaluated*. The graph is the same at analysis time and at
runtime — runtime just adds the memo table. The value *is* the node reference;
whether its node has run is separate.

**Cost caveat (load-bearing):** done naively, a node-reference-per-everything is
expensive. The representation must keep cheap things cheap — a scalar plainly,
unconditionally needed lives in a kernel's registers, **not** a heap node. Which
positions deserve to be nodes (deferred / shared / independently-demanded) vs
inlined into a kernel is the analysis below; getting it wrong is where this design
gets slow.

## Lazy control flow vs lazy data — what a kernel is

The distinction that lets a kernel be **eager** without detonating bombs:

- **Lazy control flow is free in an eager kernel.** `if c ( x ) else ( 💣 )`
  short-circuits — eager evaluation computes `c`, takes the live branch, never
  touches the dead one. So `if`/`match` stay **inside** kernels, JIT'd, eager.
- **Lazy *data* is what a kernel cannot do.** `{ a: 42, b: 💣 }` would eagerly
  evaluate both fields. So **lazy-data construction lifts out into graph nodes**:
  a struct/tuple/array whose fields are independently demandable becomes a node
  referencing its field-nodes.

So a kernel is eager straight-line compute **including** control flow, excluding
**boundaries** (intrinsics / `exec`) and **lazy-data construction**. You cut at
boundaries and at independently-demandable data positions — not at every value or
branch. Consequence: **the JIT only ever compiles eager code** (the easy
copy-and-patch kind). No "lazy JIT" is needed.

## The cost model — pay for laziness only where it's used

The guiding principle: **where laziness isn't needed, the code is exactly as fast
as an eager language.** You pay the node/memo cost *only* at genuinely
lazy/shared/effectful positions; everywhere else is unboxed register compute.

- **Node cost is a *seam* cost, not a per-op cost.** Inside a kernel there are no
  nodes — the JIT **scalarizes** values into registers (the ABI words). NodeRefs
  appear only at seams: a kernel's lazy/deferred inputs, its node-producing
  outputs, lazy-data construction, boundaries. Large kernels ⇒ rare seams ⇒
  amortized cost. This is GHC **worker/wrapper**.
- **Scalarization is recursive** — a struct field inside an array inside another
  struct, all eager, scalarizes wholesale into registers; one lazy position deep
  inside must not box its eager container (GHC per-field strictness).
- **Three tiers:** **Tier 0 — register** (kernel-local, eager, unshared;
  recomputed if needed; the common case); **Tier 1 — in-memory node** (memoised
  this run; arena `NodeId = u32`, flat-array memo); **Tier 2 — persist node**
  (content-addressed across runs — the boundary intrinsics).
- **Tier-0-vs-Tier-1 is unsolved** and needs a cost heuristic (recompute-cheap vs
  memo-overhead) — the load-bearing analysis; GHC demand analysis to raid.

## An assembly of three precedented things

"Demand-driven" is more than lazy evaluation, but every ingredient is
precedented — we inherit the vocabulary and assemble proven parts, not gamble on
research:

1. **Lazy evaluation with eager islands chosen by demand/strictness analysis —
   GHC.** Vixen's "lazy-data lifts to nodes, eager compute stays in kernels" is
   GHC demand analysis turned inside out.
2. **A memoised demand-driven dependency graph — build systems** (Bazel, Shake,
   Adapton) **and Jane Street's `incremental`.** Demand pulls; nodes memoise;
   content-addressed.
3. **Copy-and-patch JIT** (Xu & Kjølstad, 2021) — fast codegen of small
   straight-line code. Vixen's kernels.

**The novel part is the combination:** a *fine-grained lazy-data* dynamic
dataflow graph **+** *memoised / content-addressed* demand evaluation **+**
*copy-and-patch-JIT'd* straight-line pure interiors — all three at once.

## The graph is built at runtime, not compiled once

**There is no static analysis in a JIT'd language.** `import` is a function call
that resolves to something; a package can be **downloaded mid-invocation**; an
`exec` produces a value of runtime-determined shape that streams into other nodes.
So the inside-out turn (code → graph) is an **ongoing process during evaluation**,
not a compile-time pass — dynamic from the start. What we *can* do statically is a
**snapshot**: turn the code currently in hand inside out, honest about being one
frame of a moving picture. That snapshot is what the playground renders — useful,
never mistaken for "the" graph.

## Values, pending, remote handles

`Pending(String)` annihilated structure the model needs. Two distinctions it
couldn't express:

- **Pending is structured** — it carries a resolver (local-kernel / remote-handle
  / stream), not a label.
- **Pending applies to identity *and* materialization, independently:** a *local
  input tree* has identity **resolved** (hashed) but bytes **pending** (lazy
  transfer); an *`exec` output tree* has identity **pending** (discovered
  post-run) *and* bytes pending; a *CAS tree* has both resolved.

**Remote handle** — a value whose parts resolve via another host, bidirectional:

- *Inbound* (orchestrator → executor): a mounted tree is **not** shipped whole;
  the remote's file opens are the demand, bytes served as read.
- *Outbound* (executor → orchestrator): `exec` returns a **bundle of
  independently-resolving handles** — stdout stream, the observer's value, **and
  the output tree itself** — each resolving at its own time, at per-kind
  granularity (tree → path, stdout → chunk, observer → field, scalar → atomic).
  Forcing an unproduced part **blocks** the strand until it lands; a part the
  producer never makes **poisons**.

So the demand model **spans the host boundary**: a value can be pending *because
another host hasn't produced that part yet*. This is built — see
[The strand scheduler](/strand-scheduler/) (the park-on-a-remote-part mechanism)
and [exec](/exec/) (③ produced outputs).

## `Tree` — the richest primitive

A `Tree` is identified entirely by its content hash (merkle, structurally
shared). The carving:

- **Identity vs materialization.** Identity is the hash; materialization is where
  the bytes live (local fs / CAS / executor sandbox). Backing does not affect
  identity. **Ingest eagerly (hash everything, no transfer); materialize lazily
  (only what's opened).** A complete cache key with zero bytes sent.
- **Inputs vs outputs.** Input identity is (largely) knowable eagerly. **Output
  identity is not** — the tree `exec` returns is whatever the command writes; its
  hash is *discovered*, post-hoc. An `exec` output is an **identity-pending tree**,
  which is precisely why it can't be analyzed statically — by construction.

A `Tree` value is in one of two states:

- **Resolved** — a CAS `NodeHash` at a `DirectoryNode` (name-sorted entries,
  `Dir{hash}` subdirs, small/large/symlink leaves; chunks CDC-split). Immutable,
  complete. `build_merkle_tree` ingests; the VFS `mount` materializes lazily,
  serving bytes from CAS on open.
- **Producing handle** — a live reference to a running `exec`'s output. Parts
  resolve **progressively, per path**: forcing `tree[path]` yields that path's
  bytes *as soon as the producer has written it*, **blocks** on a path not yet
  written, and **poisons** if the producer completes without it. The whole-tree
  `NodeHash` is identity-pending until the producer finishes.

**Why a producing handle is mandatory — Cargo pipelining.** `rustc` emits
`libA.rmeta` at end-of-type-check, long before `libA.rlib`. Crate B's compile, on
another executor, reads `libA.rmeta` out of A's output tree *while A is still
producing `libA.rlib`* — before A's whole-tree hash can exist. An atomic,
resolve-at-completion output tree cannot express this, and pipelined Rust builds
*are* the workload. See [Design build-output materialisation](/materialise-build-output/).

**Operations.** *Slicing* `tree[path]` walks the `DirectoryEntry`s to the child
`Dir{hash}` (cost O(depth) cached `get_node`s; materialization stays lazy). On a
producing handle it is the per-path progressive demand above. *Overlay/merge* `a ⊕
b` is a new content-addressed `DirectoryNode`, deep-merge on directory collisions,
**right operand wins** on leaf collisions (so `inputs ⊕ outputs` layers outputs
over inputs); tentative — whether an output should clobber an input is unsettled.

## Caching, sandbox

Two cache levels, detailed in [Cache and trust](/cache-and-trust/): **L1** — the
key, computed *before* running, from declared inputs by identity (governs lookup,
never the read-set, never outputs); **L2** — read-set validity, recorded *after*
a run, to decide whether an L1 miss actually needs a rerun. Two levels are
necessary, not an optimization: you frequently can't pre-declare the minimal input
(include paths, `__has_include`, response files), so you must mount broadly ⇒ L1 is
forced broad ⇒ the read-set (L2) recovers precision. Soundness rides on
command-model's impurity marks.

Two distinct mechanisms make a run hermetic and trackable, and they are **not** the
same thing:

- **The VFS** (FUSE / FSKit) — the process reads and writes *through* it, so it
  **records** reads/writes to/from **mounts**: that's the **read-set** (L2) and the
  **write-set** (the output tree). Read-set capture and lazy materialization are the
  same VFS interception (Linux/macOS: confident; Windows: ProjFS gives the read-set
  but not fully-lazy transfer — they can decouple).
- **The sandbox** (landlock-exec / sandbox-exec) — **restricts** which host paths
  the command may read/write. Enforcement, not observation.

**Exemptions** are the gap: host paths reached *directly, outside the VFS* (a
discovered system toolchain), so the VFS cannot record them — untrackable by
construction, hence identified by **fingerprint** (the probe observations),
never read-tracked. See
[the cache-key + sandbox coupling](/cache-key-and-sandbox-coupled-design/) and
[the Windows sandbox direction](/windows-sandbox-direction/).

## Observability

- **The demand graph is a first-class, facet-serializable artifact** — the
  runtime runs it, tests assert over it, the playground renders it. One artifact,
  three consumers.
- **Playground** — Vixen source left, demand graph right (vite + ts + react +
  vox; TS types from the facet defs). Click a node: type, inputs,
  kernel-vs-boundary, reachability from the root.
- **Failing test → playground.** A failing case renders its graph to an image
  with reachability coloring an LLM can view directly. The bomb shows as a **dark,
  unreached node**; an engine that forces it lights up a node that should be dark.
  The picture is the oracle.
- **Emit only what's real.** A missing firehose event is honest "not built yet"; a
  zero-filled one is a lie. The taxonomy is a conformance surface (an eager
  interpreter can't fill it honestly) — keep it, never fabricate.

## Status & open questions

**Built** (`vx-runtime`): the demand-graph extraction; kernel-boundary design
pressure in the graph model; I6 source-fidelity direction (spans ride the IR);
[the strand scheduler](/strand-scheduler/) (I1–I7, on stackful coroutines); the
cross-executor remote-part handle.

**Being rebuilt**: the kernel IR, interpreter oracle, kernel dispatch, and native
copy-and-patch JIT. The intended replacement is to lower eager pure interiors to
Weavy's shared program/block representation so interpretation and native codegen
consume one artifact.

**Open:**

1. **Kernel boundary, precisely** — the async criterion is the current cut; the
   remote-handle case (forcing isn't clean materialization) refines it.
2. **Caching mechanics** — the L1 key/slot structure that avoids churn on volatile
   inputs (a stable `(signature + fingerprints)` slot carrying read-sets, not the
   volatile tree hash); L2 retention; preemptive push of changed read-set files.
   See [Cache and trust](/cache-and-trust/).
3. **Sandbox platform feasibility** — per-platform interception; L1-only fallback;
   lazy→eager degradation.
4. **Demand-graph granularity** — what a node/edge/annotation is; the Tier-0-vs-1
   cost heuristic; coalescing pure regions.
5. **The type-level partition** — is "touches an intrinsic" the right criterion,
   given pure intrinsics only need to be a boundary for *caching policy*, not
   correctness? (Not yet convinced.)

## Related

- [The strand scheduler](/strand-scheduler/) — the demand-driven concurrency
  (I1–I7) the runtime evaluates on.
- [exec](/exec/) — the build primitive; ③ produced outputs ride the producing
  handle.
- [Cache and trust](/cache-and-trust/) · [Cache key + sandbox](/cache-key-and-sandbox-coupled-design/).
- [Design build-output materialisation](/materialise-build-output/).
