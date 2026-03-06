#!/usr/bin/env bash
set -euo pipefail

# Build vx-store, vx-runner, vxd, vx
CARGO_FEATURES=""
if [[ "${VX_DIAGNOSTICS:-}" == "1" ]]; then
    echo "VX_DIAGNOSTICS=1: enabling diagnostics feature. Run './target/debug/vx debug' on hang to open a web dashboard"
    CARGO_FEATURES="--features diagnostics"
else
    echo "Tip: set VX_DIAGNOSTICS=1 to build with the diagnostics feature (slower build, richer errors)"
fi
echo "Building vx-store, vx-runner, vxd, and vx..."
cargo build --verbose $CARGO_FEATURES

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
# stale mounts cause system-wide hangs. Use a fresh directory per nextest run so
# dead mounts or stale sockets from an earlier crash cannot poison the current run.
export VX_HOME="$(mktemp -d /tmp/.vx-test.XXXXXX)"
echo "Test VX_HOME will be ${VX_HOME}"

# VX_TEST=1 enables test mode: 2s idle timeout, inherit stderr
export VX_TEST=1

# Start daemon
VX_LOG="/tmp/vx-tests.log"
if [[ "${VX_VERBOSE:-}" == "1" ]]; then
    echo "VX_VERBOSE=1: daemon output will go to stderr"
    "$VX_BIN" start --no-tui &
else
    rm "$VX_LOG" || true
    echo "Daemon output redirected to ${VX_LOG} (set VX_VERBOSE=1 for inline output)"
    "$VX_BIN" start --no-tui > "$VX_LOG" 2>&1 &
fi

# Set VXD so no other command starts their own
export VXD="$VX_HOME/vxd.sock"
"$VX_BIN" ping

# Export connection info for all tests. TestEnv::new() reads these to skip
# spawning and connect to this daemon instead.
{
    echo "VX_HOME=${VX_HOME}"
    echo "VX_TEST=${VX_TEST}"
    echo "VXD=${VXD}"
} >> "${NEXTEST_ENV}"
