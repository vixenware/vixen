# FUSE Hang Debugging Guide

## Background

vx-runner serves a FUSE filesystem (`vixenfs`) for hermetic builds. The FUSE
server runs on a **dedicated thread** with a **single-threaded tokio runtime**
(`tokio::runtime::Builder::new_current_thread()`) inside the same process as the
main multi-threaded tokio runtime. See `crates/vx-vfs/src/linux.rs`.

Child processes (rustc, zig) are spawned with `Command::new(...).output().await`
and do `chdir` + `execve` through the FUSE mount. Rust's `Command::spawn()` uses
`clone3(CLONE_VFORK)` — the parent thread blocks until the child calls `execve`.

## Architecture

```
vx-runner process
├── Main tokio runtime (multi-threaded)
│   ├── RPC server (roam) — receives compile_rust requests
│   ├── compile_rust tasks — populate VFS, spawn rustc, collect output
│   └── CAS client (roam) — talks to vx-store
├── FUSE runtime (current_thread, dedicated OS thread "fuse-runtime")
│   ├── Session loop — reads /dev/fuse, dispatches handlers
│   └── Handler tasks — serve lookup/read/write/etc by calling CasVfs
└── Shared state
    └── CasVfs
        ├── items: DiagnosticRwLock<HashMap<ItemId, VfsItem>>
        ├── prefixes: DiagnosticRwLock<HashMap<String, ActionPrefix>>
        └── blob_cache: DiagnosticMutex<HashMap<...>>
```

The `items` RwLock is accessed from **both runtimes**:
- FUSE handlers (on the FUSE current_thread runtime) acquire `items.read()`
- compile_rust tasks (on the main multi-threaded runtime) acquire
  `items.write()` to populate the VFS, then `items.read()` indirectly when
  rustc reads from the mount

## The Hang: Current Understanding

### Symptom

With 8 concurrent compilations, the system deadlocks within seconds. All 8
compile_rust tasks block trying to acquire `items` (mix of read and write),
and FUSE handlers also block on `items.read()`. Meanwhile, the FUSE session
loop can't serve kernel requests, so rustc processes (which read from the
mount) also hang.

### Diagnostic Infrastructure

We built instrumented lock wrappers (`vx-locks` crate) that track:

- **Waiters**: who is blocked trying to acquire, with backtrace + tokio task ID
- **Holders**: who currently holds the lock, with backtrace + tokio task ID
- **Acquire/release counters**: total lifetime acquires and releases
- **Cancellation safety**: `WaiterGuard` ensures waiter entries are cleaned up
  if the future is cancelled; `promote_waiter_to_holder` atomically transitions
  from waiter to holder (locks both lists under one critical section) so
  `dump_lock_diagnostics()` never sees a gap

Also wired into vx-runner's `SIGUSR1` handler: dumps FUSE telemetry (in-flight
ops, last 256 completed), roam RPC diagnostics, and lock diagnostics.

### Key Finding: "No Holder" Deadlock

Across multiple reproductions, the lock diagnostics consistently show:

```
Lock "CasVfs::items" (acquires=14842, releases=14842, holding=0):
  Holders: (none)
  Waiters (8):
    [write] task=23576 waiting for 30.037s
    [write] task=23570 waiting for 30.359s
    [read]  task=23573 waiting for 30.359s
    [read]  task=23575 waiting for 30.360s
    ...
```

- **acquires == releases** — every acquisition was matched by a release
- **holding == 0** — no guard exists anywhere
- **8 waiters blocked for 30s** — all waiting on `.await` for the tokio RwLock
- **No tracking mismatch** — the counters confirm our instrumentation is correct

This means the `tokio::sync::RwLock`'s internal semaphore believes the lock is
held, but no guard exists. The semaphore's permits were not returned even though
our `Drop` impl (which calls `remove_holder`) did run (proven by the balanced
counters).

### Roam/RPC State During Hang

```
[client] age=37.8s total_completed=102
  In-flight: 0⬆ 0⬇
```

Zero in-flight RPCs. The handlers are stuck **before** any CAS call — they
can't even acquire the `items` lock.

### FUSE State During Hang

- 100-150 in-flight FUSE operations, almost all FORGET/BATCH_FORGET
- 1-4 real ops stuck: LOOKUP, GETATTR, READ — these are the FUSE handlers
  waiting on `items.read()`
