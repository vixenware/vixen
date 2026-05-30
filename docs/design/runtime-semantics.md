# Runtime semantics — working notes

**Status: working notes from a design session. Non-normative.** This records the
vocabulary, the model, and the open questions we worked out for the
demand-driven runtime, `Tree`/`exec`, remote handles, and caching. Some sections
**correct existing spec text** (flagged inline). The goal of this document is to
give *names* to a set of things that are related but separate, so we stop
collapsing them into each other.

Cross-refs: [`spec/language/build.md`](../content/spec/language/build.md)
(Evaluation, normative), [`spec/internals.md`](../content/spec/internals.md)
(representation, normative), [`kernel-abi.md`](kernel-abi.md) (kernels),
[`command-model.md`](command-model.md) (canonical commands),
[`test-runner.md`](test-runner.md) (firehose).

---

## 0. Why this document exists (the meta-problem)

The runtime has been rebuilt several times, and each time an agent (or a human in
a hurry) reaches for the **eager, imperative, local** shortcut, because the
codebase *affords* it. Concrete instances found in this session:

- **`vx-rt`** — a whole second evaluator (`eval_source`, `RtValue`): synchronous,
  eager, no boundaries, no demand graph. It was used as the `.vx` test path,
  which means the language was being validated against an engine that *isn't the
  language*.
- **`EvalValue::Pending(String)`** in the daemon's `runtime_eval` — "pending"
  implemented as a human-readable label, doubling as the not-implemented
  sentinel and the poison channel. A flat misreading of `r[eval.pending]`.
- **`vx-ir/src/demand.rs`** — its own header says it "mirrors `vx_analysis`…to
  avoid a dependency on the analysis crate." `vx-analysis` was supposedly
  *deleted*; the logic was relocated, not removed. Its model is `Observation =
  {NoObs, NeedValue="eager evaluation required"}` with a static `lazy_fields`
  allowlist — i.e. **eager-by-default with lazy exceptions**, the inverse of the
  spec.
- **`KernelOp`** — a flat instruction set mixing pure ops (`Add`, `Select`,
  `MakePair`) with boundary ops (`AwaitRuntime`, `BuildTree`) in one enum a
  tree-walker matches. It presupposes an interpreter and erases the
  kernel/boundary partition.

**The lesson: deletion + prose has failed every time.** The rejected concept
regrows under a new name in a new location. Enforcement has to be **types** (make
the wrong thing not compile) and **executable oracles** (make the wrong thing go
red), not documentation an agent skims past. Two concrete enforcers:

- the **demand graph as a first-class, inspectable artifact** (you can render it;
  an eager tree-walker has no graph to show), and
- the **bomb** `💣` as a conformance tripwire (detonates *only when forced*; an
  eager engine over-forces and fails).

---

## 1. Vocabulary — related but separate

The payoff of the session. Each line is a distinct thing that was previously
getting fused with its neighbours.

**Evaluation**
- **root demand** — the entry point's return value; all evaluation flows
  backward from it.
- **pending** — a value not yet resolved. A *property of any value of any type*,
  not a variant. (`r[eval.pending]`.)
- **resolver** — *how* a pending value becomes resolved. The thing
  `Pending(String)` threw away. Kinds: local-kernel, remote-handle, stream.
- **kernel** — an eager, pure, local fragment that runs to completion once
  entered. An eager island in the lazy demand sea. (`kernel-abi.md`.)
- **strand** — a chain of sequentially-dependent work the scheduler runs as one
  thread of progress. The unit of concurrency.
- **suspension point** — where a strand yields control back to the runtime.
- **async operation** — see the criterion in §2. Defines suspension points and
  strand splits.

**Effects / boundaries**
- **identity** (of a value/tree) — its content hash. *What it is.*
- **materialization** — where the bytes physically live, and getting them there.
  *Separate from identity.*
