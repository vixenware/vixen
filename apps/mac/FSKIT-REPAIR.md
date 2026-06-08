# VixenFS / FSKit Notes

This is the working memory for the macOS app, installer, VixenFS extension, and
management helper. Read this before touching `apps/mac`, `crates/vx-management`,
or `crates/vx-fskit`.

The current goal is deliberately small: prove that a freshly copied
`/Applications/Vixen.app` can have its bundled VixenFS extension discovered by
macOS, enabled by the user in System Settings, and mounted by the installed
`vixen-management` helper. Do not expand the repair architecture until that
path is proven.

## BREAKTHROUGH 2026-06-08: the mount must run entirely in the user context

Everything below this section that talks about "root service runs the mount via
`launchctl asuser`" is now **falsified**. The real decisive condition is the
**owner of the `/dev/diskN` block device node**, not the audit session, not
`bsexec`, not `asuser`.

Proven matrix (all run by hand, back to back, single login session):

```text
mount -F -t vixenfs /dev/disk4 ...               (user, device root-owned)  → "Probing resource ... Permission denied" / Unable to invoke task
sudo mount -F -t vixenfs /dev/disk4 ...           (root)                     → "Unable to invoke task"
sudo launchctl bsexec <pid> /sbin/mount ...                                  → "Unable to invoke task"
sudo launchctl bsexec <pid> launchctl asuser 501 /sbin/mount ...             → "Failed to get user context: 141: Reentrancy avoided"
sudo launchctl asuser 501 /sbin/mount ...         (from terminal)            → WORKS
daemon: launchctl asuser 501 /sbin/mount ...      (from SMAppService daemon) → "Module ... is disabled"
```

Then the actual fix was found:

```text
# device attached by ROOT → /dev/disk4 owned by root → every mount attempt fails
# even after: sudo chown amos /Volumes/Vixen/vfs  AND  sudo chown amos vixen-volume.img
#   → still "Probing resource ... Permission denied"

hdiutil detach /dev/disk4
hdiutil attach -imagekey diskimage-class=CRawDiskImage -nomount \
  "/Library/Application Support/Vixen/vixen-volume.img"      # run AS THE USER (no sudo)
ls -l /dev/disk4        # brw-r----- 1 amos ...  ← device node now owned by amos
mount -F -t vixenfs /dev/disk4 /Volumes/Vixen/vfs            # AS THE USER, no sudo → WORKS
```

Conclusions:

- The FSKit extension runs as the **user** (via `fskit_agent`). It must be able
  to `open(2)` the `/dev/diskN` node. A root-owned node → "Probing resource:
  Permission denied". **`hdiutil attach` must be run by the user** so the device
  node is user-owned. chowning the *image file* or *mountpoint* afterward is not
  enough — the device node owner is set at attach time.
- With a user-owned device node, the mount needs **no root at all**:
  `mount -F -t vixenfs /dev/diskN <mountpoint>` works as the plain user.
- The **mountpoint may be root-owned**. `sudo mkdir /Volumes/ha` then a non-root
  `mount` onto it worked fine. So `/Volumes/Vixen/vfs` being root-created is OK.
- **Unmount** is also user-side: whoever mounted it can `umount` without sudo. A
  root-mounted volume needs `sudo umount`. So if the user mounts, the user
  unmounts — no privilege needed.
- This explains the daemon "Module disabled" failure: the daemon attached the
  image as root, producing a root-owned device node, so the user's FSKit
  extension could never probe it regardless of the `asuser` trampoline.

New intended design (replaces the asuser-trampoline shape):

- **Root / daemon does only what genuinely needs root, one time:**
  - create `/Volumes/Vixen` and `/Volumes/Vixen/vfs` (because `/Volumes` is
    root-owned and the user cannot `mkdir` there);
  - ensure the backing image file exists and is **user-accessible** (owned by /
    readable+writable by the active console user) so the user can `hdiutil
    attach` it.
- **User-side app / LaunchAgent does the mount lifecycle (no password):**
  - `hdiutil attach -nomount` → user-owned device node;
  - `mount -F -t vixenfs /dev/diskN /Volumes/Vixen/vfs`;
  - `umount` on teardown.

Open question to settle next: where the image lives and how the user gets write
access to it (user-owned file in `/Library/Application Support/Vixen`, an ACL,
or relocating the image under a user-writable path). The mountpoint creation
under `/Volumes` is the only hard root requirement that remains.

## FINAL ARCHITECTURE (agreed 2026-06-08): no daemon, no PKG, DMG-only

Decision: rip out SMAppService, the management queue, the privileged
`vixen-management` daemon, and the PKG. Ship a **DMG** (brew cask friendly).

Verified facts that make this safe:

