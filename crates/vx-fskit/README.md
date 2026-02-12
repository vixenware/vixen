# vx-fskit

FSKit is a state machine from hell. This crate tames it.

Two public functions: `setup()` and `teardown()`. Both return a structured
log of everything they did, tried, and observed — even on success. On failure,
they suggest manual remediation steps.

## Architecture

VixenFS has three layers:

```
┌─────────────────────────────────────────────┐
│  kernel mount (what the user sees as files) │
├─────────────────────────────────────────────┤
│  VixenFS.appex (FSKit extension, Swift)     │
│  - sandboxed process managed by fskitd      │
│  - serves /.vixenfs/health.json locally     │
│  - forwards all other I/O over roam         │
├─────────────────────────────────────────────┤
│  vx-vfsd (Rust backend, roam connection)    │
│  - actually resolves file content from CAS  │
└─────────────────────────────────────────────┘
```

The extension can outlive the backend. The kernel mount can outlive the
extension. Each layer can be in a different state. That's why there are
intermediate states, not just "mounted" and "not mounted".

## Terminology

- The **app** (Vixen.app) is **present** or **absent**.
- The **extension** (VixenFS.appex) is **registered** or not, **enabled** or
  not, and **running** or not. These are independent.
- The **backend** (vx-vfsd) is **connected** or not (from the extension's
  perspective, via roam).

## What can go wrong

From least to most alarming:

1. **Stale mountpoint directory** — a previous mount left an empty directory
   behind. Harmless.

2. **Extension not registered** — the app was never launched, or was recently
   updated. The FSKit plugin isn't registered with the system.

3. **Extension not enabled** — the extension is registered but the user (or
   MDM) disabled it in System Settings.

4. **Extension enabled but mount fails** — the extension binary was rebuilt
   locally (signature/binary changed), and fskitd refuses to load it. Only
   killing fskitd + fskit_agent forces it to pick up the new binary.

5. **Backend not connected, extension running** — vx-vfsd crashed or was
   killed. The extension stays running on purpose (exiting would cause hangs,
   see #6), but all I/O returns errors. `health.json` reports
   `connected: false`.

6. **Extension not running, mount still visible** — the extension process
   exited but the kernel mount entry persists. Shows in `df`, `statfs` says
   vixenfs, but I/O hangs or returns EIO.

7. **Ghost mount** — not in `df`, `statfs` says local fs, but `mkdir`
   returns EBUSY. The kernel has a stale mount entry that userspace tools
   don't see. `diskutil unmount force` may respond with "Refusing to
   unmount (main Data volume)".

8. **fskitd wedged** — the FSKit daemon itself has a thread blocked in a VFS
   call that will never return. No mount or unmount operations complete.
   `diskutil` hangs.

9. **Uninterruptible sleep** — the extension is not running, but processes
   that were exec'd from files *inside the VFS* are now in uninterruptible
   sleep (state D), waiting on I/O that will never complete. `sudo killall
   fskitd fskit_agent` will not help. **Only a reboot will.**

## Probing

Probes that touch the mount path (`statfs`, `mkdir`, reading `health.json`)
can hang indefinitely if the VFS is zombied — the calling thread enters
uninterruptible sleep. These must run in a **child process** that we can
kill after a timeout, not in `spawn_blocking` (a hung thread can't be
cancelled).

Safe probes (never touch the mount path, always return quickly):

1. `df` output — does userspace list a vixenfs mount here?
2. `pgrep -x VixenFS` — is the extension process running?
3. `lsof -n -P -Fpcn` — are there processes with files open under the mount?

Unsafe probes (may hang, must run in a sacrificial child process with timeout):

4. `statfs(mount_path)` — does the kernel report a vixenfs filesystem?
5. `mkdir(mount_path)` — does it return EBUSY?
6. `read($mount/.vixenfs/health.json)` — is I/O working? Is the backend connected?

The safe probes run first. If they already give a clear picture (e.g. not in
`df` and no extension running), the unsafe probes can be skipped entirely.

The `lsof` probe also feeds into teardown: before unmounting, we must kill
processes with files open under the mount. If they survive SIGKILL (still
alive after 2s), they're in uninterruptible sleep — that's state #9, game
over, reboot required.

## setup() state machine

Goal: get from any state to state 2 (fully healthy mount).

```
START
  │
  ▼
Is Vixen.app installed at /Applications/Vixen.app?
  │
  NO ──→ FAIL: "install Vixen.app from <url>"
  │
  YES
  │
  ▼
Is Vixen.app codesign valid?
  │
  NO ──→ FAIL: "reinstall Vixen.app — codesign invalid"
  │
  YES
  │
  ▼
Is extension registered? (pluginkit -m)
  │
  NO ──→ FAIL: "launch Vixen.app to register extension"
  │
  YES
  │
  ▼
Is extension enabled? (vixenfs-mount --health --json)
  │
  NO ──→ FAIL: "enable in System Settings > General > Login Items & Extensions"
  │
  YES
  │
  ▼
Probe mount point (see probing above)
  │
  ├─ clean ──→ proceed to MOUNT
  ├─ stale dir ──→ proceed to MOUNT
  ├─ already healthy ──→ SUCCESS (already mounted)
  ├─ broken mount ──→ run teardown(), then proceed to MOUNT
  │
  ▼
MOUNT
  │
  Create mount point directory (mkdir -p)
  Call /sbin/mount -F -t vixenfs <source> <mount_path>
  │
  ▼
Probe again (with retries, extension startup takes time)
  │
  ├─ State 2 ──→ SUCCESS
  ├─ anything else ──→ FAIL: "sudo killall fskitd fskit_agent"
  │
  ▼
```

## teardown() state machine

Goal: get from any state to state 1 (clean, not mounted).

```
START
  │
  ▼
Probe mount point
  │
  ├─ State 1 (clean) ──→ SUCCESS (nothing to do)
  ├─ Stale dir ──→ rmdir, SUCCESS
  │
  ├─ some kind of mount exists
  │
  ▼
KILL OPEN FILE HOLDERS
  │
  lsof — find processes with files open under mount
  SIGKILL each one, wait up to 2s for exit
  │
  ├─ all exited ──→ proceed to UNMOUNT
  ├─ survivors ──→ FAIL: uninterruptible sleep, reboot required
  │
  ▼
UNMOUNT
  │
  diskutil unmount force <mount_path>
  wait up to 1s
  │
  ▼
Probe again
  │
  ├─ clean ──→ SUCCESS
  ├─ still mounted ──→ FAIL: "sudo killall fskitd fskit_agent"
  │
  ▼
```

## Return type

Both functions return the same type:

```rust
pub struct FsKitResult {
    /// Chronological log of everything that happened.
    /// Every probe, every command, every observation.
    pub log: Vec<LogEntry>,

    /// None on success, Some on failure with suggested remediation.
    pub error: Option<FsKitError>,
}

pub struct LogEntry {
    pub timestamp: jiff::Zoned,
    pub message: String,
}

pub struct FsKitError {
    /// What went wrong.
    pub summary: String,
    /// What the user can try manually.
    pub remediation: Vec<String>,
}
```

## health.json

The VixenFS extension serves `/.vixenfs/health.json` as a synthetic file.
It never hits the backend — the extension generates it locally. This is the
only reliable way to distinguish "extension alive, backend dead" from
"extension dead, mount is zombie".

```json
{"connected": true, "backend_pid": 1234, "uptime_secs": 300}
```

```json
{"connected": false, "disconnected_at": "2026-02-12T14:30:00Z"}
```
