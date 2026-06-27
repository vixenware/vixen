# Notes folder relevance review

Baseline used for this pass:

- `docs/spec/language/core.md`, `docs/spec/language/build.md`,
  `docs/spec/stdlib/build.md`, and `docs/spec/internals.md` are the current
  spec paths.
- The current implementation path is parse -> `vx-ir` VIR -> `vx-runtime`
  analysis -> `vx-runtime-graph` -> `vx-runtime-core`/`vx-runtime-driver`.
- The old `vx-analysis`, `vx-vir`, `vx-rt`, and `vx-jit` crates are gone from
  the workspace.
- `docs/design/runtime-graph-execution/` is the current runtime-transition
  roadmap, but its checkpoint paragraphs still tell readers to verify code
  before trusting status.

## Current takeaways

- Keep future/system design notes only when they describe a still-open product
  or runtime direction.
- Retire bridge-era notes that talk about `KernelRegion`, span-keyed bridge
  lowering, deleting `vx-analysis`, or the removed `vx-vir`/`vx-rt` crates.
- Refresh any still-useful work item that names removed crates, old keyword
  names, or old runtime representations before handing it to an implementer.

## File-by-file review

| File | Status | Current read |
| --- | --- | --- |
| `notes/future/target-selection.md` | Refresh before use | Product/runtime direction may still be useful, but the `discover` / `ExecutorClass` / `stabilize` framing is stale. Current docs use ability requests, daemon-side capability probes, and predicate-based executor selection. |
| `notes/future/tree-hash.md` | Retire or rewrite | The old `TreeValue::Inline` / `FsDir` / exec-mount singleflight premise does not match the current graph runtime. `DirectoryEntryKind::Dir` no longer carries `total_size`, which the note lists as a future cleanup. |
| `notes/future/lsp-build-feedback.md` | Keep as product intent, refresh before implementation | `vx-lsp` still only publishes static/parser/resolve/cap/type diagnostics, and the orchestrator service has no `LastBuildFeedback` query. The direction is useful, but the "Current State" section is stale. |
| `notes/future/parallel-solver.md` | Keep as research/future | Still coherent with demand-driven evaluation and structured failures. Not current eval/runtime work. |
| `notes/work/011-kernel-entry-exprs-types.md` | Retire | `KernelRegion` / `KernelEntrypoint` / max-`ExprId` bridge language is from the old pipeline. VIR has body root expressions, source expressions, `ResultSource`, and slot types. |
| `notes/work/012-kernelop-strict-lowering.md` | Retire or rewrite | The note targets removed `vx-analysis` / `vx-jit` paths. Current `KernelOp` lives in `vx-runtime-graph`, and strict native-kernel work needs a fresh plan against the graph runtime. |
| `notes/work/013-strict-thunk-closure-compilation.md` | Retire or rewrite | The note targets the removed `vx-jit` mixed strict/interpreted path. If native kernel compilation returns, this needs a graph-runtime plan. |
| `notes/work/014-cc-version-real-pipeline.md` | Refresh | Direction is still useful, but the current `zoo/cc-version.vx` is aspirational: it uses `import`, package toolchain discovery, `coarse_mounts`, observer output trees, and runtime pieces that are only partially supported. |
| `notes/work/015-match-enum-lowering.md` | Superseded by work-resolve 015/016 | Literal/binding/wildcard match lowering exists. Product and variant patterns remain open, but the `.styx` fixture premise appears stale. Use the more focused work-resolve notes instead. |
| `notes/work/016-typed-expr-graph-bridge.md` | Retire | VIR is the typed, scoped, ID-addressable expression representation and explicitly replaces the old bridge/span reconstruction path. |
| `notes/work/017-cross-fn-lazy-seeding-bindings.md` | Retire or rewrite | The named seeding functions no longer exist in the current crates. If the alias-projection issue still matters, it needs a new graph-analysis repro. |
| `notes/work/018-demand-gated-lazy-seeding.md` | Retire | The note targets the removed lazy-seeding bridge path. |
| `notes/work/019-vir-expr-type-population.md` | Retire | The note targets removed `crates/vx-vir`; current VIR lives in `crates/vx-ir`. |
| `notes/work/019b-vir-binding-body-types.md` | Refresh | The title still maps conceptually to VIR binding/body metadata, but any implementation path must be checked against `crates/vx-ir` and `vx-runtime` analysis. |
| `notes/work/future/004-item.md` | Retire or rewrite | The note targets `vx-jit`, which is no longer in the workspace. Direct/recursive native-call support needs a new graph-runtime plan. |
| `notes/work/future/005-item.md` | Retire or rewrite | No longer "far future" as written. Runtime boundaries, local exec/discover, store, and VFS pieces exist partially. Needs a current build-runtime backlog instead. |
| `notes/work-resolve/013-delete-vx-analysis.md` | Mark done, retire | `vx-analysis` is no longer present. |
| `notes/work-resolve/014-lower-runtime-where-payloads.md` | Retire or rewrite | The note targets removed `vx-vir` lowering and old runtime-boundary names. Current exec metadata lowering lives in `vx-runtime::analyze` and uses `writable`, not `writable_roots`, in source syntax. |
| `notes/work-resolve/015-product-pattern-match.md` | Refresh | Tuple/struct pattern support exists in current `vx-ir` and `vx-runtime` analysis/type-checking paths; verify remaining runtime behavior with a graph-runtime test before treating this as active. |
| `notes/work-resolve/016-variant-runtime-and-lowering.md` | Superseded candidate | Current graph/runtime code has `NodeKind::Variant`, `MatchPattern::Variant`, `NodeOutput::Variant`, and enum-variant construction/type-checking. The old `vx-rt`/`vx-analysis` plan is obsolete. |
| `notes/work-resolve/017-pattern-composition.md` | Refresh | Or-pattern nodes exist in current `vx-ir` and type checking. Verify lowering/runtime behavior against `vx-runtime` before using this as an implementer note. |
| `notes/work-resolve/018-residual-exec-slice-decision.md` | Retire or rewrite | The exec-slice framing is from the removed bridge path; translate any still-useful decisions into the graph-runtime model before handing off. |

## Suggested cleanup order

1. Ask before deleting, but the strongest deletion candidates are:
   `work-resolve/013`, `work-resolve/016`, `work/011`, `work/012`, `work/013`,
   `work/016`, `work/017`, `work/018`, `work/019`, and `work/future/004`.
2. Rewrite any still-useful implementer notes around the current
   `vx-ir`/`vx-runtime`/`vx-runtime-graph` names before use.
3. Rewrite the old future/runtime notes before relying on them:
   `future/target-selection.md`, `future/tree-hash.md`, and
   `work/future/005-item.md`.
4. Leave `future/lsp-build-feedback.md` and `future/parallel-solver.md` as
   design notes, but do not treat their "Current State" claims as
   verified-current without another source pass.
