# FSKit unstick diagnosis

## Grounding

This note is intentionally first. No code was changed before writing it.

Inputs read:

- `docs/design/fission-fskit-stuck-audit.md`
- `docs/design/vix-substrate-vfs-sandbox.md`
- `docs/design/vix-substrate-vfs-sandbox-addendum.md`
- `crates/vx-fskit/src/macos/*`
- `apps/mac/xcode/VixenFS/*`
- `apps/mac/xcode/Vixen/Backend/VfsBackend.swift`
- `crates/vx-vfs/src/macos/mod.rs`
- `crates/vx-vfsd/src/{vfs,protocol,service}.rs`
- `crates/vx-exec/src/lib.rs`
- `apps/mac/{README.md,FSKIT-REPAIR.md,Justfile}`
- git history for `fission`, `fs-kitty`, `VixenFS`, and `vx-fskit`
- current SDK headers under `MacOSX27.0.sdk/System/Library/Frameworks/FSKit.framework`

Host facts:

```text
Darwin: 27.0.0 arm64
macOS: 27.0 build 26A5378j
Xcode: 27.0 build 27A5218g
SDK:   MacOSX27.0.sdk
```

## What actually stuck

The audit's corrected threat model is right: the dangerous wedge was not "the
app touched a bad mount." The dangerous wedge was "our FSKit extension received
a VFS operation and did not reply to FSKit." FSKit dispatches filesystem
operations as callbacks with reply/completion blocks. The kernel-side syscall
that caused the callback remains blocked until the extension replies.

The audit did not find a forgotten error arm. It found two classes of reply loss:

1. The old Rust FSKit handlers synchronously called unbounded backend RPCs through
   `block_on_vfs`. If the backend stayed alive but silent, `read`, `lookup`,
   `getAttributes`, `enumerate`, mutations, and writes could wait forever before
   calling the FSKit reply block.
2. Panics and poisoned mutexes inside an Objective-C FSKit callback could skip the
   reply and unwind across the ObjC boundary.

That explains the fear. A hung user-space extension is not a normal failed
process; it can pin the originating process inside the syscall, make `SIGKILL`
defer until the syscall returns, and make ordinary recovery tools hang if they
touch the mounted tree.

## What fission was

I did not find a live `fission` crate or target in the current tree. The name in
the audit maps to the current `VixenFS` / `vx-fskit` lineage plus the older
`fs-kitty` history:

- `a321962d`: first `vx-fskit` crate, health/reset tooling for fs-kitty mounts.
- `2b16308`: deleted the earlier cleanup/reset machinery after fs-kitty began
  auto-unmounting when the RPC peer exited.
- `112578e`: introduced the Swift `VixenFS` extension files.
- `973b986`: switched from generic URL resources to block-device resources,
  because the then-current macOS 15 path only worked as a block FS for this use.
- `8587f3`: fixed the probe/load split where FSKit created both a probe instance
  and a load instance, and the probe instance incorrectly bound `vixenfs.sock`.
- `706f1a5`: introduced provider prefixes: one virtual Vox connection claims one
  prefix, mounted under `/Volumes/Vixen/vfs/<prefix>/`.
- `77cdcce`: current hardening pass after the stuck audit.

Per the deliberate-holes rule, I used this history for diagnosis only. There is
no deleted code worth resurrecting wholesale.

## Current API reality

The current SDK materially improves the API surface compared with early FSKit:

- `FSKIT_API_AVAILABILITY_V1` is macOS 15.4.
- `FSKIT_API_AVAILABILITY_V2` is macOS 26.0.
- `FSKIT_API_AVAILABILITY_V3` is macOS 27.0.
- `FSGenericURLResource` and `FSPathURLResource` are V2, so they exist in this
  Darwin 27 SDK. That means the original "block-device only" pressure from the
  macOS 15 lane is no longer an SDK limitation in the same way.
- The old `FSVolumeReadWriteOperations` protocol is deprecated in macOS 27 in
  favor of `FSVolumeReadWriteHandler`, which returns result objects with updated
  attributes/free-space. The current Rust bridge still implements the old
  operation protocol, which remains available but is now the legacy lane.
- The FSKit module entitlement is present in the app extension:
  `com.apple.developer.fskit.fsmodule = true`.

Amos's prior holds: FSKit is not intrinsically impossible. The stuck point was
not a missing FSKit capability. It was mostly lifecycle/reply discipline in our
extension, plus early resource-type and registration/enablement sharp edges.
Those sharp edges are real, but they are bounded and diagnosable.

## What is already fixed in this checkout

The current `vx-fskit` code has already implemented the main audit fixes:

- `ReplyGuard` copies the FSKit reply block and calls an `EIO` reply from `Drop`
  if a path forgets to reply.
- Backend RPC awaits are wrapped by `await_vfs`, with per-operation deadlines
  and cancellation.
- FSKit callback handlers copy retainable ObjC objects and run backend work via
  `runtime().spawn_blocking`, so the FSKit callback thread is no longer held for
  the whole backend RPC.
- `RootPeerState::cancel_inflight` broadcasts cancellation on unmount and
  provider close.
- Shared mutex acquisition now uses `lock_unpoisoned` instead of propagating
  poison into future callbacks.
- `status_json_bytes` serializes outside the root-peer lock and falls back to a
  small error payload instead of panicking.

