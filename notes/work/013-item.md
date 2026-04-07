# 013: Recursive Projection Base Resolution (No “Base Must Be Binding” Loopholes)

Status: Not started.

## Goal

Make projection handling (both analysis seeding and exec_slice lowering) correct for arbitrarily nested projection chains where a projection’s base is itself a projection expression, not a direct NameRef/binding use.

This must be test-driven and non-optional: deep chains are ordinary authored code, and leaving holes here is how “demand semantics” silently regresses when a fixture shape changes.

## Scope

Slice0 products and projections:

- tuple field access `._0`, `._1`
- struct named field access `.a`
- projection chains like `outer._0._0._0` and `s.a.a`

This item does not require cross-function demand propagation (that is 014).

## Required Changes

### A. Analysis seeding (`seed_lazy_product_fields`)

`seed_lazy_product_fields` must be able to determine the sibling fields for a projection even when `projection.base` is a projection expression (and therefore `projection.base_binding` is `None`).

Concrete requirement:

- Introduce a helper that resolves “the product expression being projected from”:
  - Start from `projection.base: ExprId`.
  - If it is a NameRef use, follow the binding initializer (`BridgeLocalBinding.init_expr`) when available.
  - If it is itself a `BridgeProjection`, resolve its base recursively.
  - Stop only when you reach a `BridgeProduct` expr (tuple/struct/record).
- Use that resolved product expr to compute the set of fields, and seed **all siblings** as `LazyProductField`.
- Remove reliance on `projection.base_binding` (string) for correctness. It may remain as a debug/provenance hint, but sibling-field determination must not depend on it.

### B. Lowering (`exec_slice.rs`)

`resolve_index_for_expr` must support projection bases that are:

- NameRef (binding use)
- inline product literals
- projections (recursive)

Similarly, named-field selection must support `s.a.a` by resolving the intermediate selection recursively.

Implementation requirement:

- Add recursion with a visited set to avoid infinite loops on malformed graphs.
- Keep behavior deterministic (stable iteration order, no HashMap iteration dependence).

## Tests (Must Be Added)

Add tests in [demand_driven.rs](/Users/amos/bearcove/vixen/crates/vx-rt/tests/demand_driven.rs) (or vx-analysis unit tests if that’s a better layer) that fail without the recursive handling:

1. Deep tuple projection chain (no intermediate lets):
   - program shape: `let outer = (((7, bomb()), bomb()), bomb()); outer._0._0._0`
   - `bomb()` uses `trap("boom")`
   - expected: `7`

2. Deep named-field projection chain:
   - `struct Pair { a: U64, b: U64 }`
   - `struct Outer { a: Pair, b: U64 }` (or nested Pair structs)
   - program shape: `let s = Outer { a: Pair { a: 42, b: bomb() }, b: bomb() }; s.a.a`
   - expected: `42`, and no trap

## Acceptance Criteria

- All tests pass with the new tests added.
- `seed_lazy_product_fields` contains no correctness dependency on `projection.base_binding` (string).
- `exec_slice` projection resolution handles at least the two new regression tests without falling back to “generic” runtime projection behavior that would reintroduce eager sibling work.

## Notes

If someone tries to defer this as “follow-up”, that’s a process failure: this is a correctness gap, not a performance tweak.
