# CAS Storage Layer Abstraction

## Problem

Tests are slow because each test gets a fresh vx-store with an empty data directory.
Even with HTTP caching, toolchain extraction + blob writing happens per-test (~5-15s).

We tried a client-side approach (`StoreWithBase` wrapping `StoreClient` with Deref
shadowing), but the CAS API surface is too rich — `ensure_rust_toolchain`,
`ensure_zig_toolchain`, `ensure_registry_crate` are server-side operations that
check storage, download if missing, and store. Intercepting them on the client
means reconstructing server logic. Not viable.

## Solution: Storage trait inside vx-store

Give vx-store a proper storage abstraction. All filesystem operations currently
scattered across `StoreService` methods (`blob_path()`, `blobs_dir()`, reads,
writes) get consolidated behind a trait. Then layering becomes a storage concern,
not a client concern.

### The trait

```rust
// crates/vx-store/src/storage.rs

#[async_trait]
pub(crate) trait CasStorage: Send + Sync {
    // Blobs
    async fn get_blob(&self, hash: &Blake3Hash) -> io::Result<Option<Vec<u8>>>;
    async fn put_blob(&self, hash: &Blake3Hash, data: &[u8]) -> io::Result<()>;
    async fn has_blob(&self, hash: &Blake3Hash) -> io::Result<bool>;
    async fn stream_blob(&self, hash: &Blake3Hash) -> io::Result<Option<impl AsyncRead>>;

    // Manifests (JSON, stored separately from blobs)
    async fn get_manifest(&self, hash: &Blake3Hash) -> io::Result<Option<String>>;
    async fn put_manifest(&self, hash: &Blake3Hash, json: &str) -> io::Result<()>;

    // Toolchain specs (spec_key -> manifest_hash mapping)
    async fn get_toolchain_spec(&self, key: &Blake3Hash) -> io::Result<Option<Blake3Hash>>;
    async fn put_toolchain_spec(&self, key: &Blake3Hash, manifest_hash: &Blake3Hash) -> io::Result<()>;

    // Registry specs (spec_key -> manifest_hash mapping)
    async fn get_registry_spec(&self, key: &Blake3Hash) -> io::Result<Option<Blake3Hash>>;
    async fn put_registry_spec(&self, key: &Blake3Hash, manifest_hash: &Blake3Hash) -> io::Result<()>;

    // Cache (cache_key -> manifest_hash mapping)
    async fn get_cache(&self, key: &Blake3Hash) -> io::Result<Option<Blake3Hash>>;
    async fn put_cache(&self, key: &Blake3Hash, manifest_hash: &Blake3Hash) -> io::Result<()>;

    // Temp files (for atomic writes, tarball extraction, etc.)
    fn tmp_dir(&self) -> &Utf8Path;

    // Init (create directory structure)
    async fn init(&self) -> io::Result<()>;
}
```

### Implementations

**`FsStorage`** — what exists today, extracted into the trait:
```rust
pub(crate) struct FsStorage {
    root: Utf8PathBuf,
}
```

**`LayeredStorage`** — reads from primary, falls back to base on miss. Writes go
to primary only:
```rust
pub(crate) struct LayeredStorage {
    primary: FsStorage,
    base: FsStorage,
}

// Read: try primary, then base
// Write: primary only
// has_blob: primary || base
```

### Wiring

`StoreServiceInner` gets a `Box<dyn CasStorage>` instead of a `root: Utf8PathBuf`:

```rust
pub(crate) struct StoreServiceInner {
    storage: Box<dyn CasStorage>,
    vx_home: Utf8PathBuf,
    toolchain_manager: ToolchainManager,
    registry_manager: RegistryManager,
}
```

In `main()`, read `VX_CAS_BASE`:
- If set: `LayeredStorage { primary: FsStorage(root), base: FsStorage(base) }`
- If not set: `FsStorage(root)`

All existing methods (`blob_path()`, `blobs_dir()`, `manifests_dir()`, direct
`tokio::fs` calls in service.rs, toolchain.rs, registry.rs) get replaced with
calls through `self.storage`.

### Why this works for ensure_*

`ensure_rust_toolchain` currently:
1. Checks if toolchain spec is already resolved → reads storage
2. If not, downloads toolchain → writes to storage
3. Returns manifest hash

With `LayeredStorage`, step 1 finds the spec in the base layer → returns `Hit`
immediately. No download. The existing logic is unchanged — it just reads through
a different storage backend.

Same for `ensure_zig_toolchain` and `ensure_registry_crate`.

## Changes

### Undo client-side StoreWithBase

- Delete `crates/vx-store-proto/src/base.rs`
- Remove `pub mod base;` from `crates/vx-store-proto/src/lib.rs`
- Remove `tokio` and `tracing` deps from `crates/vx-store-proto/Cargo.toml`
  (keep `facet-json` — it was already there)
- Revert `crates/vxd/src/main.rs` — remove StoreWithBase import and wrapping
- Revert `crates/vxd/src/service.rs` — `Arc<StoreClient<...>>` not `Arc<StoreWithBase<...>>`
- Revert `crates/vxd/src/executor.rs` — same
- Revert `crates/vx-runner/src/main.rs` — remove StoreWithBase import and wrapping
- Revert `crates/vx-runner/src/vfs.rs` — `Arc<StoreClient<...>>` not `Arc<StoreWithBase<...>>`
- Revert `crates/vx-runner/src/registry.rs` — same

### Keep

- `tests/warmup-toolchains.sh` — still primes the shared CAS directory
- `.config/nextest.toml` — still runs warmup-toolchains setup script
- The `VX_CAS_BASE` env var concept — but now consumed by vx-store, not clients

### New files

- `crates/vx-store/src/storage.rs` — `CasStorage` trait, `FsStorage`, `LayeredStorage`

### Modified files in vx-store

- `crates/vx-store/src/types.rs` — `StoreServiceInner.root` → `StoreServiceInner.storage`
- `crates/vx-store/src/main.rs` — construct storage from env, replace all direct
  filesystem calls with `self.storage.*`
- `crates/vx-store/src/service.rs` — replace `self.blob_path()`, `self.blobs_dir()`,
  `self.manifests_dir()`, `self.cache_dir()`, direct `tokio::fs` calls
- `crates/vx-store/src/toolchain.rs` — replace direct filesystem calls
- `crates/vx-store/src/registry.rs` — replace direct filesystem calls

### Propagation

`VX_CAS_BASE` propagates automatically to child daemons — `spawn_service` in
vxd doesn't call `env_clear()`, so children inherit the parent environment.
Same for the test harness's `spawn_daemon`. No explicit forwarding needed.

## Scope

This is a refactor internal to vx-store. No changes to:
- The `Store` RPC trait (vx-store-proto)
- Any client code (vxd, vx-runner, vx)
- The RPC wire format

The storage trait is `pub(crate)` — purely an implementation detail of vx-store.
