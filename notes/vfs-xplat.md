# Cross-Platform VFS Architecture

## Problem Statement

Vixen provides a virtual filesystem across three platforms with different underlying mechanisms:

- **Linux**: FUSE kernel integration — direct mount, one process = one mount
- **Windows**: ProjFS kernel integration — direct mount, one process = one mount
- **macOS**: FSKit app extension — indirect, multiple peers share one extension process

The challenge is providing a unified logical model while respecting platform differences.

## Unified Logical Model

One mounted VFS contains one or more **namespaces**.
Each namespace is owned by exactly one live peer connection and backed by exactly one `Vfs` client.

- **Linux/Windows**: implicit root namespace (`""`) — the peer owns the entire mount
- **macOS**: explicit namespace per peer — multiple peers connect to the extension's Unix socket, each claiming a namespace

## Reserved Control Namespace

`/.vixenfs/*` is a **hard reserved namespace** on all platforms.

- No peer may claim it or serve content under it
- All backends (FUSE, ProjFS, FSKit) must intercept it
- `/.vixenfs/status.json` is synthesized by the extension/host process, never by a peer
- It bypasses normal namespace dispatch entirely

## macOS Connection Model

Peers connect to the extension's Unix socket (path provided via `status.json`).

### Connection Requirements

- Connection **MUST** include `vfs-namespace` metadata
- Namespace format: `[a-z0-9-]{1,64}` (lowercase ASCII alphanumeric + hyphen)
- Case-sensitive — `Build-213` and `build-213` are different, but only lowercase is valid
- **Reject** connection if metadata is missing
- **Reject** connection if namespace is already claimed (first-come, first-served)
- Invalid namespace format → reject before session establishment

### Bootstrap Invariant

`/.vixenfs/status.json` is served entirely by the extension.
It never depends on any peer connection existing.
It is always available immediately after the FSKit mount completes, even with zero peers connected.

### Status

- Served through the VFS at `/.vixenfs/status.json` — **not** via separate RPC
- Extension synthesizes this file from its internal peer table
- Fields:
  - `socket_path` — full path to the Vox listener socket
  - `bundle_identifier` — e.g., `eu.bearcove.vixenfs`
  - `bundle_version` — e.g., `1.2.3`
  - `bundle_path` — path to the extension bundle
  - `extension_pid` — process ID of the extension
  - `extension_generation` — UUID or monotonic ID for this extension lifecycle
  - `namespaces` — list of active namespaces with their peer connection state

### Init Config (Swift → Rust, called once)

```rust
#[repr(C)]
pub struct VixenFsConfig {
    pub bundle_version: *const c_char,
    pub bundle_identifier: *const c_char,
    pub app_group_path: *const c_char,
    pub socket_path: *const c_char,    // full path to the Vox listener socket
    pub is_debug: bool,
}
```

### Trust Model

All peers are **trusted local components** for now.
Any local process that can reach the Unix socket may claim any free namespace.
No entitlement, bundle ID, or audit token verification is performed at this layer.
(This is acceptable because the socket is inside the app group container, which is sandbox-isolated.)

### Disconnect Semantics

- Connection drops → namespace released automatically (no manual cleanup needed)
- Outstanding requests to a disconnected namespace → fail with `ENOTCONN`
- No automatic retry — the peer must reconnect and re-claim if needed
- Open handles, cached dentries, and inode state for a disconnected namespace become stale and should be flushed by the kernel or return errors on next access

## Service Traits

Just `Vfs` — no separate control service needed. Connection metadata carries the namespace.

```
Peer                          Extension (VixenFS)
  │                                │
  ├── conn #1: vfs-namespace="build-213" ───────┤
  │   → Vfs (lookup, read, write...)            │
  │                                             │
  └── conn #2: vfs-namespace="smoke-test-abc" ──┤
      → Vfs (lookup, read, write...)            │
```

## vx-vfs Changes

- `run_vox_session()` accepts a `namespace` parameter
- Connection metadata includes `vfs-namespace`
- Vox connection establishment supports the metadata field

## Current Platform Implementations

### Linux (FUSE)
- Spawns dedicated thread with tokio runtime
- Creates `FuseBridge` wrapping `Vfs` impl
- Mounts via `mount_with_unprivileged(bridge, mount_path)`
- Direct kernel integration, no RPC layer
- Implicit root namespace — the peer owns the entire mount tree

### Windows (ProjFS)
- Spawns dedicated thread
- Creates `ProjFsBridge` wrapping `Vfs` impl
- Mounts via `ProjectedFileSystem::new(mount_path, bridge)`
- Thread parks forever — ProjFS stays alive as long as object lives
- Direct kernel integration, no RPC layer
- Implicit root namespace — the peer owns the entire mount tree

### macOS (FSKit)
- Calls `vx_fskit::setup()` to launch extension
- Reads `socket_path` from `.vixenfs/status.json`
- Connects to Vox socket, serves `Vfs` with `vfs-namespace` metadata
- Indirect — extension process owns the actual mount
- Explicit namespace per peer connection