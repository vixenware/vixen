#!/usr/bin/env bash
# Shared variables for build and publish scripts.
# Source this file, don't execute it directly.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[1]}")" && pwd)"
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

DIST_DIR="$APP_DIR/dist/$RELEASE_VERSION"
STAGING_DIR="$DIST_DIR/staging"
SPARKLE_DIR="$DIST_DIR/sparkle"
DMG_PATH="$DIST_DIR/Vixen-${RELEASE_VERSION}.dmg"
APPCAST_PATH="$SPARKLE_DIR/appcast.xml"
