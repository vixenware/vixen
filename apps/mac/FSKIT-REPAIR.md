# VixenFS / FSKit Repair Notes

This document records the local macOS FSKit lessons learned while making the
Vixen app install, diagnose, and repair its filesystem mount. Read this before
changing the VixenFS installer, app doctor, management helper, or release build
flow.

## Core Rules

- Do not manually register VixenFS with LaunchServices. In particular, do not
  use `lsregister -f`, `lsregister -R`, or an equivalent "add/register" command
  as a repair mechanism for the installed app. FSKit and PlugInKit need to own
  the identity that the user approves in System Settings.
- It is acceptable to use `lsregister -dump` for diagnostics and `lsregister -u`
  to remove stale records that point away from `/Applications/Vixen.app`.
- Do not use `pluginkit -e use` in the one-click mount repair path. That command
  can create or elect state that is not the same identity the user approved.
  Keep it out of the reset/mount transaction unless a future investigation
  proves a precise reason to use it.
- FSKit extension discovery and user enablement are separate facts. The bundled
  app extension should be discovered as part of the installed app; user
  enablement still happens in System Settings.
- The app should check whether VixenFS is present and enabled through FSKit's
  own API. If it is not present/enabled, the app should guide the user to System
  Settings. The normal doctor path should not enumerate PlugInKit or
  LaunchServices registrations before it has a failed mount to explain.
- Use `/sbin/mount -F -t vixenfs ...` for mounting. Do not route FSKit mounting
  through DiskArbitration or `diskutil`; Apple DTS guidance has repeatedly said
  the higher-level DiskArbitration paths do not cover the FSKit behavior we need.
- Do not advertise FSKit maintenance operations unless they are real and proven
  on the mount path. A stub `FSCheckOptionSyntax` /
  `FSManageableResourceMaintenanceOperations` implementation caused `/sbin/mount`
  to run `loadResource -> startCheckWithTask -> stop extension`, never reaching
  `activateWithOptions` or `mountWithOptions`.
- For unmounting, use the `umount` CLI in a sacrificial child. Do not use
  `diskutil unmount` and do not force-unmount FSKit mounts as part of ordinary
  repair.
- If `fskitd` itself must be restarted, use direct `kill(2)` against the
  `/usr/libexec/fskitd` PID. Do not use `launchctl kill` for this.
- Do not touch the VixenFS mounted tree during diagnosis unless the test is
  explicitly about the mounted filesystem. In particular, avoid `ls /Volumes`,
  `cd /Volumes/Vixen/vfs`, and similar path touches when the filesystem may be
  wedged.
- When root privileges are needed for diagnostics, batch the diagnostics into
  one script and ask for one password prompt. Repeated `sudo -v` attempts waste
  time and have failed repeatedly in this environment.

## Known Hanging Commands

When VixenFS is wedged, seemingly harmless commands can block because they touch
filesystem or process state that crosses the wedge.

Avoid these during live recovery unless there is a specific reason and the
command is run in a sacrificial process:

- `ls /Volumes` through user aliases or wrappers
- `cd /Volumes/Vixen/vfs`
- `ps`
- `pgrep`
- `pkill`
- `killall`

Prefer explicit tools and full paths:

- `/usr/bin/log show` / `/usr/bin/log stream`
- `/usr/bin/pluginkit -mAvvv -i eu.bearcove.vixenfs`
- `/sbin/mount` to read the mount table
- `/bin/kill` or direct `kill(2)` inside the helper for `fskitd`

## Working Manual Reset Shape

The reset script that produced a successful VixenFS mount was
`apps/mac/scripts/debug-simple-fskit-reset.sh`, run once under `sudo`.

The important sequence was:

1. Inspect the console user.
2. List user-visible and root-visible LaunchServices records for
   `eu.bearcove.vixen`.
3. List user-visible and root-visible PlugInKit records for
   `eu.bearcove.vixenfs`.
4. Remove LaunchServices app records whose path is not
   `/Applications/Vixen.app`.
