+++
title = "VFS and sandbox stack"

[extra]
type = "ArchitectureNote"
topic = "vfs-sandbox"
created = "2026-05-26"
tags = ["tier-2", "vfs", "sandbox", "platform"]
+++

How Vixen interposes the cache at the byte-read level and constrains process reads to a declared read-set. Both are implemented today; this note describes what exists, not what we'd like to exist.

## VFS — three native backends

Vixen exposes one `Vfs` trait (from `vx-services`) and three platform-specific mount mechanisms. There is no uniform user-space VFS that we maintain across operating systems; each OS gets the native, kernel-blessed approach.

Source: `crates/vx-vfs/`.

| OS | Backend | Crate | File |
|---|---|---|---|
| Linux | FUSE via `fuse3` | `crates-plus/fuse3` | `vx-vfs/src/linux.rs` |
| macOS | FSKit via Vixen.app extension | `vx-fskit`, `vx-fskit-types`, `apps/mac` (Swift host) | `vx-vfs/src/macos/mod.rs` |
| Windows | ProjFS via `windows_projfs` | (external crate) | `vx-vfs/src/windows.rs` |

The macOS backend is the most involved: a Swift FSKit extension (`apps/mac`) hosts the filesystem; `vx-vfs/src/macos/` mounts the extension, resolves the socket path from the extension's `status.json`, then connects over Vox and serves the `Vfs` trait on a single root connection.

The orchestration layer `vx-vfsd` ties these together — `mount()` in `vx-vfs/lib.rs` dispatches via `cfg_if!` to the right backend, runs platform checks, performs health verification, returns a `VfsStart` handle.

## Sandbox — state of play

Same per-OS-native pattern as the VFS layer is the *intent*, but only Linux is implemented today.

| OS | Status | API | Implementation |
|---|---|---|---|
| Linux | **implemented** | Landlock LSM (+ seccomp future) | `crates/landlock-exec/` standalone exec wrapper |
| macOS | **intended, not implemented** | Apple Sandbox (`sandbox_init`, aka "seatbelt") | — |
| Windows | **not designed yet** | TBD — AppContainer / Job Objects / restricted tokens / mitigation policies | — |

The intent is documented in `vx-vfsd/src/vfs.rs`:

> Each action gets its own prefix directory. The sandbox (seatbelt on macOS, seccomp/landlock on Linux) restricts the subprocess to only access its prefix.

The macOS direction has a name (seatbelt) but no code. The Windows direction has neither.

### Linux: `landlock-exec`

A standalone binary that applies Landlock LSM rules and then `exec`s the target:

- **File reads:** `VFS_ROOT` plus system paths (`/usr`, `/lib`, `/etc` for toolchain headers).
- **File writes:** `WRITE_DIR` plus `/tmp` (rustc temp files; `TMPDIR` is deliberately ignored).
- **Network:** denied entirely.
- **Execution:** allowed from `VFS_ROOT` plus system paths.

Usage shape: `landlock-exec --vfs-root <PATH> --write-dir <PATH> -- <program> [args...]`. Declines to run on non-Linux platforms.

### macOS: recoverable from git history

A complete macOS seatbelt sandbox *was* implemented and merged, then removed during a later refactor. Recovery is mechanical.

- **Landed:** commit `f4effa0a` (PR #165, "Sandbox compiler subprocesses with macOS seatbelt", 2026-02-10).
- **Removed:** commit `8ff3cca6` (PR #254, "Remote build support + perfetto traces") as part of the legacy-runner-protocol cleanup, not because the sandbox itself was wrong.
- **Path:** `crates/vx-runner/src/sandbox.rs` — `sandboxed_command()` wrapped invocations in `sandbox-exec -p <SBPL>`.
- **SBPL profile:** `(deny default)` + `(import "system.sb")`, exec from `VFS_ROOT` + `/usr/bin` + `/Library/Developer/CommandLineTools`, reads restricted to `VFS_ROOT`, writes restricted to `WRITE_DIR` + `/private/var/folders`, network denied entirely.

Recovery: `git show f4effa0a:crates/vx-runner/src/sandbox.rs > <new-path>` and re-wire against the current runner shape. The SBPL itself doesn't need redesign.

### Windows: open

No design captured. Candidates:
- **AppContainer + capability declarations.** Microsoft's primary sandbox mechanism; used by Edge/Chrome content processes. Capability-based file system access, integrity-level enforcement.
- **Restricted tokens + Job Objects.** Classic Chromium-era approach. More code; more flexible.
- **Process Mitigation Policies layered with the above** for defence-in-depth.

This is a real blocker for Tier 2 Windows support, since "cache everything" requires the sandbox to enforce the read-set bound. See [[Windows sandbox direction]].

## Why three different stacks

This is the deliberate choice, not a placeholder.

- Each OS has a kernel-blessed interception point. Using it gets us the OS's own performance guarantees, security boundary, and operational stability.
- A uniform user-space VFS would mean maintaining one filesystem implementation across three operating systems — and reinventing what the OS already does well — for the abstract benefit of code sharing. The shared *interface* lives at the `Vfs` trait; that's the only sharing that matters.
- Sandbox technology has even less cross-platform overlap. Landlock is a Linux LSM with no equivalent semantic on macOS or Windows; macOS's `sandbox-exec` and Windows AppContainer are different shapes again. Pretending otherwise leaks operational complexity into the wrong layer.

## Related

- [[Tier 2 — Vixen Build]] — the customer-facing surface this stack powers.
- [[Cache key + sandbox: one design doc?]] — the cache-key correctness side of the same problem.
- [[Declared identity collapses the read-set under trust]] — what the sandbox closure protects.
