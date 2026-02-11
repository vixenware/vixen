#!/bin/bash

echo "== Usual suspects"
pgrep -la '(vxd|vx-|Vixen|vixen)' || echo "no one"
echo

echo "== Stuck on test mount"
pre="/private/tmp/.vx-test/nextest/"
lsof -n -P -u "$USER" -Fpcn 2>/dev/null |
awk -v pre="$pre" '
  $0 ~ /^p/ {pid=substr($0,2); next}
  $0 ~ /^c/ {cmd=substr($0,2); next}
  $0 ~ /^n/ {
    path=substr($0,2);
    if (index(path, pre)==1) {
      print pid "\t" cmd "\t" path;
    }
  }'
echo
