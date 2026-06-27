+++
title = "The FSKit doctor & kill-switch"

[extra]
type = "Doc"
tiers = ["2"]
tags = ["fskit", "vfs", "mount", "macos", "recovery", "daemon", "smappservice"]
+++

On macOS the Vixen VFS is an FSKit mount. FSKit mounts **wedge** — most often
during development (rebuild/relaunch the extension and the kernel keeps a mount
whose backing server is gone), and in production when an app update unmounts
uncleanly. A wedged mount makes the VFS unusable: every access through it hangs
or errors, and no amount of code in the extension can fix it, because the broken
part is *outside* the extension.

This specifies two artifacts that together unwedge it:

- **The doctor** — an **unprivileged** recovery tool that walks an escalation
  ladder. It does ~95% of the work as the invoking user, with no privilege.
- **The kill-switch** — a **single-capability root daemon** whose entire surface
  is "terminate `fskitd`." It is the *only* thing in the system that touches
  root, and it does exactly one thing. No agent — human or LLM — ever gets a
  general root surface; they get one fixed button.

Claims are tagged **[settled]** (decided), **[lean]** (a proposal needing a
yes/redirect), **[open]** (collected in §7). Normative requirements are
blockquoted `r[fskit.*]` rules; the implementation honours them and annotates
code with `// r[fskit.…]`, to be promoted into the Tracey spec once it lands.

Facts below were **verified on macOS 26 (Darwin 25)** against a live vixen mount,
not inferred — §2 records them so nobody re-derives them.

## 0. The split — who needs root

The whole design follows from one verified fact: **only killing `fskitd` needs
root.** Everything else operates on resources the user already owns.

| Action | Owner of the resource | Needs root? |
| --- | --- | --- |
| Probe health (stat status.json, check PIDs) | — | no |
| Graceful unmount of the vixen mount | mount made by the user | no |
| **Force** unmount (`MNT_FORCE`) | mount made by the user | **no** (see `r[fskit.facts.force-unmount]`) |
| Kill the backing extension (`VixenFS.appex`) | runs as the user | no |
| Remount | — | no |
| **Kill `fskitd`** | runs as **root** | **yes** |

> r[fskit.split]
>
> The recovery system is partitioned by privilege. All rungs except terminating
> the system FSKit daemon are performed unprivileged by the doctor. Terminating
> `fskitd` is the sole privileged operation and is delegated to the kill-switch.
> No other privileged capability exists.

## 1. Failure modes (why mounts wedge)

In rough order of frequency:

1. **Backing server died, kernel mount survived.** The `VixenFS.appex` instance
   serving the volume crashes or is rebuilt/relaunched during dev. The kernel
   LIFS mount remains; I/O through it hangs or returns errno. *The common dev
   wedge.* Fixed unprivileged (force-unmount + remount).
2. **Unclean unmount on app update.** An FSKit volume mounted during an app
   update is unmounted without warning and not cleanly (no `synchronize`). Vixen
   ships **Sparkle**, so this is a production hazard, not just dev (see §7.5).
3. **Ghost mount.** A mountpoint that looks mounted but has no live server.
   `vx-vfs` already avoids provoking these with a `mkdir`-probe rather than I/O.
4. **`fskitd` itself wedged.** Rare. The daemon holds a mount it won't release
   and unprivileged unmount cannot clear it. **This — and only this — is what the
   kill-switch is for.**
5. **Stale registration.** `pluginkit` pointing at a DerivedData build instead of
   `/Applications/Vixen.app`. Already surfaced by existing diagnostics.

## 2. Verified macOS facts (do not re-derive)

> r[fskit.facts.mount-signature]
>
> An FSKit-backed mount is identified by the `fskit` token in its mount options,
> not by the filesystem-type string. The vixen mount appears as:
> `vixen://session on /private/tmp/vx/vfs (vixenfs, …, fskit, mounted by amos)`.

> r[fskit.facts.force-unmount]
>
> Force-unmounting the user's *own* mount needs no privilege. `unmount(2)`:
> *"[EPERM] The caller is not the super-user, and the mount() was not done by the
> user."* — i.e. the user who mounted it may unmount it, including with
> `MNT_FORCE`. `vfs.usermount` does not exist on macOS; Darwin uses this
> ownership check directly.

