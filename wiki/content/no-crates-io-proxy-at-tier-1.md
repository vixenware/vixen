+++
title = "No crates.io proxy at Tier 1"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["tier-1", "crates-io", "operations", "scope"]
+++

## Context

The CCC [[CCC is a cargo fork, not a rustc wrapper|cargo fork]] could in principle redirect every source-byte fetch through Vixen-controlled infrastructure: customer's `cargo fetch` goes to a Vixen-operated proxy that forwards to crates.io on miss, stores bytes in Store, and serves future fetches locally.

This is a real value-add for some customers (availability insulation when crates.io is down, bandwidth amortisation across customers, populated Store for backfill, auditable chain). It is also a real cost — tens of TB of crates.io corpus to hold, yank/takedown synchronisation, another service to operate.

## Options considered

1. **Ship a crates.io proxy with Tier 1.** All source-byte fetches route through Vixen. Inherits both the value and the cost.
2. **No proxy at Tier 1.** `cargo-vixen` hits crates.io directly. The trusted-builder fleet also hits crates.io directly during backfill, populating Store as a side effect. Defer proxy capability to a later tier or never.

## Decision

Option 2 for Tier 1. `cargo-vixen` does not redirect source-byte fetches. The cargo `[source]` config stays at the upstream registry. Customer fetches hit crates.io directly, as plain cargo would.

## Consequences

- **Less infrastructure to operate at launch.** No mirror, no yank-sync, no crates.io-policy-surface to inherit. Smaller cold path, smaller failure surface.
- **No availability insulation in v1.** If crates.io is down, customers' fresh builds fail to fetch deps regardless of CCC. The build-output cache continues to serve hits for already-built artifacts; new dep introductions block on crates.io.
- **Store is populated by backfill, not by customer fetches.** When the trusted-builder fleet picks up a miss-signal, it fetches the declared sources from crates.io itself (once), stores the bytes in Store, materialises them in the [[VFS and sandbox stack|sandbox VFS]], builds, and pushes the output. Future backfills of the same crate find the bytes already in Store. Customer-side cargo never sees Store at the source-byte layer; it only sees cache outputs.
- **Proxy is a future capability, not a deletion.** The fork can redirect later; the wire is open. When a customer pays for the availability / audit story, we add the proxy under that tier without retrofitting CCC's correctness path.
- **No competitive moat lost.** The proxy isn't what makes CCC valuable; [[Subgraph elision]] and [[Demand-warmed cache]] are. The proxy is a separable capability.

## Related

- [[Tier 1 — CCC]]
- [[CCC is a cargo fork, not a rustc wrapper]]
- [[Trusted-builder fleet]]
- [[Backfill builds run inside the Tier-2 sandbox]]
- [[VFS and sandbox stack]]
