# Vixen.app wedge resilience: a preemptive audit

Threat model (Amos-direct, hard-won, verbatim intent): **FSKit wielded
carelessly can wedge the entire OS.** When a mount wedges, file-system
operations against it send processes into zombie (`D`, uninterruptible-sleep)
states that are nearly impossible to diagnose — and *you cannot kill your way
out*: a process blocked in-kernel on a wedged VFS does not die on `SIGKILL`
(the signal is deferred until the syscall returns, which is never), and any
process that touches the wedged mount to clean it up joins the wedge, including
the would-be killer. Kill-based recovery is a lie under this failure mode. The
nested-mount convention `/Volumes/Vixen/vfs` (never `/Volumes/Vixen` directly)
is a deliberate defense so that enumerating `/Volumes` — which macOS does
constantly — never touches a wedged FSKit root. The subsystem is "a trash
fire." The only real fix: **Vixen.app itself must be structured so the wedge
cannot reach its core.**

This audit measures the current tree against that requirement. It is companion
to `vix-substrate-vfs-sandbox.md` (§1 lore, §4/§5 the exec-leaf integration and
the `Poisoned`-outcome design) and its addendum (the seatbelt archaeology). It
reads from code as of `main`; every claim cites a file:line.

Scope reminder from the recon: the **serving side is insulated by
construction.** On macOS the byte-serving backend (the app's `VfsBackend`, or
`vx-vfsd`'s `CasVfs`/`SharedVfs`) is a *Vox RPC peer of the FSKit extension* —
it *answers* `read`/`lookup`/`readdir` calls; it never *issues* FS ops against
the mount. When FSKit wedges, the server simply stops receiving calls; it does
not block. The danger is entirely on the **caller side**: code that issues
`stat`/`open`/`read`/`readdir` against a path under the mountpoint. So the
audit is a hunt for callers, and specifically for callers that live in — or can
block — the app core.

---

## 1. The touch graph

Every code path that can transitively touch the mount, the thread/process it
runs on, and what dies when the mount wedges. Verdict key: **SAFE** (cannot
reach a wedgeable FSKit node), **SACRIFICIAL** (touches the mount only from an
isolated, bounded child process designed to be abandoned), **EXPOSED** (a wedge
propagates toward the app core, or an in-process thread blocks with no
recovery).

### App core — `Vixen.app` GUI process (`VixenMain` → `VixenApp`)

This is the process that must never wedge. It is also, today, where the two
worst offenders live.

| # | Path | Thread/proc | On wedge | Verdict |
|---|------|-------------|----------|---------|
| 1 | `DoctorService.checkBackend` → `readFileEventually("…/vfs/app/README.md")` → `readFileWithTimeout` → `Data(contentsOf:)` (`Doctor.swift:407,425,439`) | `DispatchQueue.global(qos:.userInitiated)` — a **shared** GCD pool thread | Thread blocks in uninterruptible `read(2)`, **forever**. Semaphore (`Doctor.swift:446`) times out so the doctor returns, but the thread is unrecoverable — there is no kill for an in-process thread. `readFileEventually` loops issuing a fresh async read every ≤0.25 s across a 2 s budget (`Doctor.swift:427-434`), so one wedged poll strands up to ~8 shared-pool threads; the poll repeats every 30 s (`VixenApp.swift:858`). GCD's global concurrent pool is width-bounded — sustained wedging exhausts it and starves every other `DispatchQueue.global` user in the app. | **EXPOSED — worst offender #1** |
| 2 | `ManagementService.isMountpointReady` → `stat("/Volumes/Vixen/vfs")` (`VixenApp.swift:163-167`) | SwiftUI **MainActor** (via `.task`/`onAppear`/`didBecomeActive` → `refreshManagementStatus` → `snapshot`), *and* the Doctor's detached task (`Doctor.swift:137`) | Synchronous libc `stat`, **no timeout**. `stat` of the volume *root* routes to FSKit `getattr(root)`; on a mounted-but-wedged volume it blocks. On the MainActor that **freezes the UI**. The `st_uid == getuid()` short-circuit does not save it — the `stat` executes first and is the hazard. Note this stats the mount root *itself*, which is exactly the wedged node — the nested-mount defense protects `/Volumes` enumeration, not a direct stat of `…/vfs`. | **EXPOSED — worst offender #2** |
| 3 | `DoctorService.readStatusPayload` → `/bin/cat …/vfs/.vixenfs/status.json` (`Doctor.swift:324`) | `Process` child, 1 s timeout → `terminate()`+`SIGKILL` (`Doctor.swift:522-527`) | `status.json` is served **in-extension** as a synthetic frozen snapshot (`vx-fskit/src/macos/root_peer.rs:260-284`), so it only wedges if the extension itself is wedged. On timeout the child is signalled — but a child blocked in FSKit I/O won't reap on `SIGKILL`, so a zombie child leaks. The app core moves on (semaphore-bounded). | **SACRIFICIAL** (zombie-child leak) |
| 4 | `DoctorService.checkMount` → `/sbin/mount` (`Doctor.swift:270`) | `Process` child, 2 s timeout+kill | Reads the mount *table*, never routes to FSKit. | **SAFE** |
| 5 | `ManagementService.isVixenMounted` → `/sbin/mount` (`VixenApp.swift:171`) | `Process` child, **no timeout** (`waitUntilExit`, `VixenApp.swift:376`) | Mount-table read, no FSKit touch — cannot wedge. Unbounded `waitUntilExit` is a lesser smell (a hung `/sbin/mount` would hang the caller), but `mount(8)` reading the table does not touch the volume. | **SAFE** |
| 6 | `VfsBackendService.serveLoop` — serves `VfsBackend` over `vixenfs.sock` (`VfsBackend.swift:213-289`) | detached `.utility` `Task` | It is the RPC **server**, not an FS caller — it never issues FS ops against the mount. A wedge means FSKit stops calling it; it idles and reconnects. Connect and lane-open are each 10 s-bounded (`VfsBackend.swift:236,266`). Insulated by construction. Caveat: if its *handler* blocked it would *cause* a wedge — but the in-memory demo handler (`VfsBackend.swift:110-158`) cannot block. | **SAFE** (server side) |
| 7 | AppKit surfaces: `NSWorkspace.open` (https / System Settings.app only), asset-catalog icons, no `NSOpenPanel`/`NSSavePanel` at the mount (`VixenApp.swift:637,777,783,1224`) | MainActor | No AppKit path points at the mount. The volume mounts `-o nobrowse` (`mount.rs:423`) and is nested under `/Volumes/Vixen`, so it is absent from the Finder sidebar and never subject to icon/QuickLook/recent-files resolution. This is the nested-mount + nobrowse defense working. | **SAFE** (by deliberate design) |

### `vix-management` helper (short-lived, one process per action)

Not the app core — its blast radius is the helper invocation. But it embodies
the discipline the whole design generalizes from.

| # | Path | Thread/proc | On wedge | Verdict |
|---|------|-------------|----------|---------|
| 8 | `probe::probe_vfs_async` → `/bin/cat …/status.json` and `…/<probe>` (`probe.rs:66,83`) | child, `tokio::time::timeout(3 s)` + `kill_on_drop(true)` (`probe.rs:89-92`) | Bounded child; abandoned on timeout. Also opens a *second* backend claiming a probe prefix — which the Doctor comment notes can "displace the real prefix off the mount" (`Doctor.swift:373-377`), which is why the Doctor migrated away from `probe-vfs`. Bounded, but the prefix-displacement is a design smell; `probe.rs` is effectively legacy. | **SACRIFICIAL** (prefix-displacement smell) |
| 9 | `mount::try_sacrificial_unmount` → `/sbin/umount` (`mount.rs:212-249`) | child, 2 s deadline → `child.kill()` | **The archetype.** The non-sacrificial caller (the helper) never blocks; the child is the sacrifice; escalation on timeout is `kill_fskitd`. | **SACRIFICIAL** (the seed of the design) |
| 10 | `mount::clean_mountpoint` → `std::fs::read_dir` / `remove_*` on `MOUNT_POINT` (`mount.rs:364-394`) | helper main thread, **no timeout** | Runs only *after* recovery has confirmed nothing is mounted at the point (`mount.rs:363` comment), i.e. against the plain underlying directory, not FSKit. If that invariant is ever violated (a wedged mount still present), this reads the wedged root in-process, unbounded. Helper-scoped, not app-core. | **EXPOSED-conditional** (helper only) |
| 11 | `mount::is_mounted` / `mount_output` / `vixenfs_mount_points` → `/sbin/mount` (`mount.rs:487,509`) | child | Mount-table only. | **SAFE** |
| 12 | `killswitch::terminate_fskitd` → `proc_listpids` + `kill(2)` on resolved `/usr/libexec/fskitd` (`killswitch.rs:17-46,74-117`) | helper main thread | Does not touch the mount. **But the kill target can itself resist death:** if `fskitd` is blocked in-kernel uninterruptible, `SIGKILL` is deferred and it may not exit. The escalation ends at one `SIGKILL` and returns `Ok(true)` **without a post-`SIGKILL` verification loop** (`killswitch.rs:37-45`) — it confirms exit after `SIGTERM` (3 s) but not after the final `SIGKILL`. This is the "kill-based recovery is a lie" hazard reappearing one layer down: even the kill-switch's target can zombify. | **SAFE-to-touch, PARTIAL as recovery** |

### `vx-vfsd` daemon (separate process; the CAS backend that connects to the extension)

| # | Path | Thread/proc | On wedge | Verdict |
|---|------|-------------|----------|---------|
| 13 | `read_status` → `/bin/cat …/.vixenfs/status.json` (`vx-vfs/src/macos/mod.rs:143-147`) | `moire::process::Command`, **no timeout** | Boot-time reuse-probe. Unlike `probe.rs`, this `/bin/cat` is **not** wrapped in a timeout — if the extension is wedged the `.output().await` never returns and the boot task hangs (child zombifies). Inconsistent discipline versus `probe.rs:89`. Daemon-scoped. | **EXPOSED** (unbounded), daemon |
| 14 | `boot_with_store` smoke test → `moire::fs::read(canary)` (`vx-vfsd/src/lib.rs:513-520`) | `tokio::time::timeout(5 s)`, 10 attempts | Bounded at the async layer; the underlying blocking read leaks a stuck blocking-pool thread on wedge. Boot-time only. | **SACRIFICIAL-ish** (bounded await, leaked pool thread) |
| 15 | `vfs_death_watcher` → on `vfs_dead` watch, `std::process::exit(1)` (`vx-vfsd/src/lib.rs:562-570`; watch fed by `connection.closed()` in `macos/mod.rs:107-112`) | task | The **only** liveness signal today. But it detects *session closed*, not *wedge* — a wedged-but-open session never trips it. And it converts serving-loop death into whole-process exit, raising the supervision question (§2). | (mechanism, not a touch) |
| 16 | `CasVfs`/`SharedVfs` read/serve handlers (`vx-vfsd/src/lib.rs`, `vfs.rs`) | serving tasks | Server side — answer RPCs, never issue FS ops against the mount. | **SAFE** (server side) |

### Future exec/run path (not yet built)

Runs execute *under* the mount (cwd/argv under `/Volumes/Vixen/vfs`) as **child
processes**, hence **SACRIFICIAL by construction** — the correct shape. The
`join → Outcome` discipline with a `Poisoned` variant is designed
(`vix-substrate-vfs-sandbox.md` §4, §5 item 8) but unimplemented; `vx-services`
has no `Command::new` anywhere (confirmed: the only match in `runner.rs:94` is a
doc comment). Nothing to break yet — but the design must land before the exec
leaf ships, or every run becomes an EXPOSED path.

### Verdict counts

- **SAFE: 6** — #4, #5, #6, #7, #11, #16
- **SACRIFICIAL: 4** — #3, #8, #9, #14 (all with noted caveats)
- **EXPOSED: 5** — #1, #2, #13 clean; #10 (helper-scoped/conditional), #12
  (safe-to-touch but partial as recovery)

**Worst offenders, both in the app core, both must be evicted first:**

1. **#1 `checkBackend` → `Data(contentsOf:)` on the shared GCD global pool.** A
   sustained wedge drains the app's shared concurrent-thread pool with no
   recovery. Also self-referential: it reads a path served by *this same
   process's* `serveLoop` (`VfsBackend.swift`), so a stalled backend and a
   stalled reader are one process.
2. **#2 `isMountpointReady` → `stat` on the MainActor.** A single unbounded
   `stat` of the wedged volume root freezes the UI thread — the most direct
   possible propagation of the wedge into the app core.

No file watchers (FSEvents/`DispatchSource`/kqueue/`notify`) touch the mount
anywhere in the tree (grep is empty) — one whole category of implicit,
continuous touching is absent by good fortune and must stay absent.

---

## 2. The isolation architecture (target)

The invariant: **the app core never issues an FS op against the mount — not
`stat`, not `open`, not `readdir`, not through a helper it then blocks on.**
Every mount-touching operation happens in a **sacrificial child process**,
addressed over RPC, and the deadline is on the **RPC**, never on the FS op
itself. A deadline on the FS op is a lie: the op is what wedges, and the thread
running it cannot be reclaimed.

### Process tree

```
launchd
  └─ Vixen.app  (app core — MUST stay wedge-free)
       ├─ VfsBackendService serveLoop        [server side: SAFE, keep]
       ├─ (supervises) mount-touching children over Vox RPC, deadline on RPC:
       │     ├─ prober / doctor child  → issues the stat/read, reports over RPC
       │     └─ (existing) vix-management: umount / mount / kill-fskitd / probe
       └─ NEVER: stat/open/read/readdir against /Volumes/Vixen/vfs in-process
  └─ vx-vfsd  (CAS backend — server side, SAFE; supervised, restartable)
  └─ fskitd / VixenFS.appex  (platform; the thing that wedges)
  └─ run children  (execute under the mount — SACRIFICIAL by construction)
```

The **seed already exists**: `try_sacrificial_unmount` (`mount.rs:212`) is
exactly this pattern — a bounded child, killed on timeout, with the parent
guaranteed never to join the wedge. Generalize it from "unmount" to "any mount
touch."

### Concrete moves (app core)

- **#2 → out-of-process.** `isMountpointReady`'s ownership/liveness question
  must be answered without an in-process `stat` of `…/vfs`. Split the two facts
  it conflates: *"is the mountpoint dir set up"* is answerable by stat-ing the
  **parent** `/Volumes/Vixen` (a plain APFS dir that never wedges — the whole
  point of the nested mount), and *"is the volume live"* is a mount-table read
  (`/sbin/mount`, already SAFE) plus, if content-liveness is truly needed, a
  **child-process** probe with an RPC deadline. Never `stat` the volume root on
  any thread the app awaits synchronously, least of all the MainActor.
- **#1 → out-of-process.** `checkBackend`'s "can I read a byte through the
  mount" is the one check that genuinely requires touching content. It must run
  in a **child process** (the shape `probe.rs` already has:
  `tokio::time::timeout` + `kill_on_drop`), and the app waits on the *child's
  exit / RPC reply* with a deadline — never on an in-process `Data(contentsOf:)`
  on a shared queue. If a child is too heavy per 30 s poll, gate the content
  probe behind a cheap out-of-band liveness signal (below) and only spawn it
  when that signal is already green.
- **#3** is already close (bounded child) — accept the zombie-child leak as the
  cost of the SIGKILL-can't-reap reality, or gate it behind the same liveness
  signal so it is not spawned against a known-wedged mount.

### Out-of-band wedge detection (the detector never probes the mount)

Probing *is* touching. The detector must not read the mount to decide the mount
is wedged. Instead: **`vx-vfsd` (and the app's `VfsBackend`) heartbeat their own
serving loop** — a monotonic counter/timestamp bumped each time the serving
task makes a full trip around its accept/dispatch loop, published out-of-band
(app-group file, or a side RPC on a *different* socket than the FS data path).
"Serving loop has not advanced in N seconds while FSKit has pending calls" is a
wedge signal that required **zero** FS touches to obtain. Today the only signal
is #15's `connection.closed()` → `exit(1)`, which fires on *clean* session
close and is silent on the dangerous wedged-but-open case. That gap is the
detector to build.

### What state must survive a sacrifice

The CAS makes this nearly free: **content is content-addressed, so nearly all
state is recoverable by construction.** A sacrificed prober/child holds no
authoritative state. A sacrificed/remounted volume loses only the *mount
registration* and in-flight reads; the tree hash, the CAS blobs, and the
observed-read-set (once wired, `vix-substrate-vfs-sandbox.md` §2) all re-derive
from the store. The orchestrator's run ledger (which runs are demanded /
`Poisoned`) is the one piece that must live in the app core / orchestrator, not
behind the mount — and it already does (it is the orchestrator's own state).

### Supervision

- **Who restarts `vx-vfsd`:** today it self-exits on session death (#15) with
  no restarter in-tree. Under launchd it would relaunch if configured
  `KeepAlive`; that must be explicit. The restart must go through the full
  recovery sequence (§3), not a bare re-exec against a still-wedged mount.
- **Who restarts children:** the app core / helper, which already treats each
  as fire-and-forget with a deadline.
- **Who restarts the app:** launchd (it is a GUI app; a wedge that reaches the
  core today means a hung UI, not a crash — which is *worse* than a crash,
  because launchd can restart a crash but not a hang. This is an argument for
  making the core **fail fast** — assert/abort on a detected in-core wedge — so
  supervision has something to act on).

---

## 3. The zombie protocol (detected wedge → recovery that never touches the mount)

The sequence, and what in today's code violates it.

1. **Mark affected runs `Poisoned` in the orchestrator.** Designed at the seam
   (`vix-substrate-vfs-sandbox.md` §4 — a `Poisoned` outcome distinct from a
   nonzero exit, never trusted as a clean cache miss). *Not yet implemented* —
   no `Poisoned` variant, no orchestrator run-ledger with FS-wedge state.
   **Gap, not a violation** (nothing to poison yet).
2. **Fence new demands from routing to the wedged mount.** Also unbuilt — no
   router. **Gap.**
3. **Sacrifice the FSKit extension host** via the existing escalation:
   `try_sacrificial_unmount` → on timeout `kill_fskitd` → `terminate_fskitd`
   (`mount.rs:174-187`, `killswitch.rs:17-46`). **Audit of what it actually
   kills:** it resolves `/usr/libexec/fskitd` by path and `kill(2)`s that PID
   directly (deliberately not via launchd, `killswitch.rs:5`). It does **not**
   touch the mount, so the killer does not join the wedge — correct. **But**
   (per #12) the final `SIGKILL` is fire-and-return with no verification, and a
   `fskitd` blocked in-kernel can defer `SIGKILL` indefinitely. **Violation:**
   the escalation's terminal rung has no "did it actually die, and if not,
   what" — under the exact failure mode the kill-switch exists for, it can
   silently not work. It also targets only `fskitd`, not the `fskit_agent` /
   `VixenFS.appex` extension host process that actually runs the volume; whether
   killing `fskitd` alone clears a wedged *extension* is unverified here.
4. **Remount fresh at a NEW nested path — never reuse the wedged mountpoint.**
   **Violation:** `remount`/`establish` reuse the *fixed* `MOUNT_POINT`
   `/Volumes/Vixen/vfs` (`mount.rs:149-162`, `establish:97`) and even
   `clean_mountpoint` it (#10). If `fskitd` retained a zombie registration for
   that path (the exact hazard called out at `mount.rs:107-109`), the fresh
   mount inherits the poison. The protocol wants a *fresh* nested path
   (`…/vfs-2`, `…/vfs-<gen>`); today there is one hard-coded point and the whole
   recovery aims it back at the wedged spot. This is the single biggest
   structural deviation from the zombie protocol.
5. **Rehydrate from CAS.** Free by construction — the new mount registers the
   same tree hash; blobs are in the store. No violation; nothing to do.

**Summary of violations in today's code:** (a) the kill-switch's terminal
`SIGKILL` is unverified against a target that can resist it (#12); (b) recovery
reuses the one fixed mountpoint instead of rotating to a fresh nested path
(steps 4, #10); (c) steps 1–2 (poison + fence) don't exist because the
orchestrator/run layer doesn't exist yet.

---

## 4. The wedge oracle (a chaos/drill mode that makes resilience testable)

Design: a **hang-on-Nth-read fixture inside `vx-vfsd`, behind a flag**, so the
whole zombie protocol is exercisable in CI/dev without wedging a real machine.

Feasibility in the current shape: **high, and the seam is already there.** Every
read on every platform funnels through one method — `Vfs::read` →
`CasVfs::read`, wrapped by `SharedVfs::read` (`vx-vfsd/src/lib.rs:328-341`).
`SharedVfs` already *interposes* on that path (it injects the canary before
delegating). A drill mode is the same interposition with a different behavior:

```
SharedVfs {
    inner: Arc<CasVfs>,
    canary: Arc<Canary>,
    wedge: Option<WedgeFixture>,   // new, flag-gated
}

// in read(), before delegating to CasVfs::read:
if let Some(w) = &self.wedge {
    match w.on_read() {
        WedgeAction::HangForever   => std::future::pending().await,  // never returns
        WedgeAction::HangFor(d)    => tokio::time::sleep(d).await,    // slow-but-alive
        WedgeAction::Pass          => {}
    }
}
```

- **`HangForever` on the Nth read** reproduces the true wedge (the read never
  completes) — the case that must be survived without the *server* process
  itself dying and without the *caller* thread being reclaimable.
- **`HangFor(d)`** reproduces "slow-but-alive," the case the join deadline must
  *not* poison (distinguishing legitimately-slow I/O from a wedge is the hard
  calibration called out in `vix-substrate-vfs-sandbox.md` §4).
- Flag-gated via env (`VX_VFSD_WEDGE=nth:5,forever` etc.) or a debug-only RPC,
  compiled but inert in release, in the spirit of the codebase's opt-in
  observable dumps.

What it exercises **without touching a real machine's FSKit:** on Linux/FUSE the
fixture wedges a read and the test drives the full detect → poison → fence →
sacrifice → remount-fresh → rehydrate sequence against a FUSE mount (no
`fskitd`, no OS-wide blast radius). This is the oracle Amos wants: resilience
becomes a passing/failing integration test on the production `Vfs::read` path
(the best kind of test per house rules — exercises the real path, not a mock),
instead of an aspiration you can only validate by wedging a laptop. On macOS the
same fixture, run deliberately, lets you validate that the kill-switch +
fresh-remount actually clears a *self-inflicted* wedge in a controlled window.

One caveat: `HangForever` on Linux/FUSE will strand the FUSE request thread and
leave the mount unmountable-except-lazily for the test's lifetime — so the drill
harness should run each `HangForever` case in a throwaway mount namespace / temp
mountpoint it is willing to `umount -l` and discard, mirroring in the test
harness the same "rotate to a fresh mountpoint, abandon the old" discipline the
protocol itself prescribes (§3 step 4).

---

## 5. macOS platform realities (the design must be legal under the OS's rules)

- **FSKit extension lifecycle is not ours to own.** `VixenFS.appex` runs in the
  user's `fskit_agent`, out of our process, under the **App Sandbox**
  (`apps/mac/xcode/VixenFS/VixenFS.entitlements` — `app-sandbox = true`, a
  system-extension requirement). We cannot `fork`/supervise the extension host
  directly; the OS (`fskitd` / launchd) owns its lifecycle. Our only levers are
  the ones the code already uses: mount/unmount as the user, and `kill(2)` on
  `fskitd` (`killswitch.rs`). The resilience design must route "sacrifice the
  extension host" through those levers, not through a parent/child relationship
  we don't have.
- **`mount(2)` of a block FSKit resource needs root; everything else is the
  user.** The design keeps the one privileged step (create+chown the mountpoint)
  as a single admin prompt (`VixenApp.swift:234-259`, `mount.rs:55-81`) and does
  mount/unmount passwordless as the user via a user-owned device node. A
  **fresh nested mountpoint** (§3 step 4) is therefore *not* free: a new path
  under root-owned `/Volumes/Vixen` needs the same one-time root create+chown.
  Mitigation: pre-provision a small pool of chowned nested points
  (`…/vfs`, `…/vfs-1`, `…/vfs-2`) during the single first-run admin prompt, so
  rotation at recovery time stays passwordless. This is legal and cheap and
  removes the "recovery needs a root prompt" trap.
- **AppleScript admin prompt is the only escalation.** `runPrivileged`
  (`VixenApp.swift:254-259`) shells `do shell script … with administrator
  privileges` and must run on the main thread (`VixenApp.swift:289`). Recovery
  that needs root (kill-fskitd fallback) therefore momentarily needs the main
  thread — another reason the core must never be *blocked* on the main thread by
  a wedge, or it cannot even raise its own recovery prompt.
- **`-o nobrowse` + nested mount are load-bearing platform-legal defenses**,
  already in place (`mount.rs:423`, `vx-vfs-types` mount constants). They keep
  Finder/Spotlight/`mdworker` off the volume (no auto-enumeration, no indexing
  walk). Keep them; treat any future change that surfaces the volume in Finder
  as a resilience regression, not a UX improvement.
- **No XPC in the FS data path to lose.** The app↔extension link is a Vox
  `local://` socket in the app-group container (`VfsBackend.swift:195-201`),
  not an XPC service, so there is no XPC-reconnect semantics to design around —
  the `serveLoop` reconnect (#6) already handles socket loss.

---

## Bottom line

The dangerous surface is **small and localized**: two app-core paths (#1
`checkBackend`'s in-process `Data(contentsOf:)` on the shared GCD pool, #2
`isMountpointReady`'s MainActor `stat`) are the only places a wedge reaches the
core, plus one unbounded daemon-side `/bin/cat` (#13). Everything else is
already SAFE (server-side or mount-table-only) or SACRIFICIAL (bounded children,
generalizing `try_sacrificial_unmount`). The recovery escalation exists and is
correctly structured to not join the wedge, but has two real deviations from the
zombie protocol: an **unverified terminal `SIGKILL`** against a target that can
resist it (#12), and **recovery that reuses the one fixed mountpoint** instead
of rotating to a fresh nested path (§3 step 4). The wedge oracle is cheap to
build — it is one more interposition on the `SharedVfs::read` seam that already
exists — and it is the thing that turns all of the above from an argument into a
test.
