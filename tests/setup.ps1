# Warmup toolchains for nextest on Windows
# Port of tests/warmup-toolchains.sh

$ErrorActionPreference = "Stop"

# Kill any stale vixen processes from previous runs that may hold exe files open.
# Without this, cargo cannot overwrite the binaries on Windows.
foreach ($proc in @("vxd", "vx-runner", "vx-store", "vx-vfsd")) {
    Get-Process -Name $proc -ErrorAction SilentlyContinue | ForEach-Object {
        Write-Host "Killing stale $proc (pid $($_.Id))"
        Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
    }
}

# Build vx-store, vx-runner, vxd, vx
cargo build --verbose
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# HTTP download cache persists across runs
$HTTP_CACHE = Join-Path $env:LOCALAPPDATA "vx-test-http-cache"

# Resolve absolute paths to built binaries
# $PSScriptRoot is the tests/ directory; go up one level to the repo root
$SCRIPT_DIR = Split-Path -Parent $PSScriptRoot
$TARGET_DIR = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $SCRIPT_DIR "target" }
$VXD_BIN = Join-Path $TARGET_DIR "debug\vxd.exe"
$VX_BIN = Join-Path $TARGET_DIR "debug\vx.exe"

if (-not (Test-Path $VXD_BIN)) {
    Write-Error "ERROR: $VXD_BIN not found. Build should run first."
    exit 1
}

# VX_HOME goes in TEMP to avoid interference from system indexers
$env:VX_HOME = Join-Path $env:TEMP ".vx-test\nextest\home"
$env:VX_TEST = "1"
$env:VX_HTTP_CACHE_DIR = $HTTP_CACHE
if (-not $env:RUST_LOG) { $env:RUST_LOG = "info" }

New-Item -ItemType Directory -Force -Path $env:VX_HOME | Out-Null

# Kill any stale daemon from a previous run
$ErrorActionPreference = "Continue"
& $VX_BIN kill 2>$null
$ErrorActionPreference = "Stop"
if ($LASTEXITCODE -ne 0) {
    Write-Host "NOTE: vx kill reported no daemon running (clean slate)"
}

# Spawn shared daemon. VX_TEST skips die_with_parent() so the daemon
# outlives this setup script.
$env:VXD = Join-Path $env:VX_HOME "vxd.sock"
$daemon = Start-Process -FilePath $VXD_BIN -PassThru -NoNewWindow

# Wait for daemon to become ready by pinging it. On Windows the endpoint
# is a named pipe (not a file on disk), so Test-Path cannot detect it.
$deadline = (Get-Date).AddSeconds(30)
$ready = $false
while (-not $ready) {
    if ((Get-Date) -gt $deadline) {
        Write-Host "vxd did not become ready within 30 seconds, running diagnostic ping:"
        Write-Host "  VXD=$($env:VXD)"
        & $VX_BIN ping
        Write-Error "vxd did not become ready within 30 seconds"
        exit 1
    }
    Start-Sleep -Milliseconds 500
    $ErrorActionPreference = "Continue"
    & $VX_BIN ping 2>$null >$null
    $ErrorActionPreference = "Stop"
    if ($LASTEXITCODE -eq 0) { $ready = $true }
}
Write-Host "vxd is ready (pid $($daemon.Id))"

# Build a warmup project to trigger toolchain download + materialization
$WARMUP_DIR = Join-Path $env:TEMP "vx-warmup-$(Get-Random)"
New-Item -ItemType Directory -Force -Path (Join-Path $WARMUP_DIR "src") | Out-Null

@"
[package]
name = "warmup"
version = "0.1.0"
edition = "2021"
"@ | Set-Content (Join-Path $WARMUP_DIR "Cargo.toml")

"fn main() {}" | Set-Content (Join-Path $WARMUP_DIR "src\main.rs")

try {
    Push-Location $WARMUP_DIR
    & $VX_BIN build --no-tui
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Warmup build failed, this means nothing'll work."
        exit 1
    }
} finally {
    Pop-Location
    Remove-Item -Recurse -Force $WARMUP_DIR -ErrorAction SilentlyContinue
}

# Export connection info for all tests
Add-Content $env:NEXTEST_ENV "VX_HOME=$($env:VX_HOME)"
Add-Content $env:NEXTEST_ENV "VX_TEST=$($env:VX_TEST)"
Add-Content $env:NEXTEST_ENV "VXD=$($env:VXD)"

Write-Host "Shared daemon running (pid $($daemon.Id)) at VX_HOME=$($env:VX_HOME)"