- **ingest** — compute identity (walk + hash). Eager, no transfer.
- **transfer** — move bytes to a location. Lazy, demand-driven.
- **remote handle** — a value whose parts are resolved by activity on another
  host, asynchronously and possibly independently. Bidirectional.
- **bundle of independently-resolving handles** — what `exec` returns: stream,
  exit, observer fields, each resolving at its own time.

**Caching**
- **L1 cache key** — broad, computed *before* running, from declared inputs by
  identity. Governs *lookup*.
- **read-set** — the specific files (path + content hash) a command actually
  consulted. Recorded *after* running. Provenance.
- **L2 validity** — using a recorded read-set to decide whether an L1 miss
  *actually* needs a rerun. Governs *rerun*, **never** the key.
- **declared inputs** vs **observed inputs** — what you mounted vs what was read.
- **command signature** — the canonical command identity (argv/env canonicalized;
  `command-model`). The thing L2 candidates are indexed by.
- **impurity mark** — a flag that a command is *not* a deterministic function of
  the bytes it reads (`__DATE__`, `-march=native`, mtimes). Breaks L2 soundness.

**Sandbox / mounts**
- **sandbox** — isolation *and* the instrument that measures the read-set. No
  command runs outside one.
- **fingerprint** — a cheap, reliable identity for an input that is expensive to
  walk but rarely changes (e.g. a C toolchain, known by version). Stands in for
  hashing every file.
- **tracked mount** — the spec's `mounts` (`r[exec.arg]`): fine-grained VFS
  content (`Tree`/`Blob`). Reads are intercepted ⇒ lazy transfer **and** read-set
  capture both possible ⇒ L2 available. The common case (a workspace).
- **fingerprinted mount** — the spec's `coarse_mounts` (`r[exec.coarse-mounts]`):
  a **host-backed sandbox exemption**, *not* content-addressed and *outside* the
  VFS, so its reads cannot be tracked. Its identity is a **fingerprint** — the
  probe/discovery observations attached to it (`host.which`, `host.hash`,
  version), rechecked on the runner. Fingerprint → L1 key; no read-set. The system
  C toolchain is the canonical case.

**Tooling**
- **demand graph** — the analyzed dataflow graph; the runtime's substrate, the
  test oracle, and the visualization's data. One artifact, three consumers.
- **playground** — code on the left, demand graph on the right; failing test →
  open in playground; emits an LLM-readable visual artifact.

---

## 2. Evaluation model — demand sea, kernels, strands

### Demand-driven, no build-planning phase

Demand starts at the entry point and flows backward. Building the program's
dataflow/demand graph from analyzed source is ordinary compilation. What is
**absent** is a Buck/Bazel-style up-front *build-planning* phase: the set of
boundary actions (`exec`, `acquire`, …) is not enumerated ahead of evaluation,
because their inputs depend on values only evaluation produces.

> **Proposed redraft of `r[eval.demand]`** (replaces "build"→entry point,
> disambiguates "no planning phase"):
>
> Vixen uses demand-driven dataflow evaluation. The runtime calls the entry
> point and needs its return value — that is the root demand; all evaluation
> flows backward from there. Constructing the program's dataflow/demand graph
> from analyzed source is ordinary compilation and is expected. What is absent is
> a separate *build-planning* phase in the Buck/Bazel sense: the set of boundary
> actions (`exec`, `acquire`, …) is **not** enumerated ahead of evaluation. Which
> boundaries run, and with what inputs, is discovered dynamically as demand
> reaches them — because those inputs depend on values only evaluation produces
> (`discover` results, file contents, resolved capabilities). The graph of *pure
> dataflow* may be analyzed statically; the graph of *effects* is
> demand-discovered.

### Pending by default; forcing is structured

Inside the orchestrator **nothing forces** (`r[eval.forcing]`): `a + b` with `a`
pending is itself pending; control flow propagates demand without blocking. A
pending value carries a **resolver** describing how it resolves — *not* a string.
Forcing is not a single clean "materialize everything"; see remote handles (§4).

