# AGENTS.md — Vixen build system

Vixen is a **demand-driven** build system: the language evaluates lazily,
commands run on executors, everything is content-addressed and cached.

## If you touch the runtime / evaluator / build semantics — READ THESE FIRST

The runtime is **mid-rebuild** (the old eager evaluator is being replaced). Do not
form a mental model from the current `vx-daemon/src/runtime_eval.rs` or `vx-rt` —
they are the thing being deleted.

- [`docs/design/runtime-rebuild-plan.md`](docs/design/runtime-rebuild-plan.md) —
  the sequenced rebuild plan. **Start here.**
- The demand-driven runtime, in the **wiki** (`ddc serve vixen/wiki`):
  [`wiki/content/runtime-semantics.md`](wiki/content/runtime-semantics.md) (demand
  model, node-reference values, `Tree`, the cost model),
  [`wiki/content/strand-scheduler.md`](wiki/content/strand-scheduler.md) (the
  I1–I7 concurrency contract), and [`wiki/content/exec.md`](wiki/content/exec.md)
  (the `exec` synthesis — the build primitive).
- [`docs/design/kernel-abi.md`](docs/design/kernel-abi.md) — kernels (eager pure
  islands) + the copy-and-patch JIT / ABI.
- [`docs/content/spec/language/build.md`](docs/content/spec/language/build.md) —
  normative evaluation spec (`r[eval.*]`).
- [`docs/content/spec/internals.md`](docs/content/spec/internals.md) — value
  representation (`i[impl.*]`).
- [`docs/design/runners-and-control-plane.md`](docs/design/runners-and-control-plane.md)
  + [`docs/design/product-boundaries.md`](docs/design/product-boundaries.md) —
  execution/distribution architecture (and the app.vixen.rs / CCC / Vixen-build
  split, which the code currently conflates).

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
   a **demand graph**. (The old `KernelOp` is being removed.)
4. **`💣` detonates when *forced*, not when constructed.** It is a demand tripwire.
   If `demand_basis.vx` passes for the wrong reason, or your engine forces the
   bomb, that is an *engine* bug — not a test bug.
5. **Keep the VIR front-end** (`vx-ir`: `build`/`type_collect`/`resolve`/
   `type_check`/`cap_check`/`capture`). **Replace only the back-end.**
6. **Never fork the evaluator to "make it run."** If something won't run, fix the
   real path or report it. Do not create a second engine — this has happened
   (`vx-rt`, being deleted) and it makes the language validate against a lie.
7. **Cache key = inputs, computed *before* running. Read-set = post-hoc validity,
   never the key** (`r[eval.readset]`).

## Repo conventions

- No `serde` — use `facet` / `facet-json`. Never hand-emit JSON.
- No `println!`/`eprintln!` for diagnostics — use `tracing`.
- Never edit generated files — edit the source and regenerate.
- Tests: `cargo nextest run`, not `cargo test`.
- Commit work to avoid losing it; each rebuild phase ends in a committed state.
