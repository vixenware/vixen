+++
title = "Trust is per-cache, not global"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["cache", "trust", "security"]
+++

## Context

Even with provably correct cache keys, anyone who can write `(key, value)` pairs into a shared cache can poison it for every reader. Cache poisoning is the headline attack against any shared build cache.

The question is: what is the trust model?

## Options considered

- **Per-user / per-org cache, no sharing.** Loses the value proposition. Every user pays full build cost.
- **Signed entries (Nix substituter model).** Producer signs; consumer checks against a trust list. Trust is human-decided. Workable but requires a trust list per consumer.
- **Reproducibility quorum.** N independent producers; accept entry iff all N agree bit-for-bit. Requires reproducibility. Strong, slow, expensive.
- **TEE attestation (SGX / SEV / Nitro / Apple Secure Enclave).** Hardware as trust root. Strongest, doesn't deploy everywhere.
- **Local-recompute verify.** Defeats the entire purpose of caching.

## Decision

**Trust is a property of a path *into* a cache, not a property of the cache itself.**

- Each cache has its own ingress policy. The policy decides which signatures / attestations / producer identities are acceptable.
- The local on-disk cache is trusted by construction — you wrote it.
- Reads from any other cache flow through a *gate* that verifies the entry against the policy before insertion into the local cache.
- Once an entry is in the local cache, it is read unconditionally (it has already been vetted).

A consequence: different caches with different policies coexist naturally. A dev cache might accept employee pushes (cheap, fast, blast-radius is one team). A prod cache might only accept attested-runner outputs (slow, expensive, untamperable). Same key derivation; different gates.

## Consequences

- **Caching ships *with* the trust gate from day 1.** Not "cache first, retrofit signing later." That sequencing produces CVE-grade poisoning.
- **No client can poison a cache it does not have ingress rights to.** Customer CCC instances do not have ingress rights to the public cache; they emit anonymous demand signals instead. See [[Demand-warmed cache]].
- **Org policy is expressible.** A regulated industry customer can configure their internal cache to require N-of-M signed attestations. A startup can configure theirs to accept everything signed by the team's GitHub identities. The architecture treats both as instances of the same primitive.
- **No trust list shipped with Vixen.** Trust is per-deployment configuration, not baked into the runtime.

## Related

- [[Tier 1 — CCC]] — first deployment, exercises the simplest case (one trusted producer, many read-only consumers).
- [[Cache key tightening is monotonic; loosening is dangerous]] — the correctness half of the same problem.
- [[Cache key + sandbox: one design doc?]] — still open.
