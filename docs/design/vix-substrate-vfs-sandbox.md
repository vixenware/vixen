# vix substrate: VFS + sandbox as the enforcing leaf

Purpose: map where vixen's two primitives — VFS (presents a process a world
made of exactly its declared mounts) and sandbox (makes reading beyond that
world impossible) — actually live in this repo, and design how they compose
into the exec substrate leaf the open-repo re-carve (`facet` branch
`vix-exec-seam-audit`, `vix/docs/exec-seam-audit.md`) is shrinking toward:
`materialize(tree) + launch(command) + report(events) + join → Outcome`. Open
core's real-process lane is ruled trusts-host (staging dir, no enforcement).
The product shape: vixen makes the mount-ceiling law *enforced*, not trusted.

A hard requirement carried into this doc, not a "nice to have": the
substrate leaf must produce **observed** read-sets, not merely declared ones —
same as the Rodin R2 pattern (`docs/*rodin*r2*`), observed beats declared for
tier-2 verification. Where the VFS can't yet report reads, that is a gap with
concrete work attached, not grounds to fall back to declared sets in the
design. Windows is in scope as a first-class third platform via ProjFS, whose
callback/notification model is native lazy-materialization-with-observation —
arguably the *easiest* platform to get this right on.

## 1. The VFS primitive: where it lives, what backs it

The VFS is split across four crates, and the split matters: **mount-table
declaration and CAS-backed serving live in the daemon** (`vx-vfsd`); the
per-platform crates (`vx-vfs`, `vx-fskit`) are thin bridges that forward
platform read/write calls into the daemon over Vox RPC. `vx-vfs-types` is
*not* the mount-declaration type — despite the name, it's macOS
diagnostics/health-check plumbing (`VfsIssueCode`, `VfsDiagnostic`,
`crates/vx-vfs-types/src/lib.rs:18,42,50`). The real mount/CAS types are in
`vx-vfsd` and `vx-services`.

**Mount declaration.** `ExecMount { path: String, tree: Blake3Hash }`
(`crates/vx-services/src/runner.rs:36-42`) is the declared-tree shape: a
content-addressed tree (by Blake3 hash) mounted at a path prefix. It feeds
`ExecRequest { program, args, env, mounts: Vec<ExecMount>, outputs, create_dirs }`
(`runner.rs:65-81`). `vx-vfsd`'s RPC surface mirrors this at the daemon level:
`mount(path, tree_hash: Blake3Hash)` and `flush(path) -> Blake3Hash`
(`crates/vx-vfsd/src/protocol.rs:63,75`).

**Backing store.** Content-addressed, served by `CasVfs` (`vx-vfsd/src/vfs.rs`),
which sits in front of `vx-store`'s CAS (chunked blob storage,
`crates/vx-store/src/{storage,types}.rs`) with L1/L2 chunk caches
(`vfs.rs:365-568`).

**Lazy/on-demand materialization: yes, uniformly, on all three platforms
today.** Every platform driver's `read()` implementation forwards straight
into the same `Vfs::read` trait method → `CasVfs::read`, i.e. bytes are
fetched from CAS only when a guest process actually issues a read — there is
no eager pre-staging to a directory:

- **macOS**: `vx-vfs/src/macos/mod.rs` doesn't implement a filesystem itself;
  it bridges Vox RPC to the FSKit extension (`apps/mac`), which is the actual
  filesystem. Extension-side, `read_item(item, offset, length, buffer)`
  (`crates/vx-fskit/src/macos/volume/helpers.rs:592-651`) calls
  `client.read(backend_item_id, offset, length)` on every read — no local
  caching in the extension process, every read round-trips to the daemon.
  Wired into the real `FSVolumeReadWriteOperations` trait method at
  `crates/vx-fskit/src/macos/volume.rs:788-816`.
