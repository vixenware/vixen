+++
title = "Download"
description = "Install vixen on macOS"
+++

## Install via Homebrew (recommended)

```shell
brew install --cask bearcove/tap/vixen
```

This installs the Vixen macOS app and associated runtime components.

## Direct download

Download the latest `.dmg` from GitHub Releases:

- [Latest release](https://github.com/bearcove/vixen-mac-app/releases/latest)

### System requirements

- macOS 26 (Tahoe) or later
- Apple Silicon (arm64)

### Installation steps

1. Open the downloaded `.dmg`
2. Drag `Vixen.app` to `Applications`
3. Open `Vixen.app`
4. Follow the in-app prompt to enable the VixenFS extension in: `System Settings > General > Login Items & Extensions > File System Extensions`

## CLI tools

A standalone CLI Homebrew formula (`vixen-cli`) is planned but not published yet.
For now, build from source.

## Build from source

The macOS app must be code-signed with a Developer ID certificate to provide
FSKit functionality. Local source builds require:

- Xcode 26
- An Apple Developer account with signing capability

See the [contributing guide](https://github.com/bearcove/vixen) for details.
