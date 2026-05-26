+++
title = "Subgraph elision"

[extra]
type = "ArchitectureNote"
topic = "cache"
created = "2026-05-26"
tags = ["ccc", "cache", "fork-rationale"]
+++

The foundational property that separates a [[CCC is a cargo fork, not a rustc wrapper|cargo fork]] from a `RUSTC_WRAPPER`-style sidecar. It is the reason "cache rustc invocations" is not the right framing of what CCC does.

## The wrapper view

A `RUSTC_WRAPPER` sees one rustc invocation at a time. For each one it asks the cache "do you have the output for these exact inputs?" If yes, return cached bytes. If no, invoke rustc, then upload the result.

Every invocation is examined, every input read, every intermediate `.rlib`/`.rmeta` materialised to disk so the next rustc invocation up the dep chain can read it. The wrapper has saved CPU on the rustc step but has done *all the I/O* and *all the orchestration*.

## The fork view

A cargo fork sees the *whole dep graph* before it invokes anything. It can ask the cache "do you have the final artifact for this build?" If yes, skip every intermediate step — no rustc, no linker, no `build.rs` execution, no `.rlib` writes, no disk I/O. The downstream artifact is the only thing the user needs; everything upstream of it was a means to that end.

If the *final* artifact isn't cached but some *intermediate* one is, the fork can elide that intermediate's whole subgraph — its dependencies, its `build.rs`, its proc-macro expansion — and only do the work that's downstream of the cache hit.

## Why this matters

The wrapper-vs-fork distinction looks small on a single invocation. It is enormous on a real workspace:

- A workspace with 200 crates does ~200 rustc invocations on a clean build. A wrapper hitting cache for all of them still pays the cost of *invoking* 200 processes (fork/exec/argv/env), materialising 200 `.rlib`s to disk, running cargo's own dep traversal. Wall-clock savings cap at "no rustc work" but pay all the orchestration.
- A fork that hits the cache for the final binary returns one bag of bytes and exits. Zero process invocations. Zero `.rlib` writes. Zero traversal beyond what's needed to compute the cache key.

The CPU-second savings are similar; the latency savings are orders of magnitude apart. CCC competes on wall-clock for the *whole* build, not on rustc-CPU-saved.

## Mechanism

For the fork to skip materialising an intermediate that *some downstream rustc invocation will still read*, the fork either:
- Pre-materialises the cached intermediate from the cache before invoking the downstream rustc (works with unmodified rustc, requires cargo to know all the files rustc will read), or
- Lets rustc itself read directly from the cache (requires forking rustc too).

Which of these is achievable depends on cargo's knowledge of rustc's read set — see [[Does cargo know enough to pre-materialise rustc's read set?]]. The eventual outcome is a rustc fork regardless; this Decision decides whether that fork happens with CCC or after it.

## Related

- [[Tier 1 — CCC]]
- [[CCC is a cargo fork, not a rustc wrapper]]
- [[Does cargo know enough to pre-materialise rustc's read set?]]
