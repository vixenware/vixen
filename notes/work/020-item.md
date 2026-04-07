# 020: Cross-Function Lazy Seeding Through Bindings

Status: Not started.

## Goal

`seed_cross_function_lazy_fields` currently only detects callee projections where `proj.base` is a direct use of the parameter binding. It misses indirect flows like:

```
fn pick_first(p: (U64, U64)) -> U64 (
  let q = p;
  q._0
)
```

Here `q._0` projects from `q`, not `p` directly, so `projected_fields_by_param` doesn't record that `p._0` is used. The caller's product won't get `_1` thunked.

## Required Changes

When building `projected_fields_by_param`, follow binding chains: if `proj.base` resolves (through `use_by_expr` → binding → `init_expr`) to a parameter use, record the projection against that parameter.

This is the same `resolve_to_product`-style recursive resolution already used elsewhere — apply it to projection base resolution in the cross-function seeding path.

## Tests

Add a variant of `pair_passed_to_fn_dead_sibling` where the callee introduces a local alias before projecting:

```
fn pick_first(p: (U64, U64)) -> U64 ( let q = p; q._0 )
fn main() -> U64 ( let p = (42, bomb()); pick_first(p) )
```

Expected: 42, no trap.

## Acceptance Criteria

- The aliased-parameter test passes end-to-end in vx-rt.
- `cross_function_lazy_seeding` analysis test covers the aliased case.
