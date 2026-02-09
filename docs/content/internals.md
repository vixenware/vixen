+++
title = "Internals"
insert_anchor_links = "heading"
+++

# Build Architecture

This page describes how vixen builds things internally. It's intended as a
reference for contributors and for validating our understanding of the system.

## Process Model

vixen uses a multi-process architecture. The CLI is thin; the real work happens
in long-lived daemons that communicate over roam RPC via Unix sockets.

```aasvg
 +---------+  roam/UDS  +-----------+  roam/UDS  +------------+
 | vx      |----------->| vxd       |----------->| vx-store   |
 | (CLI)   |            | (daemon)  |            | (CAS)      |
 +---------+            +-----+-----+            +------+-----+
                              |                         ^
                              | roam/UDS                |
                              v                         |
                        +-----------+    roam/UDS       |
                        | vx-runner |-------------------+
                        | (worker)  |
                        +-----------+
```

- **vx** — Thin CLI client. Sends a build request to vxd, waits for the result.
- **vxd** — The orchestrator. Parses manifests, resolves dependencies, computes
  the action graph using picante incremental queries, dispatches compile/link
  actions to vx-runner.
- **vx-store** — Content-addressed blob storage. Stores and retrieves blobs by
  blake3 hash. Also stores manifests (toolchain manifests, tree manifests, node
  manifests) and maintains the cache key mapping (cache_key -> manifest_hash).
- **vx-runner** — The compilation worker. Receives compile requests, populates
  a VFS with inputs from CAS, runs the compiler, flushes outputs back to CAS.

In the default local configuration, vxd spawns vx-store and vx-runner as child
processes (they die with their parent via `ur_taking_me_with_you`). But the
architecture is wired for remote execution: vx-store and vx-runner communicate
with vxd over roam RPC, which works over TCP just as well as Unix sockets. A
local vx-store could act as a read-through/write-through cache to a cloud store,
and runners could live entirely on remote machines. Only vxd needs to run locally.

## Content-Addressed Storage

Everything is a blob. Toolchain binaries, source files, compiled `.rlib` files,
manifests — all stored the same way. Write bytes, get a blake3 hash back. Read
bytes by hash.

```aasvg
                       +----------------------------------+
                       |            vx-store               |
                       |                                   |
  ToolchainManifest ---+-->  Blob Store                    |
  TreeManifest --------+-->  (blake3 hash -> bytes)        |
  NodeManifest --------+-->                                |
  source files --------+-->                                |
  .rlib / .o / bins ---+-->  Cache Index                   |
                       |     (cache_key -> manifest_hash)  |
                       +-----------------------------------+
```

There are three kinds of manifests, all stored as JSON blobs:

- **ToolchainManifest** — Lists components (rustc, rust-std, zig-exe, zig-lib)
  and their tree manifest hashes.
- **TreeManifest** — Lists files in a tree: relative path, blob hash, size,
  executable bit. Used for both toolchain components and source snapshots.
- **NodeManifest** — Build output record: node ID, cache key, production
  timestamp, and output entries (logical name, filename, blob hash, size).

## The VFS

vx-runner mounts a virtual filesystem backed by CAS. On macOS this is a FSKit
extension; on Linux it's FUSE. The VFS is mounted once per runner lifetime at a
stable path (default: `~/.vx/vixen/runner/`).

The mount serves multiple concurrent **action prefixes**:

```
~/.vx/vixen/runner/                    ← single persistent mount
├── build-192a3f…-04a1/                ← action prefix
│   ├── toolchain/
│   │   ├── bin/rustc                  ← CAS blob, served lazily
│   │   └── sysroot/
│   │       └── lib/rustlib/…/libstd-*.rlib
│   ├── src/
│   │   └── main.rs                    ← CAS blob (source snapshot)
│   ├── deps/
│   │   └── libutil.rlib               ← CAS blob (dependency artifact)
│   └── out/
│       └── hello                      ← writable, flushed to CAS after compile
└── build-192a3f…-7b22/                ← another concurrent action
    └── …
```

Each action prefix is ephemeral: created before a compile, torn down after.
Multiple actions share the same VFS mount — this is safe because hermeticity
comes from the sandbox (seatbelt on macOS, seccomp/landlock on Linux), not from
mount isolation. The sandbox restricts each subprocess to only access its own
prefix.

### How a file read works

When rustc reads `toolchain/bin/rustc` through the VFS:

```aasvg
  rustc process
      |
      | read() syscall
      v
  +-------------------+
  | FSKit (macOS)     |
  | or FUSE (Linux)   |
  +--------+----------+
           |
           | roam RPC
           v
  +-------------------+    get_blob(hash)    +------------+
  | vx-runner         |--(cache miss?)------>| vx-store   |
  | CasVfs            |<---------------------| (CAS)      |
  | +---------------+ |    blob bytes        +------------+
  | | blob_cache    | |
  | | (OnceCell per | |
  | |  blob hash)   | |
  | +---------------+ |
  +-------------------+
           |
           | byte range
           v
       response
```

1. The FSKit extension (macOS) or FUSE handler (Linux) receives the read syscall.
2. It forwards it to vx-runner's VFS backend via roam RPC (shared memory on macOS, stream transport elsewhere).
3. vx-runner looks up the item in its in-memory VFS tree, finds the CAS blob hash.
4. vx-runner fetches the blob from vx-store (or hits its in-process blob cache).
5. Returns the requested byte range.

