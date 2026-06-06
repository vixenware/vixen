#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: clean-build-registration.sh /path/to/Vixen.app" >&2
  exit 64
fi

app_path="$1"
lsregister="/System/Library/Frameworks/CoreServices.framework/Versions/Current/Frameworks/LaunchServices.framework/Versions/Current/Support/lsregister"

if [[ ! -d "$app_path" ]]; then
  exit 0
fi

echo "Cleaning local registration for build product: $app_path"

for extension_dir in Extensions PlugIns; do
  appex_path="$app_path/Contents/$extension_dir/VixenFS.appex"
  if [[ -d "$appex_path" ]]; then
    /usr/bin/pluginkit -r "$appex_path" >/dev/null 2>&1 || true
  fi
done

if [[ -x "$lsregister" ]]; then
  "$lsregister" -u "$app_path" >/dev/null 2>&1 || true
fi
