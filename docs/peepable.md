# `peeps::peepable` instrumentation guide

This is the quick reference for adding causal wait edges with `peeps`.

## What it does

`peepable` wraps a future so `peeps` records:

- which task was waiting
- what resource it was waiting on
- how often that wait stayed pending
- total pending time

Use it on real await points that can block progress (socket I/O, RPC waits, channel waits, lock waits).

## API

Two equivalent forms:

```rust
use peeps::PeepableFutureExt;

stream.read(&mut buf).peepable("socket.read").await?;
```

```rust
use peeps::peepable;

peepable(stream.read(&mut buf), "socket.read").await?;
```

## Naming conventions

Use stable, low-cardinality names. Prefer dot-separated resource kinds.

Good:

- `socket.connect`
- `socket.accept`
- `socket.read`
- `socket.write_all.header`
- `socket.write_all.payload`
- `socket.flush`
- `rpc.client.call`
- `rpc.server.request`
- `channel.recv`
- `channel.send`
- `lock.wait`

Avoid:

- raw payload data in names
- request-specific random IDs in names
- very long dynamic strings

If you need IDs, keep them in RPC metadata/context, not in the resource name.

## Where to instrument in roam

Minimum high-value points:

1. Connector and listener waits
- `connect().await` -> `socket.connect`
- `accept().await` -> `socket.accept`

2. Framing I/O
- `read(...).await` -> `socket.read`
- `write_all(header).await` -> `socket.write_all.header`
- `write_all(payload).await` -> `socket.write_all.payload`
- `flush().await` -> `socket.flush`

3. RPC lifecycle waits
- client call wait -> `rpc.client.call`
- server handler wait -> `rpc.server.request`

4. Sync primitives
- `recv().await` -> `channel.recv`
- bounded send waiting -> `channel.send`
- lock contention waits -> `lock.wait`

## Example patch pattern

Before:

```rust
let n = self.stream.read(&mut tmp).await?;
self.stream.write_all(&header).await?;
```

After:

```rust
use peeps::PeepableFutureExt;

let n = self.stream.read(&mut tmp).peepable("socket.read").await?;
self.stream
    .write_all(&header)
    .peepable("socket.write_all.header")
    .await?;
```

## Context propagation (cross-process)

`peepable` tracks local waiting edges. Cross-process causality still relies on RPC metadata propagation.

For full chains across processes, keep both:

- `peepable(...)` on blocking await points
- RPC metadata propagation (`chain_id`, `span_id`, `parent_span_id`) on request boundaries

## Agent checklist

1. Add `use peeps::PeepableFutureExt;`
2. Wrap every meaningful blocking `.await` with `.peepable("<resource>")`
3. Keep names stable and low-cardinality
4. Build and run checks
5. Verify dashboard shows new wait resources under task/resource views
