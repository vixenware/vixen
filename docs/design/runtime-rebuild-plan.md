# Runtime rebuild — implementation plan

**Status: plan.** Sequenced. Each phase ends in a **committed, known state**
(working, or *honestly red*). No demolition happens ahead of its replacement.

Inputs this plan synthesizes: [`kernel-abi.md`](kernel-abi.md) (the rebuild order
+ kernel ABI), [`runtime-semantics.md`](runtime-semantics.md) (demand model, value
representation, exec demand profile, two-level cache, the teeth-bomb),
[`runners-and-control-plane.md`](runners-and-control-plane.md) (executor model),
the normative spec (`r[eval.*]`, `r[impl.*]`).

---

## Goal & non-negotiables

Replace the **eager** evaluator with the **demand-driven** runtime — a lazy demand
sea with eager pure kernels. The plan must preserve these *structural incentives*
(the things that have failed every prior attempt when omitted):

1. **Pending is a property with a resolver, not a `String` and not a variant.**
   No `Pending(String)`. A value is `Resolved(payload) | Pending(resolver)` at any
   position; the orchestrator never holds a bare scalar, so eager extraction
   (`expect_u64`) does not typecheck.
2. **No flat ISA.** The runtime consumes a **demand graph**, not a `KernelOp`
   bytecode a tree-walker matches. Kernels are eager pure islands, not op-soup.
3. **The bomb detonates on *force*.** `💣` is a value whose forcing poisons; an
   eager engine that over-forces goes **red**. The conformance tripwire.
4. **The demand graph is a first-class, inspectable, facet-serializable artifact**
   — the thing the runtime runs, tests assert over, and the playground renders.
   An eager tree-walker has no graph to show; you cannot fake a correct picture.
5. **Keep the VIR front-end; replace only the back-end.** Never leave the system
   without an evaluator — the new engine comes up *alongside*, the old is removed
   only at cutover.

## The seam (keep / replace)

**Keep:** `vx-parser`; `vx-ir` **front-end** (`build.rs`, `type_collect`,
`resolve`, `type_check`, `cap_check`, `capture`); the spec docs; `command-model`;
`store`/`vfsd`; `vx-runner` (executor worker, adapted).

**Replace (removed at cutover, Phase 5):** `vx-ir` **back-end** (`demand.rs`,
`lower.rs`, `exec_types.rs` = `KernelOp`/`ExecutableProgram`); `vx-daemon`'s
`runtime_eval` **eager core** (`EvalValue`, `ProgramEvaluator`, `eval_op`); the
`vx-rt` crate (whole).

**New crate `vx-runtime`:** owns the value model, the demand graph, the kernel
interpreter, the demand-driven evaluator, and (later) the JIT. Consumes VIR.
**Linked by both** the orchestrator (`vx-daemon`) and the executor (`vx-runner`),
per `i[impl.exec.shared-evaluator]`.

---

## Phases

### Phase 0 — The spine: value model + demand-graph type

Stand up `vx-runtime` with **just the types**, no engine:

- **Value** — `Resolved(payload) | Pending(resolver)`. `resolver ∈ { LocalKernel,
  Boundary, Stream }` (and the two pending axes: identity-pending vs
  materialization-pending). `payload` uses the kernel-ABI canonical layout (shared
  payload + runtime envelope) and **can represent** scalars, pairs, structs,
  closures, trees — collapsing the `EvalValue`/`RuntimeValue` split.
- **DemandGraph** — a facet-serializable DAG: nodes (computation/value + type +
  `kernel | boundary` + reachable-from-root), edges (data deps).

*Deliverable:* the types + (de)serialization; round-trip tests; hand-built small
graphs. *Incentive locked:* `Pending(String)` is unrepresentable; no bare scalar
escapes. **Highest-leverage phase — everything conforms to these types.**

### Phase 1 — VIR → DemandGraph (the partition)

Replace `demand.rs`'s eager-planning with graph construction:

- Build the demand graph from VIR.
- **Partition** into kernels (maximal pure regions) by the **type-level**
  capability/intrinsic boundary (`kernel-abi.md`: purity is visible in the types —
  *not* a dataflow heuristic). Mark boundary nodes (`exec`, `acquire`, `Http`, …).

*Deliverable:* `VIR → DemandGraph`, inspectable. No evaluation yet. *Resolves
incrementally:* the exact kernel boundary (force vs intrinsic), strand structure.

### Phase 2 — Kernel interpreter (correctness before speed)

Interpret one kernel: eager, to completion, over canonical payloads. **No JIT.**

*Deliverable:* pure kernels evaluate; unit tests over kernels.

### Phase 3 — Demand-driven evaluator (the sea) + teeth-bomb

The orchestrator loop: resolve a demanded node → **force its inputs
(demand-guided — only what's reached)** → run the kernel (Phase 2) or (Phase 4)
dispatch the boundary → memoize. Pending-by-default; force only at the two
boundaries (`r[eval.forcing]`). Make `💣` a real detonator (forcing poisons).

*Deliverable:* pure demand-driven programs evaluate; **`demand_basis.vx` passes
because the bomb node is unreached, not by accident.** *Conformance gate:* the
teeth-bomb suite is green only on a genuinely demand-driven engine.

### Phase 4 — Boundaries: exec demand profile + executor abstraction

Implement boundary resolution per the **exec demand profile**
([`runtime-semantics.md`](runtime-semantics.md) §5): **upfront → L1 key**,
**progressive → read-set**, **produced → forced by the requester**. Introduce the
`Executor` trait + a **local-sandbox executor double** (via `vx-runner`). Mounts /
exemptions; read-set capture where the VFS supports it.

*Deliverable:* `exec` works through the real engine; `runtime_eval`'s in-file exec
tests pass on the new path.

### Phase 5 — Cutover + demolition

Point the `vx-daemon` orchestrator + test runner at `vx-runtime`. **Then** delete:
the `runtime_eval` eager core, the `vx-ir` back-end (`demand`/`lower`/`exec_types`),
and `vx-rt`. Move the `.vx` lang fixtures to where the real test runner discovers
them (preserve the data; drop the fake harness).

*Deliverable:* **one** engine. The lang suite runs on the real engine — green where
the engine is correct, honestly red where it isn't yet.

### Phase 6+ — Performance & completeness (incremental, per `kernel-abi.md`)

Copy-and-patch JIT (tiering: interpret → compile hot kernels); struct/enum layout
descriptors; unify kernel shipping with `exec` observers / `pubgrub` closures; the
L1/L2 cache **storage** (the `ccc` seam); the demand-graph **playground** (code ↔
graph, LLM-readable visual artifact) and the firehose `DemandGraph`/`Jit` events,
which become *real* (no longer fabricated) once the graph and JIT exist.

---

## Ordering notes

- **Teeth-bomb can land early.** Making `💣` forcing-poisons *before* Phase 3 turns
  `demand_basis.vx` honestly **red** on the current eager engine — proving it's
  eager. An optional first structural-incentive commit.
- **Every phase commits**; the tree is always in a known state.
- **Observability is free from Phase 0:** the demand graph is serializable from the
  start, so the playground and the firehose hang off the same artifact.

## Open semantic questions the rebuild resolves as it goes

Not blockers — each is settled inside the phase that needs it:

- exact **kernel boundary** (force vs intrinsic; pure-intrinsic-as-call) — Phase 1.
- **remote-handle** forcing and **streams** — Phase 4.
- **caching storage** (L1 slot/read-set; the `ccc` relationship) — Phase 6.
- **strand granularity** / scheduler (magical concurrency) — Phase 3/4.
