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
> daemon. It accepts no path, no PID, no filesystem type, no command — nothing
> that changes *what* it kills. There is no general "run as root" surface, by
> construction, so a compromised or malicious caller gains nothing beyond the one
> button.

> r[fskit.killswitch.target]
>
> The target is resolved **by the daemon itself** — the running launchd job
> `com.apple.filesystems.fskitd` (`/usr/libexec/fskitd`) — never from a
> caller-supplied identifier. The client cannot redirect the kill at another
> process.

> r[fskit.killswitch.terminate]
>
> Termination sends `SIGTERM`, escalating to `SIGKILL` if `fskitd` does not exit
> within a bounded wait. Because the job has no `KeepAlive`
> (`r[fskit.facts.no-keepalive]`), `fskitd` stays down until the next mount demand
> relaunches it. The daemon does **not** relaunch `fskitd` itself.

> r[fskit.killswitch.returns-to-doctor]
>
> The kill-switch kills and returns a result (killed / already-dead / failed). It
> does **not** unmount or remount — those are the doctor's unprivileged job. This
> keeps the privileged surface minimal: one verb, one effect.

## 5. Installation & trust (SMAppService)

> r[fskit.killswitch.install]
>
> The daemon ships **inside `Vixen.app`** — its launchd plist under
> `Contents/Library/LaunchDaemons/`, its executable bundled and referenced via
> `BundleProgram` — and is registered with
> `SMAppService.daemon(plistName:).register()`. Files are **not** copied into
> `/Library/LaunchDaemons` or `/Library/PrivilegedHelperTools`.

> r[fskit.killswitch.approval]
>
> First registration requires a **one-time** user approval (System Settings →
> Login Items) plus an admin authentication prompt (installing a root daemon).
> After approval there is **no per-use prompt** — the button works for every
> subsequent invocation, across reboots.

> r[fskit.killswitch.signed]
>
> The daemon is Developer-ID signed as part of the app. SMAppService refuses
> unsigned or misplaced daemons; signing is the trust root for the whole scheme.

> r[fskit.killswitch.caller-auth]
>
> The daemon authenticates its IPC peer before acting: it checks the connecting
> process's code-signing identity against Vixen's designated requirement (team +
> bundle). Only the Vixen app / `vx` CLI may press the button; arbitrary local
> processes may not.

> r[fskit.killswitch.audit]
>
> Every invocation is logged (caller identity, timestamp, outcome) via
> `tracing` / `os_log`. A privileged action is always auditable.

## 6. Failure & reporting

The doctor's outcome is data, consistent with the [failure model](/failure-model/):
a structured result naming the terminal rung and state (healthy / recovered-at-rung
/ unrecoverable + diagnostics), not a pre-formatted string. An unrecoverable wedge
after the kill-switch (e.g. SMAppService not yet approved, or `fskitd` re-wedging
immediately) is reported as such with the diagnostics attached — never swallowed.

## 7. Decision ledger (what needs deciding)

1. **Doctor location & name.** [lean] A `vx` subcommand (e.g. `vx doctor`) —
   most ergonomic for agents to call — over a standalone `vx-vfs-doctor` binary.
   Name TBD. *Need: yes/redirect.*
2. **Kill-switch transport.** [open] Native **XPC/Mach** (the SMAppService idiom,
   gives peer code-signing checks for free) vs. the **Vox `local://`** socket used
   everywhere else in Vixen. The unprivileged VFS data plane stays Vox regardless;
   this is only about the privileged hop. *Genuine fork.*
3. **Caller authorization mechanism.** [lean] Audit-token + designated-requirement
   code-signing check on the connecting peer (`r[fskit.killswitch.caller-auth]`).
   Confirm this is the bar.
4. **Capability surface.** [settled] The button is `fskitd` only. `fskit_agent` is
   user-owned and handled by the doctor's unprivileged rung
   (`r[fskit.doctor.kill-extension]`); `userfsd`/`fskit_helper` are out of scope.
5. **Sparkle clean-unmount hole.** [open] Whether the app/doctor proactively
   clean-unmounts on update (closing failure-mode §1.2). Proposed out of scope for
   v1, tracked as a known hazard.
6. **Bundle id / label.** [lean] `eu.bearcove.vixen.fskit-killswitch`, matching
   the `eu.bearcove.vixenfs` convention. Confirm.

## 8. First implementation slice (orientation)

**Reuse (exists today):**

- [`crates/vx-vfs/src/macos/mod.rs`](crates/vx-vfs/src/macos/mod.rs) — `do_mount`
  for remount (`r[fskit.facts.remount]`); `read_status` / `ensure_mount_directory`
  for the non-blocking probe (`r[fskit.doctor.detect]`).
- [`crates/vx-vfsd/src/platform/macos.rs`](crates/vx-vfsd/src/platform/macos.rs) —
  `cleanup_stuck_mount` is currently a **no-op**; this is where the doctor ladder
  lands. `collect_vfs_diagnostics` is the failure-path reporter
  (`r[fskit.doctor.observable]`).
- [`crates/vx-vfsd/src/platform/linux.rs`](crates/vx-vfsd/src/platform/linux.rs) —
  the **analog** gentle→lazy→nuclear ladder already implemented for FUSE; mirror
  its shape for parity.

**New:**

- The doctor ladder in `vx-vfsd`'s macOS platform module, plus a `vx` subcommand
  to invoke it (§7.1).
- The kill-switch daemon: a tiny binary + its launchd plist inside
  [`apps/mac/xcode/Vixen`](apps/mac/xcode/Vixen), with the `SMAppService` register
  call in the app (`VixenApp.swift`) and the matching entitlements
  (`Vixen.entitlements`). The daemon resolves and kills `fskitd`
  (`r[fskit.killswitch.target]` / `r[fskit.killswitch.terminate]`) and authenticates
  its caller (`r[fskit.killswitch.caller-auth]`).

The one empirical check to run first on a throwaway mount: confirm that killing
the appex leaves a force-unmountable zombie (vs. `fskitd` auto-reaping it). That
decides whether rung (d) is necessary or folds into (c).

## Related

- [The exec primitive](/exec/) — the VFS is exec's input/output substrate.
- [The demand-driven runtime](/runtime-semantics/) — VFS records reads/writes;
  the sandbox restricts host access.
- [Failures, poison, and Result](/failure-model/) — the doctor's result shape.