- **Linux**: `FuseBridge<T>: fuse3::raw::Filesystem` (`vx-vfs/src/linux.rs`),
  `read()` (120-136) calls `self.vfs.read(inode, offset, size)` per FUSE
  request. `crates/fuse3` is a vendored FUSE binding; its `Filesystem::read`
  trait method (`raw/filesystem.rs:113-122`) defaults to `ENOSYS` and is
  overridden here.
- **Windows**: `ProjFsBridge<T>: ProjectedFileSystemSource`
  (`vx-vfs/src/windows.rs`), `stream_file_content(path, byte_offset, length)`
  (284-335) resolves the path then calls `self.vfs.read(file_id, byte_offset,
  length)` (313-315) — invoked from ProjFS's native `GetFileData` callback.
  `crates/windows-projfs` is the raw ProjFS FFI wrapper: `PRJ_CALLBACKS`
  wires `Start/GetDirectoryEnumeration`, `GetPlaceholderInfo`, `GetFileData`,
  and `Notification` callbacks (`fs.rs:174-184`); `get_file_data_callback`
  (`fs.rs:679-742`) is the real per-call read path with offset+length
  available at the FFI boundary, before it ever reaches vixen's own code.

So the platform story is a genuine triple — macOS/FSKit, Linux/FUSE,
Windows/ProjFS — and the abstraction (`Vfs` trait, one `read()` method called
uniformly from three different platform bridges) is already shaped so a
demand-driven backing tree slots in directly: a mount is exactly "bytes that
materialize on first read," which is the same shape as a pending tree in the
open-repo `materialize(tree)` leaf.

**Operational lore: FSKit is the most fragile leg of the triple, and the code
already treats it that way.** This is load-bearing context for the design in
§4/§5, not incidental trivia. Wielded carelessly, FSKit can wedge the entire
OS — file-system operations can send processes into zombie states that are
close to undiagnosable; this subsystem has a reputation, in Amos's words, as
"a trash fire." The existing macOS code is already defensively built around
this:

- **Nested mountpoint, deliberately.** The volume mounts at
  `/Volumes/Vixen/vfs`, not at `/Volumes/Vixen` directly
  (`crates/vx-vfs-types/src/lib.rs:12-13`,
  `MACOS_VFS_MOUNT_PARENT`/`MACOS_VFS_MOUNT_POINT`). The rationale is in the
  doc comment at `vx-vfs-types/src/lib.rs:10-11` and restated at
  `vx-management/src/mount.rs:8-9`: `/Volumes/Vixen` is a plain APFS
  directory, and the FSKit mount lives *below* it precisely so that
  enumerating `/Volumes` — which macOS and every file dialog does constantly
  — never has to touch a wedged FSKit root. This is a hard-won invariant, not
  an accident of layout; do not flatten it.
- **Unmount itself is treated as an operation that can wedge, so it's never
  run inline.** `recover_existing_state()`/`unmount()`
  (`vx-management/src/mount.rs:137-146`) run a *sacrificial* child process
  (`try_sacrificial_unmount`, 212-249) with a 2-second deadline
  (`SACRIFICIAL_UNMOUNT_DEADLINE`, line 39) specifically so that "once FSKit
  is wedged, the unmount caller can become part of the wedge" — and that
  caller must never be the management daemon itself (comment, 139-141). On
  timeout the child is killed and control falls through to escalation, never
  blocks forever.
- **Escalation path**: sacrificial unmount timeout/failure → `kill_fskitd()`
  (`mount.rs:186`, implemented in `crates/vx-management/src/killswitch.rs`) →
  `terminate_fskitd()` sends `SIGTERM` to `/usr/libexec/fskitd` directly (not
  via launchd, deliberately — comment, `killswitch.rs:5`), waits a bounded 3
  seconds, escalates to `SIGKILL` if it survives
  (`killswitch.rs:17-44`). Every step in this chain is timeout-bounded; none
  of it waits indefinitely on a subsystem that might never respond.
