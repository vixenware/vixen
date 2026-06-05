#!/usr/bin/env bash
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

fail() {
  echo "error: $*" >&2
  exit 1
}

[[ -n "$PUBLIC_BASE_URL" ]] || fail "set VX_MAC_PUBLIC_BASE_URL to the public HTTPS root for the mac app bucket"
[[ -n "$R2_BUCKET" && "$R2_ENDPOINT" != "$R2_BUCKET_URL" ]] || fail "VX_MAC_R2_BUCKET_URL must include the bucket name"
[[ -n "$R2_ACCESS_KEY_ID" ]] || fail "set ACCESS_KEY_ID or AWS_ACCESS_KEY_ID for the Cloudflare R2 token"
[[ -n "$R2_SECRET_ACCESS_KEY" ]] || fail "set SECRET_ACCESS_KEY or AWS_SECRET_ACCESS_KEY for the Cloudflare R2 token"
[[ -f "$DMG_PATH" ]] || fail "DMG not found at $DMG_PATH"

R2_LATEST_PREFIX="${VX_MAC_R2_LATEST_PREFIX:-latest}"
R2_LATEST_PREFIX="${R2_LATEST_PREFIX#/}"
R2_LATEST_PREFIX="${R2_LATEST_PREFIX%/}"
[[ -n "$R2_LATEST_PREFIX" ]] || fail "VX_MAC_R2_LATEST_PREFIX must not be empty"
LATEST_DMG_KEY="${R2_LATEST_PREFIX}/Vixen.dmg"
LATEST_PKG_KEY="${R2_LATEST_PREFIX}/Vixen-Installer.pkg"
LATEST_MANIFEST_KEY="${VX_MAC_R2_LATEST_MANIFEST_KEY:-latest.json}"
LATEST_MANIFEST_KEY="${LATEST_MANIFEST_KEY#/}"
[[ -n "$LATEST_MANIFEST_KEY" ]] || fail "VX_MAC_R2_LATEST_MANIFEST_KEY must not be empty"
LATEST_MANIFEST_PATH="$SPARKLE_DIR/latest.json"

rm -rf "$SPARKLE_DIR"
mkdir -p "$SPARKLE_DIR"

SPARKLE_DMG="$SPARKLE_DIR/$(basename "$DMG_PATH")"
echo "Copying DMG to Sparkle directory..."
ditto "$DMG_PATH" "$SPARKLE_DMG"

SPARKLE_TOOLS_ROOT="${VX_MAC_SPARKLE_TOOLS_ROOT:-$XCODE_DIR/build/SourcePackages/artifacts/sparkle/Sparkle/bin}"
GENERATE_APPCAST="$SPARKLE_TOOLS_ROOT/generate_appcast"
if [[ ! -x "$GENERATE_APPCAST" ]]; then
  CANDIDATE="$(find "$XCODE_DIR/build/SourcePackages" -type f -name generate_appcast -print -quit)"
  if [[ -n "$CANDIDATE" ]]; then
    GENERATE_APPCAST="$CANDIDATE"
  fi
fi

[[ -x "$GENERATE_APPCAST" ]] || fail "could not find Sparkle generate_appcast; set VX_MAC_SPARKLE_TOOLS_ROOT"

echo "Generating Sparkle appcast..."
"$GENERATE_APPCAST" \
  --account "$SPARKLE_ACCOUNT" \
  --download-url-prefix "${PUBLIC_RELEASE_URL}/" \
  -o "$APPCAST_PATH" \
  "$SPARKLE_DIR"

[[ -f "$APPCAST_PATH" ]] || fail "generate_appcast did not produce $APPCAST_PATH"

python3 - "$APPCAST_PATH" << 'PY'
import sys
from xml.etree import ElementTree as ET

path = sys.argv[1]
tree = ET.parse(path)
root = tree.getroot()
channel = root.find("channel")
if channel is None:
    raise SystemExit("Invalid appcast: missing channel")
items = channel.findall("item")
if len(items) != 1:
    raise SystemExit(f"Expected exactly 1 appcast item for this release artifact set, got {len(items)}")
PY

