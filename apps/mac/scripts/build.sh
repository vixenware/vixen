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

echo "Re-signing app bundle with '$SIGNING_IDENTITY' (inside-out)..."

# Sign nested components first (inside-out), each with hardened runtime + timestamp
# and proper entitlements (without get-task-allow which Xcode adds for debug builds).

# Helper tool
codesign --force --options runtime --timestamp \
  --sign "$SIGNING_IDENTITY" \
  "$APP_BUNDLE_PATH/Contents/Helpers/vixenfs-mount"

# FSKit extension
codesign --force --options runtime --timestamp \
  --entitlements "$XCODE_DIR/VixenFSExtension/VixenFSExtension.entitlements" \
  --sign "$SIGNING_IDENTITY" \
  "$APP_BUNDLE_PATH/Contents/Extensions/VixenFS Extension.appex"

# Sparkle framework (XPC services inside it, then the framework itself)
SPARKLE_FW="$APP_BUNDLE_PATH/Contents/Frameworks/Sparkle.framework"
for xpc in "$SPARKLE_FW"/Versions/Current/XPCServices/*.xpc; do
  codesign --force --options runtime --timestamp --sign "$SIGNING_IDENTITY" "$xpc"
done
codesign --force --options runtime --timestamp --sign "$SIGNING_IDENTITY" "$SPARKLE_FW"

# Finally the outer app
codesign --force --options runtime --timestamp \
  --entitlements "$XCODE_DIR/Vixen/Vixen.entitlements" \
  --sign "$SIGNING_IDENTITY" \
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