- **A failed unmount leaves fskitd holding a zombie registration that
  poisons every future mount at that path until fskitd restarts**
  (`mount.rs:107-109` comment) — this is why `establish()`/`mount_volume()`
  failure paths always attempt `force_unmount_quietly` before detaching the
  backing device (`mount.rs:105-115`), rather than just giving up.
- **Existing diagnostics for this failure mode**: `vx-management/src/probe.rs`
  is a timeout-bounded health probe — `COMMAND_TIMEOUT`/`CONNECT_TIMEOUT`/
  `WAIT_FOR_SERVICE` (probe.rs:19-21, all 2-3s) bound both the Vox RPC connect
  and a round-trip `cat` of a probe file through the mount, so a wedged mount
  fails the probe instead of hanging the caller. At the type level,
  `VfsIssueCode::HealthCheckFailed` / `MountFailed`
  (`vx-vfs-types/src/lib.rs:23-24`) and `VfsHealth { extension_installed,
  extension_enabled }` (`lib.rs:57-62`) are the diagnostic vocabulary
  surfaced to the app UI — but note this is coarse (mounted/not-mounted,
  probe-passed/failed), not the same thing as the substrate leaf's per-run
  `Outcome`; there is no existing "this specific run's FS access wedged"
  event distinct from "the volume is unhealthy" today.

Implication carried into §4/§5: on macOS specifically, the integration design
should push as much logic as possible to the daemon side of the FSKit
boundary (`vx-vfsd`/`CasVfs`, which is ordinary Rust, not the FSKit
extension), keep the FSKit-facing surface (`vx-fskit`) as thin as it already
is, and give the exec substrate's launch/join path the same
timeout-then-kill discipline the mount layer already uses for unmount —
never let a run's join wait unbounded on a subsystem that has no reliable
"give up" signal.

## 2. Read observation: already built at the daemon, not yet load-bearing

This is not a speculative feature to assess for feasibility — it exists.
`vx-vfsd`'s protocol already has an RPC built for exactly this:
`tracked_observations(prefix_id, roots) -> Vec<VfsTrackedObservation>`
(`crates/vx-vfsd/src/protocol.rs:114-118`), returning:

```rust
pub enum VfsTrackedObservation {
    File { path: String, content_hash: Blake3Hash, blob_node: Option<NodeHash>, size: u64 },
    Directory { path: String, directory_node: NodeHash },
    LookupMiss { path: String, parent_path: String, directory_node: NodeHash },
}
```

`CasVfs` (`vx-vfsd/src/vfs.rs:381-412`) maintains three observation maps
(`observed_identities`, `observed_directories`, `observed_lookup_misses`),
populated by `observe_identity`/`observe_directory`/`observe_lookup_miss`
(741-795), called from the `get_attributes`, `read_dir`, and `read` RPC
handlers (1415, 1525, 1607) — i.e. **every read, on every platform, already
gets recorded**, because macOS/FSKit, Linux/FUSE, and Windows/ProjFS all
funnel through the same `Vfs::read` → `CasVfs::read` path. `tracked_observations`
(2455-2533) resolves the recorded `ItemId`s back to paths filtered by root and
returns them sorted.

The mechanism was built for a different original purpose — the comment at
`vfs.rs:381` calls it a "diagnostic tracker for CAS-backed file identity
stability" (detecting mid-flight hash drift) — but structurally it already
answers "what did this process actually touch, and with what content hash,"
which is precisely an observed read-set.

**Gaps, concretely:**

1. **Granularity is path/identity-level, not byte-range.** `observe_identity`
   records `{cas_hash, blob_node, size}` per `ItemId` touched, not per
   `(offset, length)` read call. For a mount-ceiling verification use case
   (did the process stay inside its declared tree?) this is sufficient. For a
   finer tier-2 cache key (did it read exactly these byte ranges?) it isn't
   yet — `read()`'s `(offset, length)` parameters are available at the call
   site (`vfs.rs:1559-1620`) but not threaded into the observation record.
