#!/usr/bin/env bash
# Shared variables for build and publish scripts.
# Source this file, don't execute it directly.

SOURCE_PATH="${BASH_SOURCE[1]:-${BASH_SOURCE[0]}}"
SCRIPT_DIR="$(cd "$(dirname "$SOURCE_PATH")" && pwd)"
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

NOTARY_PROFILE="${VX_MAC_NOTARY_PROFILE:-}"
SIGNING_IDENTITY="${VX_MAC_SIGNING_IDENTITY:-Developer ID Application}"
SPARKLE_PUBLIC_ED_KEY="${VX_MAC_SPARKLE_PUBLIC_ED_KEY:-${VX_MAC_SPARKLE_PUBLIC_KEY:-}}"
SPARKLE_ACCOUNT="${VX_MAC_SPARKLE_KEY_ACCOUNT:-ed25519}"
TAG="v${RELEASE_VERSION}"

R2_BUCKET_URL="${VX_MAC_R2_BUCKET_URL:-https://4051116950f6ef4b78cb96847a2221f9.r2.cloudflarestorage.com/vixen-mac-app}"
R2_BUCKET_URL="${R2_BUCKET_URL%/}"
R2_ENDPOINT="${R2_BUCKET_URL%/*}"
R2_BUCKET="${R2_BUCKET_URL##*/}"
R2_RELEASE_PREFIX="${VX_MAC_R2_RELEASE_PREFIX:-releases/${TAG}}"
R2_RELEASE_PREFIX="${R2_RELEASE_PREFIX#/}"
R2_RELEASE_PREFIX="${R2_RELEASE_PREFIX%/}"
R2_APPCAST_KEY="${VX_MAC_R2_APPCAST_KEY:-appcast.xml}"
R2_APPCAST_KEY="${R2_APPCAST_KEY#/}"
PUBLIC_BASE_URL="${VX_MAC_PUBLIC_BASE_URL:-https://mac.app.vixen.rs}"
PUBLIC_BASE_URL="${PUBLIC_BASE_URL%/}"
PUBLIC_RELEASE_URL="${PUBLIC_BASE_URL}/${R2_RELEASE_PREFIX}"
SPARKLE_FEED_URL="${VX_MAC_SPARKLE_FEED_URL:-${PUBLIC_BASE_URL}/${R2_APPCAST_KEY}}"
R2_ACCESS_KEY_ID="${AWS_ACCESS_KEY_ID:-${ACCESS_KEY_ID:-}}"
R2_SECRET_ACCESS_KEY="${AWS_SECRET_ACCESS_KEY:-${SECRET_ACCESS_KEY:-}}"

DIST_DIR="$APP_DIR/dist/$RELEASE_VERSION"
STAGING_DIR="$DIST_DIR/staging"
SPARKLE_DIR="$DIST_DIR/sparkle"
DMG_PATH="$DIST_DIR/Vixen-${RELEASE_VERSION}.dmg"
APPCAST_PATH="$SPARKLE_DIR/appcast.xml"
SIGNED_PKG_PATH="$DIST_DIR/Vixen-Installer.pkg"

resolve_codesign_identity() {
  local requested="$1"
  local label="$2"

  if [[ "$requested" =~ ^[0-9A-Fa-f]{40}$ ]]; then
    printf "%s" "$requested"
    return 0
  fi

  local matches=()
  local hash name
  while IFS=$'\t' read -r hash name; do
    if [[ "$name" == "$requested" || "$name" == "$requested:"* ]]; then
      matches+=("$hash"$'\t'"$name")
    fi
  done < <(
    security find-identity -v 2>/dev/null \
      | sed -nE 's/^[[:space:]]*[0-9]+\) ([0-9A-Fa-f]{40}) "([^"]+)".*/\1\t\2/p'
  )

  if [[ "${#matches[@]}" -eq 0 ]]; then
    echo "No $label identity matching '$requested' was found." >&2
    return 1
  fi

  if [[ "${#matches[@]}" -gt 1 ]]; then
    echo "Multiple $label identities match '$requested'; using the first certificate hash." >&2
  fi

  IFS=$'\t' read -r hash name <<< "${matches[0]}"
  printf "%s" "$hash"
}
