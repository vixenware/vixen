+++
title = "CCC service"

[extra]
type = "ArchitectureNote"
topic = "ccc-wire"
created = "2026-05-26"
tags = ["tier-1", "ccc", "service", "wire"]
+++

The server `cargo-vixen` (the [[CCC is a cargo fork, not a rustc wrapper|CCC cargo fork]]) speaks to is a **CCC-specific Vox-powered service** — not the [[VFS and sandbox stack|Store CAS]], not the Orchestrator, not `vx-runner`. None of those are the right shape for the front door:

- **Store is a dumb CAS.** Chunk operations, content-addressed, no auth, no telemetry. Anyone with a hash can fetch a chunk; nothing checks tenancy or quota. Store sits behind the CCC service; it is not exposed to customers directly.
- **Orchestrator is Tier-2-shaped.** Its `eval_function` is for evaluating Vixen build graphs end-to-end, not for "look up rustc-invocation key K."
- **vx-runner is the worker.** It executes actions inside the sandbox. Customer-side cargo doesn't and shouldn't speak to it.

## What the CCC service is

A dedicated front-door service that owns the customer-facing concerns Store deliberately doesn't. From the customer's cargo-vixen instance:

- **Credential check.** OIDC token verification (see [[CCC authentication uses OIDC trusted publishing]]). Decide whether the requester is an authenticated paying customer and which tenancy/tier they sit in.
- **Cache key lookup.** Given a derived cache key (rustc binary hash + argv + env subset + read-set hashes + cwd + …), is there a known output? If yes, return a way to get it. If no, record the miss.
- **Telemetry.** Per-request, per-tenant metering: hit count, miss count, bytes served, demand-priority budget consumed. Feeds [[Tier 2 pricing shape|the usage meter]] and the [[Demand-warmed cache|trusted-builder work queue]].
- **Demand signal emission.** Misses become entries on the trusted-builder queue, with priority weighted by tenant tier and authentication status. See [[Unauthenticated CCC requests miss but contribute demand signal]].
- **Access control on writes.** No customer pushes to the public cache; writes come only from trusted builders. The CCC service is the gate that enforces that. See [[Trust is per-cache, not global]].

## Cache key derivation is server-side

The client (`cargo-vixen`) sends *structured inputs* — not a precomputed key. The server derives the key from those inputs.

This is forced by the demand-warming requirement: if a request misses, the trusted-builder fleet needs enough information to *actually replay* the invocation later on official infrastructure. A bare cache key carries no information about what to build; the inputs do. Server-side derivation is the only shape that lets a miss become a queued, replayable work item.

Side benefit: the derivation algorithm is centrally revvable. We can [[Cache key tightening is monotonic; loosening is dangerous|tighten the key]] on the server without re-rolling every customer's cargo-vixen binary.

## Wire shape depends on access, not on registry

The boundary between "trusted-builder fetches the source" and "customer ships the source" is **access**: can the trusted-builder fleet reach the source bytes without customer credentials?

**Publicly accessible sources (the common case).** crates.io entries, public git repos referenced by `Cargo.toml`'s git-dep URL, public upstream URLs for vendored deps. The trusted builder fetches on demand from the declared location. Wire is tiny: crate identity (name + version + crates.io hash, or git URL + commit hash) + build target / profile / features + rustc toolchain identity + argv/env. The customer uploads nothing. Cache coherency is fine because git commit hashes and crates.io hashes are already content-addressing.

**Privately accessible sources.** Private GitHub/Forgejo repos, private cargo registries, customer-only vendored sources. The trusted builder cannot reach them; the customer's tree is the only authoritative source. Requires the private cache + per-customer trust settings to ensure the bytes only travel where the customer has authorised. See [[Private crate caching: when is it worth it?]] and [[Trust is per-cache, not global]].

The boundary is decided per-dep, not per-workspace. A workspace with three crates.io deps, one public-git dep, and one private-git dep routes the first four through the public cache and the fifth through whatever private-cache mechanism applies.

## What sits behind it

Store. On a hit, the CCC service translates the derived key into Store-shaped chunk operations and streams chunks back to the client (or hands off signed Store handles for direct fetch — the exact shape is part of the wire design). On a miss, the CCC service records the structured inputs as a queue entry for the trusted-builder fleet.

## Deployment shape: two layers

ccc-backend splits along a clean stateful/stateless boundary.

**Scaled-out, geographically distributed (n=many).** Serving hits, recording misses, identity verification, telemetry ingest. These are near-stateless — every instance can answer a request independently against shared CAS-replicated Store ([[vx-store regional replication]]) and a shared identity service. Deployed across regions, scales with request volume.

**Schedulers (n=1, central).** Demand-queue ordering, backfill dispatch, priority computation. A singleton owns the authoritative work queue because multiple instances would race on "who picks up this miss next?" Classic scheduler pattern; redundancy at this layer is handled by leader election rather than horizontal scale-out.

The edge layer enqueues a miss as soon as it observes one (with identity attached for tier-weighted priority); the scheduler decides when and where it gets materialised.

## Status

The CCC service does not yet exist in code under that name. Tier 1 ships it; its wire shape (operations, request/response types, streaming pattern, telemetry channel) is to be designed.

## Related

- [[Tier 1 — CCC]]
- [[CCC is a cargo fork, not a rustc wrapper]]
- [[CCC authentication uses OIDC trusted publishing]]
- [[Unauthenticated CCC requests miss but contribute demand signal]]
- [[Demand-warmed cache]]
