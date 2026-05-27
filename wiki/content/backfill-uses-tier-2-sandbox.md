+++
title = "Backfill builds run inside the Tier-2 sandbox"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["tier-1", "tier-2", "backfill", "sandbox", "operations"]
+++

## Context

When [[Demand-warmed cache|a cache miss is queued]], a worker on the [[Trusted-builder fleet]] eventually picks it up, fetches the declared inputs, builds the artifact, and pushes the output into the cache.

The simplest possible implementation is: drop the inputs in a tempdir on the trusted builder, `cargo build`, copy the output out. That's `RUSTC_WRAPPER`-style; it works; it's cheap to ship.

We don't ship that.

## Options considered

1. **Unconstrained backfill.** Worker runs the build with full host access. Outputs go to the cache.
2. **Backfill inside the same sandbox + VFS substrate Tier 2 customer work will use.** Worker mounts a VFS view scoped to the declared read-set, runs the build under the sandbox (landlock / seatbelt / AppContainer), captures outputs from the declared write-set.

## Decision

Option 2. Backfill runs inside the full Tier-2 sandbox + VFS stack even though no customer code is involved.

## Consequences

- **Cache validity guarantee.** The sandbox enforces that the build cannot read bytes outside the declared read-set. The cache key is derived from that same read-set. Therefore the cache key faithfully captures everything the build depended on — by construction, not by audit. Without the sandbox, the build could quietly read `/etc/something` we didn't hash, and the cache entry would be subtly wrong.
- **Tier 2/3 dogfooding.** Every backfill is a production exercise of the sandbox + VFS substrate. Bugs in the Linux sandbox, the macOS SBPL profile, the Windows mechanism (once designed), the VFS mount lifecycle, the FUSE/FSKit/ProjFS layers — they get hit on real workloads with real toolchains before any customer's build depends on them.
- **One stack, not two.** We do not maintain a "lightweight" backfill path and a "real" customer-build path. The sandbox + VFS is the only path; backfill exercises it because it is the substrate.
- **Per-platform completeness becomes Tier-1-blocking.** The macOS [[VFS and sandbox stack|seatbelt sandbox]] needs to be restored from git history and the [[Windows sandbox direction]] needs to be designed before backfill on those platforms is real. Cache entries for `darwin-arm64` and `windows-x86_64` are gated on those landing.
- **Cost.** Sandbox + VFS adds startup cost per build (mount, configure, exec). For a one-off backfill this is negligible compared to the build itself; for a high-volume queue it adds up. Operational cost worth absorbing for the correctness and dogfooding payoff.

## Related

- [[Trusted-builder fleet]]
- [[VFS and sandbox stack]]
- [[Windows sandbox direction]]
- [[Demand-warmed cache]]
- [[Cache key tightening is monotonic; loosening is dangerous]]
