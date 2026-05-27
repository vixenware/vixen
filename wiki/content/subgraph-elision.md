+++
title = "Subgraph elision"

[extra]
type = "ArchitectureNote"
topic = "cache"
created = "2026-05-26"
updated = "2026-05-26"
tags = ["ccc", "cache", "fork-rationale"]
+++

The property that separates a cargo fork from a `RUSTC_WRAPPER`-style sidecar. The reason "cache rustc invocations" is not the right framing of what CCC does.

## The wrapper view

A `RUSTC_WRAPPER` sees one rustc invocation at a time. For each one it asks the cache "do you have the output for these exact inputs?" — one round-trip per unit. On hit, return cached bytes; write them to `target/`. On miss, invoke rustc; upload the result.

Every invocation is examined sequentially, every input read locally, every intermediate `.rlib` / `.rmeta` materialised to disk so the next rustc invocation up the dep chain can read it. The wrapper saves CPU on rustc steps but pays all the I/O, all the per-unit RPCs, and all the orchestration cost.

## The fork view

A cargo fork sees the *whole dep graph* before it invokes anything. It sends the entire set of unit identities to ccc-backend in **one RPC**. ccc-backend responds with the hit/miss partition for every unit at once. The fork then materialises only the artifacts the user actually demands — typically just the final binary on a full hit — and runs rustc locally only for the missed units.

For each unit the fork knows the stable identity (for crates.io deps that's `(crate-name, version, registry-hash)` plus compile args plus toolchain identity — no source hashing). The cache lookup is one batched call; the demand signal for misses is implicit in the same RPC.

## Why this matters

- A workspace with 200 crates does ~200 rustc invocations on a clean build. A wrapper paying per-unit overhead on full hits still forks 200 wrapper subprocesses, materialises 200 `.rlib`s to disk, runs cargo's full traversal. Latency caps at "no rustc CPU" but pays everything else.
- A fork sees the whole graph, asks ccc-backend once. On a full hit, the fork downloads only the final binary, writes it to its expected path, and exits. No intermediate `.rlib`s touched. No per-unit subprocess. One RPC, one disk write.

The CPU-second savings are similar; the latency savings are orders of magnitude apart. CCC competes on wall-clock for the *whole* build, not on rustc-CPU-saved.

## Mechanism for the partial-hit case

When some intermediate units miss while others hit, the fork has to materialise just enough cached intermediates so the missed units' rustc invocations can find their deps. Cargo's existing `--extern` machinery enumerates the exact paths where each dep is expected; the fork places the cached bytes at those paths before invoking rustc for the missed units. The "extra" intermediates whose downstream is fully cached are never materialised.

## Related

- [The pitch](/pitch/) — Tier 1 mechanism.
