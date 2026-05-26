+++
title = "Streams as first Tier-2 primitive?"

[extra]
type = "Question"
status = "open"
created = "2026-05-26"
tags = ["tier-2", "primitive-ordering", "streams"]
+++

Tier 2 needs several foundational primitives: streams as runtime values, stream ops (lines, parse_json[T], …), closure shipping, executor classes with selection / fan-out, TreeEntry, cache-key + sandbox, HTTP fetch + tarball extract, multi-source dep resolution.

Each is independently implementable with its own narrow failing test. M1 / M2 / M3 are integration checkpoints, not implementation drivers.

## The candidates for "first"

- **Streams as runtime values.** `exec → Stream[Blob]`, observer returns `Stream[T]`. Foundational. Orthogonal to the cache decision (exec is uncached today, so streaming a Blob out of a non-cached exec doesn't commit us to anything cache-shaped). Trivially observable in tests. Unblocks the observer-returns-Stream shape that closure shipping will need to express.
- **Executor classes.** The headline foundational thing for "remote execution is first-class." But hard to exercise selection without something to ship.
- **Closure shipping.** The deepest architectural decision. Benefits from streams + executor classes existing as concrete shapes to refer to.

## Disposition

Instinct on the table: streams first. Orthogonal to the cache decision, immediately observable, unblocks the other two.

## Related

- [[Process model]] — process not mutable, but stdout/stderr are streams.
- [[Tier 2 — Vixen Build]]
