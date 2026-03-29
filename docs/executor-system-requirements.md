# Executor System Requirements

This document tracks the current external requirements for Vixen executors.
It is intentionally practical and reflects the implementation that exists
today, not the eventual ideal end state.

## Shared

- SSH reachability from the machine running `vxd`
- Sufficient disk space for:
  - copied bootstrap binaries
  - remote checkout/build artifacts
  - `VX_HOME`
- Sufficient RAM and CPU to build:
  - `vx-store`
  - `vx-vfsd`
  - `vx-runner`
  - `landlock-exec` on Linux

## Unix Remote Bring-Up

Current status:
- planned direction is `cross-compile, scp, done`
- no dedicated bootstrap RPC/protocol layer
- remote setup should assume copied binaries plus ordinary SSH command execution

Required commands / facilities:
- `sshd`
- `sh`
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
- normal UTF-8 paths under the remote staging root
- the remote host can build native Linux Rust binaries for its own architecture

## Windows Remote Bring-Up

Current status:
- still requires a replacement plan
- should converge on the same model: cross-compile, copy binaries, invoke them
- no dedicated `vx-bootstrap.exe` protocol plan anymore

Required commands / facilities today:
- OpenSSH server
- `powershell`
- `cargo`
- `rustc`
- MSVC build environment suitable for building the Rust binaries

Current behavioral assumptions today:
- user profile and toolchain paths are discoverable through PowerShell
- TCP loopback ports can be allocated for store / vfsd / runner

## Likely Future Direction

These requirements should shrink over time:

- Unix should stop requiring host cargo once stable prebuilt binaries exist.
- Windows should stop requiring local toolchain discovery hacks and ad hoc
  PowerShell setup once copied binaries are the normal path.
- Both platforms should eventually converge on:
  - SSH
  - copied prebuilt binaries
  - enough OS facilities to store and execute those binaries
