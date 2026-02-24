+++
title = "Prerequisites"
insert_anchor_links = "heading"
+++

vixen runs on Linux, macOS, and Windows. Each platform needs a virtual
filesystem layer so the build engine can project source trees and toolchains
into runners without copying files.

## All platforms

- Rust stable toolchain (for building vixen itself)
- Install vixen binaries with `cargo xtask install`

## Linux

vixen uses FUSE 3 to project the virtual filesystem.

### Debian / Ubuntu

```bash
sudo apt install fuse3 libfuse3-dev
```

### Fedora

```bash
sudo dnf install fuse3 fuse3-devel
```

### Arch Linux

```bash
sudo pacman -S fuse3
```

Your user needs permission to use FUSE. On many distributions this means being
in the `fuse` group (or equivalent `/dev/fuse` permissions).

Quick verification:

```bash
fusermount3 --version
```

## macOS

Requires **macOS 26 (Tahoe)** or later.

vixen uses FSKit via the **Vixen.app** companion application, which provides
`VixenFS`. Install the app and follow onboarding to enable the extension.

Your terminal application also needs these permissions (grant on prompt, or
configure in **System Settings > Privacy & Security**):

- **Developer Tools**
- **Full Disk Access** or **Files and Folders > Access files from other apps**
- **Network Volumes** access

## Windows

vixen uses Windows Projected File System (ProjFS) for virtual filesystem
projection.

### Enable ProjFS

Enable ProjFS from an elevated (Administrator) PowerShell:

```powershell
Enable-WindowsOptionalFeature -Online -FeatureName Client-ProjFS -NoRestart
```

Or via **Settings > Apps > Optional features > More Windows features** and
enable **Windows Projected File System**.

No reboot is required.

### Visual Studio Build Tools

Rust builds on Windows require the MSVC linker (`link.exe`). Install
[Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022)
with the **Desktop development with C++** workload.

> **PATH note:** If you use Git for Windows, ensure MSVC `link.exe` appears
> before Git's `/usr/bin/link.exe` in `PATH`. Using a Visual Studio Developer
> Command Prompt avoids this conflict.
