# 019: Populate VIR Expr.ty from Existing Type Checker

Status: Not started.

## Goal

Transfer expression types from the existing AST-based type checker onto VIR expressions by exact `SourceSpan` match. This is a transitional compatibility step only. It populates `Expr.ty` and leaves unmatched/synthetic/error expressions as `None`. Binding and body-level type population are deferred to 019b.

## Motivation

VIR work items 001-003 gave us structure, scopes, and name resolution. Every `Expr` has a `ty: Option<Ty>` field that's currently always `None`. Before VIR can replace the bridge for downstream analysis, expressions need types.

The existing type checker (`vx-resolve/src/type_check.rs`) produces `TypeCheckResult` with `expr_types: HashMap<SourceSpan, Ty>`. Rather than rewriting the type checker, we transfer its results into VIR via span matching.

This is explicitly fragile: VIR eliminates parens, has synthetic nodes, and may not have 1:1 correspondence with parser AST nodes. Unmatched expressions stay `None`. A future VIR-native type checker will replace this.

## Work

- Add `crates/vx-vir/src/type_populate.rs` with:
  ```rust
  pub fn populate_expr_types(arena: &mut VirArena, type_check: &TypeCheckResult);
  ```
- Walk all expressions in the arena.
- For each expression, look up `type_check.expr_types.get(&expr.span)`.
- Skip synthetic/error/default spans (where span is meaningless).
- Write matched types to `expr.ty`. Leave unmatched as `None`.
- No type checker rewrite.
- No `Binding.ty` population.
- No `Body.return_ty` population.

## Testing

1. Simple typed expression: `fn add(x: U64) -> U64 ( x + 1 )` — tail expr gets `ty = Some(Scalar(U64))`.
2. Paren elimination: `fn main() -> U64 ( (1 + 2) )` — VIR's inner expr still gets typed correctly despite paren node elimination.
3. Synthetic top-level block: synthetic block expr stays `None` (no matching span in checker).
4. Error/missing checker entry: no panic, `ty = None`.
5. Multi-source sanity: two sources with overlapping text ranges but different `SourceId` don't cross-populate.

## Acceptance

- `cargo check -p vx-vir` — no warnings.
- `cargo nextest run -p vx-vir` — all tests pass.
- `cargo nextest run` — only pre-existing failures.
