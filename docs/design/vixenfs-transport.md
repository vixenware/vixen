# VixenFS Transport Draft

This is draft design material for the transport and session-binding model
between:

- `VixenFS` (the macOS FSKit extension)
- the Rust VFS backend serving the `Vfs` trait from
  [crates/vx-services/src/vixenfs.rs](/Users/amos/bearcove/vixen/crates/vx-services/src/vixenfs.rs)

This is not yet normative spec text.

## Goal

Migrate the VixenFS transport from:

- SHM bootstrap
- `RoamRuntime`
- per-session transport bootstrap state

to:

- Vox
- `local://...` Unix domain socket transport on macOS
- one stable listener endpoint in the app group container

without redesigning the `Vfs` RPC trait.

## Non-Goals

This design is not trying to:

- remove `sid`
- add `sid` to every `Vfs` RPC method
- create one Unix socket per session
- preserve SHM bootstrap as a compatibility layer
- redesign the FSKit item model

## `sid` Meaning

`sid` is not a transport identifier.

`sid` is the session namespace key.

Concretely, `sid` identifies:

- one mounted VFS root
- one session record in daemon/backend state
- one routed namespace inside the mounted filesystem

It remains useful after the SHM transport is removed.

What changes is not the meaning of `sid`, but how the transport binds a
connection to a particular `sid`.

## Transport Ownership

The transport ownership model is:

- `VixenFS` listens on one stable Unix domain socket in the app group container
- Rust-side backend code connects to that socket
- Rust-side connection setup sends `sid` in connection metadata
- `VixenFS` accepts the connection and binds it to that session
- `VixenFS` then issues ordinary `Vfs` RPCs over that connection

So:

- Swift/FSKit side owns the listener
- Rust/backend side is the initiator
- `sid` is supplied during session establishment, not per-request

## Why This Model

This model is preferred because it:

1. keeps one stable discovery point
- one app-group socket path
- no per-session socket churn

2. preserves `sid` as session identity
- session namespace and routing remain explicit
- transport remains separate from namespace identity

3. avoids infecting the `Vfs` trait with routing parameters
- existing methods like `lookup(parent_id, name)` remain unchanged
- session binding happens once, at connection establishment

4. matches the actual lifecycle better than URL-based `sid`
- the extension is the long-lived mounted side
- backend sessions come and go under that mounted extension

## Stable Listener

The extension should own a single stable listener path in the app group
container.

Examples of the intended shape:

- app group root discovered via `/.vixenfs/status.json`
- stable socket path underneath that root
- transport address represented as `local://<socket-path>` on the Rust side

The important property is:

- one well-known listener path per mounted extension instance

not:

- one listener path per `sid`

## Session Binding

Session binding happens during Vox connection establishment.

The backend initiator includes metadata that names the requested session.

At minimum this metadata includes:

- `sid`

Possible future metadata could include:

- build or mount provenance
- trace/debug identifiers
- capability flags

But only `sid` is required by this design.

## Per-Connection Scope

After accept, one accepted connection is scoped to exactly one `sid`.

That means:

- the connection is bound to one mounted VFS root
- ordinary `Vfs` item IDs are interpreted within that bound session
- the Swift side does not need to attach `sid` to every method call
- the Rust side does not need one `Vfs` trait per process-wide namespace

A useful way to think about it is:

- listener is global to the extension
- accepted connection is session-local

## Trait Stability

The intended outcome is to keep the Rust service trait unchanged:

- [crates/vx-services/src/vixenfs.rs](/Users/amos/bearcove/vixen/crates/vx-services/src/vixenfs.rs)

That means the migration should preserve:

- same service name
- same method set
- same codegen workflow
- same Swift client shape conceptually

The migration should change:

- codegen backend (`vox-codegen`)
- Swift runtime (`VoxRuntime`)
- transport establishment logic

not the logical RPC surface.

## Swift Side Consequences

On the extension side, this implies:

1. `VixenFS` hosts a Vox local listener
2. incoming connections are accepted and inspected for `sid` metadata
3. accepted connections are registered by session
4. `Volume` / item operations route to the session-bound client/connection

This replaces the previous model of:

- SHM ticket bootstrap
- guest attach
- per-session transport bootstrap socket

## Rust Side Consequences

On the Rust side, this implies:

1. macOS `vx-vfs` orchestration no longer serves SHM bootstrap state
2. backend code connects to the extension's stable listener
3. connect metadata includes `sid`
4. one backend service instance is created or selected for that session
5. the resulting accepted connection serves `Vfs` for that session

This is a transport rewrite, not a schema rewrite.

## Rejected Alternatives

### Per-session Unix sockets

Rejected.

Reasons:

- makes `sid` transport-shaped instead of namespace-shaped
- creates unnecessary socket churn and cleanup complexity
- keeps too much of the old SHM bootstrap mental model alive

### `sid` in every RPC method

Rejected for now.

Reasons:

- pollutes the `Vfs` trait with session-routing parameters
- forces schema/codegen churn across the entire surface
- duplicates session information that is already connection-scoped

### `sid` encoded in the mount URL

Not preferred.

Reason:

- current lifecycle and mount flow already use a generic `vixen://session`
  source and a separate mounted-state contract
- session binding is better represented as connection metadata than as URL
  syntax

## Near-Term Implementation Plan

1. keep the Rust `Vfs` trait unchanged
2. keep Vox codegen as the Swift binding source
3. host one stable local listener in `VixenFS`
4. have Rust connect to that listener with `sid` metadata
5. bind each accepted connection to exactly one session
6. delete SHM bootstrap and per-session transport bootstrap state
7. only after that, re-enable the macOS `vx-vfs` orchestration path

## Open Questions

1. What exact metadata key should carry `sid`?
2. Should `VixenFS` accept multiple simultaneous session-bound connections on
   one mounted instance, or is one active session enough initially?
3. Where should the stable app-group socket path be published and validated?
   Current likely answer:
   - app group path remains discoverable via `/.vixenfs/status.json`
   - socket path is derived from that root by convention
4. Should the accepted Swift-side connection registry remain keyed by `sid`, or
   should it eventually collapse to a single active session model?
