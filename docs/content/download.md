+++
title = "Download"
description = "Install vixen on macOS"
+++

## Install via Homebrew (recommended)

The fastest way to get vixen on macOS:

```
brew install --cask bearcove/tap/vixen
```

This installs the vixen macOS app.

## Direct Download

Download the latest `.dmg` from GitHub Releases:

- [Latest release](https://github.com/bearcove/vixen-mac-app/releases/latest)

### System requirements

- macOS 26 (Tahoe) or later
- Apple Silicon (arm64)

### Installation

1. Open the downloaded `.dmg` file
2. Drag Vixen to the Applications folder
3. Open Vixen from Applications
4. Follow the in-app prompt to enable the VixenFS extension in System Settings > General > Login Items & Extensions > File System Extensions

## CLI Tools

The standalone CLI tools (`vx`, `vxd`) will be available via Homebrew:

```
brew install bearcove/tap/vixen-cli
```

This is not yet published. For now, build from source.

## Building from Source

The macOS app must be code-signed with a Developer ID certificate to function
(the FSKit extension requires it). Building from source for local development
requires Xcode 26 and a valid Apple Developer account.

See the [contributing guide](https://github.com/bearcove/vixen) for details.
