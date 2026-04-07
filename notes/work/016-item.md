# 016: Replace Span-Keyed `lower_strict.rs` With KernelOp-Based Strict Lowering

Status: Not started.

## Goal

Stop generating Kajit strict fragments by re-walking the raw parser AST keyed by spans. Instead, generate strict fragments from the already-lowered executable slice (`ExecutableBody.ops`) when those ops are in the strict-compatible subset.

This makes “strict compilation” a property of the lowering output, not a property of a second independent AST walk.

## Motivation

The current strict path:

- is brittle (span matching, wrapper peeling, duplicate expression discovery)
- bypasses analysis decisions (the strict lowerer ignores `KernelRegion.exprs` and extracted nodes)
- makes it hard to compile synthesized bodies (thunk bodies and closure bodies) because they do not correspond to a source `FnDecl`

KernelOps are already a statement-like IR that is closer to the intended lazy-core direction, and they can be used as the strict fragment source of truth.

## Work

- Define a strict-compatible subset of `KernelOp` that can be compiled:
- scalar const ops
- scalar arithmetic and comparisons
- string ops if Kajit supports them (or exclude initially)
- pure `Select` with strict bool condition
- Define a new strict lowerer that takes:
- `params: &[SlotId]`
- `ops: &[KernelOp]`
- `result_slot: SlotId`
- and produces a `kajit_hir::VixenTypedFunction`
- Wire this into `vx-analysis` lowering:
- After `exec_slice::lower_executable_program(...)`, attempt strict-lowering for each `ExecutableBody`.
- If strict lowering succeeds, set `kernel_kind = Strict { vtf }`, otherwise keep interpreted.
- Keep the existing AST-based strict lowerer temporarily as a fallback path for bodies that have no KernelOps representation yet (if any).
- Add tests that validate:
- a simple scalar function compiles strictly via the KernelOp path
- mixed bodies (runtime boundaries) stay interpreted

## Acceptance

- Existing tests remain green.
- At least one existing fixture that previously compiled strictly still compiles strictly.
- We can strict-compile a synthesized closure body or thunk body once 017 is done (or via a targeted unit test).

## Notes

This item is about eliminating duplicate semantics sources. It does not yet implement the full “typed Ref<T>/Obs<T> lazy core” model. It is the incremental step that makes that transition possible without rewriting strict lowering twice.
