#!/usr/bin/env bash
set -euo pipefail

app_bundle_path="${1:-/Applications/Vixen.app}"
extension_bin="$app_bundle_path/Contents/Extensions/VixenFS.appex"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [[ ! -d "$app_bundle_path" ]]; then
  echo "Expected app bundle at $app_bundle_path" >&2
  exit 1
fi

if [[ ! -d "$extension_bin" ]]; then
  echo "Expected extension at $extension_bin" >&2
  exit 1
fi

"$script_dir/fix-registration.js" "$app_bundle_path"
