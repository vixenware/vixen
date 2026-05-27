+++
title = "Decide the process model"

[extra]
type = "Task"
status = "todo"
priority = "medium"
tiers = ["2"]
tags = ["language", "tier-2", "process-model"]
created = "2026-05-26"
+++

Demand-driven caching is incompatible with live observation of process state. If you can ask "is process P still running?" mid-evaluation, the answer is non-deterministic and the cache becomes invalid.

Working shape:

- The **process itself** is *not* observable as a mutable handle. No mid-run polling, no live stdout backpressure observation.
- Its **outputs** (stdout, stderr) *are* streams that can be consumed and transformed.
- The **observer** is a closure that receives `(Process, Tree)` and returns `T` — possibly `Stream[T]`, streaming values while the remote work is still running.

This keeps the cache key well-defined (the process is opaque from the caller's perspective, identified by its inputs) while letting partial results flow back as a stream.

Not yet decided: exact API surface; what `Tree` looks like in observer arguments (probably the sandbox's write-set Tree); how streams compose with caching for the observer's result.
