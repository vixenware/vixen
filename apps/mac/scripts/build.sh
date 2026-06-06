#!/usr/bin/env bash
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

echo "Running preflight checks..."
"$SCRIPT_DIR/preflight.sh" "$RELEASE_VERSION"

rm -rf "$DIST_DIR"
mkdir -p "$STAGING_DIR" "$SPARKLE_DIR" "$EXPORT_DIR"

echo "Generating Xcode project..."
(
  cd "$XCODE_DIR"
  xcodegen generate
)

echo "Building Rust artifacts (Release)..."
(
  cd "$APP_DIR"
  just build-rust Release
)

RESOLVED_SIGNING_IDENTITY="$(resolve_codesign_identity "$SIGNING_IDENTITY" "Developer ID Application")"

cat > "$EXPORT_OPTIONS_PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "https://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>destination</key>
  <string>export</string>
  <key>manageAppVersionAndBuildNumber</key>
  <false/>
  <key>method</key>
  <string>developer-id</string>
  <key>signingStyle</key>
  <string>automatic</string>
  <key>stripSwiftSymbols</key>
  <true/>
  <key>teamID</key>
  <string>B2N6FSRTPV</string>
</dict>
</plist>
EOF

echo "Archiving Vixen.app (Release)..."
(
  cd "$XCODE_DIR"
  xcodebuild \
    -project Vixen.xcodeproj \
    -scheme Vixen \
    -configuration Release \
    -sdk macosx \
    -destination "generic/platform=macOS" \
    -archivePath "$ARCHIVE_PATH" \
    -derivedDataPath build/DerivedData \
    -clonedSourcePackagesDirPath build/SourcePackages \
    -allowProvisioningUpdates \
    ARCHS=arm64 \
    MARKETING_VERSION="$RELEASE_VERSION" \
    CURRENT_PROJECT_VERSION="$RELEASE_VERSION" \
    SPARKLE_FEED_URL="$SPARKLE_FEED_URL" \
    SPARKLE_PUBLIC_ED_KEY="$SPARKLE_PUBLIC_ED_KEY" \
    archive
)

"$SCRIPT_DIR/clean-build-registration.sh" "$PRODUCTS_APP_BUNDLE_PATH" || true
"$SCRIPT_DIR/clean-build-registration.sh" "$ARCHIVE_PATH/Products/Applications/Vixen.app" || true
"$SCRIPT_DIR/clean-build-registration.sh" "$XCODE_DIR/build/DerivedData/Build/Intermediates.noindex/ArchiveIntermediates/Vixen/InstallationBuildProductsLocation/Applications/Vixen.app" || true

echo "Exporting Vixen.app with Xcode-managed Developer ID signing..."
(
  cd "$XCODE_DIR"
  xcodebuild \
    -exportArchive \
    -archivePath "$ARCHIVE_PATH" \
    -exportPath "$EXPORT_DIR" \
    -exportOptionsPlist "$EXPORT_OPTIONS_PLIST" \
    -allowProvisioningUpdates
)

if [[ ! -d "$APP_BUNDLE_PATH" ]]; then
  echo "Expected app bundle at $APP_BUNDLE_PATH but it was not found." >&2
  exit 1
fi

MANAGEMENT_HELPER="$APP_BUNDLE_PATH/Contents/MacOS/vixen-management"
if [[ ! -f "$MANAGEMENT_HELPER" ]]; then
  echo "Expected management helper at $MANAGEMENT_HELPER but it was not found." >&2
  exit 1
fi

echo "Signing bundled management helper with '$SIGNING_IDENTITY' ($RESOLVED_SIGNING_IDENTITY)..."
codesign --force --options runtime --timestamp --sign "$RESOLVED_SIGNING_IDENTITY" \
  --identifier "eu.bearcove.vixen" \
  "$MANAGEMENT_HELPER"

echo "Refreshing exported app signature after helper signing..."
codesign --force --options runtime --timestamp \
  --preserve-metadata=entitlements \
  --sign "$RESOLVED_SIGNING_IDENTITY" \
  "$APP_BUNDLE_PATH"

VIXENFS_APPEX="$APP_BUNDLE_PATH/Contents/Extensions/VixenFS.appex"
appex_signature="$(codesign -dvvv "$VIXENFS_APPEX" 2>&1 || true)"
case "$appex_signature" in
  *"Authority=Developer ID Application"*) ;;
  *)
    echo "VixenFS.appex was not exported with a Developer ID Application signature:" >&2
    echo "$appex_signature" >&2
    exit 1
    ;;
esac

if codesign -d --entitlements :- "$VIXENFS_APPEX" 2>/dev/null | grep -q "com.apple.security.get-task-allow"; then
  echo "VixenFS.appex still has get-task-allow after Developer ID export." >&2
  exit 1
fi

codesign --verify --deep --strict --verbose=2 "$APP_BUNDLE_PATH"
"$SCRIPT_DIR/clean-build-registration.sh" "$APP_BUNDLE_PATH" || true

APP_STAGING_PATH="$STAGING_DIR/Vixen.app"
ditto "$APP_BUNDLE_PATH" "$APP_STAGING_PATH"

echo "Creating DMG $DMG_PATH..."
create-dmg \
  --volname "Vixen" \
  --window-size 660 400 \
  --icon-size 128 \
  --icon "Vixen.app" 170 185 \
  --app-drop-link 490 185 \
  --background "$APP_DIR/dmg-background.png" \
  --no-internet-enable \
  "$DMG_PATH" \
  "$STAGING_DIR"

echo ""
echo "Build complete:"
echo "  DMG: $DMG_PATH"
echo ""
echo "Review the DMG, then run:"
echo "  just release-pkg $RELEASE_VERSION"
echo "  just release-publish $RELEASE_VERSION"