aws_cp() {
  local src="$1"
  local key="$2"
  local content_type="$3"
  local cache_control="$4"
  AWS_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID" \
    AWS_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY" \
    AWS_EC2_METADATA_DISABLED=true \
    AWS_DEFAULT_REGION="${AWS_DEFAULT_REGION:-auto}" \
    aws --endpoint-url "$R2_ENDPOINT" s3 cp \
      "$src" "s3://${R2_BUCKET}/${key}" \
      --content-type "$content_type" \
      --cache-control "$cache_control"
}

echo "Uploading release artifacts to R2..."
aws_cp "$DMG_PATH" "${R2_RELEASE_PREFIX}/$(basename "$DMG_PATH")" \
  "application/octet-stream" "public, max-age=31536000, immutable"
aws_cp "$DMG_PATH" "$LATEST_DMG_KEY" \
  "application/octet-stream" "public, max-age=300"
aws_cp "$APPCAST_PATH" "${R2_RELEASE_PREFIX}/appcast.xml" \
  "application/xml" "public, max-age=300"
aws_cp "$APPCAST_PATH" "$R2_APPCAST_KEY" \
  "application/xml" "public, max-age=300"

if [[ -f "$SIGNED_PKG_PATH" ]]; then
  aws_cp "$SIGNED_PKG_PATH" "${R2_RELEASE_PREFIX}/$(basename "$SIGNED_PKG_PATH")" \
    "application/octet-stream" "public, max-age=31536000, immutable"
  aws_cp "$SIGNED_PKG_PATH" "$LATEST_PKG_KEY" \
    "application/octet-stream" "public, max-age=300"
else
  echo "Installer pkg not found at $SIGNED_PKG_PATH; skipping pkg upload."
fi

python3 - "$LATEST_MANIFEST_PATH" "$RELEASE_VERSION" "$TAG" \
  "${PUBLIC_RELEASE_URL}/$(basename "$DMG_PATH")" \
  "${PUBLIC_BASE_URL}/${LATEST_DMG_KEY}" \
  "${PUBLIC_BASE_URL}/${R2_APPCAST_KEY}" \
  "${PUBLIC_RELEASE_URL}/$(basename "$SIGNED_PKG_PATH")" \
  "${PUBLIC_BASE_URL}/${LATEST_PKG_KEY}" \
  "$([[ -f "$SIGNED_PKG_PATH" ]] && printf yes || printf no)" << 'PY'
import json
import sys

(
    path,
    version,
    tag,
    release_dmg_url,
    latest_dmg_url,
    appcast_url,
    release_pkg_url,
    latest_pkg_url,
    has_pkg,
) = sys.argv[1:]

manifest = {
    "version": version,
    "tag": tag,
    "dmg": {
        "release_url": release_dmg_url,
        "latest_url": latest_dmg_url,
    },
    "appcast_url": appcast_url,
}

if has_pkg == "yes":
    manifest["installer"] = {
        "release_url": release_pkg_url,
        "latest_url": latest_pkg_url,
    }

with open(path, "w", encoding="utf-8") as f:
    json.dump(manifest, f, indent=2, sort_keys=True)
    f.write("\n")
PY

aws_cp "$LATEST_MANIFEST_PATH" "$LATEST_MANIFEST_KEY" \
  "application/json" "public, max-age=300"

echo ""
echo "R2 release complete:"
echo "  DMG:     ${PUBLIC_RELEASE_URL}/$(basename "$DMG_PATH")"
echo "  Latest:  ${PUBLIC_BASE_URL}/${LATEST_DMG_KEY}"
if [[ -f "$SIGNED_PKG_PATH" ]]; then
  echo "  PKG:     ${PUBLIC_RELEASE_URL}/$(basename "$SIGNED_PKG_PATH")"
  echo "  Latest:  ${PUBLIC_BASE_URL}/${LATEST_PKG_KEY}"
fi
echo "  Appcast: ${PUBLIC_BASE_URL}/${R2_APPCAST_KEY}"
echo "  Manifest: ${PUBLIC_BASE_URL}/${LATEST_MANIFEST_KEY}"
