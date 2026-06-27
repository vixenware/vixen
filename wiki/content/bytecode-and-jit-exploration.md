+++
title = "Bytecode + JIT exploration for hot Vixen code"

[extra]
type = "Task"
status = "todo"
priority = "low"
tiers = ["2", "3"]
tags = ["language", "runtime", "performance"]
created = "2026-05-26"
+++

The current Vixen runtime direction is graph-driven evaluation with eager pure
kernel islands. The retired tree-walking and `vx-jit` paths should not come back;
interpretation and native execution are being rebuilt on the shared Weavy
lowered-program substrate from the Facet repository. The heavy lifting in any
Vixen build is still dispatched to native intrinsics (sandbox, exec, fetch,
parse, constraint-solve), but hot pure interiors (especially the cargo resolver
model under `@vixen/rust`) will eventually want better than graph dispatch alone.

Plan:

- **Weavy interpretation first.** Lower eligible eager kernels to Weavy's
  program/block shape and run them with the shared interpreter runner. This is
  the oracle for native execution.
- **Copy-and-patch JIT when it earns its place.** Compile the same lowered Weavy
  representation for hot kernels — pure-function-shaped chunks called repeatedly
  with similar shapes. Mentioned in the pitch's enterprise feature catalogue
  (#23) as a sellable capability.

Design challenge: Vixen is demand-driven. Which parts of the graph want to be
lowered to Weavy vs which want to stay graph-shaped? Pure functions and
inner-loop arithmetic are JIT-natural; anything crossing an executor boundary,
doing `exec`, or interleaving with cache lookups is graph-natural. The important
constraint is that interpretation and JIT share the same lowered representation,
not that Vixen invent a second evaluator.

Benchmarks on representative workloads (cargo resolution, template rendering,
large build-graph evaluation) tell us when Weavy interpretation is enough and
when native copy-and-patch pays for itself.
