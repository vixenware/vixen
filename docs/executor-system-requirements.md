# Executor System Requirements

This document tracks the current external requirements for Vixen executors.
It is intentionally practical and reflects the implementation that exists
today, not the eventual ideal end state.

## Shared

- SSH reachability from the machine running `vxd`
- Sufficient disk space for:
  - the bootstrap cache
  - remote checkout/build artifacts
  - `VX_HOME`
- Sufficient RAM and CPU to build:
  - `vx-store`
  - `vx-vfsd`
  - `vx-runner`
  - `landlock-exec` on Linux

## Unix SSH Bootstrap

Current status:
- uses `vx-bootstrap` over SSH stdio
- source sync is file-manifest based
- Git is no longer required for the Unix bootstrap transport

Required commands / facilities:
- `sshd`
- `sh`
- `head`
- `gzip`
- `mkdir`
- `chmod`
- `mv`
- `rm`
- `pgrep`
- `pkill`
- `cargo`
- `rustc`
- a working linker / C toolchain suitable for building the Rust binaries

Current behavioral assumptions:
- normal UTF-8 paths under the bootstrap cache root
- symlinks in the local source tree are ignored by the sync path
- `gzip -dc` is available on the remote host
- the remote host can build native Linux Rust binaries for its own architecture

## Windows SSH Bootstrap

Current status:
- still uses the legacy PowerShell + Git-based path
- this is expected to be replaced by `vx-bootstrap.exe`

Required commands / facilities today:
- OpenSSH server
- `powershell`
- `git`
- `cargo`
- `rustc`
- MSVC build environment suitable for building the Rust binaries

Current behavioral assumptions today:
- user profile and toolchain paths are discoverable through PowerShell
- the bootstrap root may be inferred from `CARGO_HOME`, `RUSTUP_HOME`, `TMP`, or `TEMP`
- TCP loopback ports can be allocated for store / vfsd / runner

## Likely Future Direction

These requirements should shrink over time:

- Unix should stop requiring host cargo once stable prebuilt binaries exist.
- Windows should stop requiring Git and most PowerShell command construction once
  `vx-bootstrap.exe` is in place.
- Both platforms should eventually converge on:
  - SSH
  - a tiny bootstrap binary
  - enough OS facilities to store and execute that binary
