#!/usr/bin/env bash
set -euo pipefail

app_bundle_path="${1:-/Applications/Vixen.app}"
extension_bin="$app_bundle_path/Contents/Extensions/VixenFS.appex"

if [[ ! -d "$app_bundle_path" ]]; then
  echo "Expected app at $app_bundle_path. Install Vixen.app first." >&2
  exit 1
fi

if [[ ! -d "$extension_bin" ]]; then
  echo "Expected extension bundle at $extension_bin. Install may have failed." >&2
  exit 1
fi

echo "=== app signature ==="
codesign -d --verbose=4 "$app_bundle_path"

echo "=== extension signature ==="
codesign -d --verbose=4 "$extension_bin"

echo "=== extension entitlements ==="
codesign -d --entitlements :- "$extension_bin" | plutil -p -

echo "=== pluginkit FSKit registration ==="
pluginkit -m -p com.apple.fskit.fsmodule | rg "eu\\.bearcove\\.vixenfs|vixenfs|vixen" || true
