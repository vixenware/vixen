+++
title = "vx-store regional replication"

[extra]
type = "Question"
status = "open"
created = "2026-05-26"
tags = ["tier-1", "storage", "replication", "operations"]
+++

[[Trusted-builder fleet]] is geographically distributed; `vx-store` (the CAS that holds cache content) is regionally replicated. The exact replication topology is not yet decided.

## What needs deciding

- **Replication style.** Active-active (every chunk lands in every region's replica), regional primary with read replicas, geographically partitioned with cross-region pull on miss, something else.
- **Consistency model.** Strong (every region sees writes immediately, latency cost), eventual (regions converge over time, brief stale reads), or read-your-writes per tenant.
- **Cache-hit hot path.** Does a `cargo-vixen` client in the US hit the US replica only, or fail over to EU on a regional miss? What's the latency / cost trade-off?
- **Backfill destination.** When a US-region trusted builder produces an output, does it land in the US replica and replicate to EU, or land in a single canonical region and propagate?
- **Eviction independence.** Are LRU decisions made per-region or globally?

## Constraints from above

- The output of a build is content-addressed by blake3 of the bytes; the *cache mapping* (`key → output_handle`) is the authoritative artifact, but the bytes themselves are CAS-replicable trivially.
- Writes to the cache come only from trusted builders (see [[Demand-warmed cache]]); this constrains the write-side replication shape but doesn't dictate it.
- The Hetzner Hel1 deployment is the initial single-region case; replication only matters once a second region (probably US) comes online.

## What unblocks resolution

A concrete spec for the first two regions (Hetzner Hel1 + US vendor TBD), expected traffic patterns once Tier 1 has a few customers, and what failover SLA we want to promise.

## Related

- [[Trusted-builder fleet]]
- [[CCC service]]
- [[Demand-warmed cache]]
