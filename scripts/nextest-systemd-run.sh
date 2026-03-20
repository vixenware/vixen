#!/usr/bin/env bash
set -euo pipefail

memory_max="${VX_NEXTEST_MEMORY_MAX:-2G}"

exec systemd-run \
  --user \
  --scope \
  --quiet \
  --same-dir \
  -p "MemoryMax=${memory_max}" \
  "$@"
