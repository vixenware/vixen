+++
title = "Process model"

[extra]
type = "Question"
status = "open"
created = "2026-05-26"
tags = ["tier-2", "process-model", "streams"]
+++

Demand-driven caching is incompatible with live observation of process state. If you can ask "is process P still running?" mid-evaluation, the answer is non-deterministic and the cache becomes invalid.

But Vixen also needs streams as values: `proc.stdout` as `Stream[Blob]`, observer closures that return `Stream[T]` while remote work is still running, parse-lines, parse-JSON, …

## Working shape

- The **process itself** is *not* observable as a mutable handle. No mid-run polling, no live stdout backpressure observation.
- Its **outputs** (stdout, stderr) *are* streams that can be consumed and transformed.
- The **observer** is a closure that receives `(Process, Tree)` and returns *something* — which can be `Stream[T]`, streaming back values while the remote work is still running.

This keeps the cache key well-defined (the process is opaque from the caller's perspective, identified by its inputs) while letting partial results flow back as a stream of values.

## What's not decided

- Exact API surface.
- What `Tree` looks like in observer arguments.
- How streams compose with caching for the observer's result (cache the closure-returned stream? re-derive each time?).

## Related

- [[Streams as first Tier-2 primitive?]]
- [[Tier 2 — Vixen Build]]
