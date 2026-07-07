+++
title = "vixen"
+++

vixen is a general-purpose build system with an emphasis on Rust.

You point it at a Cargo workspace and it builds it — resolving, fetching,
compiling, testing — with one difference that changes daily life: **vixen can
prove what it did and didn't have to do.** Every build step is identified by
the content of its inputs, every step's reads are tracked, and every process
runs hermetically. The result, in practice:

- **Change a comment — nothing rebuilds.** Identity is content, not timestamps.
- **Change a function — its dependents rebuild, nothing else.** Dependencies
  are tracked at item granularity, not file granularity.
- **Rerun eleven tests, skip 189 — soundly.** Computed from the artifacts
  themselves, never guessed from coverage.
- **Trust the cache.** Undeclared inputs are impossible, so a cache hit is a
  proof, not a hope — and "why did this rebuild?" has a
  [readable answer](/using/receipts/).

**[Why vixen?](/why/)** is the honest comparison with cargo, Bazel, and Buck2 —
including what they do better today. **[Using vixen](/using/)** gets you from
install to first build. **[Status](/status/)** says exactly what works right
now, dated.

Curious how any of this is possible? The machinery — a demand-driven,
content-addressed build language called vix — has
[its own site](/dev/), for people who want the internals.
