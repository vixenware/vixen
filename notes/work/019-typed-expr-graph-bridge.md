# 019: Incremental Typed Expression Graph In The Bridge (Replace Ad Hoc AST Walks)

Status: Not started.

## Goal

Move from “lowering by re-walking parser AST” to “lowering by stable, typed, ID-addressable expression nodes”, without taking on a full new IR all at once.

## Motivation

Even after 017, we still rely on span matching and raw AST access in multiple places (bridge enrichment, exec_slice lowering, evaluator tests). A small typed expression graph in the bridge gives us:

- stable ExprId addressing without span heuristics
- explicit child edges (no `syntax().descendants()` re-discovery)
- one place to attach expression types

This is the incremental path toward a real lazy-core IR.

## Work

- Add a `BridgeExprNode` (or similar) keyed by `ExprId` with:
- node kind (literal, name ref, binary op, call, tuple, struct literal, field, if, closure)
- child ExprIds for structural edges
- optional resolved callee target (for direct calls)
- type `Ty` (if known)
- Populate it during bridge construction using the existing `vx_parser::Expr` AST once, but store only stable IDs and small enums.
- Refactor exec_slice lowering to use `BridgeExprNode` instead of `expr_nodes_by_id` AST scanning.
- Refactor any remaining “span-keyed” strict lowering machinery to use ExprIds directly.

## Acceptance

- Existing tests remain green.
- `exec_slice` no longer needs to build a `HashMap<ExprId, Expr>` by scanning AST descendants.

## Notes

This work item is intentionally “bridge-local” and should not create a new public IR. It is a stepping stone so later work can introduce lazy-core operations with fewer moving parts.