5. Keep the installed root VixenFS appex record. Do not remove the installed
   `/Applications/Vixen.app/Contents/Extensions/VixenFS.appex` record just
   because LaunchServices reports it as link-disabled.
6. Kill `fskitd` through the installed management helper.
7. Mount through the installed management helper.

The known-good result from `/tmp/reset.txt` was:

```text
/dev/disk4 on /Volumes/Vixen/vfs (vixenfs, local, nodev, nosuid, noowners, noatime, fskit)
```

The app repair path should stay as close to this sequence as possible. If a new
step is added, it must have a clear reason and logging proving why it is needed.

One optimization is intentional: the app repair transaction tries the direct
mount first. If the mount works, it stops there and does not pay the
LaunchServices scan/cleanup cost. If the mount fails, it restarts FSKit and
tries the mount again before collecting recent FSKit logs. Only when the
persistent failure points at registration state does it fall back to the stale
record cleanup sequence above.

## Current App Repair Strategy

The GUI "Repair VixenFS" button runs:

```text
/Applications/Vixen.app/Contents/MacOS/vixen-management repair
```

through one administrator approval prompt. The helper then performs one
transaction:

1. Spawn a fresh helper process and run the direct `mount` command.
2. If the mount succeeds, stop immediately.
3. If the mount fails, restart `fskitd` with direct `kill(2)`.
4. Wait briefly for FSKit to come back.
5. Spawn a fresh helper process and retry the direct `mount` command.
6. If the retry succeeds, stop immediately.
7. If the retry fails, collect recent FSKit logs.
8. If the failure/logs point at stale registration state, clean stale
   containing-app LaunchServices records for the logged-in user and for root.
9. Restart `fskitd` again and run one final fresh-helper mount attempt.

The automatic setup, refresh, and "Open Settings" paths must not call
`repair-extension`. They can register the management daemon or open System
Settings, but extension repair is explicit diagnostics only.

`management register` and `management refresh` only manage the `SMAppService`
management daemon registration. They must not run LaunchServices or PlugInKit
cleanup as a side effect of setup.

Uninstall cleanup is explicit. The app menu action runs the app-side
`SMAppService` unregister call, then asks for administrator approval once and
runs:

```text
/Applications/Vixen.app/Contents/MacOS/vixen-management uninstall
```

That command unmounts VixenFS, detaches the mock backing device through the
existing unmount path, removes Vixen-owned root support state, and removes the
old legacy `/Library/LaunchDaemons` plist if present. It does not install,
register, or elect the FSKit extension.

The separate CLI command:

```text
/Applications/Vixen.app/Contents/MacOS/Vixen management repair-extension
```

still exists for explicit diagnostics/experiments, but it is not part of the
one-click mount repair path.

## Source Map

- `apps/mac/xcode/Vixen/VixenApp.swift`
  - SwiftUI onboarding and doctor UI.
  - The GUI repair button runs the elevated helper transaction.
  - The `management` CLI bridge lives here too.
- `apps/mac/xcode/Vixen/Doctor.swift`
  - Doctor checks for extension presence/enabled state, mount table state,
    status payload, and VFS/backend probe state.
- `crates/vx-management/src/repair.rs`
  - The one-click end-to-end repair transaction.
- `crates/vx-management/src/extension.rs`
  - LaunchServices and PlugInKit diagnostics/cleanup.
  - Must not create/register VixenFS identities.
- `crates/vx-management/src/killswitch.rs`
  - Direct `kill(2)` implementation for `/usr/libexec/fskitd`.
- `crates/vx-management/src/mount.rs`
  - Sparse mock block image, `hdiutil attach`, `/sbin/mount -F -t vixenfs`,
    sacrificial unmount, and backing-device detach.
- `crates/vx-management/src/uninstall.rs`
  - Explicit administrator-approved uninstall cleanup for the mount, mock image,
    management support directory, and legacy LaunchDaemon plist.
- `crates/vx-management/src/probe.rs`
  - VFS/backend probe that reads through the mounted filesystem when a mount
    exists.