- The main Vixen app is **not sandboxed** (`Vixen.entitlements` has no
  `com.apple.security.app-sandbox`; the `temporary-exception.mach-lookup` keys
  are sandbox-only and inert). So the app may spawn `hdiutil` and `/sbin/mount`
  directly.
- A non-root user **can** `umount` only a filesystem mounted on a directory
  **they own** — so the mountpoint must be chowned to the user (not just
  created). That, plus the `mkdir` under root-owned `/Volumes`, is the entire
  privileged surface.
- `/Volumes/Vixen/vfs` stays the mountpoint: FSKit docs say block devices must
  mount under `/Volumes` ("random things break" otherwise — and they break even
  when we follow the docs), and it's the stable CAS base for every file path.

The one-time privileged op (single admin prompt at first run, e.g. AppleScript
`do shell script … with administrator privileges`):

```sh
mkdir -p /Volumes/Vixen/vfs && chown <console-uid> /Volumes/Vixen/vfs
```

Self-healing first-run check: on launch, if `/Volumes/Vixen/vfs` is missing or
not owned by the console user, prompt once to (re)create+chown it. If a
hand-created `/Volumes/Vixen` survives reboot, the prompt is seen exactly once;
if `diskarbitrationd` reaps it, it reappears after reboot. Never per-mount.

Everything else is user-side and passwordless:

