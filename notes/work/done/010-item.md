# 010: Inline Product Laziness (Inline Projections)

`seed_lazy_product_fields` in bridge_to_analysis.rs only handles
let-bound products. Inline projections like `Pair { a: 42, b: bomb() }.a`
never get lazy extraction because there's no `base_binding`.

Status: Done (commit `3ce8651e`).

## What changed

- The bridge and analysis now seed lazy sibling fields for inline tuple/struct projections.
- Exec-slice lowering resolves projection chains and named field selection for slice0 shapes.

## Fixtures fixed

- 20_partial_force — nested inline tuple projection
- 29_struct_dead_field — inline struct literal projection
