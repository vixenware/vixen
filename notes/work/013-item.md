# 013: Recursive Projection Base Resolution (No “Base Must Be Binding” Loopholes)

Status: In progress (partially addressed in commit `13bfeb8e`, but not yet accepted).

## Goal

Make projection handling (both analysis seeding and exec_slice lowering) correct for arbitrarily nested projection chains where a projection’s base is itself a projection expression, not a direct NameRef/binding use.

This must be test-driven and non-optional: deep chains are ordinary authored code, and leaving holes here is how “demand semantics” silently regresses when a fixture shape changes.

## Why This Is Still Open After `13bfeb8e`

`13bfeb8e` improves exec_slice resolution and adds runtime tests for deep chains, but that is not sufficient to accept this item:

- The analysis-side resolver in `seed_lazy_product_fields` still has an explicit hole for “base is another projection” (a TODO-shaped comment).
- “bomb() did not run” tests can pass purely because exec_slice statically inlined the selected sub-expression and never constructed the product, which does not prove the lazy-product seeding infrastructure is correct or used.

Acceptance must therefore include a test that exercises `LazyProductField` seeding and the tuple/struct construction thunking path (i.e., proves we emit `AllocThunk` for dead siblings at the product construction site).

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

- Introduce a helper that resolves “the product expression being projected from” (recursively, including the case where the base is itself a projection):
  - Start from `projection.base: ExprId`.
  - If it is a NameRef use, follow the binding initializer (`BridgeLocalBinding.init_expr`) when available.
  - If it is itself a `BridgeProjection`, resolve its base recursively.
  - Stop only when you reach a `BridgeProduct` expr (tuple/struct/record).
- Use that resolved product expr to compute the set of fields, and seed **all siblings** as `LazyProductField`.
- Remove reliance on `projection.base_binding` (string) for correctness. It may remain as a debug/provenance hint, but sibling-field determination must not depend on it.

Non-negotiable detail (prevents “passes by inlining”):

- `LazyProductField` seeds must attach to the **product construction ExprId** (the `BridgeProduct.expr` / initializer product expr), not only to the projection receiver ExprId, so that exec_slice tuple/struct lowering (which consults `lazy_*_by_expr` keyed by the product expr being lowered) can actually thunk dead siblings.

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

3. (NEW, required) Analysis + lowering integration test that cannot pass by full projection inlining:
   - Add a `vx-analysis` unit test that lowers a function where a nested product must be constructed as the returned value, but a deeper projection exists in the same body that makes one sibling dead.
   - The test must assert that lowering emits `KernelOp::AllocThunk` for the dead sibling at the relevant tuple/struct literal construction site.
   - The test must fail if the system only inlines the projected leaf and never exercises `LazyProductField` seeding.

## Acceptance Criteria

- All tests pass with the new tests added.
- `seed_lazy_product_fields` contains no correctness dependency on `projection.base_binding` (string).
- Analysis-side resolution correctly handles the case where `projection.base` is itself a projection expression (no TODO-shaped hole).
- The new vx-analysis integration test proves we emit `AllocThunk` for dead siblings during product construction (i.e., the seeding infrastructure is exercised, not bypassed by inlining).

## Notes

If someone tries to defer this as “follow-up”, that’s a process failure: this is a correctness gap, not a performance tweak.
