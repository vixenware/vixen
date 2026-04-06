# Cross-Platform VFS Architecture

## Problem Statement

Vixen provides a virtual filesystem across three platforms with different underlying mechanisms:

- **Linux**: FUSE kernel integration — direct mount, one process = one mount
- **Windows**: ProjFS kernel integration — direct mount, one process = one mount
- **macOS**: FSKit app extension — indirect, multiple peers share one extension process

The challenge is providing a unified logical model while respecting platform differences.

## Unified Logical Model

**One peer = one prefix = one `Vfs` client**

- FUSE/ProjFS: naturally one-mount-per-process, the prefix is implicit
- macOS: multiple peers connect to the extension's Unix socket, each claiming a prefix

## macOS Connection Model

Peers connect to the extension's Unix socket (path provided via `status.json`).

### Connection Requirements

- Connection **MUST** include `vfs-prefix` metadata
- Metadata value: alphanumeric characters + `-` only (e.g., `build-213`, `smoke-test-abc`)
- No `/`, no dots, no special characters
- **Reject** connection if metadata is missing
- **Reject** connection if prefix is already claimed (first-come, first-served)
- Connection drops → prefix released automatically (no manual cleanup needed)

### Status

- Served through the VFS at `/.vixenfs/status.json` — **not** via separate RPC
- Extension synthesizes this file from its internal peer table
- Contents: list of active peers with their claimed prefixes, connection state, bundle version, app group path, socket path

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

`status.json` should include `socket_path` directly — no path construction assumptions.

## Service Traits

Just `Vfs` — no separate control service needed. Connection metadata carries the prefix.

```
Peer                          Extension (VixenFS)
  │                                │
  ├── conn #1: vfs-prefix="build-213" ──────────┤
  │   → Vfs (lookup, read, write...)            │
  │                                             │
  └── conn #2: vfs-prefix="smoke-test-abc" ─────┤
      → Vfs (lookup, read, write...)            │
```

## vx-vfs Changes

- `run_vox_session()` accepts a `prefix` parameter
- Connection metadata includes `vfs-prefix`
- Vox connection establishment supports the metadata field

## Current Platform Implementations

### Linux (FUSE)
- Spawns dedicated thread with tokio runtime
- Creates `FuseBridge` wrapping `Vfs` impl
- Mounts via `mount_with_unprivileged(bridge, mount_path)`
- Direct kernel integration, no RPC layer

### Windows (ProjFS)
- Spawns dedicated thread
- Creates `ProjFsBridge` wrapping `Vfs` impl
- Mounts via `ProjectedFileSystem::new(mount_path, bridge)`
- Thread parks forever — ProjFS stays alive as long as object lives
- Direct kernel integration, no RPC layer

### macOS (FSKit)
- Calls `vx_fskit::setup()` to launch extension
- Reads `socket_path` from `.vixenfs/status.json`
- Connects to Vox socket, serves `Vfs` with `vfs-prefix` metadata
- Indirect — extension process owns the actual mount

## Future Considerations

- Prefix routing trie for efficient FS operation dispatch
- Dynamic `status.json` generation from peer table
- Graceful handling of peer disconnects (prefix flush)
- Test isolation via unique prefixes per test run