There are still panic surfaces (`runtime()` creation, `root_peer_state()` before
initialization, Swift bootstrap `fatalError`s), but the audited hot operation
paths no longer look like the original wedge mechanism.

## What the VFS must provide

The substrate contract is already shaped for the desired FSKit mount:

- One real FSKit mount at `/Volumes/Vixen/vfs`.
- Virtual provider prefixes under it; the daemon claims
  `vx_vfs_types::MACOS_VFS_DAEMON_PROVIDER_PREFIX` over a Vox provider lane.
- Read observation lives in `vx-vfsd`, not in `vx-exec`: `CasVfs` records
  `File`, `Directory`, and `LookupMiss`, and `tracked_observations(prefix, roots)`
  returns sorted observations relative to the provider root.
- Progressive output trees need close/release events from the VFS path. The
  current copy-staging executor has no daemon close callback and explicitly
  marks that gap in `tree_events_from_subfiles`.

## Current staging tax

`crates/vx-exec` is the thing to replace:

- It creates a temporary root.
- It hard-links or copies declared inputs into that root.
- It runs `sandbox-exec` with the temp root as the read root.
- It synthesizes read observations from the staging process, not from native VFS
  calls made by the guest process.
- It emits progressive tree events only after process exit because it has no VFS
  close path.

That path proves protocol shape, but it pays exactly the copy-staging tax the
FSKit mount should remove.

## Minimal viable mount path

The smallest viable mount is mostly present already:

1. `apps/mac/xcode/VixenFS/VixenFS.swift` is the Swift `UnaryFileSystemExtension`
   entrypoint.
2. `RustFSKitBootstrap.swift` creates the Rust-backed `FSUnaryFileSystem`.
3. `Info.plist` declares `FSFileSystemType = vixenfs`, block-resource support,
   `FSMediaTypes = Partitionless`, and no generic/path/server URL support.
4. `crates/vx-management/src/mount.rs` handles the mock backing image and
   `/sbin/mount -F -t vixenfs`.
5. `crates/vx-vfs/src/macos/mod.rs` mounts/reuses VixenFS, reads
   `/.vixenfs/status.json`, connects to `vixenfs.sock`, and claims the daemon
   provider prefix.
6. `apps/mac/xcode/Vixen/Backend/VfsBackend.swift` is already a read-only
   in-memory proof provider under prefix `app`; the daemon path can claim its
   own prefix without colliding.

So the remaining proof is not "can we write an FSKit filesystem from scratch."
It is "does this installed/signed/enabled app-extension path build, mount, serve
through the daemon provider, record observations, unmount, and repeat on this
machine."

## Current likely walls

Manual or machine-local gates:

- FSKit extension discovery/enablement is a System Settings gate. The app must
  be installed/opened so macOS discovers the bundled appex, and the user may need
  to enable VixenFS under File System Extensions.
- The active PlugInKit record matters. `pluginkit -mAvvv -i eu.bearcove.vixenfs`
  is the source of truth for which bundle macOS will launch.
- The mountpoint under `/Volumes` may need one privileged creation/chown step.
- For the current block-resource path, the backing image must be attached by the
  user so the resulting `/dev/diskN` node is user-owned and the user FSKit
  extension can open it.
- `apps/mac/FSKIT-REPAIR.md` contains both a newer "no daemon, user-side mount"
  architecture and older retained repair-path text. Treat the breakthrough and
  final-architecture sections as newer than the later "Current Product Repair
  Path" section unless live code proves otherwise.

Implementation/code walls:

- The current app backend is read-only and in-memory. That is enough for a
  native mount smoke, not enough for daemon CAS proof.
- `vx-vfsd` observations exist, but `vx-exec` does not yet call the daemon VFS
  path or drain `tracked_observations`.
- The Rust bridge uses deprecated-in-macOS-27 read/write protocols. That is not
  a blocker today, but the next FSKit cleanup should consider moving to the V3
  handler protocols after the mount proof is green.

## Next proof sequence

1. Build Rust workspace and mac app/extension through the existing `apps/mac`
   Justfile path.
2. Install/open `Vixen.app`.
3. Verify PlugInKit points at `/Applications/Vixen.app/.../VixenFS.appex`.
4. If disabled, stop and ask Amos to enable VixenFS in System Settings.
5. Mount through the user-side VixenFS primitive.
6. Read:
   - `/Volumes/Vixen/vfs/.vixenfs/status.json`
   - `/Volumes/Vixen/vfs/app/hello.txt` for the app proof provider
   - the daemon provider path once `vx-vfsd` is running and has claimed its prefix
7. Call `tracked_observations` after a daemon-provider read and verify a `File`
   observation with the expected content hash.
8. Unmount with `umount /Volumes/Vixen/vfs`, then repeat mount/read/unmount.

Pass/fail interpretation:

- If mount fails before the extension launches, inspect registration,
  entitlement, enablement, mountpoint ownership, and device ownership.
- If mount succeeds but `status.json` hangs or is invalid, the extension path is
  still the problem.
- If `status.json` works but `/app/*` does not, debug provider-lane connection
  and prefix routing.
- If app prefix works but daemon prefix does not, the wall is in `vx-vfsd`
  mount/provider wiring, not FSKit itself.
- If daemon prefix reads but observations are empty, the wall is observation
  scope/root filtering or `tracked_observations` integration.
