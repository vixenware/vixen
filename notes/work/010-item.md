# 010: Inline Product Laziness (Inline Projections)

`seed_lazy_product_fields` in bridge_to_analysis.rs only handles
let-bound products. Inline projections like `Pair { a: 42, b: bomb() }.a`
never get lazy extraction because there's no `base_binding`.

Status: Not started.

## What needs to change

In `bridge_to_analysis.rs`, `seed_lazy_product_fields` needs to also
track `bridge.products` (not just `binding_products`) and match
projections by `base` expr, not just `base_binding`.

## Fixtures blocked

- 20_partial_force — nested inline tuple projection
- 29_struct_dead_field — inline struct literal projection
