#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_DIR="$(cd "$APP_DIR/../.." && pwd)"
XCODE_DIR="$APP_DIR/xcode"
DERIVED_DATA_DIR="$XCODE_DIR/build/DerivedData"
PRODUCTS_DIR="$DERIVED_DATA_DIR/Build/Products/Release"
APP_BUNDLE_PATH="$PRODUCTS_DIR/Vixen.app"

RELEASE_VERSION="${1:-}"
if [[ -z "$RELEASE_VERSION" ]]; then
  RELEASE_VERSION="$(tr -d '[:space:]' < "$APP_DIR/VERSION")"
fi

if [[ -z "$RELEASE_VERSION" ]]; then
  echo "Release version is empty. Pass one explicitly or set apps/mac/VERSION." >&2
  exit 1
fi

if [[ ! "$RELEASE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Release version '$RELEASE_VERSION' must be strict semver (X.Y.Z)." >&2
  exit 1
fi

RELEASE_REPO="${VX_MAC_RELEASE_REPO:-bearcove/vixen-mac-app}"
RELEASE_TARGET="${VX_MAC_RELEASE_TARGET:-main}"
NOTARY_PROFILE="${VX_MAC_NOTARY_PROFILE:-}"
SIGNING_IDENTITY="${VX_MAC_SIGNING_IDENTITY:-Developer ID Application}"
SPARKLE_FEED_URL="${VX_MAC_SPARKLE_FEED_URL:-https://github.com/${RELEASE_REPO}/releases/latest/download/appcast.xml}"
SPARKLE_PUBLIC_ED_KEY="${VX_MAC_SPARKLE_PUBLIC_ED_KEY:-}"
SPARKLE_ACCOUNT="${VX_MAC_SPARKLE_KEY_ACCOUNT:-ed25519}"
TAG="v${RELEASE_VERSION}"

echo "Running release preflight checks..."
"$SCRIPT_DIR/preflight.sh" "$RELEASE_VERSION"

DIST_DIR="$APP_DIR/dist/$RELEASE_VERSION"
STAGING_DIR="$DIST_DIR/staging"
SPARKLE_DIR="$DIST_DIR/sparkle"
DMG_PATH="$DIST_DIR/Vixen-${RELEASE_VERSION}.dmg"
APPCAST_PATH="$SPARKLE_DIR/appcast.xml"

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

echo "Re-signing app bundle with '$SIGNING_IDENTITY'..."
codesign --force --deep --options runtime --timestamp --sign "$SIGNING_IDENTITY" "$APP_BUNDLE_PATH"
codesign --verify --deep --strict --verbose=2 "$APP_BUNDLE_PATH"

APP_STAGING_PATH="$STAGING_DIR/Vixen.app"
ditto "$APP_BUNDLE_PATH" "$APP_STAGING_PATH"
ln -s /Applications "$STAGING_DIR/Applications"

echo "Creating DMG $DMG_PATH..."
hdiutil create \
  -volname "Vixen" \
  -srcfolder "$STAGING_DIR" \
  -format UDZO \
  -ov \
  "$DMG_PATH"

echo "Submitting DMG for notarization..."
xcrun notarytool submit "$DMG_PATH" --keychain-profile "$NOTARY_PROFILE" --wait

echo "Stapling notarization ticket..."
xcrun stapler staple "$DMG_PATH"

ditto "$DMG_PATH" "$SPARKLE_DIR/"

SPARKLE_TOOLS_ROOT="${VX_MAC_SPARKLE_TOOLS_ROOT:-$XCODE_DIR/build/SourcePackages/artifacts/sparkle/Sparkle/bin}"
GENERATE_APPCAST="$SPARKLE_TOOLS_ROOT/generate_appcast"
if [[ ! -x "$GENERATE_APPCAST" ]]; then
  CANDIDATE="$(find "$XCODE_DIR/build/SourcePackages" -type f -name generate_appcast -print -quit)"
  if [[ -n "$CANDIDATE" ]]; then
    GENERATE_APPCAST="$CANDIDATE"
  fi
fi

if [[ ! -x "$GENERATE_APPCAST" ]]; then
  echo "Could not find Sparkle's generate_appcast binary. Set VX_MAC_SPARKLE_TOOLS_ROOT or ensure Sparkle tools are present." >&2
  exit 1
fi

DOWNLOAD_PREFIX="https://github.com/${RELEASE_REPO}/releases/download/${TAG}/"

echo "Generating Sparkle appcast..."
"$GENERATE_APPCAST" \
  --account "$SPARKLE_ACCOUNT" \
  --download-url-prefix "$DOWNLOAD_PREFIX" \
  -o "$APPCAST_PATH" \
  "$SPARKLE_DIR"

if [[ ! -f "$APPCAST_PATH" ]]; then
  echo "generate_appcast did not produce $APPCAST_PATH" >&2
  exit 1
fi

python3 - "$APPCAST_PATH" << 'PY'
import sys
from xml.etree import ElementTree as ET

path = sys.argv[1]
tree = ET.parse(path)
root = tree.getroot()
ns = {"rss": "http://www.w3.org/2005/Atom"}
channel = root.find("channel", ns)
if channel is None:
    raise SystemExit("Invalid appcast: missing channel")
items = channel.findall("item", ns)
if len(items) != 1:
    raise SystemExit(f"Expected exactly 1 appcast item for this release artifact set, got {len(items)}")
PY

NOTES_FILE="$(mktemp)"
printf "" > "$NOTES_FILE"

echo "Creating GitHub release in $RELEASE_REPO..."
gh release create "$TAG" \
  "$DMG_PATH#Vixen-${RELEASE_VERSION}.dmg" \
  "$APPCAST_PATH#appcast.xml" \
  --repo "$RELEASE_REPO" \
  --target "$RELEASE_TARGET" \
  --title "Vixen ${RELEASE_VERSION}" \
  --notes-file "$NOTES_FILE"

rm -f "$NOTES_FILE"

echo "Release complete:"
echo "  Repository: $RELEASE_REPO"
echo "  Tag:        $TAG"
echo "  DMG:        $DMG_PATH"
echo "  Appcast:    $APPCAST_PATH"
