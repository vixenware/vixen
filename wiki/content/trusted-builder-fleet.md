+++
title = "Trusted-builder fleet"

[extra]
type = "ArchitectureNote"
topic = "infrastructure"
created = "2026-05-26"
tags = ["tier-1", "infrastructure", "fleet", "operations"]
+++

What it is, where it runs, what runs on it.

## What it is

The pool of machines that materialise cache entries on demand-signal misses. Inputs come from publicly-accessible sources (crates.io, public git URLs — see [[CCC service]]); outputs are pushed back into the public cache under their derived keys. Customers never speak to a trusted builder directly; they speak to [[CCC service]], which queues misses for the fleet to pick up.

## Where it runs

- **Initial deployment:** Hetzner dedicated machines in Hel1 (Helsinki). Single-region, single-vendor, simplest ops shape.
- **Expansion:** a US presence is needed for latency to American customers. Candidate vendors surveyed: Latitude.sh as the closest Hetzner-in-the-US operational match; OVHcloud (Vint Hill VA / Hillsboro OR); Hetzner Ashburn itself if dedicated SKUs are live there; Equinix Metal at the premium end.
- The fleet is geographically distributed; **vx-store** replicates regionally. Replication shape TBD — see [[vx-store regional replication]].

## What runs on a trusted builder

- **Multiple `ccc-backend` VMs.** Each Hetzner dedicated host runs several ccc-backend instances. ccc-backend is the service that fronts the [[CCC service]] surface — terminates Vox connections from cargo-vixen clients, verifies OIDC, looks up keys, streams cache content, records telemetry, queues misses.
- **vx-store** (or a regional replica thereof). The CAS lives here for the cache content.
- **The backfill workers.** Pick up queued miss-signals, fetch source bytes from declared public locations, build, push outputs.

Notably *not* present at Tier 1:
- **`vx-runner` is Tier 2/3.** The runner service ships customer work into the executor fleet; that's a Tier-2-and-later capability, not part of the Tier 1 fleet shape.

## Backfill runs inside the same sandbox + VFS as Tier 2/3 customer work

The backfill workers do not just shell out and `cargo build`. They run the build inside the same sandbox + VFS substrate Tier 2 uses for customer code — [[VFS and sandbox stack|landlock on Linux now, seatbelt on macOS, AppContainer on Windows once designed]]. See [[Backfill builds run inside the Tier-2 sandbox]] for the rationale.

## Status

The fleet does not yet exist operationally. Tier 1 launch deploys the first instance. The component breakdown above is the architectural target; specific machine counts, regional split, and replication topology are commercial questions to resolve before launch.

## Related

- [[Tier 1 — CCC]]
- [[CCC service]]
- [[Demand-warmed cache]]
- [[Backfill builds run inside the Tier-2 sandbox]]
- [[vx-store regional replication]]
- [[Trust is per-cache, not global]]