The blob cache in vx-runner (`CasVfs::blob_cache`) is safe because blobs are
immutable — same hash always means same content. Concurrent reads of the same
blob coalesce via `OnceCell` (single-flight).

## Input Resolution

Before building anything, vxd needs to figure out *what* to build and *with what*.

### Manifest parsing

vxd reads `Cargo.toml` and extracts:

- **Package**: name, edition (defaults to 2021)
- **Targets**: lib (`src/lib.rs`) and/or bin (`src/main.rs`). Only one `[[bin]]`
  target is allowed in v0.
- **Dependencies**: path dependencies and registry dependencies (with `Cargo.lock`
  for version pinning).

Anything outside v0 scope is **rejected with an error**: workspaces, build scripts,
proc macros, `[features]`, `[dev-dependencies]`, `[build-dependencies]`, git
dependencies, optional dependencies, multiple bin targets, `[[test]]`/`[[bench]]`/
`[[example]]` targets.

### Target triple

The target triple is detected from the host by running `rustc -vV` and parsing the
`host:` line. There is no cross-compilation support yet — the target is always the
host.

### Rust toolchain

The Rust channel is currently hardcoded to **stable**. `rust-toolchain.toml` is
not read. vxd asks vx-store to ensure the toolchain is available:

1. Fetch the channel manifest from `static.rust-lang.org/dist/channel-rust-stable.toml`
2. Download the `rustc` and `rust-std` tarballs (with SHA256 verification)
3. Extract each tarball, ingest every file into CAS as a blob
4. Build a `TreeManifest` per component (rustc, rust-std)
5. Build a `ToolchainManifest` listing both components
6. Store everything in CAS, keyed by a toolchain ID derived from the component SHA256s

The toolchain ID is a blake3 hash of the component SHA256 digests and target
triples, so the same toolchain content always produces the same ID.

### Zig toolchain

For C/C++ projects (`vx.styx` manifests), the Zig version is currently hardcoded
to **0.15.2**. The acquisition flow is similar: download from `ziglang.org`, split
into `zig-exe` and `zig-lib` components, ingest into CAS, build a
`ToolchainManifest`.

## Build Execution

### Action prefix lifecycle

For each compile action, vx-runner:

1. **Creates** a prefix: `build-{timestamp_hex}-{random_hex}`
2. **Populates toolchain**: fetches the ToolchainManifest from CAS, then each
   component's TreeManifest, and registers every file in the VFS tree. Files
   aren't actually fetched yet — they're lazy. The VFS just records the blob
   hash and size for each path.
3. **Populates source**: same pattern via source TreeManifest.
4. **Populates deps**: for each dependency, fetches its NodeManifest, finds the
   rlib output entry, registers `deps/lib{name}.rlib` in the VFS.
5. **Creates output dir**: `out/` is a writable directory in the VFS.
6. **Runs the compiler**: `rustc --sysroot <prefix>/toolchain/sysroot --crate-name
   ... -o <prefix>/out/...  <prefix>/src/main.rs`
7. **Flushes outputs**: reads `out/` write buffers, `put_blob` each to CAS.
8. **Stores manifest**: creates a NodeManifest with the output blob hashes.
9. **Tears down**: removes the prefix and all its VFS items.

### Toolchain layout in the VFS

The toolchain manifest contains named components. Each component's files are
placed under `toolchain/` with these rules:

| Component | VFS path |
|-----------|----------|
| `rustc` | `toolchain/{path}` (e.g. `toolchain/bin/rustc`) |
| `rust-std` | `toolchain/sysroot/{path}` (e.g. `toolchain/sysroot/lib/...`) |
| `zig-exe` | `toolchain/{path}` |
| `zig-lib` | `toolchain/{path}` |
| anything else | `toolchain/{component_name}/{path}` |

## Cache Key Computation

A cache key captures the full set of inputs to an action. If the cache key
matches, the cached output is guaranteed correct.

For a Rust compile action, the cache key includes:
- Crate name, type, edition
- Target triple, profile
- Toolchain manifest hash (captures entire toolchain identity)
- Source manifest hash (captures all source file contents)
- Dependency manifest hashes
- Compiler flags

This is computed by `vx_cache::rust_compile_cache_key()`.

## Test Architecture

Each integration test creates an isolated `TestEnv`:

- A temporary `VX_HOME` directory (simulates `~/.vx/`)
- A temporary project directory
- A freshly spawned vxd daemon (which spawns vx-store and vx-runner)

This means **every test gets its own daemon, its own CAS, and its own VFS
mount point**. The daemons die when the TestEnv drops.

HTTP responses (toolchain downloads) are cached in a shared directory
(`~/Library/Caches/vx-test-http-cache/`) across all test runs, so toolchains
are only downloaded once. But every test still has to:

1. Mount a new VFS (FSKit extension mount/unmount cycle)
2. Populate the CAS from HTTP cache
3. Register toolchain files in a fresh VFS
4. Read every toolchain file through the VFS with a cold macOS file cache

This is the main bottleneck: the macOS filesystem cache is per-mount-point.
A new mount means every file access is a cold read through FSKit -> roam RPC ->
CAS, even though the underlying blobs are identical.