- `crates/vx-fskit/src/macos/filesystem.rs`
  - FSKit probe implementation. For this block-backed filesystem,
    `probeResource` must return a usable result with stable container/volume
    identity. A recognized-only result is not mountable enough for this path.
  - Do not add check/format maintenance conformance back without a mount-path
    integration test showing the full `loadResource -> activateWithOptions ->
    mountWithOptions` sequence still happens.
- `apps/mac/scripts/debug-simple-fskit-reset.sh`
  - The manual reset oracle. Keep this script as the reference when comparing
    the app repair transaction.
- `apps/mac/scripts/clean-build-registration.sh`
  - Cleans Xcode/archive/export app registrations that local release builds
    create.
- `apps/mac/installer/scripts/preinstall`
  - Quits the running Vixen app before installation, escalating from graceful
    termination to `SIGTERM` and then `SIGKILL`, and records whether the app
    should be reopened.
- `apps/mac/installer/scripts/postinstall`
  - Ensures management queues, asks the installed app to register the
    management service, and reopens the app if preinstall recorded that it had
    been running.

## Xcode And Local Builds

Xcode archive/build products can be registered with LaunchServices as part of
the build/archive process. This is expected local-build behavior, not evidence
that `/Applications/Vixen.app` is wrong by itself.

Those build-product records are stale for release testing because they point at
paths such as:

- `apps/mac/xcode/build/DerivedData/.../InstallationBuildProductsLocation/Applications/Vixen.app`
- `apps/mac/dist/<version>/export/Vixen.app`
- mounted DMG paths under `/Volumes/dmg.*`

Release scripts should clean those registrations after archive/export. Repair
code may unregister stale app records, but it must not manually add/register the
installed app or extension.

## Diagnostics To Capture

Use full command paths.

```bash
/bin/cat "/Library/Application Support/Vixen/management/repair-status.json" 2>/dev/null || true
/bin/cat "/Library/Application Support/Vixen/management/last-action.json" 2>/dev/null || true
/usr/bin/tail -n 320 "/Library/Application Support/Vixen/management/logs/vixen-management.log"
/usr/bin/pluginkit -mAvvv -i eu.bearcove.vixenfs
/sbin/mount
```

Useful unified log predicates:

```bash
/usr/bin/log show --last 30m --style compact --predicate \
  'subsystem == "eu.bearcove.vixen" AND (category == "Management" OR category == "Doctor" OR category == "DoctorUI" OR category == "App")'

/usr/bin/log show --last 30m --style compact --predicate \
  '(process == "fskitd" OR process == "fskit_agent" OR process == "VixenFS" OR process == "diskarbitrationd") AND (eventMessage CONTAINS[c] "vixen" OR eventMessage CONTAINS[c] "vixenfs" OR eventMessage CONTAINS[c] "bearcove" OR eventMessage CONTAINS[c] "disabled" OR eventMessage CONTAINS[c] "mount")'
```

If root-visible PlugInKit/LaunchServices state is needed, create a single
diagnostic script and run it once with `sudo`; do not ask for a series of
interactive sudo prompts.

## Recent Failure Pattern

In the `0.0.100` app flow, the GUI did call the elevated helper directly, the
helper killed `fskitd` via direct PID signal, PlugInKit showed exactly one
current VixenFS record at `/Applications/Vixen.app`, and the fresh helper still
failed to mount with:

```text
Module eu.bearcove.vixenfs is disabled!
mount: Unable to invoke task
```

The app-side doctor saw the extension as present and enabled, while the mount
path still reported it disabled. Logs also showed `fskitd` and `fskit_agent`
talking about different-looking module identities during some attempts. Treat
that as evidence to preserve, not yet as a proven root cause.

The lesson from this failure is not "add more registration commands." The repair
path now tries the direct mount first, then falls back to the reset sequence only
after a real mount failure.

In the `0.0.101` flow, the GUI still had hidden automatic calls to
`repair-extension` in setup/open-settings paths. That was wrong: it could mutate
session-facing extension state before the user pressed the repair button, and it
made later failures harder to interpret. Automatic app paths must not run
`repair-extension`, and the normal doctor should not run a PlugInKit registration
enumeration before trying the cheap mount/restart path.
`repair-extension`.