### Kernels — eager islands in the lazy sea

A kernel is eager, pure, local, run to completion once its inputs are ready and
its result is demanded. It touches no capability and performs no async operation.
Pure intrinsics (`parse`, `pubgrub_solve`) may be **called from inside** a kernel
(the JIT emits a call). Codegen is copy-and-patch; interpret first, compile when
hot (`kernel-abi.md`).

### Strands — where concurrency lives

The user cannot spawn threads. The only source of parallelism is the demand graph
**separating independent strands** the scheduler owns ("magical concurrency,"
`r[eval.concurrency]`). Therefore:

- A **kernel** is a maximal *sequential* segment within a strand, between
  suspension points.
- You cannot JIT across a suspension (the strand yields there).
- You cannot JIT two strands as one (the scheduler owns when/where each runs).
  → kernels are jitted **individually, never fused**.
- **Pure forks coalesce** into one kernel (`(a*2, b*3)` runs sequentially —
  splitting buys nothing). **Async forks split** into real strands (`(http_a(),
  http_b())` overlap). Heavy data-crunching that *would* want parallelism is done
  Rust-side inside intrinsics, so Vixen-level concurrency is *only* across
  external-world operations.

### The async criterion (a rule, not a list)

> **async ⟺ the result is not a pure function of its Vixen-value inputs** — it
> observes or affects the external world (network, processes, host state).

Worked: `http` async, `exec` async, host-ops in a `discover` closure async,
`acquire` async (likely). `parse` **sync** (pure function of bytes),
`pubgrub_solve` **sync** (pure function of constraints). A criterion classifies a
*new* intrinsic without editing a list.

### Two orthogonal axes (previously fused as "intrinsic")

- **async** — external/non-deterministic. **Defines the kernel boundary, strand
  splits, host crossings.** `{http, exec, acquire, host-ops}`.
- **Persist-cache** — expensive-enough-to-memoize-to-disk. A **policy**. `{http,
  exec, acquire, parse, pubgrub_solve}`.

`parse`/`pubgrub` are the proof they differ: **Persist-cached but synchronous**
(cached because expensive, not because non-deterministic). The partition rides on
the **async** axis alone; caching is the separate (still-open) axis.

---

## 3. Values, pending, remote handles

`Pending(String)` annihilated structure that the model genuinely needs:

- **Pending is structured** — it carries a resolver (local-kernel / remote-handle
  / stream), not a label.
- **Pending applies to identity *and* materialization, independently:**
  - *local input tree* — identity **resolved** (hashed), bytes **pending** (lazy
    transfer). You know what it is; you haven't shipped it.
  - *`exec` output tree* — identity **pending** (discovered post-run), bytes
    **pending**. You don't even know its hash yet.
  - *CAS tree* — identity resolved, bytes resolved.

  This is the distinction `Pending(String)` could not express, and it is the
  whole game.

**Remote handle** — a value whose parts resolve via another host. Bidirectional:

- *Inbound* (orchestrator → executor): a mounted tree is **not** shipped whole;
  the remote's file opens are the demand, and bytes are served as read. The tree
  is still pending on the remote side.
- *Outbound* (executor → orchestrator): `exec` returns a **bundle of
  independently-resolving handles** — stdout stream, `exit`, observer fields —
  each resolving at its own time (cf. query-surface's independently-timed
  records: `rmeta` resolves off the stream the instant it's emitted, `exit` only
  at close).

So the demand model **spans the host boundary**: a value can be pending *because
another host hasn't produced that part yet*.

---

## 4. `Tree` — the richest primitive, under-specified

A `Tree` is identified entirely by its content hash (merkle, structurally shared;
`i[impl.tree]`). The session's carving:

