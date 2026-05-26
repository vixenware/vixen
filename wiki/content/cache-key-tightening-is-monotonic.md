+++
title = "Cache key tightening is monotonic; loosening is dangerous"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["cache", "correctness", "ops"]
+++

## Context

Vixen the language is demand-driven with automatic memoisation. Caching is not optional — it is how the language works. Two executions with the same key MUST produce the same result, or the cache silently returns wrong bytes. This is the worst possible failure mode for a build system.

The question is: what happens when we eventually realise a cache key we've been using is *not specific enough* — that two genuinely different invocations have been colliding under one key?

## Options considered

1. **Cache invalidation / flush.** Wipe the cache. Slow, expensive, breaks every customer's build until the cache rewarms. Ops-hostile.
2. **Migration step that re-keys existing entries.** Requires us to know the new key for the old inputs, which we generally don't (the inputs aren't necessarily replayable).
3. **Tighten the key derivation; let old entries become unreachable.** New key derivation produces a fresh keyspace. Old confused entries are not under any key the new derivation produces, so lookups against them all miss. They age out by LRU eviction.

## Decision

Option 3. We tighten by mixing more inputs into the key derivation. We never invalidate, never flush, never migrate.

## Consequences

- **Tightening is always safe.** Fresh keyspace, no collision possibility with prior bad entries. Customers see cache misses for the previously-confused invocations until the trusted-builder fleet repopulates correct entries under the new key. Slow but not broken.
- **Loosening is the dangerous direction.** Removing an input from the key derivation can cause two genuinely different invocations to collide. If a loosening change ships, every previously-correct entry might now serve wrong bytes for some new invocation. We do not loosen without an exhaustive migration plan; preferably we never loosen.
- **No emergency flush button.** This is a feature, not a bug. The flush button is the failure mode we are designing against.
- **Cache poisoning detection translates to "tighten and republish."** If we discover a producer was wrong about some input, tightening the key isolates the bad entries to a now-unreachable keyspace.

## Related

- [[Tier 1 — CCC]] — first product to ship this discipline.
- [[Tier 2 — Vixen Build]] — same machinery, more inputs.
- [[Trust is per-cache, not global]] — the other half of correct caching.
