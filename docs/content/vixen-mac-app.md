+++
title = "Vixen Mac app"
insert_anchor_links = "heading"
+++

## Why a mac app exists

On macOS, vixen needs a virtual filesystem provider for hermetic execution.
The supported path is FSKit, which requires a signed and notarized host app to
register a File System Extension.

`Vixen.app` provides:

- The FSKit extension (`VixenFS`)
- The `vixenfs-mount` helper used by the CLI runtime
- Onboarding UI that guides users through enabling the extension in System Settings

![The Vixen mac app showing settings](/img/vixen-app.png)

## Local development build

You can build the app locally:

```shell
just apps/mac/install
```

However, unsigned local builds do not provide a production-ready setup for end
users. For normal usage, install the official release.

## Install the official build

```shell
brew install --cask bearcove/tap/vixen
```

Release artifacts are published from:

- [bearcove/vixen-mac-app](https://github.com/bearcove/vixen-mac-app)
