# 011: Kernel Entrypoint ExprIds + Types (Stop Using “Max ExprId” Heuristics)

Status: Not started.

## Goal

Make `KernelRegion` and `KernelEntrypoint` refer to the real body expression and real types, so later phases can stop guessing.

## Motivation

Today `kernel_shell` computes `result_ty` as “type of the max ExprId in the body” and `AnalyzedBody.kernel_entries[0].source.expr` as “first expr in the body”. These are placeholders and will break as soon as ExprId ordering does not correlate with evaluation order.

This also blocks “analysis drives lowering” work because you cannot reliably name:

- the kernel entry expression
- the kernel result expression
- the kernel result type

## Work

- Extend the bridge to record, per top-level function body:
- the ExprId of the function body expression (block tail expr or `body_expr`)
- the ExprId of the “result expression” if distinct (for blocks, the tail expr)
- Update `analyzed_body_shell` and `kernel_shell` to use those recorded ExprIds.
- Compute `KernelRegion.result_ty` from `bridge.expr_types` using the recorded result ExprId.
- Compute `KernelParam.ty` directly from the binding’s type (not by trying to find an expr at the param span).
- Add tests that assert:
- `KernelEntrypoint.source.expr` points at the body expression (not an arbitrary first expr).
- `KernelRegion.result_ty` matches the type checker’s return type for simple bodies and block bodies.

## Acceptance

- All existing tests stay green.
- New tests cover:
- a function with a block body containing `let` statements and a tail expression
- a function with an expression body `( x + 1 )`

## Notes

This is the smallest change that makes “kernel identity” stable enough for strict-fragment extraction and later lowering. It is also a prerequisite for replacing span-keyed strict lowering.
