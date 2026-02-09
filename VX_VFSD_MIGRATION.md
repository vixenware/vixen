# vx-vfsd Migration Plan

## Overview

Issue #106 aims to separate the FUSE VFS into a dedicated daemon (`vx-vfsd`) to isolate
it from the process that spawns children (`vx-runner`).

## Current Status

### ✅ Completed

**vx-vfsd daemon**:
- RPC protocol defined (`Vfsd` trait: mkdir, mount, flush, rm_rf)
- All 4 RPC methods fully implemented
- Daemon lifecycle: connects to vx-store, mounts FUSE, starts RPC server
- Sentinel mode: dies with parent, cleans up mount
- Uses figue for CLI parsing (consistent with other crates)

**Integration prep**:
- `VfsdClient` wrapper created in vx-runner (not yet used)
- vx-vfsd added as dependency to vx-runner

### 🚧 TODO: Complete Migration

**1. Update vx-runner to use vx-vfsd**

Current architecture:
```rust
// vx-runner owns CasVfs directly
let vfs = Arc::new(CasVfs::new(cas));

// mounts FUSE itself
vx_vfs::mount(VfsConfig { mount_path }, SharedVfs(vfs.clone())).await?;

// calls VFS methods directly
vfs.create_prefix(prefix_id).await;
vfs.add_file_to_prefix(prefix, path, hash, size, exec).await;
vfs.flush_prefix_to_cas(prefix).await;
```

Target architecture:
```rust
// vx-runner connects to vx-vfsd via RPC
let vfsd = VfsdClient::new(connect_to_vfsd(&args.vfsd_endpoint).await?);

// NO FUSE mounting (vx-vfsd handles this)

// calls RPC methods
vfsd.create_prefix(prefix_id).await;  // → mkdir("/prefix/")
vfsd.mount_tree(prefix, "src", hash).await;  // → mount("/prefix/src", hash)
vfsd.flush_prefix_manifest(prefix, "out").await;  // → flush("/prefix/out")
```

**Changes needed**:
- Remove `vfs` module from vx-runner (move to vx-vfsd or delete)
- Remove `SharedVfs` wrapper
- Remove FUSE mounting code (lines ~253-349 in main.rs)
- Replace `Arc<CasVfs>` with `VfsdClient` in `RunnerService`
- Update all `self.vfs.*` calls to use RPC methods

**2. Change tree population strategy**

Current: vx-runner iterates tree manifest entries and calls `add_file_to_prefix` for each:
```rust
for entry in manifest.entries {
    vfs.add_file_to_prefix(prefix, &entry.path, entry.blob, ...).await;
}
```

Target: vx-runner uploads tree manifest to CAS, then mounts it once:
```rust
let tree_hash = cas.put_tree_manifest(manifest).await?;
vfsd.mount_tree(prefix, "toolchain", tree_hash).await?;
```

**Files to update**:
- `src/main.rs`: populate_toolchain_in_prefix, populate_source_tree, etc.
- `src/service_vfs.rs`: All methods that iterate manifest entries

**3. Update vxd to spawn vx-vfsd**

vxd needs to:
1. Spawn vx-vfsd first (with `--mount`, `--listen`, `--store` args)
2. Parse vx-vfsd's stdout to get actual listen address
3. Spawn vx-runner with `--vfsd-endpoint` pointing to vx-vfsd
4. Tie lifecycles (when one dies, kill the other)

**4. Update vx-runner CLI**

Add arguments:
```rust
struct Args {
    /// vx-vfsd RPC endpoint
    vfsd_endpoint: Endpoint,

    // Remove: VFS mount path (now owned by vx-vfsd)
}
```

**5. Testing**

- Unit tests: VfsdClient methods
- Integration tests: vx-runner + vx-vfsd via RPC
- End-to-end: Full build through vxd → vx-vfsd + vx-runner

## Benefits

After migration:
- **Process isolation**: FUSE server doesn't spawn children
- **Cleaner separation**: VFS concerns isolated in vx-vfsd
- **Easier debugging**: VFS issues don't crash build execution
- **Better resource management**: Can restart VFS without restarting builds

## Migration Risk

**High-risk changes**:
- FUSE mounting logic (kernel interaction)
- Tree manifest population (many files)
- Prefix lifecycle management

**Mitigation**:
- Keep vx-runner's current VFS code in separate branch
- Add feature flag for "use-vfsd-rpc" vs "embedded-vfs"
- Gradual rollout: test with small builds first

## Estimated Effort

- vx-runner refactoring: ~4-6 hours
- vxd integration: ~2-3 hours
- Testing & fixes: ~3-4 hours
- **Total**: ~10-13 hours of focused work
