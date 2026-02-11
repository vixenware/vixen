+++
title = "Prerequisites"
insert_anchor_links = "heading"
+++

# Prerequisites

vixen runs on Linux, macOS, and Windows. Each platform requires a virtual
filesystem layer so the build engine can project source trees and toolchains
into the runner without copying files.

## All platforms

- **Rust toolchain** (stable) for building vixen itself
- Install with `cargo xtask install`

## Linux

FUSE 3 is used to project the virtual filesystem.

**Debian / Ubuntu:**

```bash
sudo apt install fuse3 libfuse3-dev
```

**Fedora:**

```bash
sudo dnf install fuse3 fuse3-devel
```

**Arch Linux:**

```bash
sudo pacman -S fuse3
```

Your user must have permission to use FUSE. On most distros this means being in
the `fuse` group, or `/dev/fuse` having world-readable permissions. You can
verify with:

```bash
fusermount3 --version
```

## macOS

Requires **macOS 26 (Tahoe)** or later.

vixen uses FSKit via the **Vixen.app** companion application, which provides a
filesystem extension (VixenFS). Install the app and follow its onboarding flow
to enable the extension.

Your terminal application also needs the following permissions (grant when
prompted, or configure in **System Settings > Privacy & Security**):

- **Developer Tools** access
- **Full Disk Access** or **Files and Folders > Access files from other apps**
- **Network Volumes** access (Files and Folders > Access Network volumes)

## Windows

vixen uses the Windows Projected File System (ProjFS) to project the virtual
filesystem.

### Enable ProjFS

ProjFS is a Windows optional feature that is not enabled by default. Enable it
from an **elevated** (Administrator) PowerShell:

```powershell
Enable-WindowsOptionalFeature -Online -FeatureName Client-ProjFS -NoRestart
```

Or via **Settings > Apps > Optional features > More Windows features** and
check "Windows Projected File System".

No reboot is required.

### Visual Studio Build Tools

The MSVC linker (`link.exe`) is required for building Rust on Windows. Install
the [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022)
with the **"Desktop development with C++"** workload.

> **PATH note:** If you use Git for Windows, ensure the MSVC `link.exe` comes
> before Git's `/usr/bin/link.exe` in your PATH. Running `cargo build` from
> a Visual Studio Developer Command Prompt avoids this issue.
