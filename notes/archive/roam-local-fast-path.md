# Roam local fast paths: exact-layout control and brokered shared payloads

## Status

This is a design note for Vixen's local same-host RPC path.

It records two related but independent optimization ideas:

1. an exact-layout local control-plane fast path
2. a shared-memory payload broker for large buffers and other immutable payloads

The intent is to keep one RPC model while allowing much faster local execution
when peers are on the same machine and running the same build.

## Problem

Today, moving daemon RPC onto shared memory still leaves a lot of work on the
table:

- postcard encode/decode is not free
- variable-length layouts are not cheaply traversable in place
- control messages still materialize owned Rust values
- payload buffers still often get copied across process boundaries

So "shared memory transport" is not the same thing as "zero-copy RPC".

The local VFS/store hot path is especially sensitive to this. For local builds,
the relevant stack is often:

- `VixenFS` / FSKit extension
- `vx-vfsd`
- `vx-store`

Those peers are all on the same host, but we still pay for:

- repeated control-plane serialization
- partial decoding and reflection
- per-hop payload copying

## Core split

There are two distinct optimization axes here.

### 1. Exact-layout local control plane

This is about small control messages:

- requests
- responses
- channels
- capability exchange
- retries and session coordination

Instead of an evolving serialized codec like postcard, same-version local peers
could use exact little-endian layouts with known offsets and handles.

This is **not** compatible with schema evolution in the general case. That is
acceptable for the local fast path:

- local exact-layout mode requires the same build or otherwise exact schema
  agreement
- general Roam schema evolution still exists for remote peers and mixed-version
  cases

So the contract is:

- exact-layout local mode when versions match exactly
- evolving general RPC mode otherwise

### 2. Brokered shared payloads

This is about large payloads:

- file contents
- directory entry batches
- manifest/index blobs
- other VFS/store data-plane buffers

This optimization is independent of the control-plane codec. Even if control
messages stayed serialized, large payloads could still move by shared-memory
handle instead of by copy.

## Desired architecture

Keep one protocol model:

- same method/channel semantics
- same request/response model
- same capability system
- same retry/reconnect story

But allow different local execution modes underneath:

- a general evolving RPC backend
- an exact-layout local backend
- a brokered shared-payload facility

The control connection can start on a universal transport:

- Unix socket
- TCP
- another ordinary Roam transport

Then the peers negotiate capabilities during handshake:

- same host or not
- same build/schema or not
- availability of a local shared-payload broker
- availability of exact-layout local mode

If both sides agree, the session upgrades parts of its behavior:

- control messages may use exact local layouts
- payloads may use shared-memory handles

The original control transport stays available for coordination and fallback.

## Shared-payload broker

The simplest correct model is to make shared-memory allocation the
responsibility of one local authority process.

That means:

- one daemon owns allocation metadata
- one daemon owns handle space
- one daemon handles dead-peer cleanup
- one daemon handles quotas and accounting
- peers do not all participate in one symmetric shared allocator

This broker can be a dedicated daemon. It should be separate in concept from
the transport itself:

- Roam still handles control-plane RPC
- the broker handles shared payload lifetimes and allocation

Peers talk to the broker over RPC too. The RPC is control-plane only:

- allocate or lease a region
- publish a buffer
- export a handle to another peer
- drop a handle
- map a region
- reap a dead peer's outstanding refs

The bulk bytes live in shared memory, not inside ordinary RPC payloads.

## Why a broker beats pairwise shared arenas

Pairwise shared-memory negotiation works for two peers, but gets messy for
three:

- `A -> B -> C`
- dead-peer detection has to be coordinated
- forwarding handles across hops becomes ad hoc
- allocator ownership gets ambiguous

A broker gives one stable local domain:

- `A` can hand `B` a handle from the broker
- `B` can forward that handle to `C`
- all three speak in the same handle space
- cleanup remains centralized

This is especially attractive for the local VFS/store payload plane.

## Published reads versus writable leases

The lifecycle split should be explicit.

### Writable leases

Writable regions that are still being filled are connection- or session-scoped:

- if the peer dies mid-write, revoke them
- reclaim immediately
- do not treat them as published data

### Published immutable buffers

Once a payload is published for reading:

- treat it as immutable
- export read handles to peers
- keep it alive while exported refs exist

If a peer dies:

- its refs are gone
- the broker can reclaim those refs immediately or on the next GC pass

The key rule is:

- published immutable buffers do not need the live control connection to remain
  valid
- but dead-peer detection is sufficient to reap them when the peer is gone

## What this does not require

This does **not** require every heap object in every daemon to be shared-memory
resident from birth.

The promising target is narrower:

- the VFS/store payload plane

That includes things like:

- content blobs
- metadata replies
- directory entry batches
- manifest/index payloads

Control-plane objects can stay ordinary process-local Rust values.

Even within the payload plane, there are staged options:

- copy ordinary owned data into brokered shared memory on export
- keep hot payloads in shared slabs from birth
- later, let some services directly produce into broker-owned regions

So "shared from birth" is a useful long-term optimization for hot payloads, not
a requirement for the whole system.

## Programming model

The user-facing model should still feel like ordinary RPC returning values.

For example, a store read should feel conceptually like:

- return a higher-level shared-capable buffer type

not:

- every application handler manually receives an allocator token and writes into
  raw pointers

Under the hood, the runtime can still carry placement and broker metadata.
That is an implementation detail of the local fast path, not the public
programming model.

So the likely split is:

- surface API: return shared-capable values and buffers
- runtime implementation: negotiate exact-layout and brokered handle transfer

## Relationship to schema evolution

The exact-layout local fast path should not try to solve schema evolution.

Instead:

- exact-layout mode assumes exact local agreement
- evolving mode handles mixed-version peers and remote peers

That means same-host peers can start on a general transport and then negotiate:

- if exact versions match, use exact local layout
- otherwise stay on the evolving wire format

This keeps the fast path honest and keeps the general RPC story flexible.

## Conclusion

The right way to think about local fast paths is not:

- "replace the whole RPC stack with a separate SHM protocol"

It is:

- keep one RPC semantic model
- allow a same-version exact-layout control-plane fast path
- allow a brokered shared-payload fast path
- let peers negotiate into those modes after an ordinary bootstrap connection

That preserves one system model while still giving the local VFS/store path a
real chance at becoming close to zero-copy.
