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

Getting a local build to actually work with FSKit requires being a registered
Apple developer and going through the full signing/notarization setup.

In practice, this path is for core maintainer work (currently Amos Wenger), not
for general users. For normal usage, install the official release.

## Install the official build

```shell
brew install --cask bearcove/tap/vixen
```

Release artifacts are published from:

- [bearcove/vixen-mac-app](https://github.com/bearcove/vixen-mac-app)