> r[fskit.facts.no-keepalive]
>
> `com.apple.filesystems.fskitd` has **no `KeepAlive`** key; it is launch-on-
> demand (`fskitd(8)`: invoked "when volume creation is requested or when
> previously-created volumes are detected"). Therefore killing it does not cause
> a respawn loop — it stays down until the next mount demand relaunches it
> cleanly. Killing is recoverable by design.

> r[fskit.facts.topology]
>
> The data path is kernel **LIFS** → `fskitd` (root, system LaunchDaemon, the
> `…fskitd.livefs` Mach service) → the **`VixenFS.appex`** instance (runs as the
> *user*, the per-mount volume server) → the Rust VFS over Vox. A per-user
> `fskit_agent` (also user-owned) coordinates the GUI-launched extension.

> r[fskit.facts.remount]
>
> A fresh mount is established with `/sbin/mount -F -t vixenfs vixen://session
> <path>` (`-F` = "Forces the file system type be considered as an FSModule
> delivered using FSKit"). This is the existing `vx-vfs` mount path and is reused
> verbatim.

## 3. The doctor (unprivileged)

> r[fskit.doctor.user-level]
>
> The doctor runs entirely as the invoking user. It never holds, requests, or
> acquires root itself. The only privileged step it can cause is an explicit call
> to the kill-switch (§4), which is a separate process.

> r[fskit.doctor.ladder]
>
> Recovery is an **ordered escalation**. Each rung is attempted only if the mount
> is still wedged after the previous one. The order is:
> **(a)** detect → **(b)** graceful unmount → **(c)** force unmount → **(d)** kill
> the backing extension, then force unmount again → **(e)** escalate to the
> kill-switch → **(f)** remount. The doctor stops at the first rung that leaves a
> clean, remountable state.

> r[fskit.doctor.detect]
>
> Wedged-ness is probed **without issuing I/O through the mount**: enumerate the
> mount table for the `fskit`-tagged entry at the target path, and probe via a
> bounded-timeout stat of `.vixenfs/status.json` / the `mkdir`-probe. A probe that
> would block on a dead server must time out, never hang the doctor.

> r[fskit.doctor.force-unmount]
>
> Force unmount calls `unmount(2)` with `MNT_FORCE` on the user's own mount
> (equivalently `umount -f`). Per `r[fskit.facts.force-unmount]` this requires no
> privilege. Prefer the syscall over shelling out.

> r[fskit.doctor.kill-extension]
>
> The doctor may terminate the backing `VixenFS.appex` instance serving the
> mount (identified by `eu.bearcove.vixenfs` / its process), then force-unmount
> again. The extension runs as the user, so this needs no privilege. Killing the
> user-owned `fskit_agent` belongs to this rung too — it is **not** a job for the
> kill-switch.

> r[fskit.doctor.escalate]
>
> The kill-switch is invoked **only** when the user-level rungs (force-unmount +
> extension kill) fail to clear the mount — the signature that `fskitd` itself is
> wedged. Escalation is the last rung before remount and is never the first
> response.

> r[fskit.doctor.remount]
>
> Once the mount is clear, the doctor re-establishes it through the existing
> `vx-vfs` mount path (`r[fskit.facts.remount]`). Remount is unprivileged and is
> not the kill-switch's responsibility.

> r[fskit.doctor.observable]
>
> Every rung emits `tracing`. The doctor reports which rung resolved the wedge,
> and on total failure attaches the existing macOS diagnostics
> (`collect_vfs_diagnostics`: pluginkit registration, fskit module list, VixenFS
> processes, recent `fskitd`/`fskit_agent`/extension logs). No `println`/`eprintln`.

> r[fskit.doctor.idempotent]
>
> The doctor on a healthy mount is a no-op that reports healthy; repeated runs are
> safe.

## 4. The kill-switch (privileged)

> r[fskit.killswitch.single-capability]
>
> The daemon exposes **exactly one** operation: terminate the system FSKit
> daemon. There is no message payload — the trigger (§5) carries no path, PID,
> type, command, or argument of any kind; its mere presence *is* the request.
> Nothing a caller supplies can change *what* the daemon does. There is no
> general "run as root" surface, by construction, so a malicious caller gains
> nothing beyond the one button.

> r[fskit.killswitch.target]
>
> The target is resolved **by the daemon itself** — the running launchd job
> `com.apple.filesystems.fskitd` (`/usr/libexec/fskitd`) — never from caller
> input. The client cannot redirect the kill at another process. The implementation
> targets the fixed launchd service (`launchctl kill ... system/com.apple.filesystems.fskitd`);
> it must not use caller-provided PIDs or name-enumerating tools such as `pgrep`
> or `killall`, because the process table may itself be wedged.

> r[fskit.killswitch.terminate]
>
> Per launch the daemon performs **at most one** terminate regardless of how many
> triggers are queued: `SIGTERM`, escalating to `SIGKILL` if `fskitd` does not
> exit within a bounded wait. Because the job has no `KeepAlive`
> (`r[fskit.facts.no-keepalive]`), `fskitd` stays down until the next mount demand
> relaunches it. The daemon does **not** relaunch `fskitd`, unmount, or remount —
> those are the doctor's unprivileged job. One trigger, one effect.

## 5. Transport — the trigger queue

The privileged hop is **not** XPC and **not** Vox. XPC is painful from Rust; Vox
is a full RPC framework for what is a single fire-and-forget signal. Instead the
daemon is a launchd **`QueueDirectories`** job: it is not even running until
launchd starts it because a file appeared in its queue. This is the disciplined
form of "the daemon watches a path; drop a file to fire it."

> r[fskit.killswitch.transport]
>
> The trigger is a **file dropped into a queue directory** watched by launchd via
> `QueueDirectories`. The doctor creates a uniquely-named file in the queue; that
> wakes the daemon. The daemon runs `r[fskit.killswitch.terminate]`, writes its
> audit record, **drains** the queue (unlinks all trigger files — required so
> launchd stops relaunching), and exits. The plist sets `RunAtLoad` (to
> create/repair the queue directory on load) and **no** `KeepAlive`.

> r[fskit.killswitch.ack-by-drain]
>
> There is no response channel. The doctor treats **removal of its trigger file
> within a bounded timeout as the acknowledgement** that the daemon ran; a timeout
> means the kill-switch is unavailable (not installed / not approved), which the
> doctor reports. Success of the kill itself is confirmed by the doctor
> independently re-probing `fskitd`/the mount before remounting — the privileged
> side never reports status.

> r[fskit.killswitch.auth-by-perms]
>
> Authorization is by **filesystem permission on the queue directory**, not by
> authenticating the caller's code identity. The queue is writable only by the
> `admin` group (owner `root`, group `admin`, mode `1730`); a non-admin process
> cannot fire it. We deliberately accept admin-uid gating over XPC audit-token
> code-signing checks **because the capability is a recoverable restart**: the
> worst an unwanted-but-authorized trigger can do is bounce `fskitd` for a
> transient VFS stall — not escalation, data loss, or persistence. The narrowness
> of the capability is the security property; the transport need only gate *who
> may fire it*. (If code-identity gating is ever required, the upgrade is an XPC
> Mach service with audit-token verification, at the cost of XPC-from-Rust.)

> r[fskit.killswitch.queue-perms]
>
> On each launch the daemon **asserts** the queue directory's ownership and mode
> before acting, repairs them if wrong, and refuses to act on a world-writable
> queue. A misconfigured queue must not silently widen who can press the button.

> r[fskit.killswitch.install]
>
> The daemon ships **inside `Vixen.app`**: plist at
> `Contents/Library/LaunchDaemons/eu.bearcove.vixen.fskit-killswitch.plist`,
> executable at `Contents/MacOS/fskit-killswitch` (referenced by the plist's
> bundle-relative `BundleProgram`). It is registered with
> `SMAppService.daemon(plistName: "eu.bearcove.vixen.fskit-killswitch.plist").register()`.
> Nothing is copied to `/Library/LaunchDaemons` or `/Library/PrivilegedHelperTools`.

> r[fskit.killswitch.approval]
>
> First registration requires a **one-time** user approval (System Settings →
> Login Items) plus an admin authentication prompt. Afterwards there is **no
> per-use prompt** — the button works for every subsequent invocation, across
> reboots.

> r[fskit.killswitch.signed]
>
> The daemon is signed with team `B2N6FSRTPV` as part of the app. SMAppService
> refuses unsigned or misplaced daemons; the signature is the trust root that
> makes the in-bundle install safe.

> r[fskit.killswitch.audit]
>
> Every launch logs the caller-visible facts (timestamp, whether `fskitd` was
> found, signal sent, outcome) via `os_log` and an append-only record in the
> kill-switch support directory. A privileged action is always auditable.

### Concrete names [settled]

| thing | value |
| --- | --- |
| app bundle id | `eu.bearcove.vixen` |
| team id | `B2N6FSRTPV` |
| daemon label / bundle id | `eu.bearcove.vixen.fskit-killswitch` |
| daemon plist (in app) | `Contents/Library/LaunchDaemons/eu.bearcove.vixen.fskit-killswitch.plist` |
| daemon executable (in app) | `Contents/MacOS/fskit-killswitch` |
| support dir | `/Library/Application Support/Vixen/fskit-killswitch/` (`root`-owned) |
| trigger queue dir | `…/fskit-killswitch/queue/` (`root:admin`, `1730`) |
| audit log | `…/fskit-killswitch/audit.log` (`root`-written) |

## 6. Failure & reporting

The doctor's outcome is data, consistent with the [failure model](/failure-model/):
a structured result naming the terminal rung and state (healthy / recovered-at-rung
/ unrecoverable + diagnostics), not a pre-formatted string. An unrecoverable wedge
after the kill-switch (e.g. SMAppService not yet approved, or `fskitd` re-wedging
immediately) is reported as such with the diagnostics attached — never swallowed.

## 7. Decision ledger

1. **Doctor location & name.** [lean] A `vx` subcommand (e.g. `vx doctor`) — most
   ergonomic for agents to call — over a standalone `vx-vfs-doctor` binary. Name
   TBD. *Need: yes/redirect.*
2. **Kill-switch transport.** [settled] launchd `QueueDirectories` file-drop (§5).
   Not XPC, not Vox. Rationale + threat model in `r[fskit.killswitch.transport]` /
   `r[fskit.killswitch.auth-by-perms]`.
3. **Caller authorization.** [settled] `admin`-group write on the trigger queue;
   no code-identity check (`r[fskit.killswitch.auth-by-perms]`), justified by the
   recoverable-restart capability.
4. **Capability surface.** [settled] The button is `fskitd` only. `fskit_agent` is
   user-owned and handled by the doctor's unprivileged rung
   (`r[fskit.doctor.kill-extension]`); `userfsd`/`fskit_helper` are out of scope.
5. **Sparkle clean-unmount hole.** [open] Whether the app/doctor proactively
   clean-unmounts on update (closing failure-mode §1.2). Out of scope for v1,
   tracked as a known hazard.
6. **Bundle id / label.** [settled] `eu.bearcove.vixen.fskit-killswitch`, team
   `B2N6FSRTPV` (§5 concrete names).
7. **SMAppService from a sandboxed app.** [verify] `Vixen.app` is sandboxed
   (`com.apple.security.app-sandbox`). Confirm `SMAppService` daemon registration
   is permitted from a sandboxed app (login-item/agent registration is; daemon
   registration must be verified). Does not affect the transport or names above —
   the trigger is dropped by the unsandboxed `vx` CLI, not the app.

## 8. First implementation slice (orientation)

**Reuse (exists today):**

- `crates/vx-vfs/src/macos/mod.rs` — `do_mount`
  for remount (`r[fskit.facts.remount]`); `read_status` / `ensure_mount_directory`
  for the non-blocking probe (`r[fskit.doctor.detect]`).
- `crates/vx-vfsd/src/platform/macos.rs` —
  `cleanup_stuck_mount` is currently a **no-op**; this is where the doctor ladder
  lands. `collect_vfs_diagnostics` is the failure-path reporter
  (`r[fskit.doctor.observable]`).
- `crates/vx-vfsd/src/platform/linux.rs` —
  the **analog** gentle→lazy→nuclear ladder already implemented for FUSE; mirror
  its shape for parity.

**New:**

- The doctor ladder in `vx-vfsd`'s macOS platform module, plus a `vx` subcommand
  to invoke it (§7.1).
- The kill-switch daemon: a **tiny Rust binary** (no XPC, no Vox — just a
  fixed launchd service-target signal + draining the queue dir) plus its
  launchd plist (`QueueDirectories` +
  `RunAtLoad`, no `KeepAlive`), both bundled in
  `apps/mac/xcode/Vixen` per §5's concrete names. The
  `SMAppService.daemon(…).register()` call and the helper's signing live in the
  app (`VixenApp.swift`, `Vixen.entitlements`, the xcodeproj build phase copying
  the binary into `Contents/MacOS`). The daemon resolves and signals the fixed
  `com.apple.filesystems.fskitd` launchd job
  (`r[fskit.killswitch.target]` / `.terminate`); the doctor fires it by writing
  a file (`r[fskit.killswitch.transport]`) and acks by drain
  (`r[fskit.killswitch.ack-by-drain]`).

The one empirical check to run first on a throwaway mount: confirm that killing
the appex leaves a force-unmountable zombie (vs. `fskitd` auto-reaping it). That
decides whether rung (d) is necessary or folds into (c).

## Related

- [The exec primitive](/exec/) — the VFS is exec's input/output substrate.
- [The demand-driven runtime](/runtime-semantics/) — VFS records reads/writes;
  the sandbox restricts host access.
- [Failures, poison, and Result](/failure-model/) — the doctor's result shape.