- **Identity vs materialization.** Identity is the hash; materialization is where
  the bytes live (local fs / CAS / executor sandbox). Backing does not affect
  identity. **Ingest eagerly (hash everything, no transfer); materialize lazily
  (only what's opened).** Hashing ≠ transferring — you can have a complete cache
  key with zero bytes sent.
- **Inputs vs outputs.** Input identity is (largely) knowable eagerly. **Output
  identity is not** — the tree `exec` returns is whatever the command writes; its
  hash is *discovered*, post-hoc. An `exec` output is an **identity-pending
  tree**, which is precisely why it "can't be analyzed statically" — by
  construction, not by weakness of the analysis.
- **Provenance / states** — local-dir-backed, CAS-backed, executor-produced;
  ingested-but-not-materialized vs materialized; identity-resolved vs
  identity-pending.

**Open:** sub-tree slicing (`tree[path]`, O(1)), overlay/merge semantics, and the
full state machine of a `Tree` across these axes. `Tree` deserves its own spec
section; it does not have an adequate one today.

---

## 5. `exec`

The right frame (the demand-first reframe): don't model `exec` by *what crosses a
host* — model it by **what it demands, and when**. Demand is what forces; locality
is incidental. `exec`'s demand is **two-phase**, and the two phases *are* the two
cache levels (§6).

**① Upfront demand** — forced *before dispatch*, because it is what addresses the
cache:
- **argv** — fully resolved (the command identity; canonical via command-model).
- **env** — fully resolved.
- **observer** — code identity + resolved captures (it is a shipped kernel).
- **fingerprinted mounts** (`coarse_mounts`) — their fingerprint (rechecked probe
  observations); identity only.
- **tracked mounts** (`mounts`) — addressed by identity, but **not** contents.

*Forcing here* resolves the argv/env/capture dataflow and pins mount identities.
**No file bytes move yet.** → This set **is the L1 key**. Check the cache; hit ⇒
done, never dispatch.

**② Progressive demand** — discovered *during execution*; the remote tells us what
it needs:
- **tracked-mount file contents** — demanded per-read as the command opens each
  file.

*Forcing here* transfers that file's bytes to the executor (pull, or push if
predicted). Each read is recorded. → This set **is the L2 read-set**.

**③ Produced** — *not* demands; outputs that resolve over time, forced **only by
the requester peer** (the peer that issued the `exec`, transitively up to the root
demand), and only for the parts it actually demands:
- **exit code** — at completion.
- **stdout / stderr** — incremental streams.
- **output tree** — identity discovered as it is written; materialized into the
  store.
- **observer's value** — fields resolve progressively (`rmeta` early, `rlib`
  late). The observer runs **on the executor** as a shipped kernel
  (`i[impl.exec.shipped-closures]`, `i[impl.exec.shared-evaluator]`); its fields
  cross back only as the requester forces them.

So an `exec` is **inert except for demand from both peers**: the executor pulls
*inputs* (the reads, ②), the requester pulls *outputs* (③). Neither side pushes —
push is only the predictive optimization on the input side.

**The template generalizes.** Every intrinsic is a triple `(upfront, progressive,
produced)`, and **"synchronous" just means "no progressive phase"**:
- `parse` — upfront: the whole input; progressive: none; produced: the AST (sync).
- `Http` — upfront: the request; progressive: none; produced: the response.
- `pubgrub_solve` — upfront: solver + domain-closure identity; progressive: the
  consulted `(P→versions)`/`(P,V→deps)` facts (its read-set); produced: the
  resolution.

That lines up with the async criterion (§2): external/non-deterministic ⟺ it
discovers demands as it runs ⟺ it has a progressive phase.

