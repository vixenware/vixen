# Notes folder relevance review

Baseline used for this pass:

- `docs/content/spec/language/core.md`, `docs/content/spec/language/build.md`,
  and `docs/content/spec/internals.md` are the current authority.
- The current implementation path is VIR -> `KernelOp` -> runtime/JIT.
- `vx-analysis` is gone from the workspace.
- `docs/design/strict-fragment-abi.md` is still draft design, not normative.

## Current takeaways

- Keep future/system design notes only when they describe a still-open product
  or runtime direction.
- Retire bridge-era notes that talk about `KernelRegion`, span-keyed bridge
  lowering, or deleting `vx-analysis`.
- Refresh any still-useful work item that names `vx-analysis`, old keyword
  names, or old runtime representations before handing it to an implementer.

## File-by-file review

| File | Status | Current read |
| --- | --- | --- |
| `notes/future/target-selection.md` | Keep, refresh later | Still aligned with the spec's `discover`, `exec`, `ExecutorClass`, and `stabilize` direction. It is product/runtime design, not immediate eval-lowering work. Some details should eventually move into normative spec once settled. |
| `notes/future/tree-hash.md` | Retire or rewrite | The old `TreeValue::Inline` / `FsDir` / exec-mount singleflight premise does not match the current daemon runtime. `DirectoryEntryKind::Dir` no longer carries `total_size`, which the note lists as a future cleanup. |
| `notes/future/lsp-build-feedback.md` | Keep as product intent, refresh before implementation | `vx-lsp` still only publishes static/parser/resolve/cap/type diagnostics, and the orchestrator service has no `LastBuildFeedback` query. The direction is useful, but the "Current State" section is stale. |
| `notes/future/parallel-solver.md` | Keep as research/future | Still coherent with demand-driven evaluation and structured failures. Not current eval/runtime work. |
| `notes/work/011-kernel-entry-exprs-types.md` | Retire | `KernelRegion` / `KernelEntrypoint` / max-`ExprId` bridge language is from the old pipeline. VIR has body root expressions, source expressions, `ResultSource`, and slot types. |
| `notes/work/012-kernelop-strict-lowering.md` | Mark mostly done, rewrite residual | KernelOp-based strict lowering exists in `crates/vx-jit/src/lower_to_strict.rs` and is wired through `compile_source_with_types`. Residual: no `KernelKind::Strict`, no mixed interpreted/JIT execution, and no synthesized-body strict lowering. |
| `notes/work/013-strict-thunk-closure-compilation.md` | Keep as active future | Still relevant. Current strict lowering explicitly rejects synthesized bodies via `original_body_count`, so extracted thunks/closure bodies are not strict-compiled. |
| `notes/work/014-cc-version-real-pipeline.md` | Refresh | Direction is still useful, but the current `zoo/cc-version.vx` is aspirational: it uses `import`, package toolchain discovery, `coarse_mounts`, observer output trees, and runtime pieces that are only partially supported. |
| `notes/work/015-match-enum-lowering.md` | Superseded by work-resolve 015/016 | Literal/binding/wildcard match lowering exists. Product and variant patterns remain open, but the `.styx` fixture premise appears stale. Use the more focused work-resolve notes instead. |
| `notes/work/016-typed-expr-graph-bridge.md` | Retire | VIR is the typed, scoped, ID-addressable expression representation and explicitly replaces the old bridge/span reconstruction path. |
| `notes/work/017-cross-fn-lazy-seeding-bindings.md` | Keep, update names | Still relevant. Current `collect_param_projections` only recognizes direct parameter name refs before projection, so callee-local aliases still look like an under-approximation. |
| `notes/work/018-demand-gated-lazy-seeding.md` | Mark done | Current `seed_lazy_fields` processes reachable bodies only. |
| `notes/work/019-vir-expr-type-population.md` | Mark done | `crates/vx-vir/src/type_populate.rs` implements `populate_expr_types`, with tests. |
| `notes/work/019b-vir-binding-body-types.md` | Mark partial | Let/destructuring binding types and `body_result_ty` exist. Parameter binding types still stay `None` by test, so that is the live remainder. |
| `notes/work/future/004-item.md` | Keep future | Still relevant: strict lowering does not support direct calls/recursion in the JIT path. |
| `notes/work/future/005-item.md` | Retire or rewrite | No longer "far future" as written. Runtime boundaries, local exec/discover, store, and VFS pieces exist partially. Needs a current build-runtime backlog instead. |
| `notes/work-resolve/013-delete-vx-analysis.md` | Mark done, retire | `vx-analysis` is no longer present. |
| `notes/work-resolve/014-lower-runtime-where-payloads.md` | Mark mostly done, refresh residual | `exec ... where { observer, mounts, writable, create_dirs, coarse_mounts }` lowers through VIR. The note uses stale `writable_roots` wording; runtime still rejects `coarse_mounts`, and closure captures for runtime-boundary closures are not supported. |
| `notes/work-resolve/015-product-pattern-match.md` | Keep active | VIR has tuple/struct pattern nodes, but `lower_match` only handles wildcard, binding, and literal patterns. |
| `notes/work-resolve/016-variant-runtime-and-lowering.md` | Keep active, rewrite file paths | Runtime still has no `RtValue::Variant`, `KernelOp::MakeVariant`, or `CheckTag`; `lower_name_ref` still rejects variant refs. Replace `vx-analysis` references with `vx-vir`. |
| `notes/work-resolve/017-pattern-composition.md` | Keep active after 015/016 | Or-pattern nodes exist in VIR and type checking, but lowering does not handle them. Guard clauses are not represented on VIR `MatchArm` today, so that portion needs a parser/VIR model decision first. |
| `notes/work-resolve/018-residual-exec-slice-decision.md` | Keep, refresh | Still useful as a decision bucket, but current facts differ: `TypeOp::As` lowers as passthrough, range is supported only inside `.slice(...)`, and map literals are supported only as runtime-boundary `mounts` payloads. |

## Suggested cleanup order

1. Mark done or retire the obvious completed/superseded notes:
   `work-resolve/013`, `work/011`, `work/016`, `work/018`, `work/019`.
2. Refresh the still-active implementer notes around the current VIR/runtime
   names: `work-resolve/014` through `018`, `work/017`, and `work/019b`.
3. Rewrite the two old future/runtime notes before relying on them:
   `future/tree-hash.md` and `work/future/005-item.md`.
4. Leave `future/target-selection.md`, `future/lsp-build-feedback.md`, and
   `future/parallel-solver.md` as design notes, but do not treat their
   "Current State" claims as verified-current without another source pass.
