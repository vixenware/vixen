#!/usr/bin/env bash
set -euo pipefail

if [[ "$(id -u)" != "0" ]]; then
  echo "run with sudo: sudo bash apps/mac/scripts/debug-simple-fskit-reset.sh" >&2
  exit 64
fi

LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Versions/Current/Frameworks/LaunchServices.framework/Versions/Current/Support/lsregister"
APP="/Applications/Vixen.app"
EXT="$APP/Contents/Extensions/VixenFS.appex"
HELPER="$APP/Contents/MacOS/vixen-management"

console_uid="$(/usr/bin/stat -f "%u" /dev/console)"
console_user="$(/usr/bin/stat -f "%Su" /dev/console)"

run_as_console_user() {
  /bin/launchctl asuser "$console_uid" /usr/bin/sudo -u "$console_user" "$@"
}

list_vixen_app_records() {
  "$@" -dump | /usr/bin/awk '
    BEGIN { path = ""; id = "" }
    /^--------------------------------------------------------------------------------/ {
      if (id == "eu.bearcove.vixen" && path ~ /\/Vixen\.app$/) print path
      path = ""; id = ""; next
    }
    /^[[:space:]]*path:/ {
      sub(/^[[:space:]]*path:[[:space:]]*/, "")
      sub(/[[:space:]]*\(0x[0-9a-fA-F]+\).*$/, "")
      path = $0
    }
    /^[[:space:]]*identifier:/ {
      sub(/^[[:space:]]*identifier:[[:space:]]*/, "")
      id = $0
    }
    END {
      if (id == "eu.bearcove.vixen" && path ~ /\/Vixen\.app$/) print path
    }
  '
}

print_vixenfs_ls_record() {
  "$@" -dump | /usr/bin/awk '
    BEGIN { show = 0 }
    /^plugin id:[[:space:]]+eu\.bearcove\.vixenfs/ { show = 1 }
    show { print }
    show && /^--------------------------------------------------------------------------------/ { exit }
  '
}

section() {
  printf '\n== %s ==\n' "$1"
}

section "console user"
printf 'uid=%s user=%s\n' "$console_uid" "$console_user"

section "user Vixen.app LaunchServices before"
run_as_console_user bash -c "$(declare -f list_vixen_app_records); list_vixen_app_records '$LSREGISTER'"

section "root Vixen.app LaunchServices before"
list_vixen_app_records "$LSREGISTER"

section "user VixenFS PlugInKit before"
run_as_console_user /usr/bin/pluginkit -mAvvv -i eu.bearcove.vixenfs || true

section "root VixenFS PlugInKit before"
/usr/bin/pluginkit -mAvvv -i eu.bearcove.vixenfs || true

section "root VixenFS LaunchServices before"
print_vixenfs_ls_record "$LSREGISTER" || true

section "remove non-installed user Vixen.app records"
while IFS= read -r path; do
  [[ -n "$path" ]] || continue
  [[ "$path" == "$APP" ]] && continue
  printf 'user lsregister -u %s\n' "$path"
  run_as_console_user "$LSREGISTER" -u "$path" || true
done < <(run_as_console_user bash -c "$(declare -f list_vixen_app_records); list_vixen_app_records '$LSREGISTER'")

section "remove non-installed root Vixen.app records"
while IFS= read -r path; do
  [[ -n "$path" ]] || continue
  [[ "$path" == "$APP" ]] && continue
  printf 'root lsregister -u %s\n' "$path"
  "$LSREGISTER" -u "$path" || true
done < <(list_vixen_app_records "$LSREGISTER")

section "keep installed root VixenFS appex record"
if [[ -d "$EXT" ]]; then
  printf 'keeping %s\n' "$EXT"
else
  printf 'missing %s\n' "$EXT"
fi

section "user VixenFS PlugInKit after cleanup"
run_as_console_user /usr/bin/pluginkit -mAvvv -i eu.bearcove.vixenfs || true

section "root VixenFS PlugInKit after cleanup"
/usr/bin/pluginkit -mAvvv -i eu.bearcove.vixenfs || true

section "root VixenFS LaunchServices after cleanup"
print_vixenfs_ls_record "$LSREGISTER" || true

section "kill fskitd"
"$HELPER" kill-fskitd || true
/bin/sleep 3

section "mount"
"$HELPER" mount || true
/bin/sleep 2

section "management last action"
if [[ -f "/Library/Application Support/Vixen/management/last-action.json" ]]; then
  /bin/cat "/Library/Application Support/Vixen/management/last-action.json"
  printf '\n'
else
  echo "no last-action.json"
fi

section "mount table"
/sbin/mount | /usr/bin/awk 'tolower($0) ~ /vixen|vixenfs|\/volumes\/vixen/ { print }' || true

section "recent FSKit Vixen logs"
/usr/bin/log show --style compact --last 5m --predicate \
  '(process == "fskitd" OR process == "fskit_agent" OR process == "VixenFS") AND (eventMessage CONTAINS[c] "eu.bearcove.vixenfs" OR eventMessage CONTAINS[c] "disabled" OR eventMessage CONTAINS[c] "module" OR eventMessage CONTAINS[c] "Recognized")' || true
