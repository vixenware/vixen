+++
title = "exec caches the observer's return value, not the Process bytes"

[extra]
type = "Decision"
status = "accepted"
date = "2026-05-26"
tags = ["tier-2", "exec", "cache", "language"]
+++

## Context

`exec(args) where { ..., observer: Fn(Process, Tree) #sandbox -> T }` is the central build primitive. When the runtime demands its result, it derives a cache key and consults the cache. On a hit, what comes back: the observer's `T` value, or the raw `Process` (stdout, stderr, exit code, write-set Tree) for the observer to re-run against?

## Options considered

1. **Cache the Process bytes.** Hit avoids running the process but re-runs the observer fresh. Observer source isn't in the key; revving the observer doesn't invalidate the underlying process result. Cost: every hit pays observer evaluation again.
2. **Cache the observer's return value `T`.** Hit avoids both running the process *and* re-running the observer. Observer source must be in the key; revving the observer invalidates downstream cache entries. Cost: more cache entries when observers change.

## Decision

Option 2. The cache stores the observer's `T`. A hit returns the value immediately, with no process run and no observer evaluation.

## Consequences

- **Hits are cheap.** A cached value is ready; no work is done to materialise it beyond deserialisation.
- **Streams in return values are "fully realised but presented as streams."** When `T = Stream[U]`, a cache hit yields a stream whose elements are all already known. The consumer can iterate immediately at full bandwidth; backpressure semantics are vestigial on a hit.
- **Observer source identity must be in the cache key.** This is the price. Changing the observer's body invalidates every cache entry derived from it. Customers ship observer changes deliberately, the same way they ship source changes.
- **Observers want to be small.** Because their identity is part of the key, large observers create cache churn. The idiomatic shape is "observer = a tiny function that extracts the structured value the caller cares about." Big logic belongs in the rest of the Vixen program, not inside the observer.
- **Hit propagation through the build graph is uniform.** Every node's result is `T`-shaped — a value, not a fragment of a process — so the dataflow looks the same regardless of whether each node was a hit or a miss.
- **The cache shape becomes consistent across kinds of work.** `exec`, `acquire`, `render`, `stabilize` all produce a `T`; all cache that `T` against their inputs. There is no special "process result" cache vs "computed result" cache.

## Related

- [[Values are hashable, comparable, and serialisable]]
- [[Process model]]
- [[Streams as first Tier-2 primitive?]]
- [[Tier 2 — Vixen Build]]
- [[Cache key tightening is monotonic; loosening is dangerous]]
