# Windows follow-up issues

Issues to file on bearcove/vixen for Windows platform parity. These are all currently `#[cfg(unix)]`-gated and no-op on Windows.

---

## 1. Windows diagnostics signal for peeps dumps

**Title:** Windows: implement diagnostics signal handler (replace SIGUSR1)

**Description:**
On Unix, `peeps::install_sigusr1` registers a SIGUSR1 handler that dumps thread/task diagnostics to `/tmp/peeps-dumps/`. This is `#[cfg(unix)]`-gated and completely absent on Windows.

We need a Windows-compatible mechanism to trigger peeps dumps in `vxd`, `vx-store`, `vx-runner`, and `vx-vfsd`. Options:
- A named event (CreateEvent/SetEvent)
- A control handler (Ctrl+Break)
- A command sent over the existing named pipe IPC

Affected files:
- `crates/vxd/src/main.rs` (line ~359)
- `crates/vx-store/src/main.rs` (lines ~75, ~110-137)
- `crates/vx-runner/src/main.rs` (lines ~256, ~325-338)
- `crates/vx-vfsd/src/main.rs` (line ~153)

---

## 2. `vx debug` command on Windows

**Title:** Windows: implement `vx debug` diagnostics collection

**Description:**
The `cmd_debug` function in `crates/vx/src/main.rs` uses `libc::kill(*pid as i32, libc::SIGUSR1)` to signal each vixen process and collect peeps dumps. This is `#[cfg(unix)]`-gated and does nothing on Windows.

Needs:
- A way to signal running vixen processes to dump diagnostics (depends on the signal mechanism chosen in the diagnostics signal issue)
- A Windows-compatible equivalent of `find_vixen_pids()` which currently uses `pgrep`

Affected file: `crates/vx/src/main.rs` (lines ~959-963)

---

## 3. `find_vixen_pids` on Windows

**Title:** Windows: implement process discovery (`find_vixen_pids`)

**Description:**
`find_vixen_pids()` in both `crates/vx/src/main.rs` and `crates/vx-fuse-stress/src/main.rs` uses `pgrep -la` to find running vixen processes. `pgrep` doesn't exist on Windows.

Options:
- Use `tasklist /FI "IMAGENAME eq vxd.exe"` etc.
- Use the `sysinfo` crate
- Use Windows API (`EnumProcesses` / `CreateToolhelp32Snapshot`)

Affected files:
- `crates/vx/src/main.rs` (`find_vixen_pids` function)
- `crates/vx-fuse-stress/src/main.rs` (`find_vixen_pids` function, line ~175)

---

## 4. `vx-fuse-stress` named pipe readiness check

**Title:** Windows: proper vxd readiness check in vx-fuse-stress

**Description:**
In `crates/vx-fuse-stress/src/main.rs`, the vxd readiness check currently falls back to `vxd_sock.exists()` on Windows, which doesn't verify the named pipe is actually accepting connections. Should use `vx_io::net::try_connect` or equivalent to confirm the daemon is ready.

Affected file: `crates/vx-fuse-stress/src/main.rs` (lines ~466-480)

---

## 5. Hang diagnostics on Windows

**Title:** Windows: implement hang diagnostics in vx-fuse-stress

**Description:**
`dump_hang_diagnostics()` in `crates/vx-fuse-stress/src/main.rs` is mostly Unix-specific:
- SIGUSR1 signaling is `#[cfg(unix)]`-gated (currently creates an empty `pids` vec on Windows)
- Peeps dump directory is hardcoded to `/tmp/peeps-dumps`
- Linux-specific diagnostics (`/proc`, `/sys/fs/fuse`) and macOS-specific diagnostics (`sample`, FSKit) have no Windows equivalents

For Windows/ProjFS, useful diagnostics could include:
- ProjFS virtualization instance state
- Open file handles on the mount directory
- `vx-vfsd` internal VFS state dump

Affected file: `crates/vx-fuse-stress/src/main.rs` (lines ~129-168)
