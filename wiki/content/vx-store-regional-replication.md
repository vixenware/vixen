+++
title = "Design vx-store regional replication topology"

[extra]
type = "Task"
status = "todo"
priority = "medium"
tiers = ["1", "2"]
tags = ["store", "replication", "infrastructure"]
created = "2026-05-26"
+++

The trusted-builder fleet is geographically distributed; `vx-store` (the CAS that holds cache content) is regionally replicated. The exact topology is not yet decided.

What needs deciding:

- **Replication style.** Active-active (every chunk lands in every region's replica), regional primary with read replicas, geographically partitioned with cross-region pull on miss, something else.
- **Consistency model.** Strong (writes visible in every region immediately, latency cost), eventual (regions converge, brief stale reads), read-your-writes per tenant.
- **Cache-hit hot path.** Does a `cargo-vixen` client in the US hit the US replica only, or fail over to EU on a regional miss?
- **Backfill destination.** When a US-region trusted builder produces an output, does it land in the US replica and replicate to EU, or land in a canonical region and propagate?
- **Eviction independence.** LRU decisions made per-region or globally?
- **Per-retention-tier differences.** Pinned content (packages) probably wants stronger durability than transient (cache). Different topologies per tier?

Constraints from above: bytes are content-addressed (replication is trivially safe at the byte layer); writes come only from trusted builders.

Unblocks resolution: concrete spec for the first two regions (Hetzner Hel1 + the chosen US vendor), expected traffic patterns once Tier 1 has a few customers, what failover SLA we want to promise.