2. **Nothing outside `vx-vfsd` calls it.** `grep -rl tracked_observations`
   across the repo returns only `vfs.rs`, `protocol.rs`, `service.rs` (the
   last just forwards the RPC, `service.rs:225-231`). `ExecResult`
   (`runner.rs:84-98`) — the result type a real exec would return — has an
   `outputs: Vec<ExecOutput>` write-set field but **no observed-read-set
   field**. The orchestrator/runner traits in `vx-services` are themselves
   still protocol shells (`runner.rs:5-7`: *"placeholders, not a stable
   execution model"*) with no subprocess-spawning implementation at all yet
   — so there is no current call site that both launches a process and
   drains `tracked_observations` afterward. This is wiring work, not new
   mechanism.
3. **Windows discards a second, platform-native observation signal.** ProjFS
   itself emits `PRJ_NOTIFY_FILE_OPENED` and
   `PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_MODIFIED`/`..._NO_MODIFICATION`
   notifications, and the wrapper crate already receives and parses them
   (`windows-projfs/src/fs.rs:188-201` sets the notification mask;
   `744-820` `notification_callback` maps `PRJ_NOTIFICATION_FILE_OPENED →
   Notification::FileOpened`, forwarded to `source.handle_notification`).
   But `vx-vfs/src/windows.rs`'s `handle_notification` (337-371) drops
   `FileOpened` straight into an ignored `other => trace!(...)` arm
   (365-368). This is the ProjFS-native equivalent of the crown-move read
   observation the daemon already does via `CasVfs::read` — it's a second,
   independent confirmation channel (platform-attested rather than
   application-attested), sitting unused. Wiring it up gives Windows a
   two-source read attestation (CAS-level + OS-level) essentially for free.

Verdict: read-observation is not a research question, it's an integration
task. The daemon-side mechanism exists uniformly across all three platforms
today; the work is (a) optionally add byte-range granularity, (b) wire
`tracked_observations` into the exec result path once `vx-services` grows a
real process-launching implementation, (c) on Windows specifically, stop
discarding `PRJ_NOTIFY_FILE_OPENED` and treat it as a second observation
source.

## 3. The sandbox primitive: where it lives, what it enforces

**Linux has a real, working enforcement mechanism.** `crates/landlock-exec`
combines Landlock (LSM file-access ACLs) and seccomp-bpf (syscall filtering),
entirely `cfg(target_os = "linux")`-gated. Entry point: `sandbox::run()`
(`crates/landlock-exec/src/sandbox.rs`), invoked from `main()` after
`secmem_proc::harden_process()`. CLI contract:

```
landlock-exec --read-root <PATH> --write-root <PATH> --exec-root <PATH> -- <program> [args...]
```

Policy expressiveness is now the same declared-root generator shape as
`seatbelt-exec`: read roots get read-only `PathBeneath` rules, write roots
get read/write rules, and exec roots get read/execute rules. File/device
roots are narrowed to file-compatible access rights before rule emission.
The ruleset targets Landlock ABI V4 with `CompatLevel::HardRequirement`, so
kernels without V4 filesystem and TCP rights fail during setup instead of
silently degrading. TCP bind/connect are denied by declaring
`AccessNet::{BindTcp, ConnectTcp}` and adding no `NetPort` allowances; the
seccomp-bpf layer remains as defense in depth for server-side
`accept`/`accept4`/`listen`. Landlock still does not confine arbitrary
non-TCP networking.

**macOS now has a seatbelt mechanism with the same declared-root shape.**
`crates/seatbelt-exec` generates a deny-default profile from declared read
roots, write roots, and toolchain/exec roots, with its audit trail in
`docs/design/seatbelt-strict-profile.md`. That gives macOS and Linux matching
policy-compiler inputs, but the substrate launcher still needs to feed its
mount table into either wrapper.

**Windows has no explicit sandbox mechanism found either** — no AppContainer,
job-object, or restricted-token code turned up in this pass. This is a gap to
track alongside macOS, though ProjFS's virtualization root already confines
*visible* files to the mount by construction; it says nothing about syscall
or network confinement of the child process.

**Composition today: none.** `vx-services`'s `orchestrator.rs`/`runner.rs`/
`daemon.rs`/`test_runner.rs` are RPC protocol shells with no
`std::process::Command` invocation anywhere — no code today launches a
subprocess under sandbox enforcement, and no code derives a sandbox
allow-list from a declared mount table. `ExecMount` (§1) is the one existing
data shape that *would* need to feed a sandbox policy, but nothing does that
yet — `runner.rs:60-64`'s own comment defers it: "Sandbox confinement...
and caching join next; this is the mount → run → flush spine." Mounting
(`vx-management/src/mount.rs`, block-device/volume plumbing via `hdiutil`) and
process execution are fully separate concerns in the current code; neither
`vx-management/src/extension.rs` (system-extension registration) nor
`probe.rs` (health-check, spawns unsandboxed `/bin/cat` to round-trip a
probe) touch sandboxing either.

## 4. Integration design: mounts compile to sandbox policy, VFS reads become the verification oracle

Target substrate leaf (from the open-repo re-carve):
`materialize(tree) → launch(command) → report(events) → join → Outcome`,
with read-sets as a first-class part of `Outcome`.

**materialize(tree).** Already the natural job of `vx-vfsd`'s `mount(path,
tree_hash)` (`protocol.rs:63`): register a CAS tree hash at a path prefix.
Because reads are lazy (§1), "materialize" doesn't mean staging bytes to
disk — it means registering the mount so that *subsequent* reads resolve
against that tree. This is already true on all three platforms, so
`materialize(tree)` is closer to done than the exec-seam-audit's
trusts-host staging-dir lane: vixen's version is enforced-by-construction
(the process literally cannot see any tree that wasn't mounted) rather than
enforced-by-convention (a directory the process is merely expected not to
escape).

**launch(command), policy compiled from declared mounts.** The declared
mount set (`ExecMount { path, tree }`, the same struct as §1/§2) is exactly
the input a sandbox policy compiler needs: for each declared mount, emit a
read-only Landlock `PathBeneath` rule (Linux) rooted at that mount's path; on
macOS, the equivalent would be a generated seatbelt profile (still to build,
see gaps) scoping file reads to the same paths; output/write directories
compile to read-write rules the same way `landlock-exec` treats declared
write roots today. The ceiling law becomes literal: *the set of paths the
sandbox permits is derived from, and can never exceed, the set of paths the
VFS declared* — one generator, not two policies that could drift apart. This
is the "role declarations become the sandbox policy" move Amos named: the
Linux wrapper now compiles from arbitrary read/write/exec root lists, one
Landlock rule per declared root instead of one blanket read-only root.

**report(events), join → Outcome, with *observed* not declared read-sets.**
This is where §2's existing-but-unwired mechanism becomes load-bearing
rather than diagnostic. On process exit, the orchestrator drains
`tracked_observations(prefix_id, roots)` from `vx-vfsd` and attaches it to
the `Outcome`/`ExecResult` alongside the write-set that already exists
there. This is the R2 pattern verbatim: tier-2 verification compares the
*observed* touched-path set (what `CasVfs` actually recorded reads for)
against the *declared* mount set, and a narrower observed set is a pruning
opportunity (fewer paths needed next time → tighter cache key), while a
*wider* observed set than declared should be impossible by construction
(the sandbox enforces the ceiling) — if it ever isn't, that's a sandbox
policy-compilation bug to catch, not a runtime hazard, because Landlock
already denies the read before it happens. On Windows, this join gets a
second independent witness for free once `PRJ_NOTIFY_FILE_OPENED` is no
longer discarded (§2 gap 3): cross-checking `CasVfs`'s application-level
observation against ProjFS's OS-level open notification catches any future
implementation bug in the CAS read-observation hook itself.

**join → Outcome must survive a zombie FSKit, by design, not by luck.** The
orchestrator owns the run lifecycle in the target substrate shape, which
means it — not the FS layer — decides when a run is over. Given the lore in
§1 (FSKit can wedge the OS; the failure mode is close to undiagnosable), the
join path for a macOS-backed run needs exactly the discipline
`vx-management/src/mount.rs` already applies to unmount: a bounded wait on
the child/FS interaction, and on timeout, kill the child, mark the run
**poisoned** (not merely failed — a poisoned run's cache entry must not be
trusted as a clean miss, since a wedged FSKit read may have returned garbage
or never returned at all), and let the orchestrator abandon that run without
blocking the rest of the demand graph. This mirrors the existing
sacrificial-unmount pattern almost exactly: the *orchestrator* is the
non-sacrificial party (analogous to the management daemon that must never
become part of the wedge), the *run's join wait* is the sacrificial
operation with a deadline, and escalation on timeout is "kill the child
process, and if the mount itself is what's actually wedged, fall back to the
existing `kill_fskitd()`/`unmount()` recovery path before any further run is
scheduled against that mount." Concretely this needs: (a) a bounded join
timeout distinct from "the child is still legitimately running" (FSKit reads
can be slow-but-alive vs. actually wedged — the existing 2s/3s constants in
`mount.rs`/`killswitch.rs` are a starting calibration, not necessarily right
for a real workload's I/O pattern), (b) a `Poisoned` outcome variant
alongside success/failure so the orchestrator can distinguish "this run's
result is untrustworthy because the substrate wedged" from an ordinary
nonzero exit, and (c) the orchestrator must not retry the poisoned run
against the same still-wedged mount — it should trigger mount recovery (or
route to a fresh mount) before rescheduling.

**Why Windows/ProjFS is the easiest platform for this, not an afterthought.**
ProjFS's callback model *is* the demand-driven-tree shape already: a
placeholder is a pending node, `GetFileData` is the demand trigger, and
`PRJ_NOTIFY_FILE_OPENED`/`FILE_HANDLE_CLOSED_*` are observation events built
into the platform primitive, not something vixen has to invent. The
`windows-projfs` wrapper crate already receives and parses all of this
(`fs.rs:174-184, 188-201, 744-820`) — the only Windows-side gap is that
`vx-vfs/src/windows.rs` throws the notification away. Compare macOS, where
lazy materialization requires an entire out-of-process FSKit extension and a
Vox RPC round-trip per read, or Linux, where FUSE gives on-demand reads but
no native open/read notification channel at all (observation there rests
solely on the `CasVfs::read` application-level hook). Windows gets
demand-driven materialization *and* a second observation channel from one
platform API.

## 5. Gaps list (what exists vs what the substrate leaf needs)

| Component | macOS | Linux | Windows |
| --- | --- | --- | --- |
| Lazy materialization | Done (FSKit → Vox RPC → `CasVfs::read`) | Done (FUSE → `CasVfs::read`) | Done (ProjFS `GetFileData` → `CasVfs::read`) |
| Read observation (CAS-level, path/identity granularity) | Done, unwired (`tracked_observations` exists, no caller) | Done, unwired (same) | Done, unwired (same) |
| Read observation (OS-native, second channel) | N/A (no OS-native read-notify surface found) | N/A (FUSE has no equivalent notify channel) | Received, parsed, **discarded** (`windows.rs:365-368`) — smallest fix in this table |
| Byte-range observation granularity | Not implemented (path/identity only) | Not implemented | Not implemented |
| Sandbox enforcement mechanism | Done (`seatbelt-exec`: generated deny-default sandbox profile) | Done (`landlock-exec`: Landlock + seccomp-bpf) | **Missing entirely** — no AppContainer/job-object/restricted-token code found |
| Policy compiled from declared mounts | Done for `seatbelt-exec` read/write/toolchain root lists; substrate still needs to pass its mount table into this generator | Done for `landlock-exec` read/write/exec root lists; substrate still needs to pass its mount table into this generator | N/A until Windows mechanism exists |
| Mount-table + sandbox-policy composition (single declaration → both primitives) | Missing | Missing (mechanism exists, not driven by mount declarations) | Missing |
| `ExecResult`/`Outcome` observed-read-set field | Missing on all platforms — `runner.rs:84-98` has only a write-set | | |
| Actual subprocess launching in `vx-services` | Missing on all platforms — `orchestrator.rs`/`runner.rs`/`daemon.rs` are RPC shells, no `Command::new` anywhere | | |
| Zombie/wedge handling in the exec join path | **Missing** — mount-level unmount already has bounded sacrificial-child + kill-escalation (`mount.rs`/`killswitch.rs`); the run-level join path has no equivalent yet, and no `Poisoned` outcome variant exists | Not applicable — no comparable wedge failure mode documented for FUSE | Not applicable — no comparable wedge failure mode documented for ProjFS |

Adapter work, by component (not by time):

1. **Wire substrate mount declarations into `landlock-exec`'s policy
   compiler.** The wrapper now accepts arbitrary read/write/exec root lists;
   the remaining integration step is feeding it the real mount table and
   output roots from the substrate launcher.
2. **Wire substrate mount declarations into `seatbelt-exec`'s policy
   compiler.** The macOS wrapper now accepts declared roots; the remaining
   integration step is the same launcher handoff as Linux.
3. **Build the Windows enforcement mechanism from zero** — AppContainer or
   restricted-token launch scoped to the same declared-mount input.
   Currently nothing to extend either.
4. **Stop discarding `PRJ_NOTIFY_FILE_OPENED`** in
   `vx-vfs/src/windows.rs:337-371` — smallest fix in the whole list, and the
   one that gives Windows its second observation channel.
5. **Wire `tracked_observations` into the exec result path**: add an
   observed-read-set field to `ExecResult`/`runner.rs`, have the (currently
   nonexistent) orchestrator-side process launcher drain
   `tracked_observations(prefix_id, roots)` on join and attach it. This
   requires (6) to exist first, since there's no real launcher today.
6. **Give `vx-services` orchestrator/runner an actual process-launching
   implementation** — today they're protocol shells with no
   `std::process::Command` call anywhere; this is the piece that would call
   (1)/(2)/(3)'s sandboxed launch and then (5)'s observation drain.
7. **(Optional, lower priority) byte-range observation granularity** — thread
   `read()`'s existing `(offset, length)` parameters into
   `CasVfs::observe_identity` if tier-2 verification ever needs finer than
   path/identity-level touch tracking.
8. **Give the exec join path the same timeout-then-kill discipline the mount
   layer already has for unmount.** Add a bounded join deadline distinct
   from "legitimately slow," a `Poisoned` outcome variant so the orchestrator
   never trusts a wedged run's result as a clean miss, and a rule that a
   poisoned run triggers mount recovery (or reroutes to a fresh mount)
   before any retry — mirroring `try_sacrificial_unmount` →
   `kill_fskitd()` almost exactly, but at run granularity instead of mount
   granularity. macOS/FSKit-only concern; no equivalent wedge mode is
   documented for FUSE or ProjFS.
9. **Catalog and extend macOS FS-layer diagnostics for run-level (not just
   mount-level) failure.** Today's diagnostics — `probe.rs`'s timeout-bounded
   RPC-connect + `cat` round-trip, and `VfsIssueCode::HealthCheckFailed`/
   `MountFailed` / `VfsHealth` — answer "is the volume healthy," not "did
   *this run's* FS access wedge." The orchestrator needs a per-run signal
   (e.g. an event on the poisoned-run path) distinct from the existing
   mount-wide health check.
