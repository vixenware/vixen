+++
title = "Bytecode and JIT exploration"

[extra]
type = "Question"
status = "open"
created = "2026-05-26"
tags = ["language", "runtime", "performance", "jit"]
+++

The current Vixen runtime is a tree-walking interpreter. That's fine for a long time — the heavy lifting in any Vixen build is dispatched to native intrinsics (sandbox, exec, fetch, parse, constraint-solve) — but eventually some hot paths in Vixen code (especially the cargo resolver model under `vixen-rust`) will want better than tree-walking.

## What we plan to explore

- **Bytecode compilation first.** Compile Vixen functions to a flat bytecode and execute that. Cheaper to build than a real JIT; still meaningful speedup over tree-walking for inner loops.
- **JIT second, if bytecode isn't enough.** A real JIT would target hot Vixen kernels — pure-function-shaped chunks of code that get called repeatedly with similar shapes. Mentioned in [[Enterprise feature candidates]] (#23) as a sellable capability ("customer writes a hot custom step in Vixen, gets near-native perf, never has to ship a C plugin").

## The design challenge

Vixen is demand-driven. The build is shaped as a graph; nodes evaluate when their results are needed. *Which parts of that graph want to be JIT-compiled vs which parts want to stay graph-shaped?*

- **JIT-natural:** pure functions, inner-loop arithmetic, parsing, resolver inner loops — code that runs many times with similar shapes.
- **Graph-natural:** anything that crosses an executor boundary (closure shipping), anything that does `exec` (process spawning), anything that interleaves with cache lookups — the demand-driven, side-effecting shape.

The split is not obvious. Exploring without committing to a real JIT — bytecode first — lets us learn the shape before paying the JIT engineering cost.

## What unblocks resolution

A bytecode compiler for Vixen (target IR exists as `vx-ir`), benchmarks against the tree-walking interpreter on representative workloads (cargo resolution, template rendering, large build-graph evaluation). The performance gap tells us whether bytecode is enough or whether JIT is required.

## Related

- [[Domain logic lives in Vixen, not in the runtime]]
- [[Rust/Vixen FFI shape]]
- [[Enterprise feature candidates]] — feature #23.
