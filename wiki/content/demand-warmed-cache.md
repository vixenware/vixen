+++
title = "Demand-warmed cache"

[extra]
type = "ArchitectureNote"
topic = "cache"
created = "2026-05-26"
tags = ["cache", "trust", "operations"]
+++

How the cache gets populated without extending push access to clients.

## The flow

1. A CCC (or Vixen Build) client computes a cache key for some invocation.
2. It looks up the key in the cache. If hit, it returns the cached bytes and skips the underlying invocation.
3. If miss, two things happen in parallel:
   - The client runs the invocation locally (or on its own runner) and uses the result.
   - The client emits a **demand signal** to the trusted-builder service: "someone asked for key K and missed."
4. The trusted-builder service receives the demand signal and adds key K to its work queue.
5. The trusted-builder fleet — running on official, attested infrastructure — eventually picks up K, materialises the result, and inserts it into the cache under K, signed.
6. Future lookups by other clients hit.

## Trust properties

- **No client ever pushes.** Push access is restricted to trusted builders. Cache poisoning by clients is impossible by construction.
- **Demand signals are unprivileged.** They are pure observations of demand. They cannot inject content.
- **The trusted-builder fleet is the choke point for trust.** Whatever signatures / attestations / TEE evidence we require, they apply uniformly to every entry the public cache serves.
- **Customer-private caches override at the gate.** An enterprise customer can configure their internal cache to accept their own builders' pushes; the public cache still doesn't accept them. See [[Trust is per-cache, not global]].

## Why not just have clients push?

It's the difference between a cache and a CVE. Once clients can write, any compromised client can poison every reader. The only way to keep the cache safe is to make writes structurally impossible from the untrusted side.

## What the demand signal actually carries

Not just the cache key. The signal carries the **structured inputs** — argv, env subset, read-set as Store content-addressed handles, write-set spec, working directory — because the trusted-builder fleet needs enough information to actually replay the invocation on official infrastructure. A bare key has no replay value. See [[CCC service]].

The inputs reference Store-addressed content; they do *not* embed file bytes inline. A trusted builder picking up the queue entry fetches the read-set from Store on its own, materialises it inside a sandbox VFS, executes, and pushes the output back into the cache under the derived key.

## Privacy

The inputs are not plaintext source. The read-set is content-addressed handles into Store; only Store knows what bytes those handles resolve to, and Store is gated by the same trust model. argv and env are visible to us — they have to be for replay to work — but `cargo-vixen` filters env to a determined-relevant subset before sending.

We do *not* attach customer identity, repo URL, or other tenant metadata to the public demand signal in the basic flow. Premium tiers can opt in to attaching identity if they want priority queue placement. See [[Tier 2 pricing shape]].

## Failure modes

- **Demand signals lost in transit.** The customer's build still completes (they ran the invocation locally). The cache just stays cold for that key. Eventually some other customer signals the same demand and the builder catches up. No correctness issue.
- **Trusted builder fleet undersized.** Cache warms slowly. Customer sees degraded hit rate. Operational problem, not correctness problem.
- **Cache key wrong.** See [[Cache key tightening is monotonic; loosening is dangerous]]. Tighten, never loosen.

## Related

- [[Tier 1 — CCC]]
- [[Trust is per-cache, not global]]
