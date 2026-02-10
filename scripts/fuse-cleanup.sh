#!/usr/bin/env bash
set -euo pipefail

for conn in /sys/fs/fuse/connections/*/abort; do
  [ -e "$conn" ] && echo 1 | sudo tee "$conn"
done

# Unmount all vixen FUSE mounts (nextest, stress test, etc.)
grep -o '/tmp/vixenfs[^ ]*' /proc/mounts 2>/dev/null | while read -r mp; do
  echo "unmounting $mp"
  fusermount3 -uz "$mp" 2>/dev/null || true
done

if grep -q vixen /proc/mounts 2>/dev/null; then
  echo "STILL MOUNTED"
  exit 1
else
  echo "clean"
fi
