# FUSE Cleanup Strategy

## Problem
vx-runner spawns vx-vfsd which mounts FUSE. If vx-vfsd crashes or is killed, the mount point becomes stuck (Transport endpoint is not connected). This blocks subsequent mount attempts.

## Detection Strategy

### 1. Check if mount path exists in /proc/mounts
```bash
grep "path_to_mount" /proc/mounts
```
Should show type `fuse` if mounted.

### 2. Try to stat the mount point
```bash
stat /path/to/mount
```
If returns errno ENOTCONN (107 "Transport endpoint is not connected"), it's stuck.

### 3. Verify FUSE daemon is dead
Check if any process is actively serving the FUSE mount.

## Cleanup Escalation Strategy

Try each step in order. If one succeeds, stop. If one fails, escalate to next.

### Level 1: Gentle unmount
```bash
fusermount3 -u /path/to/mount
```
Tries normal unmount. Only works if FUSE daemon is responsive or cleanly exited.

### Level 2: Lazy unmount
```bash
fusermount3 -uz /path/to/mount
```
Detaches the mount immediately, cleanup happens when no longer in use. More forceful.

### Level 3: Nuclear option
```bash
# First: abort all FUSE connections
for conn in /sys/fs/fuse/connections/*/abort; do
  [ -e "$conn" ] && echo 1 | sudo tee "$conn" >/dev/null
done

# Then: unmount
fusermount3 -uz /path/to/mount
```
Forces FUSE kernel driver to abort, then unmounts.

## Implementation Plan

### On entry (before spawning vx-vfsd)
1. Check if mount path is stuck (detection strategy above)
2. If stuck and matches our pattern (`*/vixen/runner`), try cleanup escalation
3. Only proceed if cleanup succeeds or mount point is clean
4. Log all cleanup actions
5. Surface clear errors if cleanup fails

### On exit (vx-vfsd shutdown)
- Use Drop guard or signal handler to unmount cleanly
- Ensures we don't leave stuck mounts behind

## Current Experiment

Testing stuck mount at: `/tmp/.vx-test/nextest/home/vixen/runner`

Symptoms:
```
df: /tmp/.vx-test/nextest/home/vixen/runner: Transport endpoint is not connected
```

### Step 1: Detection

**Check /proc/mounts:**
```bash
$ grep "/tmp/.vx-test/nextest/home/vixen/runner" /proc/mounts
vixenfs /tmp/.vx-test/nextest/home/vixen/runner fuse rw,nosuid,nodev,relatime,user_id=501,group_id=501 0 0
```
✅ Mount exists, type=fuse

**Try to stat:**
```bash
$ stat /tmp/.vx-test/nextest/home/vixen/runner
stat: cannot stat '/tmp/.vx-test/nextest/home/vixen/runner': Transport endpoint is not connected (os error 107)
```
✅ Returns ENOTCONN (errno 107) - confirms stuck mount

### Step 2: Cleanup - Level 1 (Gentle)

**Try gentle unmount:**
```bash
$ fusermount3 -u /tmp/.vx-test/nextest/home/vixen/runner
$ echo $?
0
```
✅ SUCCESS! Exit code 0

**Verify cleanup:**
```bash
$ grep "/tmp/.vx-test/nextest/home/vixen/runner" /proc/mounts
$ echo $?
1
```
✅ Mount is gone from /proc/mounts

## Findings

**Surprising result:** The gentle unmount (`fusermount3 -u`) works even on stuck mounts with ENOTCONN. We don't need the nuclear option in this case.

**Recommendation:** Try levels in order, most cases will succeed at Level 1.

## Implementation Status

✅ **Implemented in vx-runner** (`cleanup_stuck_mount` function in `main.rs`)

Before spawning vx-vfsd, the function:
1. Checks `/proc/mounts` for the mount path
2. Tries to `stat()` the path, detects ENOTCONN (errno 107) for stuck mounts
3. Verifies path matches pattern `*/vixen/runner` for safety
4. Escalates through cleanup levels:
   - Level 1: `fusermount3 -u` (gentle)
   - Level 2: `fusermount3 -uz` (lazy)
   - Level 3: Abort FUSE connections via sysfs, then unmount
5. Logs all cleanup attempts
6. Returns error if all attempts fail

The cleanup runs automatically before every vx-vfsd spawn, ensuring clean state.