- The FUSE session loop on the current_thread runtime is blocked because its
  handler tasks can't complete

### What We've Ruled Out

1. **Tracking bug in vx-locks** — acquire/release counters match perfectly,
   promotion is atomic, cancellation is handled
2. **`/dev/fuse` fd inheritance** — added `FD_CLOEXEC` on the fuse fd so child
   processes don't inherit it. Hang still reproduces.
3. **Fork interference** — the unmount sentinel fork is currently disabled
4. **RPC deadlock** — zero in-flight RPCs during hang
5. **CAS blob_cache mutex** — not contended during hang

### Theories

1. **`tokio::sync::RwLock` cross-runtime issue**: The RwLock is shared between
   two tokio runtimes (main multi-threaded + FUSE current_thread). When a task
   on one runtime releases the lock, the waker for the next waiter may target
   the other runtime. If waker delivery across runtimes is unreliable (e.g.,
   the waker notifies the wrong reactor, or the notification is lost), the
   semaphore permit is returned but no task wakes up to claim it.

2. **Semaphore permit leak on ARM64**: This runs on aarch64 (OrbStack/Linux).
   ARM64 has a weaker memory model than x86. If tokio's semaphore uses
   `Relaxed` or `Release` ordering somewhere that needs `SeqCst` on ARM64, a
   permit could be "returned" from one thread's perspective but not visible to
   waiters on another thread.

3. **`add_entry_to_prefix` lock thrashing**: This function acquires and releases
   `items.read()` then `items.write()` **per path component** in a loop. With 8
   concurrent callers, each doing dozens of read→write→read→write cycles, the
   RwLock's internal waiter queue could reach a degenerate state.

### Reproduction

```bash
# Build with diagnostics
cargo build -p vx-runner --features diagnostics

# Run stress test (hangs within 1-5 attempts usually)
cargo run -p vx-fuse-stress -- --projects 8 --concurrency 8 --rounds 5 --timeout-secs 10

# Or loop until hang:
for i in $(seq 1 20); do
  cargo run -p vx-fuse-stress -- --projects 8 --concurrency 8 --rounds 5 --timeout-secs 10 2>&1 \
    | tee /tmp/fuse-stress.log
  grep -q "HANG DIAGNOSTICS" /tmp/fuse-stress.log && echo "HANG on attempt $i" && break
done
```

When a hang is detected, the stress test sends SIGUSR1 to vx-runner, which
dumps FUSE telemetry + roam diagnostics + lock diagnostics to stderr.

## Investigating a Hang

### 1. Find the stuck processes

```bash
ps aux | grep -E 'vx-runner|vx-store|vxd' | grep -v grep
```

Look for `D` state (uninterruptible sleep) — these are stuck in FUSE requests.
`S` / `Sl` is normal (sleeping or multi-threaded sleeping).

### 2. Quick thread state overview

```bash
PID=<vx-runner-pid>
for tid in $(ls --color=never /proc/$PID/task/); do
  comm=$(cat /proc/$PID/task/$tid/comm 2>/dev/null)
  state=$(sudo cat /proc/$PID/task/$tid/stack 2>/dev/null | head -1)
  echo "$tid $comm: $state"
done
```

**What to look for**:
- `kernel_clone` — tokio worker stuck in `clone3(CLONE_VFORK)`, spawning child
- `do_epoll_wait` — reactor thread(s); check `comm` to see if `fuse-runtime`
- `__futex_wait` — normal for idle tokio worker threads
- `__fuse_simple_request` — only appears in child processes, not threads

### 3. Check children

```bash
ps --ppid $PID -o pid,stat,wchan,comm --no-headers
```

Then get stacks for children in D state:

```bash
sudo cat /proc/<child-pid>/stack
```

