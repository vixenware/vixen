#!/usr/bin/env bash
set -euo pipefail

# Build vx-store, vx-runner, vxd, vx
echo "Building vx-store, vx-runner, vxd, and vx..."
cargo build --verbose --features diagnostics

# Resolve absolute paths to built binaries
SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-${SCRIPT_DIR}/target}"

VX_BIN="${TARGET_DIR}/debug/vx"

# HTTP download cache persists across runs (regular files, safe in ~/Library/Caches)
if [[ "$(uname)" == "Darwin" ]]; then
    VX_HTTP_CACHE_DIR="${HOME}/Library/Caches/vx-test-http-cache"
else
    VX_HTTP_CACHE_DIR="${XDG_CACHE_HOME:-${HOME}/.cache}/vx-test-http-cache"
fi
export VX_HTTP_CACHE_DIR
echo "HTTP cache dir is at ${VX_HTTP_CACHE_DIR}"

# VX_HOME goes in /tmp — sockets and VFS mounts must NOT be in ~/Library/Caches
# or any location that macOS services scan (Finder, Spotlight, etc.), because
# stale mounts cause system-wide hangs.
export VX_HOME="/tmp/.vx-test/nextest/home"
mkdir -p "${VX_HOME}"
echo "Test VX_HOME will be ${VX_HOME}"

# These together make vxd die (and take its children with it) when nextest exits.
export VX_TEST=1
export VX_NEXTEST_PID="${PPID}"

"$VX_BIN" start

# Export connection info for all tests. TestEnv::new() reads these to skip
# spawning and connect to this daemon instead.
{
    echo "VX_HOME=${VX_HOME}"
    echo "VX_TEST=${VX_TEST}"
    echo "VX_HTTP_CACHE_DIR=${VX_HTTP_CACHE_DIR}"
    echo "VX_NEXTEST_PID=${VX_NEXTEST_PID}"
} >> "${NEXTEST_ENV}"