**Open seam:** exactly how a *tracked* mount enters the L1 key vs. the read-set
(the don't-churn-the-volatile-key question, §6) — left to the storage design.

---

## 6. Caching — two levels

The load-bearing correction: **the cache key must be computable without running
the command.** Therefore the read-set can *never* be part of the key (else you'd
execute to compute the key that decides whether to execute — circular). And:
**cache keys are over inputs, never outputs.**

### L1 — cache key (lookup, *before* running)

`(command signature, env, mount tree hashes)` — declared inputs by identity. The
mounts are the *whole declared input* (e.g. the entire workspace), by content
hash: broad, possibly over-approximate, but knowable up front. Exact hit ⇒ reuse
the output, never execute. **This is `r[eval.memoization]` / build.md:119, and it is
correct as written.**

### L2 — read-set validity (rerun avoidance, *after* an L1 miss)

An L1 miss means "this declared-input bundle is new," **not** "rerunning produces
something new." A prior run of the same command signature recorded its
**read-set** (paths + content hashes actually consulted). On an L1 miss, before
executing, validate a candidate prior result: are all files in its read-set
byte-identical in the current inputs? If yes ⇒ the output still holds (reuse it,
index under the new L1 key). If a *read* file changed ⇒ run. **The read-set
validates; it never looks up.**

*Example.* Whole codebase mounted as input; a `cc` command reads a handful of
files. Edit a file `cc` never read ⇒ L1 key moves ⇒ L1 miss ⇒ L2 finds none of
`cc`'s read-set changed ⇒ reuse, no rebuild. This is the gcc `-MD` depfile
discipline, generalized.

### Why two levels are *necessary* (not an optimization)

You frequently **cannot pre-declare the minimal input** — what a command reads is
decided at runtime (include search paths, `__has_include`, response files). So you
*must* mount broadly ⇒ the L1 key is *forced* broad ⇒ which would over-rerun on
any workspace change ⇒ so the read-set (L2) is *required* to recover precision.
The dynamism of reads forces the split. (This is command-model's
"discovered-from-content inputs.")

### Soundness

> If a command is a deterministic function of the bytes it reads, then *every
> file in the recorded read-set unchanged* ⇒ *it reads the same set and produces
> the same output* — because what gets read next is itself a function of what's
> been read so far.

Non-determinism breaks the theorem; that is exactly what command-model's
**impurity marks** exist to flag. So the property that makes L2 sound is the one
command-model already tracks. Three pieces snap together: build.md's broad key
(L1), command-model's signature + impurity (L2 candidate + determinism escape),
the firehose's provenance (read-set recording).

### Mount kind drives the strategy

Not all inputs cache the same way; the *kind of mount* decides:

- **Fingerprinted inputs** (`coarse_mounts`) — host-backed exemptions, identified
  by rechecked probe observations; not content-addressed, not trackable (outside
  the VFS). The **fingerprint goes in the L1 key**; no read-set. Read-tracking is
  impossible here and unnecessary (same probe ⇒ same facts). Their reuse is
  validated by *rechecking the probe observations* on the runner — the
  fingerprinted-mount analogue of the read-set.
- **Tracked inputs** (`mounts`) — content in the VFS; commonly volatile (a
  workspace). The full tree hash changes on almost every edit, so **storing it as
  an L1 key is near-useless** — a flood of single-use keys that never hit again.
  Reuse comes from **L2 read-set validation**, not from the volatile whole-tree
  hash. A tracked input contributes to *rerun avoidance* (read-set), not to a
  stored key.

Open consequence: the L1 key looks like `(command signature +
fingerprinted-input fingerprints)` — the *stable* part — under which results
carry their read-set; the volatile tree hash should **not** churn the key. But
"don't store the volatile whole-tree key" does **not** mean "no lookup": there
must still be a **declared-inputs→read-set lookup** — from a command and its
declared inputs to the candidate prior results and their recorded read-sets —
plus the store that holds
those results. Where and how that index lives, and how it's kept from growing
without bound, is its own design problem (§10). Exact key/slot structure is TBD.

### Push, don't only pull

On a rerun, the orchestrator already knows the read-set and which of its files
changed (it holds the new hashes). So it can **push the changed read-set files to
the executor preemptively**, instead of waiting for the executor to pull them on
demand. Pull is the default; push is the latency optimization for the
known-changed set.

### Charter rot to fix

[`stdlib/build.md:849`](../content/spec/stdlib/build.md) — "the solve **caches on
the read-set it consulted** … the same read-set discipline as the sandbox" — and
[`:151`](../content/spec/stdlib/build.md) — "provenance for **cache keying**" —
both describe the read-set as if it *keys* the cache. Fix: introduce the
**key vs read-set** vocabulary; `r[eval.memoization]` stays the L1 key; a new rule
states the read-set governs *invalidation/rerun and never the key*; pubgrub's
`(P→versions, P,V→deps)` facts are an L2 read-set, not a key.

---

## 7. Sandbox & read-set measurement (the fork point)

- **The sandbox is the instrument that measures the read-set.** No command runs
  outside a sandbox, because outside it there is no read-set — hence no L2.
- **Read-set measurement requires read *interception*, which requires a
  fine-grained VFS mount.** The same VFS does double duty: **lazy materialization**
  (serve bytes as opened) **and** **read-set capture** (record what's opened) —
  they are the same interception.
- **Fingerprinted mounts (`coarse_mounts`) are a deliberate mode, not a
  fallback.** They are host-backed exemptions (a discovered system toolchain),
  *outside* the VFS, so their reads are inherently untrackable — and that's fine,
  because the identity is the rechecked **fingerprint** (probe observations), not
  tracked content. Fingerprint → L1 key; no read-set.

So there is a coupling worth stating plainly: **L2 (and lazy transfer) are
available exactly where the input is a tracked (VFS) mount on a platform that can
intercept reads.** Two different reasons read-set capture is unavailable:
**fingerprinted by design** (a host-backed `coarse_mount` — never tracked, by
construction) or **forced** (a tracked mount on a platform that can't intercept —
a real limitation, below). Don't conflate them.

**Platform feasibility — the fork:**
- **Linux** — a virtual filesystem works. **Confident.**
- **macOS** — a virtual filesystem works. **Confident.**
- **Windows** — no easy VFS. The **Projected File System** gives
  *notify-on-first-read* (files materialize on first access). That is coarser
  than full interception and the materialization is eager-on-touch rather than
  fully lazy, but it does yield the *set of files read* — so it is likely **good
  enough for the read-set (L2)**, just not for fully lazy transfer. Full VFS
  (WinFSP, Dokany, …) means either a Windows kernel driver (significant
  engineering) or a paid license (not affordable now) — parked as a future
  option.

So Windows hints that **read-set capture and lazy transfer can decouple**: you
might get the read-set (L2) without full lazy materialization. The clean
Linux/Mac case gives both from the same VFS interception.

**Open questions for the fork:** exact interception mechanism per platform; what
the runtime does on mounts without read-set capture (L1-only? refuse?); how lazy
materialization degrades to eager where full VFS isn't available; the
relationship between the VFS, the store, and the executor.

---

## 8. Observability & tooling

- **Bring back an analysis crate** — but as the producer of a first-class,
  facet-serializable **demand graph**, *not* the old eager-planning `vx-analysis`.
  Same input (VIR), opposite purpose: the graph is the **product** (the runtime
  runs it, tests assert over it, the playground renders it), not a partition
  thrown away after lowering.
- **Playground** — Vixen source left, analyzed demand graph right (vite + ts +
  react + vox; TS types generated from the facet defs). Click a node: type,
  inputs, kernel-vs-boundary, reachability from the root.
- **Failing test → playground.** A failing case renders its demand graph to an
  **image with reachability coloring** an LLM can view directly — better than
  thousands of lines of JSON. The bomb shows as a **dark, unreached node**; an
  engine that forces it lights up a node that should be dark. The picture is the
  oracle.
- **Firehose taxonomy is the target, not theater.** `DemandGraph {nodes, edges,
  lazy_fields, reachable_bodies}`, `Jit {strategy, code_bytes}`,
  `SlotForced`/`SlotProduced`, `CacheLookup` describe exactly what we want out of
  a test system, *and* are a conformance surface (an eager interpreter cannot fill
  them honestly). Rule: **emit only what's real** — a missing event is honest
  "not built yet," a zero-filled one is a lie. Kill the fabrication, keep the
  taxonomy and the emission seam.

---

## 9. Demolition & rebuild

**KEEP**
- `vx-parser` (whole).
- `vx-ir` front-end: VIR construction (`build.rs`), `type_collect`, `resolve`,
  `type_check`, **`cap_check`** (the partition rides on capability types),
  `capture`, arena/expr/stmt/pattern/scope/item/def/ids/diagnostics.
- The charter: `build.md`, `internals.md`, `kernel-abi.md`; the firehose
  taxonomy + emission seam; `command-model`; store / vfsd.

**KILL**
- `crates/vx-rt` (whole crate) and its dev-dependents' use of it.
- `vx-daemon/src/runtime_eval.rs` eager core (`EvalValue`, `ProgramEvaluator`,
  `eval_op`); read the boundary machinery before it goes (it's local-exec, the
  wrong model, but informative).
- `vx-ir/src/exec_types.rs` (`KernelOp` / `ExecutableProgram` / `ResultSource`) —
  the flat ISA. The load-bearing deletion.
- `vx-ir/src/lower.rs` (lowers to the flat ISA).
- Fabricated firehose details (zero-filled `DemandGraph`, never-emitted `Jit`).

**ASSESS (do not delete on an unproven premise)**
- `vx-ir/src/demand.rs` — *not* condemned as a "planning phase" (that argument was
  wrong; building a demand graph is fine). It is the **old dataflow way** to
  compute a partition the **type-level** approach (capabilities/intrinsics as the
  boundary) intends to replace — and that type-level approach is *days old and not
  yet proven*. Keep until it is. Separately verify whether its `lazy_fields`
  allowlist actually contradicts pending-by-default (read the body, don't pattern-
  match the doc comment).

**Rebuild order** (from `kernel-abi.md`): partition pass → kernel interpreter →
layout descriptors → value↔ABI bridge → copy-and-patch backend → unify with
closure shipping. The runtime consumes **VIR directly**; no demand-planning pass;
pending by default; eager only inside kernels.

---

## 10. Open questions (consolidated)

1. **Kernel boundary, precisely.** Force boundary vs async boundary vs intrinsic
   boundary — the async criterion (§2) is the current best cut, but the
   remote-handle case (forcing isn't clean materialization) is unsettled.
2. **Remote-handle model.** What crosses as a handle vs materializes; how parts
   resolve independently; how forcing a handle streams.
3. **Caching mechanics.** L1 key/slot structure that avoids churn on volatile
   inputs (a stable `(signature + fingerprints)` slot carrying read-sets, *not*
   the volatile tree hash); L2 candidate retention; preemptive push of changed
   read-set files; how non-determinism is handled end-to-end via impurity marks;
   interaction with the store.
4. **Sandbox platform feasibility (the fork).** Per-platform interception; L1-only
   fallback; lazy→eager degradation.
5. **Demand-graph granularity.** What a node/edge/annotation is; coalescing pure
   regions; where strands split exactly.
6. **`demand.rs` fate** — gated on whether the type-level partition holds.
7. **Spec work — landed and remaining.** *Landed:* the `r[eval.demand]` redraft;
   `r[eval.readset]` (read-set is rerun-validity, never the key); the corrected
   pubgrub wording. *Remaining:* `r[eval.forcing.boundary]` overstates the
   boundary ("inputs fully resolved before shipped," "output fully materialized")
   and must be revised once **remote handles** are specified; a real `Tree`
   spec section (§4); the full L1/L2 storage/lookup mechanism (§6).
8. **The type-level partition itself** — is "touches an intrinsic" the right
   criterion, given pure intrinsics (`parse`) only need to be a boundary for
   *caching policy*, not correctness? (Amos: not yet convinced.)
