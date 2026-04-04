#!/usr/bin/env bash
set -euo pipefail

app_bundle_path="${1:-/Applications/Vixen.app}"
extension_bin="$app_bundle_path/Contents/Extensions/VixenFS.appex"

if [[ ! -d "$app_bundle_path" ]]; then
  echo "Expected app bundle at $app_bundle_path" >&2
  exit 1
fi

if [[ ! -d "$extension_bin" ]]; then
  echo "Expected extension at $extension_bin" >&2
  exit 1
fi

echo "Unregistering any discovered VixenFS extension"
pluginkit -r "$extension_bin" || true

echo "Re-registering installed VixenFS extension"
pluginkit -a "$extension_bin"

if [[ -x /System/Library/Frameworks/CoreServices.framework/Versions/Current/Frameworks/LaunchServices.framework/Versions/Current/Support/lsregister ]]; then
  /System/Library/Frameworks/CoreServices.framework/Versions/Current/Frameworks/LaunchServices.framework/Versions/Current/Support/lsregister -f -R -trusted "$app_bundle_path"
fi

echo "=== pluginkit FSKit registration ==="
pluginkit -m -p com.apple.fskit.fsmodule | rg "eu\\.bearcove\\.vixenfs|vixenfs|vixen" || true
