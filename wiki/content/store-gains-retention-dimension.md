+++
title = "Store gains a retention dimension for packages"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["store", "packaging", "infrastructure"]
+++

## Context

`vx-store` was designed as a build cache: every entry is in principle re-derivable from its inputs, evictable under LRU, costs money in proportion to working set size rather than cumulative-corpus size. Build artifacts age out; nothing breaks if they do, because the next build that wants them just re-derives them.

[[Package management for Vixen]] introduces content that breaks this model. A lockfile records a package handle directly; if Store evicts that handle's bytes under LRU, the lockfile dangles and the build can never reproduce. "Eviction breaks builds forever" is not a behaviour we ship.

This is a real semantic-surface change for Store, but it's also bounded. The full Store design can stay open; one retention dimension is enough to unblock packages.

## Decision

Store entries gain a **retention** flag: `transient` (default, evictable under LRU) or `pinned` (never evicted, replicated under stronger durability guarantees). Same content-addressing, same dedup, same wire — one extra word of metadata per entry.

## Consequences

- **Cost model splits cleanly.** Transient storage scales with build volume (most of Store). Pinned storage scales with cumulative published-package corpus — large but predictable. Adding a published-package version monotonically adds known bytes; running a build doesn't.
- **Eviction stays simple.** Transient = LRU. Pinned = never. No new algorithm, just a check in the eviction path.
- **Wire stays uniform.** A `put` operation carries a retention flag. Publish-time writes are pinned; cache writes are transient. Reads don't distinguish.
- **Replication policy can differ per tier.** Pinned content probably wants stronger guarantees (replicate to N regions, archive cold copies eventually). Transient can be opportunistic. Decoupled.
- **Yanks fit cleanly.** A yank removes the index entry but leaves the bytes pinned. Anyone whose lockfile already referenced the handle continues to build. New resolutions skip the yanked version. Mirrors cargo's effective behaviour.
- **No archival tier required on day one.** Pinned-in-hot-Store is fine until corpus growth says otherwise. Cold/archival storage is a future operational concern, not a design prerequisite.
- **Unpublish is effectively a no-op for bytes.** Crates.io doesn't really let you unpublish either (only yank, only for 72 hours). We can adopt the same policy: pinned bytes are pinned in practice forever; yanks are an index-side action.

## What this deliberately does *not* commit to

- The complete Store design. Replication topology, sharding, write-quorum semantics, garbage collection ordering — all stay open and live in [[vx-store regional replication]]. Pinned content participates the same way transient does, just with a tighter durability bar.
- A separate physical store. There is one Store; entries are tagged. No "package CAS vs build CAS" split — same machinery, different metadata.

## Related

- [[Package management for Vixen]]
- [[vx-store regional replication]]
- [[Lockfiles are build outputs, not input mutations]]
- [[Trusted-builder fleet]]
