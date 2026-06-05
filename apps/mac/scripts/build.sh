#!/usr/bin/env bash
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

echo "Running preflight checks..."
"$SCRIPT_DIR/preflight.sh" "$RELEASE_VERSION"

rm -rf "$DIST_DIR"
mkdir -p "$STAGING_DIR" "$SPARKLE_DIR"

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

echo "Building Vixen.app (Release)..."
(
  cd "$XCODE_DIR"
  xcodebuild \
    -project Vixen.xcodeproj \
    -scheme Vixen \
    -configuration Release \
    -sdk macosx \
    -derivedDataPath build/DerivedData \
    -clonedSourcePackagesDirPath build/SourcePackages \
    MARKETING_VERSION="$RELEASE_VERSION" \
    CURRENT_PROJECT_VERSION="$RELEASE_VERSION" \
    SPARKLE_FEED_URL="$SPARKLE_FEED_URL" \
    SPARKLE_PUBLIC_ED_KEY="$SPARKLE_PUBLIC_ED_KEY" \
    build
)

if [[ ! -d "$APP_BUNDLE_PATH" ]]; then
  echo "Expected app bundle at $APP_BUNDLE_PATH but it was not found." >&2
  exit 1
fi

RESOLVED_SIGNING_IDENTITY="$(resolve_codesign_identity "$SIGNING_IDENTITY" "Developer ID Application")"

echo "Stripping embedded provisioning profiles (not valid for Developer ID)..."
find "$APP_BUNDLE_PATH" -name "embedded.provisionprofile" -print -delete

echo "Re-signing app bundle with '$SIGNING_IDENTITY' ($RESOLVED_SIGNING_IDENTITY) inside-out..."

MANAGEMENT_HELPER="$APP_BUNDLE_PATH/Contents/MacOS/vixen-management"
if [[ ! -f "$MANAGEMENT_HELPER" ]]; then
  echo "Expected management helper at $MANAGEMENT_HELPER but it was not found." >&2
  exit 1
fi
codesign --force --options runtime --timestamp --sign "$RESOLVED_SIGNING_IDENTITY" \
  "$MANAGEMENT_HELPER"

# FSKit extension
codesign --force --options runtime --timestamp \
  --entitlements "$XCODE_DIR/VixenFS/VixenFS.entitlements" \
  --sign "$RESOLVED_SIGNING_IDENTITY" \
  "$APP_BUNDLE_PATH/Contents/Extensions/VixenFS.appex"

# Sparkle framework (sign nested tools/apps first, then XPCs, then framework)
SPARKLE_FW="$APP_BUNDLE_PATH/Contents/Frameworks/Sparkle.framework"
SPARKLE_VER="$SPARKLE_FW/Versions/Current"

if [[ -f "$SPARKLE_VER/Autoupdate" ]]; then
  codesign --force --options runtime --timestamp --sign "$RESOLVED_SIGNING_IDENTITY" \
    "$SPARKLE_VER/Autoupdate"
fi

if [[ -d "$SPARKLE_VER/Updater.app" ]]; then
  if [[ -f "$SPARKLE_VER/Updater.app/Contents/MacOS/Updater" ]]; then
    codesign --force --options runtime --timestamp --sign "$RESOLVED_SIGNING_IDENTITY" \
      "$SPARKLE_VER/Updater.app/Contents/MacOS/Updater"
  fi
  codesign --force --options runtime --timestamp --sign "$RESOLVED_SIGNING_IDENTITY" \
    "$SPARKLE_VER/Updater.app"
fi

for xpc in "$SPARKLE_VER"/XPCServices/*.xpc; do
  [[ -e "$xpc" ]] || continue
  codesign --force --options runtime --timestamp --sign "$RESOLVED_SIGNING_IDENTITY" "$xpc"
done

codesign --force --options runtime --timestamp --sign "$RESOLVED_SIGNING_IDENTITY" "$SPARKLE_FW"

codesign --force --options runtime --timestamp \
  --entitlements "$XCODE_DIR/Vixen/Vixen.entitlements" \
  --sign "$RESOLVED_SIGNING_IDENTITY" \
  "$APP_BUNDLE_PATH"

codesign --verify --deep --strict --verbose=2 "$APP_BUNDLE_PATH"

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
echo "Review the DMG, then run: just publish $RELEASE_VERSION"
