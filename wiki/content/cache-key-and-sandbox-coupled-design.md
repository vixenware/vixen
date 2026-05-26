+++
title = "Cache key + sandbox: one design doc?"

[extra]
type = "Question"
status = "open"
created = "2026-05-26"
tags = ["cache", "sandbox", "process"]
+++

The cache key for an exec invocation is "exhaustively, every byte the process read, plus the binary, plus the invocation arguments." The sandbox is what enforces that the process *cannot* read bytes outside the read-set we hashed.

These are not two decisions. They are one decision with two halves. If the sandbox lets the process read `/etc/passwd` and we didn't hash it, the cache is wrong. The sandbox spec and the cache-key spec must be derived from each other.

## What's not decided

- Should we write a single design document covering both, before any cache code lands?
- What is the minimum sandbox closure we accept (FHS-free, store-path-only, sealed /proc and /etc — Nix-style)?
- Which environment variables are key-bearing?
- How do we handle ambient toolchains under "fact discovery / attached obligations"?
- How do digests of declared identities (e.g. "xcode 26.4 manifest hash") collapse the read-set into a single token, under what trust assumptions?

## Disposition

Pre-decision, exec is **not cached** in Vixen today — deliberately. Better to ship no cache than a half-baked one that silently returns wrong bytes.

The instinct on the table: yes, write the doc. Cache correctness is irreversible at scale and the sandbox design falls out of "what bytes can a process read."

## Related

- [[Cache key tightening is monotonic; loosening is dangerous]]
- [[Trust is per-cache, not global]]
- [[Tier 2 — Vixen Build]]
