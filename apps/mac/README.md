# Vixen.app (macOS FSKit host)

`apps/mac` contains the macOS app, FSKit extension, and mount helper used by `vx-vfs`.

## Bundle IDs

- App: `eu.bearcove.vixen`
- FS extension: `eu.bearcove.vixenfs`

## Build (local)

1. Install [XcodeGen](https://github.com/yonaskolb/XcodeGen):
   - `brew install xcodegen`
2. Generate the Xcode project:
   - `cd apps/mac/xcode`
   - `xcodegen generate`
3. Open the generated project in Xcode.
4. Build the `Vixen` scheme (this also builds `VixenFSExtension` and `vixenfs-mount`).

## Install and activate

1. Copy app to Applications:
   - `cp -R build/Debug/Vixen.app /Applications/`
2. Launch once:
   - `open /Applications/Vixen.app`
3. In macOS settings, enable `Vixen` under File System Extensions.

## Runtime expectations from `vx-vfs`

`vx-vfs` expects:

- App path: `/Applications/Vixen.app`
- Mount helper: `/Applications/Vixen.app/Contents/Helpers/vixenfs-mount`
- Extension id visible in `pluginkit`: `eu.bearcove.vixenfs`
