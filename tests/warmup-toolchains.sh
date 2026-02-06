#!/usr/bin/env bash
set -euo pipefail

# Nextest setup script: prime a shared CAS with toolchains so per-test
# CAS clients can read from it via LayeredStorage (VX_CAS_BASE).
#
# This runs once before all tests. When the shared CAS already has
# toolchains from a previous run, this is a fast no-op.

# Platform-appropriate cache directory
if [[ "$(uname)" == "Darwin" ]]; then
    CACHE_BASE="${HOME}/Library/Caches/vx-test-cas"
    HTTP_CACHE="${HOME}/Library/Caches/vx-test-http-cache"
else
    CACHE_BASE="${XDG_CACHE_HOME:-${HOME}/.cache}/vx-test-cas"
    HTTP_CACHE="${XDG_CACHE_HOME:-${HOME}/.cache}/vx-test-http-cache"
fi

CAS_DIR="${CACHE_BASE}/cass"

# Fast path: if toolchain specs already exist, skip warmup.
# The toolchains/ dir in CAS holds spec->manifest mappings; if it has
# content, the full toolchain data (blobs, manifests) is also present.
if [[ -d "${CAS_DIR}/toolchains" ]] && [[ -n "$(ls -A "${CAS_DIR}/toolchains" 2>/dev/null)" ]]; then
    echo "Toolchains already in shared CAS at ${CAS_DIR}"
    echo "VX_CAS_BASE=${CAS_DIR}" >> "${NEXTEST_ENV}"
    exit 0
fi

echo "Priming shared CAS with toolchains..."

# Create a minimal project to trigger toolchain fetch
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

# Resolve absolute paths to built binaries (build-daemons ran first)
SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
AETHER_BIN="${SCRIPT_DIR}/target/debug/vx-aether"
VX_BIN="${SCRIPT_DIR}/target/debug/vx"

if [[ ! -x "${AETHER_BIN}" ]]; then
    echo "ERROR: ${AETHER_BIN} not found. build-daemons should run first."
    exit 1
fi

# Spawn daemon at shared VX_HOME
export VX_HOME="${CACHE_BASE}"
export VX_HTTP_CACHE_DIR="${HTTP_CACHE}"
export VX_INHERIT_STDIO=1
export RUST_LOG="${RUST_LOG:-info}"

"${AETHER_BIN}" &
DAEMON_PID=$!

# Give daemon time to initialize (spawn cass, rhea, bind sockets)
sleep 2

# Build the warmup project to trigger toolchain download + materialization
(cd "${WARMUP_DIR}" && "${VX_BIN}" build --no-spawn) || {
    echo "WARNING: warmup build failed, tests will still work but may be slower"
    kill "${DAEMON_PID}" 2>/dev/null || true
    wait "${DAEMON_PID}" 2>/dev/null || true
    exit 0
}

# Shut down the warmup daemon
kill "${DAEMON_PID}" 2>/dev/null || true
wait "${DAEMON_PID}" 2>/dev/null || true

echo "Shared CAS primed at ${CAS_DIR}"
echo "VX_CAS_BASE=${CAS_DIR}" >> "${NEXTEST_ENV}"
