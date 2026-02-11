#!/usr/bin/env bash
set -euo pipefail

# Build vx-store, vx-runner, vxd, vx
cargo build --verbose --features diagnostics

# HTTP download cache persists across runs (regular files, safe in ~/Library/Caches)
if [[ "$(uname)" == "Darwin" ]]; then
    HTTP_CACHE="${HOME}/Library/Caches/vx-test-http-cache"
else
    HTTP_CACHE="${XDG_CACHE_HOME:-${HOME}/.cache}/vx-test-http-cache"
fi


# Resolve absolute paths to built binaries
SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-${SCRIPT_DIR}/target}"
VXD_BIN="${TARGET_DIR}/debug/vxd"
VX_BIN="${TARGET_DIR}/debug/vx"

if [[ ! -x "${VXD_BIN}" ]]; then
    echo "ERROR: ${VXD_BIN} not found. build-daemons should run first."
    exit 1
fi

# VX_HOME goes in /tmp — sockets and VFS mounts must NOT be in ~/Library/Caches
# or any location that macOS services scan (Finder, Spotlight, etc.), because
# stale mounts cause system-wide hangs.
export VX_HOME="/tmp/.vx-test/nextest/home"
export VX_TEST=1
export VX_HTTP_CACHE_DIR="${HTTP_CACHE}"
export RUST_LOG="${RUST_LOG:-info}"

mkdir -p "${VX_HOME}"

# Kill any stale daemon (and its children: store, runner) from a previous run.
# vx kill handles all cases: graceful RPC shutdown if vxd is alive, PID-file
# cleanup if vxd is dead but children survived, and socket/pidfile removal.
"${VX_BIN}" kill || echo "NOTE: vx kill reported no daemon running (clean slate)"
pkill "vxd" || true # Actually still useful

# Spawn shared daemon. VX_TEST skips die_with_parent() so the daemon
# outlives this setup script. VX_NEXTEST_PID is needed because the
# process tree walk can't find nextest through pipe subshells.
VX_NEXTEST_PID="${PPID}" "${VXD_BIN}" &
DAEMON_PID=$!

# Build a warmup project to trigger toolchain download + materialization
WARMUP_DIR=$(mktemp -d)
trap 'rm -rf "${WARMUP_DIR}"' EXIT

mkdir -p "${WARMUP_DIR}/src"
cat > "${WARMUP_DIR}/Cargo.toml" << 'TOML'
[package]
name = "warmup"
version = "0.1.0"
edition = "2021"
TOML
echo 'fn main() {}' > "${WARMUP_DIR}/src/main.rs"

warmup_ok=0
for attempt in $(seq 1 40); do
    if warmup_output=$(cd "${WARMUP_DIR}" && "${VX_BIN}" build --no-spawn --no-tui 2>&1); then
        warmup_ok=1
        break
    fi

    if grep -q "failed to connect to vxd" <<<"${warmup_output}"; then
        sleep 0.25
        continue
    fi

    echo "${warmup_output}"
    echo "Warmup build failed with a real build error (not a daemon startup race)."
    exit 1
done

if [[ "${warmup_ok}" -ne 1 ]]; then
    echo "Warmup build failed after waiting for daemon startup."
    echo "Last error:"
    echo "${warmup_output:-<none>}"
    exit 1
fi

# Export connection info for all tests. TestEnv::new() reads these to skip
# spawning and connect to this daemon instead.
{
    echo "VX_HOME=${VX_HOME}"
    echo "VX_TEST=${VX_TEST}"
} >> "${NEXTEST_ENV}"

echo "Shared daemon running (pid ${DAEMON_PID}) at VX_HOME=${VX_HOME}"
