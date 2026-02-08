# FSKit ghost mount investigation

Date: 2026-02-08

## Summary

After a VFS session dies (FSKit extension disconnects), orphaned `rustc`/`ld`
processes that were executing from VFS-mounted binaries enter uninterruptible
wait (`U` state). These processes hold kernel references to files on the dead
mount, preventing `mount(2)` from reusing the mount point. The result is a
mount point that:

- `mount(2)` refuses with EBUSY (error 16)
- `umount` says "not currently mounted"
- `diskutil unmount force` tries to unmount the underlying Data volume (disk3s5)
- `rm -rf` fails with "Permission denied" on the directory
- `lsof +D` and `fuser` find nothing (they can't traverse into the dead mount)
- `lsof` (full scan) reveals the orphaned processes

Only a full `lsof | grep <mount_path>` finds the stuck processes. Targeted
tools (`lsof +D`, `fuser`) fail because they try to traverse the dead
filesystem.

## Reproduction

1. Run integration tests: `cargo nextest run -p vx`
2. Tests spawn shared daemons (vxd, vx-store, vx-runner)
3. vx-runner mounts VFS at `/tmp/.vx-test/nextest/home/vixen/runner`
4. Tests compile Rust code — rustc/ld execute from VFS-mounted binaries
5. Kill the test run mid-flight (or let it finish and kill daemons)
6. The FSKit extension session dies, TCP connection closes
7. rustc/ld processes that were mid-compilation are now stuck:
   - Their executable text segments are memory-mapped from VFS files
   - The VFS backend is gone — reads block forever
   - They enter `U` (uninterruptible wait) state, ppid=1 (orphaned)
8. Kernel holds a reference to the mount point because of these processes
9. Next mount attempt gets EBUSY from `mount(2)`

## Evidence from 2026-02-08

### fskitd logs show mount(2) returning EBUSY

Three consecutive mount attempts, all failing identically:

```
15:40:58.349 E fskitd: mount(2) error: 16
15:42:08.539 E fskitd: mount(2) error: 16
15:43:18.849 E fskitd: mount(2) error: 16
```

Each time, the full FSKit lifecycle completes successfully (extension launches,
activates volume, gets endpoint, accepts connection) — then `mount(2)` fails
and everything tears down.

### Orphaned processes holding mount references

```
$ lsof 2>/dev/null | grep '/tmp/.vx-test'

rustc  10041 amos txt 0000 0,0 0 0 /private/tmp/.vx-test/nextest/home/vixen/runner/tc-.../bin/rustc
rustc  10042 amos txt 0000 0,0 0 0 /private/tmp/.vx-test/nextest/home/vixen/runner/tc-.../bin/rustc
...
ld     10136 amos txt 0000 0,0 0 0 /private/tmp/.vx-test/nextest/home/vixen/runner/build-.../out/app...rcgu.o
ld     10137 amos txt 0000 0,0 0 0 /private/tmp/.vx-test/nextest/home/vixen/runner/build-.../out/app...rcgu.o
...
```

12 rustc processes, 5 ld processes. All:
- ppid=1 (orphaned — original parent died)
- State `U` (uninterruptible wait)
- Running for 35+ minutes
- Have VFS files mapped as `txt` (executable text segments)

### `diskutil unmount force` is the wrong tool

```
$ diskutil unmount force /tmp/.vx-test/nextest/home/vixen/runner
Volume Data on disk3s5 failed to unmount
```

This resolves the path through to the underlying APFS Data volume and tries to
unmount **that** (the system volume). FSKit mounts are not APFS volumes —
`diskutil` is not the right tool.

### Standard unmount tools don't see it

```
$ umount /tmp/.vx-test/nextest/home/vixen/runner
umount: not currently mounted

$ fuser /tmp/.vx-test/nextest/home/vixen/runner
(empty)

$ lsof +D /tmp/.vx-test/nextest/home/vixen/runner
(empty)
```

### But there IS ghost kernel state

```
$ diskutil unmount force /tmp/.vx-test/nextest/home/vixen/runner
Volume Data on disk3s5 failed to unmount    # <-- sees a volume

$ diskutil unmount force /tmp/nonexistent
Unmount failed for /tmp/nonexistent         # <-- different error = different state
```

## What doesn't fix it

| Action | Result |
|--------|--------|
| `sudo killall fskitd fskit_agent` | Ghost state persists |
| `diskutil unmount force <path>` | Tries to unmount system Data volume |
| `umount <path>` | "not currently mounted" |
| Killing vx-runner/vx-store/vxd daemons | Ghost state persists |
| `rm -rf` on parent directory | Permission denied on mount point |

## What fixes it

Killing the orphaned processes releases the mount references:

- `kill <pid>` works for the rustc processes (regular signal delivery)
- `kill -9 <pid>` needed for the ld processes (SIGKILL required)

After all orphans are killed, check for Finder too — Finder browsing the
mount point directory also holds a reference that prevents `mount(2)`:

```
Finder 62100 amos 13r DIR 1,13 96 369929873 /private/tmp/.vx-test/nextest/home/vixen
```

Closing the Finder window (or killing Finder's fd) releases the last reference.

**Discovery recipe:**
```
lsof 2>/dev/null | grep '<mount_path>'
```
Then kill everything found. `lsof +D` and `fuser` will NOT find them.

**This is necessary but not sufficient.** Even after killing all processes and
closing all file descriptors (lsof shows nothing), the kernel mount table can
retain ghost state from the dead FSKit mount. In this state:

- `mount(2)` returns EBUSY
- `umount` / `sudo umount -f` says "not currently mounted"
- `diskutil unmount force` tries to unmount the underlying APFS volume

No userspace API can observe or clear this state. **Reboot is the only
known fix** once the ghost mount is orphaned in the kernel.

## Web search findings

### FSKit is known-buggy — Apple DTS acknowledges it

From [Apple Developer Forums thread 788609](https://developer.apple.com/forums/thread/788609)
(June–July 2025), DTS Engineer Kevin Elliott:

> "Unfortunately, more bugs have been found so you're going to need to
> wait for more fixes. Thanks for your patience and I'll try and let you
> know when you should test again."

The thread documents multiple FSKit issues:
- `mount -F` as root fails ("Unable to invoke task") — the module is only
  visible to the user who enabled it in System Settings
- Permission errors (EPERM/code 13) when mounting real (non-RAM) disks
- A DiskArbitration bug preventing FSKit volumes from working correctly
- KEXT drivers always probe before FSKit modules, blocking third-party
  FSKit modules for filesystem types that have a built-in KEXT

### Other findings

- [macFUSE issue #793](https://github.com/macfuse/macfuse/issues/793) describes
  a similar ghost mount issue with FUSE (mount hangs irrevocably, blocks shell).
- FSKit documentation is sparse. The
  [fskitd(8) man page](https://keith.github.io/xcode-man-pages/fskitd.8.html)
  says almost nothing about cleanup or error handling.
- `mount -F` forces the filesystem type to be treated as an FSKit FSModule.
- The `/Volumes`-only restriction claim from search summaries is **unverified** —
  Apple's own [FSKitSample](https://github.com/KhaosT/FSKitSample) mounts at
  `/tmp/TestVol` using `mount -F`.

## Root cause analysis

The chain of failure:

1. **vx-runner spawns rustc/ld via `posix_spawn`** — these processes execute
   binaries that live on the VFS mount and memory-map `.rlib` files from it
2. **VFS session dies** — FSKit extension terminates, TCP connection closes,
   Rust backend stops serving reads
3. **rustc/ld block on VFS I/O** — they're mid-read on files that no longer
   have a backend. The kernel puts them in uninterruptible wait.
4. **Processes become unkillable orphans** — ppid=1, state U, holding file
   references to the dead mount
5. **mount(2) returns EBUSY** — kernel won't mount on a path that still has
   active references from the dead mount

## Recommendations

### Short term: kill children before letting VFS die

When vx-runner detects VFS death (the new `vfs_dead` watch channel), it should
kill all child processes that were spawned into VFS prefixes before they enter
uninterruptible wait. If the children are killed promptly (while the VFS
backend is still draining), they'll exit cleanly and release their file
references.

### Medium term: process group tracking

vx-runner should track all spawned rustc/ld processes (or use process groups)
so it can `kill -9` the entire group when the VFS dies. This is defense in
depth — even if the graceful kill races with the VFS teardown.

### Medium term: try mounting under /Volumes

Unverified claims suggest FSKit works better with `/Volumes` mount paths.
Worth testing whether ghost mount state still occurs when mounting at
`/Volumes/.vixen-runner` instead of `/tmp/...` or `~/.vx/...`.

### Long term: file an FSKit bug with Apple

Multiple issues to report:

1. **Ghost mount state**: a dead FSKit mount creates kernel state that no
   userspace tool can observe or clear. `mount` doesn't show it, `umount`
   says "not currently mounted", but `mount(2)` returns EBUSY. Only a
   reboot clears it.

2. **Processes blocked on dead mount get stuck forever**: rustc/ld in `U`
   state, blocked on VFS I/O that will never complete. They should receive
   EIO when the FSKit extension dies, not block indefinitely.

3. **`lsof +D` and `fuser` can't find references**: only a full `lsof` scan
   reveals processes holding dead-mount files. Targeted tools fail because
   they can't traverse the dead filesystem.

4. **No FSKit-specific unmount API**: `diskutil unmount` resolves to the
   underlying APFS volume. There's no tool to unmount a dead FSKit mount.
