#!/usr/bin/env bash
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

TAP_DIR="${VX_MAC_TAP_DIR:-$HOME/bearcove/homebrew-tap}"

if [[ ! -d "$TAP_DIR" ]]; then
  echo "Tap directory not found at $TAP_DIR" >&2
  exit 1
fi

if [[ ! -f "$DMG_PATH" ]]; then
  echo "DMG not found at $DMG_PATH — run publish first." >&2
  exit 1
fi

DOWNLOAD_URL="https://github.com/${RELEASE_REPO}/releases/download/${TAG}/Vixen-${RELEASE_VERSION}.dmg"

echo "Computing SHA256 of $DMG_PATH..."
SHA256="$(shasum -a 256 "$DMG_PATH" | awk '{print $1}')"

echo "SHA256: $SHA256"

cd "$TAP_DIR"

echo "Pulling latest tap changes..."
git pull --rebase

FORMULA_PATH="Casks/vixen.rb"

if [[ ! -f "$FORMULA_PATH" ]]; then
  echo "Formula not found at $TAP_DIR/$FORMULA_PATH" >&2
  exit 1
fi

echo "Updating formula..."

# Update version, URL, and SHA256
sed -i '' -E "s|version \"[^\"]+\"|version \"$RELEASE_VERSION\"|" "$FORMULA_PATH"
sed -i '' -E "s|url \"[^\"]+\"|url \"$DOWNLOAD_URL\"|" "$FORMULA_PATH"
sed -i '' -E "s|sha256 \"[^\"]+\"|sha256 \"$SHA256\"|" "$FORMULA_PATH"

echo "Committing changes..."
git add "$FORMULA_PATH"
git commit -m "vixen: update to $RELEASE_VERSION"

echo "Pushing to tap..."
git push

echo ""
echo "Tap updated:"
echo "  Formula: $FORMULA_PATH"
echo "  Version: $RELEASE_VERSION"
echo "  SHA256:  $SHA256"
