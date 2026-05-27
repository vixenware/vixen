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

The current Vixen runtime is a tree-walking interpreter. Fine for a long time — the heavy lifting in any Vixen build is dispatched to native intrinsics (sandbox, exec, fetch, parse, constraint-solve). Eventually some hot paths (especially the cargo resolver model under `@vixen/rust`) will want better than tree-walking.

Plan:

- **Bytecode first.** Compile Vixen functions to a flat bytecode, execute that. Cheaper than a real JIT; still meaningful speedup over tree-walking on inner loops.
- **JIT later, if bytecode isn't enough.** Target hot Vixen kernels — pure-function-shaped chunks called repeatedly with similar shapes. Mentioned in the pitch's enterprise feature catalogue (#23) as a sellable capability.

Design challenge: Vixen is demand-driven. Which parts of the graph want to be JIT-compiled vs which want to stay graph-shaped? Pure functions and inner-loop arithmetic are JIT-natural; anything crossing an executor boundary, doing `exec`, or interleaving with cache lookups is graph-natural. Exploring bytecode first lets us learn the shape before paying JIT engineering cost.

Benchmarks against tree-walking on representative workloads (cargo resolution, template rendering, large build-graph evaluation) tell us whether bytecode is sufficient.
