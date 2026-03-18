# Vixen.app (macOS FSKit host)

![Vixen fox logo](../../art/fox.svg)

`apps/mac` contains the macOS app and FSKit extension used by `vx-vfs`.

Brand accent color: `#E88602`.

## Bundle IDs

- App: `eu.bearcove.vixen`
- FS extension: `eu.bearcove.vixenfs`

## Build (local)

Quick path (from repo root):

- `just apps/mac/install`

Or manual path:

1. Install [XcodeGen](https://github.com/yonaskolb/XcodeGen):
   - `brew install xcodegen`
2. Generate the Xcode project:
   - `cd apps/mac/xcode`
   - `xcodegen generate`
3. Open the generated project in Xcode.
4. Build the `Vixen` scheme (this also builds `VixenFS`).

## Release (local macOS)

Canonical app version lives in `apps/mac/VERSION`.

Run:

- `just apps/mac/preflight`
- `just apps/mac/release`
- or `just apps/mac/release 0.2.0`

Required environment variables:

- `VX_MAC_NOTARY_PROFILE`: `notarytool` keychain profile name.
- `VX_MAC_SPARKLE_PUBLIC_ED_KEY`: Sparkle Ed25519 public key (embedded in app).

Optional environment variables:

- `VX_MAC_RELEASE_REPO` (default: `bearcove/vixen-mac-app`)
- `VX_MAC_RELEASE_TARGET` (default: `main`)
- `VX_MAC_SIGNING_IDENTITY` (default: `Developer ID Application`)
- `VX_MAC_SPARKLE_FEED_URL` (default: `https://github.com/$VX_MAC_RELEASE_REPO/releases/latest/download/appcast.xml`)
- `VX_MAC_SPARKLE_KEY_ACCOUNT` (default: `ed25519`)
- `VX_MAC_SPARKLE_TOOLS_ROOT` (default: `apps/mac/xcode/build/SourcePackages/artifacts/sparkle/Sparkle/bin`)

The release flow:

1. Runs preflight (macOS/tools/credentials/repo access/version/tag checks)
2. Builds `Vixen.app` (Release)
3. Re-signs with Developer ID identity
4. Creates `Vixen-<version>.dmg` containing `Vixen.app` + `/Applications` symlink
5. Notarizes and staples DMG
6. Generates `appcast.xml` via Sparkle `generate_appcast`
7. Creates a GitHub release/tag in the app repo and uploads DMG + appcast

## Install and activate

1. Copy app to Applications:
   - `cp -R build/Debug/Vixen.app /Applications/`
2. Launch once:
   - `open /Applications/Vixen.app`
3. In macOS settings, enable `Vixen` under File System Extensions.

## Runtime expectations from `vx-vfs`

`vx-vfs` expects:

- App path: `/Applications/Vixen.app`
- App Group path source: `/.vixenfs/status.json` (`app_group_path`) on the mounted VixenFS volume
- Extension id visible in `pluginkit`: `eu.bearcove.vixenfs`
