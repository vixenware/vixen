# 021: Demand-Gated Lazy Seeding

Status: Not started.

## Goal

`seed_cross_function_lazy_fields` (and `seed_lazy_product_fields`) currently run unconditionally over all call sites and projections in the bridge, regardless of whether the containing body is reachable from the entry point. This makes the analysis artifact larger than necessary — LazyProductField nodes get seeded in disconnected functions that will never execute.

## Required Changes

After demand propagation determines which bodies are reachable (have `result_observation != NoObs`), filter lazy seeding to only process projections and call sites in demanded bodies. This could mean:

1. Running lazy seeding after `apply_one_step_local_call_demands`, or
2. Passing the set of demanded bodies into the seeding functions and skipping non-demanded bodies.

Option 2 is simpler and doesn't require reordering the analysis pipeline.

## Tests

- Add a test with a disconnected function containing a product+projection. Verify that no LazyProductField is seeded for the disconnected body.
- Existing tests must still pass (demanded bodies should still get seeded).

## Acceptance Criteria

- LazyProductField nodes are only seeded for bodies reachable from the entry point.
- No regressions in existing tests.
- Analysis artifact size is reduced for programs with disconnected functions.
