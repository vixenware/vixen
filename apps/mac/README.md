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
2. Regenerate the VixenFS Swift bindings:
   - `cargo xtask vixenfs-codegen`
3. Build the Vox FFI staticlib used by `VoxRuntime`:
   - `cargo build --release -p vox-ffi`
4. Validate the Swift package against the local Vox runtime:
   - `swift build --package-path apps/mac -Xlinker -L../vox/target/release`
5. Generate the Xcode project:
   - `cd apps/mac/xcode`
   - `xcodegen generate`
6. Open the generated project in Xcode.
7. Build the `Vixen` scheme (this also builds `VixenFS`).

## Release (local macOS)

Canonical app version lives in `apps/mac/VERSION`.

Run:

- `just apps/mac/preflight`
- `just apps/mac/release`
- or `just apps/mac/release 0.2.0`

Required environment variables:

- `VX_MAC_NOTARY_PROFILE`: `notarytool` keychain profile name.
- `VX_MAC_SPARKLE_PUBLIC_ED_KEY`: Sparkle Ed25519 public key (embedded in app).
- `ACCESS_KEY_ID` / `SECRET_ACCESS_KEY`: Cloudflare R2 token credentials.

Optional environment variables:

- `VX_MAC_SIGNING_IDENTITY` (default: `Developer ID Application`)
- `VX_MAC_PUBLIC_BASE_URL` (default: `https://mac.app.vixen.rs`)
- `VX_MAC_R2_BUCKET_URL` (default: `https://4051116950f6ef4b78cb96847a2221f9.r2.cloudflarestorage.com/vixen-mac-app`)
- `VX_MAC_R2_RELEASE_PREFIX` (default: `releases/v<version>`)
- `VX_MAC_R2_APPCAST_KEY` (default: `appcast.xml`)
- `VX_MAC_R2_LATEST_PREFIX` (default: `latest`)
- `VX_MAC_R2_LATEST_MANIFEST_KEY` (default: `latest.json`)
- `VX_MAC_SPARKLE_FEED_URL` (default: `$VX_MAC_PUBLIC_BASE_URL/$VX_MAC_R2_APPCAST_KEY`)
- `VX_MAC_SPARKLE_PUBLIC_KEY` (legacy alias for `VX_MAC_SPARKLE_PUBLIC_ED_KEY`)
- `VX_MAC_SPARKLE_KEY_ACCOUNT` (default: `ed25519`)
- `VX_MAC_SPARKLE_TOOLS_ROOT` (default: `apps/mac/xcode/build/SourcePackages/artifacts/sparkle/Sparkle/bin`)

The R2 credentials may also be provided as `AWS_ACCESS_KEY_ID` /
`AWS_SECRET_ACCESS_KEY`, which is what the AWS CLI reads natively.

The release flow:

1. Runs preflight (macOS/tools/notary/R2/Sparkle credentials)
2. Builds `Vixen.app` (Release)
3. Resolves the Developer ID Application certificate to a stable hash, strips
   development provisioning profiles, and re-signs the app bundle inside-out
4. Creates `Vixen-<version>.dmg` containing `Vixen.app` + `/Applications` symlink
5. Creates, signs, and notarizes `Vixen-Installer.pkg`; the package installs
   `Vixen.app`, prepares the management queues, and registers the bundled
   management daemon with `SMAppService`
6. Notarizes and staples DMG
7. Generates `appcast.xml` via Sparkle `generate_appcast`
8. Uploads the DMG, appcast, installer pkg, stable `latest/*` aliases, and
   `latest.json` to Cloudflare R2

Stable URLs after publish:

- `https://mac.app.vixen.rs/latest/Vixen-Installer.pkg`
- `https://mac.app.vixen.rs/latest/Vixen.dmg`
- `https://mac.app.vixen.rs/latest.json`
- `https://mac.app.vixen.rs/appcast.xml`

Human-facing install and onboarding instructions belong in the logged-in
`app.vixen.rs` dashboard. The mac release scripts only produce signed artifacts
and machine-readable update metadata.

## Management daemon

`Vixen.app` contains the privileged management helper and its bundled
`SMAppService` LaunchDaemon plist:

- helper: `Contents/MacOS/vixen-management`
- plist: `Contents/Library/LaunchDaemons/eu.bearcove.vixen.management.plist`

The installer does not install a competing plist in `/Library/LaunchDaemons`.
Its postinstall script removes the old `/Library/LaunchDaemons` registration if
present, runs the helper's `ensure-queues` command, and invokes the installed app:

- `/Applications/Vixen.app/Contents/MacOS/Vixen management register`

Useful installed-app commands:

- `/Applications/Vixen.app/Contents/MacOS/Vixen management status`
- `/Applications/Vixen.app/Contents/MacOS/Vixen management register`
- `/Applications/Vixen.app/Contents/MacOS/Vixen management unregister`
- `/Applications/Vixen.app/Contents/MacOS/Vixen management remount`

The queue-trigger commands (`mount`, `unmount`, `remount`, `kill-fskitd`) only
drop fixed trigger files into the root-owned management queues. The privileged
work still runs inside `vixen-management` under launchd.

