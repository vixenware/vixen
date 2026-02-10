#!/usr/bin/env bash
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

if [[ ! -f "$DMG_PATH" ]]; then
  echo "DMG not found at $DMG_PATH — run 'just build $RELEASE_VERSION' first." >&2
  exit 1
fi

SUBMISSION_ID="${VX_MAC_SUBMISSION_ID:-}"

if [[ -n "$SUBMISSION_ID" ]]; then
  echo "Resuming with existing submission ID: $SUBMISSION_ID"
else
  echo "Submitting DMG for notarization..."
  SUBMIT_OUTPUT="$(xcrun notarytool submit "$DMG_PATH" --keychain-profile "$NOTARY_PROFILE" 2>&1)" || true
  echo "$SUBMIT_OUTPUT"

  SUBMISSION_ID="$(echo "$SUBMIT_OUTPUT" | awk '/  id:/{print $2; exit}')"

  if [[ -z "${SUBMISSION_ID:-}" ]]; then
    echo "No submission ID returned; cannot proceed." >&2
    exit 1
  fi
fi

echo "Submission ID: $SUBMISSION_ID"
echo "Polling for notarization result (timeout: 4 hours)..."

DEADLINE=$(( $(date +%s) + 14400 ))
POLL_INTERVAL=15

while :; do
  if [[ $(date +%s) -ge $DEADLINE ]]; then
    echo "Timed out waiting for notarization after 4 hours." >&2
    echo "Fetching log for submission $SUBMISSION_ID..."
    xcrun notarytool log "$SUBMISSION_ID" --keychain-profile "$NOTARY_PROFILE" 2>/dev/null || true
    exit 1
  fi

  INFO="$(xcrun notarytool info "$SUBMISSION_ID" --keychain-profile "$NOTARY_PROFILE" 2>&1)" || true

  if echo "$INFO" | grep -q 'status: Accepted'; then
    echo "$INFO"
    echo "Notarization accepted."
    break
  fi

  if echo "$INFO" | grep -q 'status: Invalid'; then
    echo "$INFO"
    echo ""
    echo "Notarization failed. Fetching log for submission $SUBMISSION_ID..."
    xcrun notarytool log "$SUBMISSION_ID" --keychain-profile "$NOTARY_PROFILE"
    exit 1
  fi

  if echo "$INFO" | grep -q 'status: In Progress'; then
    printf "."
  else
    # Transient error (connectTimeout, etc.) — keep polling
    printf "x"
  fi

  sleep "$POLL_INTERVAL"
done

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

echo ""
echo "Release complete:"
echo "  Repository: $RELEASE_REPO"
echo "  Tag:        $TAG"
echo "  DMG:        $DMG_PATH"
echo "  Appcast:    $APPCAST_PATH"
