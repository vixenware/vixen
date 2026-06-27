# AGENTS.md — Vixen build system

Vixen is a **demand-driven** build system: the language evaluates lazily,
commands run on executors, everything is content-addressed and cached.

## If you touch the runtime / evaluator / build semantics — READ THESE FIRST

The runtime is **mid-rebuild**. The old eager evaluator has been retired; do not
reconstruct a mental model from older notes that mention
`vx-daemon/src/runtime_eval.rs` or `vx-rt`. Those eager paths must stay gone. The
current execution boundary is the graph-runtime split (`vx-runtime-graph`,
`vx-runtime-core`, `vx-runtime-driver`) plus the transitional public
`vx-runtime::graph_exec` / `vx-runtime::eval` edge.

- [`docs/design/runtime-graph-execution/README.md`](docs/design/runtime-graph-execution/README.md) —
  the current graph-execution checkpoint and anti-fallback checklist. **Start here.**
- The demand-driven runtime, in the **wiki** (`ddc serve wiki` from repo root,
  or `ddc serve vixen/wiki` from `/Users/amos/vixenware`):
  [`wiki/content/runtime-semantics.md`](wiki/content/runtime-semantics.md) (demand
  model, node-reference values, `Tree`, the cost model),
  [`wiki/content/strand-scheduler.md`](wiki/content/strand-scheduler.md) (the
  I1–I7 concurrency contract), and [`wiki/content/exec.md`](wiki/content/exec.md)
  (the `exec` synthesis — the build primitive).
- [`docs/design/kernel-abi.md`](docs/design/kernel-abi.md) — kernels (eager pure
  islands) + the copy-and-patch JIT / ABI.
- The wiki is becoming the spec corpus. `docs/spec` still exists as transitional
  source material, but do not treat it as a separate authority or add new spec
  content there unless Amos explicitly redirects the migration.
- [`docs/design/runners-and-control-plane.md`](docs/design/runners-and-control-plane.md)
  + [`docs/design/product-boundaries.md`](docs/design/product-boundaries.md) —
  execution/distribution architecture (and the app.vixen.rs / CCC / Vixen-build
  split, which the code currently conflates).
- Tracey is no longer a standalone repo tool here. Requirement tracking is being
  folded into the Dodeca-backed wiki/spec flow, and the old Tracey MCP/config
  path is temporarily not a thing. Do not recreate `.config/tracey/config.styx`;
  use Dodeca/wiki requirement tracking as it lands.

## Non-negotiables (these have been violated repeatedly)

1. **The evaluator is demand-driven, not eager.** A lazy *demand sea* with eager
   pure *kernel islands*. There is **no synchronous tree-walking interpreter**. If
   you find yourself writing `fn eval(expr) -> Value` that matches and recurses,
   stop — that is the failure mode being removed.
2. **A value is a *reference to a node*; "resolved" is a node's memo state, not a
   value variant — and never a `String`.** A pending value is just a node not yet
   evaluated. No bare scalar escapes the orchestrator (`r[eval.forcing]`).
   `Pending(String)` is the canonical mistake. Lazy *data* (struct/tuple fields)
   becomes graph nodes; lazy *control flow* (`if`/`match`) stays inside eager
   kernels. See the demand-driven-runtime + strand-scheduler wiki pages.
3. **No flat instruction set the evaluator matches op-by-op.** The runtime consumes
   a **demand graph**. Source-blind graph kernel payloads are allowed; the old
   VIR back-end `KernelOp`/`ExecutableProgram` shape must not come back.
4. **`💣` detonates when *forced*, not when constructed.** It is a demand tripwire.
   If `demand_basis.vx` passes for the wrong reason, or your engine forces the
   bomb, that is an *engine* bug — not a test bug.
5. **Keep the VIR front-end** (`vx-ir`: `build`/`type_collect`/`resolve`/
   `type_check`/`cap_check`/`capture`). **Replace only the back-end.**
6. **Never fork the evaluator to "make it run."** If something won't run, fix the
   real path or report it. Do not create a second engine — this happened with the
   retired `vx-rt`, and it makes the language validate against a lie.
7. **Cache key = inputs, computed *before* running. Read-set = post-hoc validity,
   never the key** (`r[eval.readset]`).

## Tests during the runtime rebuild

The target is the architecture, not "all green all the time." Green tests are
evidence only when they exercise the intended graph-driven path. A passing suite
through a cheat path, fallback, source tree-walker, scratch executor, or fake
oracle is negative evidence.

- Do not restore deleted architecture, add compatibility fallbacks, or keep a
  runtime-local cheat path just to make tests pass.
- Some tests are contracts; some are scaffolding that can hold the rebuild back.
  When a test enforces the wrong architecture, change, delete, or quarantine it
  in the same committed state as the architectural cut, with the reason recorded.
- Honest red states are allowed when the failure is an explicit unsupported
  graph/runtime boundary or a missing production service. Red is not acceptable
  when the proposed "fix" is to recreate the old eager evaluator or scratch
  process runner.
- Prefer integration tests that exercise Vixen language and production-shaped
  boundary paths. Do not let the current test suite define the architecture.
- Record failing test names and failure modes for honest red commits. The record
  must explain why the failure moves the code toward the specified model.

## Repo conventions

- No `serde` — use `facet` / `facet-json`. Never hand-emit JSON.
- No `println!`/`eprintln!` for diagnostics — use `tracing`.
- Never edit generated files — edit the source and regenerate.
- Tests: `cargo nextest run`, not `cargo test`.
- Commit work to avoid losing it; each rebuild phase ends in a committed state.

## macOS app / FSKit

Before touching `apps/mac`, `crates/vx-management`, or the VixenFS extension,
read [`apps/mac/FSKIT-REPAIR.md`](apps/mac/FSKIT-REPAIR.md). The important
rules are:

- Do not manually register VixenFS with LaunchServices as a repair mechanism.
  `lsregister -dump` and `lsregister -u` are diagnostic/cleanup tools only.
- Do not use `pluginkit -e use` in automatic app setup, the doctor refresh, or
  the one-click repair path.
- The app may open System Settings for user enablement, but it must not hide
  extension-registration mutations behind that button.
- The one-click repair path is cheap-first: direct mount, direct `kill(2)` of
  `fskitd`, mount again, then recent FSKit logs. Only if the persistent failure
  points at registration state should it pay the slow stale app-record cleanup
  cost and retry.