## Install and activate

1. Copy app to Applications:
   - `cp -R build/Debug/Vixen.app /Applications/`
2. Launch once:
   - `open /Applications/Vixen.app`
3. In macOS settings, enable `Vixen` under File System Extensions.

## Extension registration model

There are three moving parts in local development:

- the app bundle path that contains `VixenFS.appex`
- PlugInKit's current registration for `eu.bearcove.vixenfs`
- the `fskit_agent` / `extensionkitservice` processes that try to launch the registered extension

### Pick the right install path for the experiment

There is one supported extension launch shape in this repo.

- Hybrid Swift entrypoint plus Rust FSKit objects:
  - use `just -f apps/mac/Justfile install`
  - the extension entrypoint stays [VixenFS.swift](xcode/VixenFS/VixenFS.swift)
  - Swift calls into [RustFSKitBootstrap.swift](xcode/VixenFS/RustFSKitBootstrap.swift)
  - the Rust runtime is linked from the `vx-fskit` crate
  - this is the path to use when debugging the real FSKit host with Rust-backed `FSUnaryFileSystem` / `FSVolume` / `FSItem` objects

The old standalone executable-swap experiment has been retired. `install-rust-fs` and
`inject-rust-vixen-extension` now print guidance and exit instead of mutating the bundle.

### What actually registers the extension

- `Vixen.app` calls `pluginkit -a <appex-path>` on launch from [VixenApp.swift](xcode/Vixen/VixenApp.swift).
- Our explicit reload helper in [reload-vixen-fs-extension.sh](scripts/reload-vixen-fs-extension.sh) does:
  - `pluginkit -r <appex-path>`
  - `pluginkit -a <appex-path>`
  - `lsregister -f -R -trusted <app-bundle>`
- `xcodebuild` itself also re-registers the built app with LaunchServices at the end of a build:
  - `RegisterWithLaunchServices ... lsregister -f -R -trusted .../Vixen.app`

That last point matters: a GUI/Xcode build under the global DerivedData folder can silently replace the active registration, even if you expected `/Applications/Vixen.app` or the repo-local `apps/mac/xcode/build/DerivedData/...` copy to be the one in use.

### Which copy wins

Ask PlugInKit, not your memory:

- `pluginkit -mAvvv -i eu.bearcove.vixenfs`

That prints the currently registered path and UUID for the active FSKit extension. During debugging, this is the source of truth.

Typical paths we have seen:

- repo-local build: `apps/mac/xcode/build/DerivedData/Build/Products/<Config>/Vixen.app/...`
- global Xcode DerivedData: `~/Library/Developer/Xcode/DerivedData/.../Vixen.app/...`
- installed app: `/Applications/Vixen.app/...`

If the path is not the one you think you are testing, you are debugging the wrong binary.

### Failure mode: stale extension UUIDs

The easiest way to get confused is:

1. build one copy of the app
2. let PlugInKit register UUID `A`
3. build or register another copy with the same bundle id
4. PlugInKit now knows UUID `B`
5. `fskit_agent` or `extensionkitservice` still tries to launch stale UUID `A`

When that happens, launch fails before the extension process becomes useful. The logs look like:

- `Failed to initialize _EXExtensionIdentity`
- `Failed to find extension: <old-uuid>`
- `Exception caught during decoding of received selector prepareWithLaunchConfiguration:reply:`
- mount fails with `com.apple.extensionKit.errorDomain error 2`

This is not an in-extension bug. It means the host side is still holding an older identity than the one PlugInKit currently advertises.

### Practical reset procedure

If registration gets poisoned, first decide which app copy should win, then reset to that copy only.

For an installed app:

- `just apps/mac/reload-fs-extension-registration /Applications/Vixen.app`

For a specific build product:

- `pluginkit -r <old-appex-path>`
- `pluginkit -a <new-appex-path>`
- `lsregister -f -R -trusted <new-app-bundle>`
- `pluginkit -mAvvv -i eu.bearcove.vixenfs`

Do not trust a successful build alone. Always verify the active registration path afterward.

### Minimal debugging checklist

Before debugging a mount failure, check these in order:

1. Confirm which experiment you mean to run.
   - Hybrid FSKit host: `just -f apps/mac/Justfile install`
   - Standalone injected Rust executable: `just -f apps/mac/Justfile install-rust-fs`
2. Verify the winning registration:
   - `pluginkit -mAvvv -i eu.bearcove.vixenfs`
3. If FSKit host state looks stale, restart the daemons:
   - `sudo killall fskitd fskit_agent`
4. Re-register the installed app if needed:
   - `just apps/mac/reload-fs-extension-registration /Applications/Vixen.app`
5. Only then retry the mount or `vx eval`.

If step 2 shows a path other than the app copy you intend to test, stop there and fix registration
first. Otherwise you are debugging the wrong binary.

## Runtime expectations from `vx-vfs`

`vx-vfs` expects:

- App path: `/Applications/Vixen.app`
- App Group path source: `/.vixenfs/status.json` (`app_group_path`) on the mounted VixenFS volume
- Extension id visible in `pluginkit`: `eu.bearcove.vixenfs`