- Backing image relocates to a **user-owned** path:
  `~/Library/Application Support/Vixen/vixen-volume.img` (plain user dir — avoids
  Group Containers entitlement subtleties; the image is just backing bytes, not
  the extension's socket).
- `hdiutil attach -nomount` run **as the user** → user-owned `/dev/diskN`.
- `mount -F -t vixenfs /dev/diskN /Volumes/Vixen/vfs` as the user.
- `umount` as the user on teardown.

Implementation sequence (preserve the oracle — prove before deleting):

1. Make the mount primitive run fully user-side: relocate the image to the
   user path, drop all `launchctl asuser`/`bsexec`/root branches, plain
   `hdiutil` + `/sbin/mount`. Test by hand as the user (no sudo) — this is the
   new oracle.
2. Add the one-time admin prompt (mkdir + chown the mountpoint) on the Swift
   first-run path.
3. Wire the app to spawn the user-side mount binary directly (no queue, no
   SMAppService).
4. Delete: `queue.rs`, launchd mode in `main.rs`, `Button::execute_from_launchd`,
   `killswitch` daemon coupling, the SMAppService `ManagementService` enum in
   `VixenApp.swift`, `installer/eu.bearcove.vixen.management.bundle.plist`,
   `installer/scripts/postinstall` daemon setup, `scripts/dist-pkg.sh`, and the
   `pkg*` Justfile targets.
5. DMG + brew cask for distribution.

Keep as user-side helpers: `mount.rs` primitives (attach/mount/umount/detach),
`extension.rs` (diagnostics), `probe.rs` (health check), `session.rs`
(console-user lookup, used to find the uid to chown to).

## Current Ground Truth

- VixenFS is an FSKit app extension bundled at:

  ```text
  /Applications/Vixen.app/Contents/Extensions/VixenFS.appex
  ```

- Copying the app into `/Applications` is not enough for System Settings to show
  the extension. Opening the containing app is what causes macOS to discover the
  bundled extension through the normal LaunchServices / PlugInKit app-extension
  path.
- Once the containing app has been opened, `pluginkit -mAvvv -i
  eu.bearcove.vixenfs` should show one record pointing at the installed app.
  Example known-good shape:

  ```text
  +    eu.bearcove.vixenfs(0.0.105)
              Path = /Applications/Vixen.app/Contents/Extensions/VixenFS.appex
         Parent Bundle = /Applications/Vixen.app
              SDK = com.apple.fskit.fsmodule
  ```

- User enablement is still a System Settings action. The app may open the right
  Settings page and explain what to enable. It must not secretly mutate
  extension registration while doing that.
- The clean manual root mount worked. That proves the installed app bundle,
  FSKit extension, entitlements, user enablement, and plain FSKit mount path are
  fundamentally valid. This is the baseline we are replacing one step at a
  time:

  ```sh
  sudo su
  mkdir -p "/Library/Application Support/Vixen" /Volumes/Vixen/vfs
  rm -f "/Library/Application Support/Vixen/vixen-volume.img"
  mkfile -n 100m "/Library/Application Support/Vixen/vixen-volume.img"
  hdiutil attach -imagekey diskimage-class=CRawDiskImage -nomount \
    "/Library/Application Support/Vixen/vixen-volume.img"
  mount -F -t vixenfs /dev/diskN /Volumes/Vixen/vfs/
  mount | rg -i 'vixen'
  exit
  ```

  Expected mount-table result:

  ```text
  /dev/diskN on /Volumes/Vixen/vfs (vixenfs, local, nodev, nosuid, noowners, noatime, fskit)
  ```

- The first replacement step after the manual mount is not stale-record cleanup
  or a VFS provider probe. It is:

  ```sh
  sudo /Applications/Vixen.app/Contents/MacOS/vixen-management mount
  ```

  from a manually installed DMG app after the extension is visible and enabled.
  That direct helper path has worked.
- **FALSIFIED (see BREAKTHROUGH section above).** The old belief was that the
  root service attaches the image and runs the mount leaf via
  `launchctl asuser "$CONSOLE_UID" /sbin/mount ...`. This only ever worked from a
  terminal (login audit session) and fails from the SMAppService daemon with
  "Module disabled" — because the daemon-attached device node is root-owned and
  the user's FSKit extension cannot open it. The real fix is to attach **and**
  mount in the user context so the device node is user-owned. Kept here only to
  warn against re-deriving the dead end.

## Manual Baseline From A Clean DMG Install

This is the path that worked and should remain the oracle while replacing one
manual step at a time.

1. Uninstall/clean up any existing Vixen app and mount state.
2. Open the DMG and drag `Vixen.app` to `/Applications`.
3. Open `/Applications/Vixen.app` once.
4. Confirm macOS has discovered the extension if needed:

   ```sh
   pluginkit -mAvvv -i eu.bearcove.vixenfs
   ```

   There should be exactly one record, and it should point at
   `/Applications/Vixen.app/Contents/Extensions/VixenFS.appex`.

5. In System Settings, enable VixenFS.
6. Establish the mount manually as root:

   ```sh
   sudo su
   mkdir -p "/Library/Application Support/Vixen" /Volumes/Vixen/vfs
   rm -f "/Library/Application Support/Vixen/vixen-volume.img"
   mkfile -n 100m "/Library/Application Support/Vixen/vixen-volume.img"
   hdiutil attach -imagekey diskimage-class=CRawDiskImage -nomount \
     "/Library/Application Support/Vixen/vixen-volume.img"
   mount -F -t vixenfs /dev/diskN /Volumes/Vixen/vfs/
   mount | rg -i 'vixen'
   exit
   ```

## Current Product Repair Path

After the clean install, app open, System Settings enablement, and management
item approval, the app repair button should fire the root-service `mount`
queue. The service then performs the same primitive as:

   ```sh
   sudo /Applications/Vixen.app/Contents/MacOS/vixen-management mount
   ```

Then check the result without touching arbitrary paths:

   ```sh
   mount | rg -i vixen
   cat /Volumes/Vixen/vfs/.vixenfs/status.json
   ```

The app UI should not ask for an AppleScript administrator password here. The
already-approved management service owns the privileged work.

If this fails, capture the helper log and FSKit logs before changing anything:

```sh
/usr/bin/tail -n 240 "/Library/Application Support/Vixen/management/logs/vixen-management.log" 2>/dev/null || true
/usr/bin/log show --last 10m --style compact --predicate \
  '(process == "fskitd" OR process == "fskit_agent" OR process == "VixenFS") AND (eventMessage CONTAINS[c] "vixen" OR eventMessage CONTAINS[c] "vixenfs" OR eventMessage CONTAINS[c] "bearcove" OR eventMessage CONTAINS[c] "disabled" OR eventMessage CONTAINS[c] "mount")'
```

## Installer Discipline

The pkg installer must preserve the same shape as the DMG path: one containing
app bundle at `/Applications/Vixen.app`. It must not package
`VixenFS.appex` as its own component. FSKit extension discovery comes from
opening the containing app, and FSKit enablement comes from System Settings.

`apps/mac/scripts/dist-pkg.sh` deletes the generated component plist's
`ChildBundles` key before `pkgbuild` runs. The intended component metadata names
only:

```text
Applications/Vixen.app
```

The installer may prepare management support directories and ask macOS to know
about the background management item, but that is separate from FSKit extension
discovery and enablement.

## Minimal Intended Product Shape

- The first-run app screen handles only FSKit extension discovery and user
  enablement.
- The app can diagnose without privileges:
  - whether PlugInKit sees the installed VixenFS extension,
  - whether System Settings reports it enabled through the app's FSKit-facing
    state,
  - whether `/sbin/mount` shows `/Volumes/Vixen/vfs`,
  - whether `/.vixenfs/status.json` is readable when mounted.
- The first repair operation should be the smallest privileged primitive,
  queued to the approved management service:

  ```text
  vixen-management mount
  ```

  The service creates/attaches the image as root and runs only `/sbin/mount`
  via `launchctl asuser <console_uid>`.

- If direct helper mount fails with stale FSKit state, the next privileged
  primitive is direct `kill(2)` of `/usr/libexec/fskitd`, then `mount` again.
- Anything more expensive or more invasive needs evidence from the failed mount
  and logs before it runs.
- The app's VFS-provider/backend probe is not the mount oracle. The mount oracle
  is `/sbin/mount -F -t vixenfs` plus the status file. Real VFS provider routing
  should be tested through the real VFS provider path, not a synthetic probe
  that may not match the prefix-routing design.

## What We Learned The Hard Way

- A stuck FSKit mount can poison ordinary commands. Avoid touching the mounted
  tree unless the test is specifically about the mounted filesystem.
- `/bin/ls` may be safe when shell aliases are not. User aliases can route to
  tools that touch more filesystem state.
- `ps`, `pgrep`, `pkill`, and `killall` can hang while FSKit is wedged. Do not
  build recovery around them.
- `log show` and `log stream` stayed usable during the wedge and are safe first
  diagnostics.
- There can be more than one `VixenFS` process. This usually reflects
  ExtensionKit / `fskit_agent` state, not proof that the app intentionally
  launched duplicate filesystems.
- Killing `fskitd` can unwedge the system, but it affects all FSKit modules and
  must be the last-resort primitive, not the first button.
- Mounting at `/Volumes/Vixen/vfs` reduces blast radius compared with mounting
  directly at `/Volumes/Vixen`.
- `mkfile` is `/usr/sbin/mkfile`, not `/usr/bin/mkfile`.
- macOS app-extension discovery is normal: install/copy the app, open the app,
  then System Settings can show the extension.

## Do Not Do This

- Do not manually register the installed Vixen app or VixenFS extension as a
  repair mechanism. In particular, do not use:
  - `lsregister -f`
  - `lsregister -R`
  - `lsregister -trusted`
  - `pluginkit -a`
  - `pluginkit -e use`
- Do not call those commands automatically from app startup, "Open Settings",
  doctor refresh, installer postinstall, or one-click repair.
- `lsregister -dump`, `lsregister -u`, and `pluginkit -mAvvv` are diagnostic or
  cleanup tools only. They are not product install APIs.
- Do not use Disk Utility, DiskArbitration, or `diskutil unmount` for FSKit
  mount/unmount recovery. The supported path we are using is the `mount` CLI and
  the `umount` CLI in a sacrificial child where needed.
- Do not use `launchctl kill` to restart `fskitd`. Use direct `kill(2)` against
  the `/usr/libexec/fskitd` PID when the last-resort kill switch is needed.
- Do not add FSKit check/format/maintenance metadata or access-check protocol
  conformance unless there is a proven implementation and an integration test
  showing that `mount` still reaches the real FSKit mount path. A stub check
  path caused `mount` to enter check/stop behavior instead of mounting, and
  access-check conformance is unnecessary for the current mount oracle.
- Do not run expensive LaunchServices scans before trying the cheap mount
  primitive. The slow registration scan is not a first-line repair.
- Do not hide privileged work behind vague UI. The app repair button should say
  it is asking Vixen Management to repair VixenFS.
- Do not ask for password prompts for ordinary repair when the management
  service is already installed and approved.
- Do not move app doctor logic into the root service. The service owns fixed
  privileged primitives; the app owns diagnosis and user flow.
- Do not treat "Backend Connection" as an install failure when the mount and
  status file are healthy. That is a separate VFS-provider/routing question.

## Useful Files

- `apps/mac/xcode/Vixen/VixenApp.swift`
  - first-run UI, doctor UI, and the `Vixen management ...` CLI bridge.
- `apps/mac/xcode/Vixen/Doctor.swift`
  - non-privileged diagnostics.
- `apps/mac/xcode/VixenFS/RustFSKitBootstrap.swift`
  - app-group path and FSKit Rust bootstrap.
- `crates/vx-management/src/mount.rs`
  - mock image creation, `hdiutil attach`, and `/sbin/mount -F -t vixenfs`.
- `crates/vx-management/src/killswitch.rs`
  - direct `fskitd` kill switch.
- `crates/vx-management/src/repair.rs`
  - current overbuilt repair transaction. Treat with suspicion until the direct
    helper mount test is proven.
- `crates/vx-management/src/extension.rs`
  - diagnostic/stale-record cleanup. Must not register or elect extensions.
- `crates/vx-management/src/probe.rs`
  - synthetic VFS probe. Not the current mount oracle.
- `apps/mac/installer/scripts/preinstall`
  - installer app-quit behavior.
- `apps/mac/installer/scripts/postinstall`
  - installer management-service setup. Must not register/elect VixenFS.
