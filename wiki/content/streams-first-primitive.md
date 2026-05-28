+++
title = "Implement streams as the first Tier-2 primitive"

[extra]
type = "Task"
status = "todo"
priority = "high"
tiers = ["2"]
tags = ["language", "tier-2", "streams"]
created = "2026-05-26"
+++

Tier 2 needs several foundational primitives — streams as runtime values, ability-based executor selection / fan-out, closure shipping, TreeEntry, cache-key + sandbox design, HTTP fetch + tarball extract, multi-source dep resolution. Each is independently implementable with its own narrow failing test.

Streams first. Reasons:

- Orthogonal to the cache decision. Streaming a `Blob` out of a non-cached exec doesn't commit us to anything cache-shaped.
- Immediately observable in tests — assert against a `Stream[Bytes]` produced by an exec.
- Unblocks the observer-returns-`Stream[T]` shape that closure shipping will need to express.

Surface lives in `core.vx` already (`Stream[T]` namespace, the operations on it). What's missing is the runtime substrate for evaluating those operations against actual process outputs.

Sub-questions to resolve as part of the implementation: hot vs cold (probably cold on miss, fully-materialised on hit), backpressure semantics, finite vs possibly-infinite, what happens when a stream value crosses the cache boundary (already decided: streams are fully realised on hit; see [Cache and trust](/cache-and-trust/#exec-caches-the-observer-s-return-value-not-the-process-bytes)).
