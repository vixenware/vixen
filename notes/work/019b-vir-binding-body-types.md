# 019b: Derive Binding and Body Result Types on VIR

Status: Not started. Depends on 019.

## Goal

After 019 populates `Expr.ty`, derive types for bindings and body results from expression types already on the arena.

## Work

- **Binding.ty** — derive locally:
  - Let binding: type of its initializer expression (`arena[let.init].ty`).
  - Parameter binding: from declared/resolved param type if available, or from `TypeCheckResult.binding_types` by span match.
  - Pattern match binding: from scrutinee type or leave `None`.
- **Body result type** — define as "resolved type of `body.root_expr`":
  - For blocks: type of the tail expression (or unit if no tail).
  - Derive from `arena[arena[body.root_expr]].ty` after expr types are populated.
  - Note: `Body.return_ty` is currently `Option<TypeRefId>` (syntax-level). A resolved `Ty` would be a new field or a separate lookup. Decide at implementation time whether to add a field or just read from root_expr.

## Testing

- `fn add(x: U64) -> U64 ( x + 1 )` — param binding `x` has `ty = Some(Scalar(U64))`.
- `fn main() ( let x = 1; x + 1 )` — let binding `x` has type derived from init.
- Body result type matches tail expression type.

## Acceptance

- All existing tests pass.
- New tests cover binding and body result type derivation.