Common child stacks:
- `__fuse_simple_request` → `fuse_lookup` → `chdir` — child waiting for FUSE
- `d_alloc_parallel` → `execve` — child waiting for dentry lock (held by another child's lookup)
- `do_sigtimedwait` — sentinel process, normal
- `futex_wait` — child running normally (rustc threads idle)

### 4. Check FUSE connection state

```bash
# How many FUSE requests are waiting for userspace?
cat /sys/fs/fuse/connections/*/waiting

# Find which connection belongs to our mount
grep vixen /proc/mounts
ls /sys/fs/fuse/connections/
```

If `waiting > 0` and the FUSE thread is in `epoll_wait`, the server missed the
notification.

### 5. Check fd and epoll registration

```bash
# Find the /dev/fuse fd
ls -la --color=never /proc/$PID/fd/ | grep fuse

# Find all epoll fds
ls -la --color=never /proc/$PID/fd/ | grep eventpoll

# Check which epoll fd has the fuse fd registered (look for tfd=17 or whatever)
for fd in 3 5 11 13; do
  echo "=== epoll fd $fd ==="
  cat /proc/$PID/fdinfo/$fd | grep -E 'tfd|pos'
done
```

### 6. strace

```bash
# Check nothing else is tracing the process
cat /proc/$PID/status | grep TracerPid
# If TracerPid != 0, kill the stale tracer:
#   sudo kill <TracerPid>

# Check ptrace_scope allows attachment
cat /proc/sys/kernel/yama/ptrace_scope
# If 1, set to 0:
sudo sh -c 'echo 0 > /proc/sys/kernel/yama/ptrace_scope'

# Find the fuse-runtime TID
for tid in $(ls --color=never /proc/$PID/task/); do
  echo "$tid $(cat /proc/$PID/task/$tid/comm 2>/dev/null)"
done | grep fuse-runtime

# Trace the FUSE thread specifically
sudo timeout 5 strace -p <fuse-runtime-tid> -e epoll_pwait,readv,read -t
```

**Warning**: gdb on unoptimized Rust debug builds can OOM (exit 137). Prefer
kernel stacks + strace. Background strace (with `&`) can leave stale tracers
that block future attachments — always use `timeout`.

## Cleaning Up After a Hang

### 1. Unmount ghost FUSE mounts

```bash
# Check for lingering mounts
grep vixen /proc/mounts

# Lazy unmount (works even if mount is stuck)
fusermount3 -uz <mount-path>
```

### 2. Abort stuck FUSE connections

D-state processes are stuck in kernel waiting for a FUSE response that will never
come. They can't be killed with signals (not even SIGKILL). The only way to
unstick them is to abort the FUSE connection:

```bash
# List FUSE connections
ls /sys/fs/fuse/connections/

# Abort — this makes all pending requests return with error,
# freeing D-state processes
echo 1 | sudo tee /sys/fs/fuse/connections/<N>/abort
```

After aborting, `fusermount3 -uz` should work if it didn't before.

### 3. Verify cleanup

```bash
# Should show no vixen mounts
grep vixen /proc/mounts

# Should show no vx-runner processes
ps aux | grep vx-runner | grep -v grep

# Should show no FUSE connections (or only unrelated ones)
ls /sys/fs/fuse/connections/
```

### Script

`scripts/fuse-cleanup.sh` — aborts all FUSE connections and unmounts all
vixen mounts (nextest, stress test, etc.).

## For AI assistants

- The user knows how to upgrade deps and rebuild. Don't offer to run `cargo build`
  or `cargo update` — just make the code changes.
- `cargo nextest` (and the test suite) runs `tests/warmup*.sh` which does
  `cargo build`. No need to build separately.
- `cargo clean` is not the bug. It's never the bug. Don't suggest it.

## Notes

- Child processes use `die_with_parent` (`PR_SET_PDEATHSIG`), so they die when
  the parent dies — UNLESS they're in D state (uninterruptible kernel sleep), in
  which case they can't receive signals and must be freed via FUSE abort.
- `vx kill` prints a message about cleaning up orphaned processes but doesn't
  actually kill anything in the fallback path — the processes rely on
  `die_with_parent` to clean up.
- The sentinel process (forked before `/dev/fuse` is opened) inherits no fuse
  fds and is there for cleanup purposes (currently disabled for debugging).
- FUSE handlers in fuse3 are `spawn()`'d as separate tasks on the FUSE runtime.
  The dispatch loop reads one request, spawns a handler, and immediately loops
  back to read the next. Handlers that call `CasVfs` methods may do cross-runtime
  I/O (e.g., `cas.get_blob()` fetches from vx-store).
- `CasVfs` uses `DiagnosticRwLock` for `items` and `DiagnosticMutex` for
  `blob_cache` (from `vx-locks` crate). No locks are held across
  `cmd.output().await` (the `clone3` call).
- Platform: aarch64 Linux (OrbStack on macOS). ARM64's weak memory model may
  be relevant.
