//! vxd - Build orchestration daemon
//!
//! The daemon orchestrates builds but never touches the filesystem directly.
//! All file I/O and compilation is delegated to vx-runner.
//!
//! Responsibilities:
//! - Own the picante incremental computation database
//! - Compute cache keys
//! - Check store for cache hits
//! - Send compile requests to execd
//! - Materialize final outputs locally (via execd or direct store fetch)

mod action_graph;
mod build_failure;
mod cas_tree;
mod db;
mod error;
mod executor;
mod inputs;
mod queries;
mod service;

#[cfg(test)]
mod tests;

use ::moire::process::Child;
use ::moire::sync::SyncMutex;
use base64::Engine;
use camino::{Utf8Path, Utf8PathBuf};
use eyre::Result;
use facet::Facet;
use figue as args;
use moire::task::FutureExt;
use roam_core::acceptor;
use roam_core::{AcceptedConnection, BareConduit, ConnectionAcceptor, Driver};
use roam_stream::StreamLink;
use roam_types::{ConnectionSettings, Metadata};
use std::fs;
use std::fs::OpenOptions;
use std::net::TcpListener;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vx_io::boot::{BootDispatcher, BootGate, BootTx};
use vx_io::net::{Endpoint, Listener};
use vx_io::spawn::ServiceHandle;
use vx_runner_proto::RunnerClient;
use vx_store_proto::StoreClient;
use vxd_proto::{ProgressListenerClient, VxdDispatcher};

use crate::service::VxdServiceDeps;
pub use db::Database;
pub use inputs::*;
pub use queries::*;
pub use service::VxdService;

pub type UpstreamCaller = roam_core::DriverCaller;

const RECOMMENDED_NOFILE_SOFT_LIMIT: u64 = 65_536;
const VIXEN_REPO_MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");
#[cfg(unix)]
const SSH_BOOTSTRAP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// vxd - Build orchestration daemon
#[derive(Facet, Debug)]
struct CliArgs {
    /// Listen endpoint (default: $VX_HOME/vxd.sock)
    #[facet(default, args::named)]
    listen: Option<String>,

    /// VX home directory (default: ~/.vx)
    #[facet(default, args::named)]
    vx_home: Option<String>,

    /// vx-store endpoint — if absent, spawns vx-store automatically
    #[facet(default, args::named)]
    store: Option<String>,

    /// vx-runner endpoint — if absent, spawns vx-runner automatically
    #[facet(default, args::named)]
    runner: Option<String>,

    /// Standard CLI options (--help, --version, --completions)
    #[facet(flatten)]
    builtins: args::FigueBuiltins,
}

#[derive(Clone, Debug)]
struct Args {
    /// VX_HOME directory
    vx_home: Utf8PathBuf,

    /// Store endpoint — Some if VX_STORE is set (connect to existing), None means spawn
    store_endpoint: Option<Endpoint>,

    /// Exec endpoint — Some if VX_RUNNER is set (connect to existing), None means spawn
    exec_endpoint: Option<Endpoint>,

    /// Bind endpoint
    bind: Endpoint,

    /// Additional build.vx executor class mappings.
    build_vx_executor_classes: Vec<BuildVxExecutorClassConfig>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BuildVxExecutorClassConfig {
    pub(crate) class_label: String,
    pub(crate) backend: vx_eval::ExecutorBackendKind,
    pub(crate) target: Option<String>,
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct SshBootstrapPaths {
    bare_repo_dir: String,
    checkout_dir: String,
    vx_home: String,
    store_root: String,
    store_socket: String,
    vfsd_socket: String,
    runner_socket: String,
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct SshBootstrapWindowsPaths {
    bare_repo_dir: String,
    checkout_dir: String,
    vx_home: String,
    store_root: String,
    store_endpoint: String,
    vfsd_endpoint: String,
    runner_endpoint: String,
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum SshBootstrapTargetPlatform {
    Unix,
    Windows,
}

#[cfg(unix)]
impl SshBootstrapTargetPlatform {
    fn as_tag(&self) -> &'static str {
        match self {
            Self::Unix => "unix",
            Self::Windows => "windows",
        }
    }
}

#[cfg(unix)]
impl SshBootstrapTargetInfo {
    fn windows_bootstrap_root(&self) -> &str {
        self.windows_bootstrap_root
            .as_deref()
            .expect("windows bootstrap root only exists for Windows targets")
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct SshBootstrapTargetInfo {
    platform: SshBootstrapTargetPlatform,
    home: String,
    windows_bootstrap_root: Option<String>,
}

pub(crate) fn parse_build_vx_executor_classes(
    value: &str,
) -> Result<Vec<BuildVxExecutorClassConfig>> {
    let mut configs = Vec::new();
    let mut seen = std::collections::HashSet::<String>::new();

    for raw_entry in value.split(';') {
        let entry = raw_entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (class_label, raw_spec) = entry.split_once('=').ok_or_else(|| {
            eyre::eyre!(
                "invalid VX_BUILD_EXECUTOR_CLASSES entry {entry:?} (expected class=backend[:target])"
            )
        })?;
        let class_label = class_label.trim();
        let raw_spec = raw_spec.trim();
        if class_label.is_empty() {
            eyre::bail!("invalid VX_BUILD_EXECUTOR_CLASSES entry {entry:?} (empty class label)");
        }
        if !seen.insert(class_label.to_string()) {
            eyre::bail!(
                "invalid VX_BUILD_EXECUTOR_CLASSES: class {class_label:?} appears more than once"
            );
        }

        let (backend, target) = if raw_spec == "local" {
            (vx_eval::ExecutorBackendKind::Local, None)
        } else {
            let (backend_tag, target) = raw_spec.split_once(':').ok_or_else(|| {
                eyre::eyre!(
                    "invalid VX_BUILD_EXECUTOR_CLASSES spec {raw_spec:?} for class {class_label:?}"
                )
            })?;
            let backend = vx_eval::ExecutorBackendKind::from_tag(backend_tag.trim())
                .ok_or_else(|| eyre::eyre!("unknown build.vx executor backend {backend_tag:?}"))?;
            (backend, Some(target.trim().to_string()))
        };

        match backend {
            vx_eval::ExecutorBackendKind::Local => {
                if target.is_some() {
                    eyre::bail!(
                        "invalid VX_BUILD_EXECUTOR_CLASSES spec {raw_spec:?}: local executors do not take a target"
                    );
                }
            }
            vx_eval::ExecutorBackendKind::Daemon | vx_eval::ExecutorBackendKind::SshBootstrap => {
                let Some(target) = target.as_ref() else {
                    eyre::bail!(
                        "invalid VX_BUILD_EXECUTOR_CLASSES spec {raw_spec:?}: backend {} requires a target",
                        backend.as_tag()
                    );
                };
                if target.is_empty() {
                    eyre::bail!(
                        "invalid VX_BUILD_EXECUTOR_CLASSES spec {raw_spec:?}: backend {} requires a non-empty target",
                        backend.as_tag()
                    );
                }
            }
            vx_eval::ExecutorBackendKind::Class => {
                eyre::bail!("VX_BUILD_EXECUTOR_CLASSES only accepts concrete backends, not class");
            }
        }

        configs.push(BuildVxExecutorClassConfig {
            class_label: class_label.to_string(),
            backend,
            target,
        });
    }

    Ok(configs)
}

#[cfg(unix)]
fn ssh_bootstrap_label_slug(class_label: &str) -> String {
    class_label
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => ch,
            _ => '-',
        })
        .collect()
}

#[cfg(unix)]
fn ssh_bootstrap_paths(remote_home: &str, class_label: &str) -> SshBootstrapPaths {
    let class_slug = ssh_bootstrap_label_slug(class_label);
    let base = format!("{remote_home}/.cache/vixen/ssh-bootstrap/{class_slug}");
    let vx_home = format!("{base}/vx-home");
    SshBootstrapPaths {
        bare_repo_dir: format!("{base}/repo.git"),
        checkout_dir: format!("{base}/repo"),
        store_root: format!("{vx_home}/store"),
        store_socket: format!("{vx_home}/store.sock"),
        vfsd_socket: format!("{vx_home}/vfsd.sock"),
        runner_socket: format!("{vx_home}/runner.sock"),
        vx_home,
    }
}

#[cfg(unix)]
fn ssh_bootstrap_windows_paths(
    bootstrap_root: &str,
    class_label: &str,
    store_port: u16,
    vfsd_port: u16,
    runner_port: u16,
) -> SshBootstrapWindowsPaths {
    let class_slug = ssh_bootstrap_label_slug(class_label);
    let base = format!(r"{bootstrap_root}\{class_slug}");
    let vx_home = format!(r"{base}\vx-home");
    SshBootstrapWindowsPaths {
        bare_repo_dir: format!(r"{base}\repo.git"),
        checkout_dir: format!(r"{base}\repo"),
        vx_home: vx_home.clone(),
        store_root: format!(r"{vx_home}\store"),
        store_endpoint: format!("127.0.0.1:{store_port}"),
        vfsd_endpoint: format!("127.0.0.1:{vfsd_port}"),
        runner_endpoint: format!("127.0.0.1:{runner_port}"),
    }
}

#[cfg(unix)]
fn windows_drive_prefix(path: &str) -> Option<&str> {
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        Some(&path[..2])
    } else {
        None
    }
}

#[cfg(unix)]
fn windows_bootstrap_root(
    remote_home: &str,
    cargo_home: Option<&str>,
    rustup_home: Option<&str>,
    tmp: Option<&str>,
    temp: Option<&str>,
) -> String {
    let preferred_drive = [cargo_home, rustup_home, tmp, temp]
        .into_iter()
        .flatten()
        .find_map(windows_drive_prefix);
    match preferred_drive {
        Some(drive) => format!(r"{drive}\vixen\ssh-bootstrap"),
        None => format!(r"{remote_home}\.cache\vixen\ssh-bootstrap"),
    }
}

#[cfg(unix)]
fn ssh_bootstrap_local_socket_endpoint(class_label: &str, service_name: &str) -> Endpoint {
    let class_slug = ssh_bootstrap_label_slug(class_label);
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    Endpoint::Local(Utf8PathBuf::from(format!(
        "/tmp/vx-ssh-{class_slug}-{service_name}-{}-{unique}.sock",
        std::process::id()
    )))
}

#[cfg(unix)]
fn ssh_bootstrap_unique_suffix() -> String {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    format!("{}-{unique}", std::process::id())
}

#[cfg(unix)]
fn ssh_bootstrap_local_tcp_endpoint() -> Result<Endpoint> {
    Ok(Endpoint::Tcp {
        host: "127.0.0.1".to_string(),
        port: allocate_loopback_port()?,
    })
}

#[cfg(unix)]
fn ssh_bootstrap_control_socket_path(class_label: &str) -> Utf8PathBuf {
    let class_slug = ssh_bootstrap_label_slug(class_label);
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    Utf8PathBuf::from(format!(
        "/tmp/vx-ssh-{class_slug}-ctl-{}-{unique}.sock",
        std::process::id()
    ))
}

#[cfg(unix)]
struct SshBootstrapConnection {
    runner_handle: ServiceHandle,
    store_handle: ServiceHandle,
}

#[cfg(unix)]
async fn emit_ssh_bootstrap_progress(
    boot_tx: Option<&BootTx>,
    listener: &Option<Arc<ProgressListenerClient>>,
    class_label: &str,
    message: impl Into<String>,
) {
    let message = message.into();
    if let Some(boot_tx) = boot_tx {
        boot_tx.phase(format!(
            "bootstrapping build executor class:{class_label}: {message}"
        ));
    }
    if let Some(listener) = listener {
        let _ = listener
            .log_message(format!(
                "build.vx executor class:{class_label}: {}",
                message
            ))
            .await;
    }
}

#[cfg(unix)]
const SSH_BOOTSTRAP_REMOTE_SCRIPT: &str = r#"set -euo pipefail
bare_repo_dir="$1"
commit="$2"
checkout_dir="$3"
vx_home="$4"
store_root="$5"
store_socket="$6"
vfsd_socket="$7"
runner_socket="$8"

restore_scrubbed_files() {
  git -C "$checkout_dir" checkout -- Cargo.lock crates/vx/Cargo.toml crates/vxd/Cargo.toml || true
}

mkdir -p "$(dirname "$bare_repo_dir")" "$(dirname "$checkout_dir")" "$vx_home" "$store_root"
if [ ! -f "$bare_repo_dir/HEAD" ]; then
  rm -rf "$bare_repo_dir"
  git init --bare "$bare_repo_dir"
fi
if [ ! -d "$checkout_dir/.git" ]; then
  rm -rf "$checkout_dir"
  git clone "$bare_repo_dir" "$checkout_dir"
fi
git -C "$checkout_dir" remote set-url origin "$bare_repo_dir"
restore_scrubbed_files
git -C "$checkout_dir" fetch origin "refs/vixen-bootstrap/$commit"
git -C "$checkout_dir" checkout --detach "$commit"
python3 - "$checkout_dir" <<'PY'
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
needle = 'tree-sitter-vixen = { path = "/Users/amos/bearcove/tree-sitter-vixen" }\n'
for rel in ("crates/vx/Cargo.toml", "crates/vxd/Cargo.toml"):
    path = root / rel
    if not path.exists():
        continue
    text = path.read_text()
    updated = text.replace(needle, "")
    if updated != text:
        path.write_text(updated)
PY
pkill -9 -f "$checkout_dir/target/debug/vx-runner" || true
pkill -9 -f "$checkout_dir/target/debug/vx-store" || true
pkill -9 -f "$checkout_dir/target/debug/vx-vfsd" || true
while pgrep -f "$checkout_dir/target/debug/vx-runner" >/dev/null; do sleep 0.1; done
while pgrep -f "$checkout_dir/target/debug/vx-store" >/dev/null; do sleep 0.1; done
while pgrep -f "$checkout_dir/target/debug/vx-vfsd" >/dev/null; do sleep 0.1; done
cargo build --manifest-path "$checkout_dir/Cargo.toml" -p vx-store -p vx-vfsd -p vx-runner -p landlock-exec
restore_scrubbed_files
rm -f "$store_socket" "$vfsd_socket" "$runner_socket"
trap 'restore_scrubbed_files; jobs -p | xargs -r kill || true' EXIT
"$checkout_dir/target/debug/vx-store" --root "$store_root" --listen "unix:$store_socket" &
export VX_HOME="$vx_home"
exec "$checkout_dir/target/debug/vx-runner" \
  --vx-home "$vx_home" \
  --store "unix:$store_socket" \
  --vfsd "unix:$vfsd_socket" \
  --listen "unix:$runner_socket"
"#;

#[cfg(unix)]
fn powershell_single_quote(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(unix)]
fn powershell_encoded_command(script: &str) -> String {
    let utf16le = script
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect::<Vec<_>>();
    base64::engine::general_purpose::STANDARD.encode(utf16le)
}

#[cfg(unix)]
fn powershell_command_line(script: &str) -> String {
    let encoded = powershell_encoded_command(script);
    format!("powershell -NoProfile -ExecutionPolicy Bypass -EncodedCommand {encoded}")
}

#[cfg(unix)]
fn ssh_bootstrap_windows_command(checkout_dir: &str, paths: &SshBootstrapWindowsPaths) -> String {
    let checkout_dir = powershell_single_quote(checkout_dir);
    let vx_home = powershell_single_quote(&paths.vx_home);
    let store_root = powershell_single_quote(&paths.store_root);
    let store_endpoint = powershell_single_quote(&paths.store_endpoint);
    let vfsd_endpoint = powershell_single_quote(&paths.vfsd_endpoint);
    let runner_endpoint = powershell_single_quote(&paths.runner_endpoint);
    let script = format!(
        concat!(
            "$ErrorActionPreference = 'Stop'; ",
            "$vxHome = '{vx_home}'; ",
            "$storeRoot = '{store_root}'; ",
            "$storeEndpoint = '{store_endpoint}'; ",
            "$vfsdEndpoint = '{vfsd_endpoint}'; ",
            "$runnerEndpoint = '{runner_endpoint}'; ",
            "$existing = Get-Process vx-runner,vx-store,vx-vfsd -ErrorAction SilentlyContinue; ",
            "if ($existing) {{ ",
            "  $existing | Stop-Process -Force; ",
            "  for ($i = 0; $i -lt 200; $i++) {{ ",
            "    if (-not (Get-Process vx-runner,vx-store,vx-vfsd -ErrorAction SilentlyContinue)) {{ break }}; ",
            "    Start-Sleep -Milliseconds 100; ",
            "  }}; ",
            "  if (Get-Process vx-runner,vx-store,vx-vfsd -ErrorAction SilentlyContinue) {{ throw 'timed out waiting for prior bootstrap processes to exit' }}; ",
            "}}; ",
            "Write-Host ('vx-bootstrap endpoints store=' + $storeEndpoint + ' vfsd=' + $vfsdEndpoint + ' runner=' + $runnerEndpoint); ",
            "$env:VX_HOME = $vxHome; ",
            "$endpointTests = @($storeEndpoint, $vfsdEndpoint, $runnerEndpoint); ",
            "foreach ($endpoint in $endpointTests) {{ ",
            "  $hostPort = $endpoint.Split(':'); ",
            "  $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Parse($hostPort[0]), [int]$hostPort[1]); ",
            "  try {{ $listener.Start() }} finally {{ $listener.Stop() }} ",
            "}}; ",
            "$checkoutDir = '{checkout_dir}'; ",
            "$storeExe = Join-Path $checkoutDir 'target\\debug\\vx-store.exe'; ",
            "$runnerExe = Join-Path $checkoutDir 'target\\debug\\vx-runner.exe'; ",
            "$storeProc = Start-Process -FilePath $storeExe -ArgumentList @('--root', $storeRoot, '--listen', $storeEndpoint) -PassThru; ",
            "try {{ & $runnerExe --vx-home $vxHome --store $storeEndpoint --vfsd $vfsdEndpoint --listen $runnerEndpoint }} ",
            "finally {{ ",
            "if ($storeProc -and -not $storeProc.HasExited) {{ Stop-Process -Id $storeProc.Id -Force }} }}"
        ),
        vx_home = vx_home,
        store_root = store_root,
        store_endpoint = store_endpoint,
        vfsd_endpoint = vfsd_endpoint,
        runner_endpoint = runner_endpoint,
        checkout_dir = checkout_dir,
    );
    powershell_command_line(&script)
}

#[cfg(unix)]
async fn prepare_ssh_bootstrap_windows_checkout(
    control_path: &Utf8Path,
    target: &str,
    bare_repo_dir: &str,
    commit: &str,
    checkout_dir: &str,
    vx_home: &str,
    store_root: &str,
) -> Result<()> {
    let script = format!(
        concat!(
            "$ErrorActionPreference = 'Stop'; ",
            "$bareRepoDir = '{bare_repo_dir}'; ",
            "$commit = '{commit}'; ",
            "$checkoutDir = '{checkout_dir}'; ",
            "$vxHome = '{vx_home}'; ",
            "$storeRoot = '{store_root}'; ",
            "$existing = Get-Process vx-runner,vx-store,vx-vfsd -ErrorAction SilentlyContinue; ",
            "if ($existing) {{ ",
            "  $existing | Stop-Process -Force; ",
            "  for ($i = 0; $i -lt 200; $i++) {{ ",
            "    if (-not (Get-Process vx-runner,vx-store,vx-vfsd -ErrorAction SilentlyContinue)) {{ break }}; ",
            "    Start-Sleep -Milliseconds 100; ",
            "  }}; ",
            "  if (Get-Process vx-runner,vx-store,vx-vfsd -ErrorAction SilentlyContinue) {{ throw 'timed out waiting for prior bootstrap processes to exit' }}; ",
            "}}; ",
            "New-Item -ItemType Directory -Force -Path (Split-Path -Parent $bareRepoDir), (Split-Path -Parent $checkoutDir), $vxHome, $storeRoot | Out-Null; ",
            "if (-not (Test-Path (Join-Path $bareRepoDir 'HEAD'))) {{ if (Test-Path $bareRepoDir) {{ Remove-Item -Recurse -Force $bareRepoDir }}; git init --bare $bareRepoDir; if ($LASTEXITCODE -ne 0) {{ throw 'git init --bare failed' }} }}; ",
            "if (-not (Test-Path (Join-Path $checkoutDir '.git'))) {{ if (Test-Path $checkoutDir) {{ Remove-Item -Recurse -Force $checkoutDir }}; git clone $bareRepoDir $checkoutDir; if ($LASTEXITCODE -ne 0) {{ throw 'git clone failed' }} }}; ",
            "git -C $checkoutDir remote set-url origin $bareRepoDir; if ($LASTEXITCODE -ne 0) {{ throw 'git remote set-url failed' }}; ",
            "git -C $checkoutDir checkout -- Cargo.lock crates/vx/Cargo.toml crates/vxd/Cargo.toml *> $null; if ($LASTEXITCODE -ne 0) {{ throw 'git checkout restore failed' }}; ",
            "git -C $checkoutDir fetch origin ('refs/vixen-bootstrap/' + $commit); if ($LASTEXITCODE -ne 0) {{ throw 'git fetch failed' }}; ",
            "git -C $checkoutDir checkout --detach $commit; if ($LASTEXITCODE -ne 0) {{ throw 'git checkout --detach failed' }}; ",
            "$manifests = @((Join-Path $checkoutDir 'crates\\vx\\Cargo.toml'), (Join-Path $checkoutDir 'crates\\vxd\\Cargo.toml')); ",
            "foreach ($manifest in $manifests) {{ if (Test-Path $manifest) {{ $lines = Get-Content $manifest; $updated = @($lines | Where-Object {{ $_ -notlike 'tree-sitter-vixen = *' }}); if ($updated.Count -ne $lines.Count) {{ Set-Content -Path $manifest -Value $updated }} }} }}; ",
            "cargo build --manifest-path (Join-Path $checkoutDir 'Cargo.toml') -p vx-store -p vx-vfsd -p vx-runner; if ($LASTEXITCODE -ne 0) {{ throw 'cargo build failed' }}; ",
            "git -C $checkoutDir checkout -- Cargo.lock crates/vx/Cargo.toml crates/vxd/Cargo.toml *> $null; if ($LASTEXITCODE -ne 0) {{ throw 'git checkout restore failed' }}; ",
            "Write-Output $checkoutDir"
        ),
        bare_repo_dir = powershell_single_quote(bare_repo_dir),
        commit = powershell_single_quote(commit),
        checkout_dir = powershell_single_quote(checkout_dir),
        vx_home = powershell_single_quote(vx_home),
        store_root = powershell_single_quote(store_root),
    );
    ssh_control_output_windows(control_path, target, &script).await?;
    Ok(())
}

async fn command_output(program: &str, args: &[&str]) -> Result<String> {
    let mut command = moire::process::Command::new(program);
    command.args(args);
    let output = command.output().await?;
    if !output.status.success() {
        eyre::bail!(
            "{program} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn resolve_vixen_repo_root() -> Result<Utf8PathBuf> {
    let root = command_output(
        "git",
        &[
            "-C",
            VIXEN_REPO_MANIFEST_DIR,
            "rev-parse",
            "--show-toplevel",
        ],
    )
    .await?;
    Utf8PathBuf::from_path_buf(std::path::PathBuf::from(root)).map_err(|path| {
        eyre::eyre!(
            "vixen source checkout path is not valid UTF-8: {}",
            path.display()
        )
    })
}

async fn ensure_clean_vixen_repo(repo_root: &Utf8Path) -> Result<()> {
    let status = command_output(
        "git",
        &[
            "-C",
            repo_root.as_str(),
            "status",
            "--short",
            "--untracked-files=all",
        ],
    )
    .await?;
    if status.is_empty() {
        return Ok(());
    }
    eyre::bail!(
        "ssh bootstrap requires a clean committed Vixen repo at {}; uncommitted changes:\n{}",
        repo_root,
        status
    );
}

#[cfg(unix)]
fn allocate_loopback_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

#[cfg(unix)]
fn ssh_control_command(control_path: &Utf8Path, target: &str) -> moire::process::Command {
    let mut command = moire::process::Command::new("ssh");
    command.arg("-S").arg(control_path).arg(target);
    command
}

#[cfg(unix)]
async fn ssh_control_output_unix(
    control_path: &Utf8Path,
    target: &str,
    args: &[&str],
) -> Result<String> {
    let mut command = ssh_control_command(control_path, target);
    let remote_command = args
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    command.arg(remote_command);
    let output = command.output().await?;
    if !output.status.success() {
        eyre::bail!(
            "ssh to {target} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(unix)]
async fn ssh_control_output_windows(
    control_path: &Utf8Path,
    target: &str,
    script: &str,
) -> Result<String> {
    let mut command = ssh_control_command(control_path, target);
    command.arg(powershell_command_line(script));
    let output = command.output().await?;
    if !output.status.success() {
        eyre::bail!(
            "ssh to {target} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(unix)]
async fn wait_for_ssh_control_master(control_path: &Utf8Path, target: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let mut command = moire::process::Command::new("ssh");
        command
            .arg("-S")
            .arg(control_path)
            .arg("-O")
            .arg("check")
            .arg(target);
        match command.output().await {
            Ok(output) if output.status.success() => return Ok(()),
            Ok(_) | Err(_) if tokio::time::Instant::now() >= deadline => {
                eyre::bail!("{}", ssh_control_master_not_ready_message(target, None))
            }
            Ok(_) | Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
}

#[cfg(unix)]
fn ssh_control_master_not_ready_message(target: &str, log_detail: Option<&str>) -> String {
    match log_detail
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
    {
        Some(detail) => {
            format!("ssh control master for {target} did not become ready: {detail}")
        }
        None => format!("ssh control master for {target} did not become ready"),
    }
}

#[cfg(unix)]
fn ssh_bootstrap_log_detail(log_path: &Utf8Path) -> Option<String> {
    fs::read_to_string(log_path.as_std_path())
        .ok()
        .and_then(|contents| {
            let trimmed = contents.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
}

#[cfg(unix)]
async fn detect_ssh_bootstrap_target(
    control_path: &Utf8Path,
    target: &str,
) -> Result<SshBootstrapTargetInfo> {
    let windows_version = ssh_control_command(control_path, target)
        .arg("ver")
        .output()
        .await?;
    if windows_version.status.success() {
        let env_dump = ssh_control_output_windows(
            control_path,
            target,
            concat!(
                "function Get-VxEnv([string]$name) { ",
                "  $value = [Environment]::GetEnvironmentVariable($name, 'Process'); ",
                "  if ([string]::IsNullOrEmpty($value)) { $value = [Environment]::GetEnvironmentVariable($name, 'User') }; ",
                "  if ([string]::IsNullOrEmpty($value)) { $value = [Environment]::GetEnvironmentVariable($name, 'Machine') }; ",
                "  $value ",
                "}; ",
                "$pairs = @(",
                "  'HOME=' + [Environment]::GetFolderPath('UserProfile'), ",
                "  'CARGO_HOME=' + (Get-VxEnv 'CARGO_HOME'), ",
                "  'RUSTUP_HOME=' + (Get-VxEnv 'RUSTUP_HOME'), ",
                "  'TMP=' + (Get-VxEnv 'TMP'), ",
                "  'TEMP=' + (Get-VxEnv 'TEMP')",
                "); ",
                "$pairs | ForEach-Object { Write-Output $_ }"
            ),
        )
        .await?;
        let mut home = None::<String>;
        let mut cargo_home = None::<String>;
        let mut rustup_home = None::<String>;
        let mut tmp = None::<String>;
        let mut temp = None::<String>;
        for line in env_dump.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            let value = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
            match key {
                "HOME" => home = value,
                "CARGO_HOME" => cargo_home = value,
                "RUSTUP_HOME" => rustup_home = value,
                "TMP" => tmp = value,
                "TEMP" => temp = value,
                _ => {}
            }
        }
        let home = home.ok_or_else(|| {
            eyre::eyre!("ssh bootstrap target {target} returned an empty Windows home")
        })?;
        let bootstrap_root = windows_bootstrap_root(
            &home,
            cargo_home.as_deref(),
            rustup_home.as_deref(),
            tmp.as_deref(),
            temp.as_deref(),
        );
        return Ok(SshBootstrapTargetInfo {
            platform: SshBootstrapTargetPlatform::Windows,
            home,
            windows_bootstrap_root: Some(bootstrap_root),
        });
    }

    let home = ssh_control_output_unix(control_path, target, &["printenv", "HOME"]).await?;
    if home.is_empty() {
        eyre::bail!("ssh bootstrap target {target} returned an empty HOME");
    }

    Ok(SshBootstrapTargetInfo {
        platform: SshBootstrapTargetPlatform::Unix,
        home,
        windows_bootstrap_root: None,
    })
}

#[cfg(unix)]
async fn allocate_windows_ssh_bootstrap_ports(
    control_path: &Utf8Path,
    target: &str,
) -> Result<(u16, u16, u16)> {
    let output = ssh_control_output_windows(
        control_path,
        target,
        concat!(
            "$listeners = @(); ",
            "for ($port = 30000; $port -lt 49152 -and $listeners.Count -lt 3; $port++) { ",
            "  try { ",
            "    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $port); ",
            "    $listener.Start(); ",
            "    $listeners += $listener; ",
            "  } catch { } ",
            "}; ",
            "if ($listeners.Count -ne 3) { throw 'failed to reserve three Windows bootstrap ports outside the dynamic range' }; ",
            "$ports = $listeners | ForEach-Object { $_.LocalEndpoint.Port }; ",
            "$listeners | ForEach-Object { $_.Stop() }; ",
            "$ports -join ','"
        ),
    )
    .await?;

    let ports = output
        .split(',')
        .map(str::trim)
        .map(str::parse::<u16>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| eyre::eyre!("failed to parse remote Windows bootstrap ports: {error}"))?;

    let [store_port, vfsd_port, runner_port]: [u16; 3] =
        ports.try_into().map_err(|ports: Vec<u16>| {
            eyre::eyre!("expected three remote Windows bootstrap ports, got {ports:?}")
        })?;
    Ok((store_port, vfsd_port, runner_port))
}

#[cfg(unix)]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(unix)]
fn ssh_forward_spec(local: &Endpoint, remote: &str) -> Result<String> {
    match local {
        Endpoint::Local(path) => Ok(format!("{}:{remote}", path.as_str())),
        Endpoint::Tcp { host, port } => Ok(format!("{host}:{port}:{remote}")),
    }
}

#[cfg(unix)]
fn ssh_bootstrap_git_push_url(
    target: &str,
    bare_repo_dir: &str,
    platform: &SshBootstrapTargetPlatform,
) -> String {
    match platform {
        SshBootstrapTargetPlatform::Unix => format!("ssh://{target}{bare_repo_dir}"),
        SshBootstrapTargetPlatform::Windows => {
            format!("ssh://{target}/{}", bare_repo_dir.replace('\\', "/"))
        }
    }
}

fn write_windows_git_ssh_wrapper(control_path: &Utf8Path) -> Result<Utf8PathBuf> {
    let wrapper_path = Utf8PathBuf::from(format!("{control_path}.windows-git-ssh.sh"));
    std::fs::write(
        &wrapper_path,
        format!(
            r#"#!/bin/sh
set -eu
host=$1
cmd=${{2-}}
case "$cmd" in
  git-upload-pack\ \'*\'|git-receive-pack\ \'*\')
    prog=${{cmd%% *}}
    path=${{cmd#*\'}}
    path=${{path%\'}}
    case "$path" in
      /?:/*) path=${{path#/}} ;;
    esac
    cmd="$prog \"$path\""
    ;;
esac
exec ssh -S {control_path} -o ControlMaster=no "$host" "$cmd"
"#,
            control_path = shell_quote(control_path.as_str()),
        ),
    )?;
    Ok(wrapper_path)
}

#[cfg(unix)]
async fn ensure_ssh_bootstrap_bare_repo(
    control_path: &Utf8Path,
    target: &str,
    bare_repo_dir: &str,
    platform: &SshBootstrapTargetPlatform,
) -> Result<()> {
    match platform {
        SshBootstrapTargetPlatform::Unix => {
            let bare_repo_parent = Utf8Path::new(bare_repo_dir).parent().ok_or_else(|| {
                eyre::eyre!("failed to determine bare repo parent for {bare_repo_dir}")
            })?;
            ssh_control_output_unix(
                control_path,
                target,
                &[
                    "sh",
                    "-lc",
                    &format!(
                        "mkdir -p {parent} && if [ ! -f {dir}/HEAD ]; then rm -rf {dir}; git init --bare {dir}; fi",
                        parent = shell_quote(bare_repo_parent.as_str()),
                        dir = shell_quote(bare_repo_dir)
                    ),
                ],
            )
            .await?;
        }
        SshBootstrapTargetPlatform::Windows => {
            let script = format!(
                "$ErrorActionPreference = 'Stop'; $dir = '{}'; New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dir) | Out-Null; if (-not (Test-Path (Join-Path $dir 'HEAD'))) {{ if (Test-Path $dir) {{ Remove-Item -Recurse -Force $dir }}; git init --bare $dir }}",
                powershell_single_quote(bare_repo_dir)
            );
            ssh_control_output_windows(control_path, target, &script).await?;
        }
    }
    Ok(())
}

#[cfg(unix)]
async fn push_vixen_repo_over_ssh(
    repo_root: &Utf8Path,
    target: &str,
    control_path: &Utf8Path,
    bare_repo_dir: &str,
    platform: &SshBootstrapTargetPlatform,
    commit: &str,
) -> Result<()> {
    let remote_url = ssh_bootstrap_git_push_url(target, bare_repo_dir, platform);
    let core_ssh_command = match platform {
        SshBootstrapTargetPlatform::Unix => {
            format!("ssh -S {control_path} -o ControlMaster=no")
        }
        SshBootstrapTargetPlatform::Windows => {
            let wrapper_path = write_windows_git_ssh_wrapper(control_path)?;
            format!("sh {}", shell_quote(wrapper_path.as_str()),)
        }
    };
    let mut command = moire::process::Command::new("git");
    command
        .arg("-C")
        .arg(repo_root)
        .arg("-c")
        .arg(format!("core.sshCommand={core_ssh_command}"))
        .arg("push")
        .arg("--force")
        .arg(remote_url)
        .arg(format!("{commit}:refs/vixen-bootstrap/{commit}"));
    let output = command.output().await?;
    if matches!(platform, SshBootstrapTargetPlatform::Windows) {
        let wrapper_path = Utf8PathBuf::from(format!("{control_path}.windows-git-ssh.sh"));
        let _ = std::fs::remove_file(wrapper_path.as_std_path());
    }
    if !output.status.success() {
        eyre::bail!(
            "git push for ssh bootstrap failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

async fn connect_endpoint_with_backoff(
    name: &str,
    endpoint: &Endpoint,
    timeout: Duration,
) -> Result<(roam_core::DriverCaller, roam_core::SessionHandle)> {
    let deadline = tokio::time::Instant::now() + timeout;
    let backoff_ms = [10, 25, 50, 100, 250, 500, 1000];
    let mut last_err = None;

    for delay in backoff_ms.iter().copied().chain(std::iter::repeat(1000)) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }

        match tokio::time::timeout(
            remaining.min(Duration::from_secs(1)),
            vx_io::net::connect_session(endpoint),
        )
        .await
        {
            Ok(Ok(session)) => return Ok(session),
            Ok(Err(error)) => last_err = Some(error.to_string()),
            Err(_) => last_err = Some("connection timed out".to_string()),
        }

        if tokio::time::Instant::now() + Duration::from_millis(delay) > deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }

    let err_detail = last_err.unwrap_or_else(|| "unknown error".to_string());
    eyre::bail!("{name} unreachable after {timeout:?} at {endpoint}: {err_detail}");
}

#[cfg(unix)]
async fn connect_ssh_bootstrap_runner(
    vx_home: &Utf8PathBuf,
    class_label: &str,
    target: &str,
    boot_tx: Option<&BootTx>,
    progress_listener: Option<Arc<ProgressListenerClient>>,
) -> Result<SshBootstrapConnection> {
    emit_ssh_bootstrap_progress(
        boot_tx,
        &progress_listener,
        class_label,
        format!("checking local Vixen repo for ssh target {target}"),
    )
    .await;
    let repo_root = resolve_vixen_repo_root().await?;
    ensure_clean_vixen_repo(&repo_root).await?;
    let commit = command_output("git", &["-C", repo_root.as_str(), "rev-parse", "HEAD"]).await?;
    let class_slug = ssh_bootstrap_label_slug(class_label);
    let log_path = vx_home.join(format!("runner-ssh-bootstrap-{class_slug}.log"));
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let master_stdout_file = log_file.try_clone()?;
    let master_stderr_file = log_file.try_clone()?;
    let ssh_stdout_file = log_file.try_clone()?;
    let ssh_stderr_file = log_file.try_clone()?;
    let control_path = ssh_bootstrap_control_socket_path(class_label);
    emit_ssh_bootstrap_progress(
        boot_tx,
        &progress_listener,
        class_label,
        format!("opening ssh control connection to {target}"),
    )
    .await;
    let mut master = moire::process::Command::new("ssh");
    master
        .arg("-M")
        .arg("-N")
        .arg("-S")
        .arg(&control_path)
        .arg("-o")
        .arg("ControlMaster=yes")
        .arg("-o")
        .arg("ControlPersist=no")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=5")
        .arg("-o")
        .arg("ServerAliveInterval=30")
        .arg("-o")
        .arg("ServerAliveCountMax=3")
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::from(master_stdout_file))
        .stderr(Stdio::from(master_stderr_file))
        .kill_on_drop(true);
    let master_child = master.spawn()?;
    if wait_for_ssh_control_master(&control_path, target)
        .await
        .is_err()
    {
        let log_detail = ssh_bootstrap_log_detail(&log_path);
        let message = ssh_control_master_not_ready_message(target, log_detail.as_deref());
        drop(master_child);
        eyre::bail!(message);
    }
    emit_ssh_bootstrap_progress(
        boot_tx,
        &progress_listener,
        class_label,
        format!("probing ssh target {target}"),
    )
    .await;
    let target_info = detect_ssh_bootstrap_target(&control_path, target).await?;
    emit_ssh_bootstrap_progress(
        boot_tx,
        &progress_listener,
        class_label,
        format!("detected {} host", target_info.platform.as_tag()),
    )
    .await;
    let mut windows_checkout = None;
    let (local_store_endpoint, local_runner_endpoint, store_remote, runner_remote, remote_command) =
        match target_info.platform {
            SshBootstrapTargetPlatform::Unix => {
                let local_runner_endpoint =
                    ssh_bootstrap_local_socket_endpoint(class_label, "runner");
                let local_runner_socket = match &local_runner_endpoint {
                    Endpoint::Local(path) => path.clone(),
                    Endpoint::Tcp { .. } => unreachable!("unix ssh bootstrap uses local sockets"),
                };
                let local_store_endpoint =
                    ssh_bootstrap_local_socket_endpoint(class_label, "store");
                let local_store_socket = match &local_store_endpoint {
                    Endpoint::Local(path) => path.clone(),
                    Endpoint::Tcp { .. } => unreachable!("unix ssh bootstrap uses local sockets"),
                };
                if let Some(parent) = local_runner_socket.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if local_runner_socket.exists() {
                    let _ = std::fs::remove_file(local_runner_socket.as_std_path());
                }
                if local_store_socket.exists() {
                    let _ = std::fs::remove_file(local_store_socket.as_std_path());
                }
                let paths = ssh_bootstrap_paths(&target_info.home, class_label);
                let socket_suffix = ssh_bootstrap_unique_suffix();
                let store_socket = format!("{}/store-{socket_suffix}.sock", paths.vx_home);
                let vfsd_socket = format!("{}/vfsd-{socket_suffix}.sock", paths.vx_home);
                let runner_socket = format!("{}/runner-{socket_suffix}.sock", paths.vx_home);
                let remote_command = format!(
                    "sh -lc {} sh {} {} {} {} {} {} {} {}",
                    shell_quote(SSH_BOOTSTRAP_REMOTE_SCRIPT),
                    shell_quote(&paths.bare_repo_dir),
                    shell_quote(&commit),
                    shell_quote(&paths.checkout_dir),
                    shell_quote(&paths.vx_home),
                    shell_quote(&paths.store_root),
                    shell_quote(&store_socket),
                    shell_quote(&vfsd_socket),
                    shell_quote(&runner_socket),
                );
                (
                    local_store_endpoint,
                    local_runner_endpoint,
                    store_socket,
                    runner_socket,
                    remote_command,
                )
            }
            SshBootstrapTargetPlatform::Windows => {
                let checkout_paths = ssh_bootstrap_windows_paths(
                    target_info.windows_bootstrap_root(),
                    class_label,
                    0,
                    0,
                    0,
                );
                let checkout_dir = checkout_paths.checkout_dir.clone();
                windows_checkout = Some(checkout_paths);
                let local_store_endpoint = ssh_bootstrap_local_tcp_endpoint()?;
                let local_runner_endpoint = ssh_bootstrap_local_tcp_endpoint()?;
                let (remote_store_port, remote_vfsd_port, remote_runner_port) =
                    allocate_windows_ssh_bootstrap_ports(&control_path, target).await?;
                let paths = ssh_bootstrap_windows_paths(
                    target_info.windows_bootstrap_root(),
                    class_label,
                    remote_store_port,
                    remote_vfsd_port,
                    remote_runner_port,
                );
                let remote_command = ssh_bootstrap_windows_command(&checkout_dir, &paths);
                (
                    local_store_endpoint,
                    local_runner_endpoint,
                    paths.store_endpoint,
                    paths.runner_endpoint,
                    remote_command,
                )
            }
        };

    emit_ssh_bootstrap_progress(
        boot_tx,
        &progress_listener,
        class_label,
        format!("syncing local Vixen repo to {target}"),
    )
    .await;
    let bare_repo_dir = match target_info.platform {
        SshBootstrapTargetPlatform::Unix => {
            ssh_bootstrap_paths(&target_info.home, class_label).bare_repo_dir
        }
        SshBootstrapTargetPlatform::Windows => {
            ssh_bootstrap_windows_paths(target_info.windows_bootstrap_root(), class_label, 0, 0, 0)
                .bare_repo_dir
        }
    };
    ensure_ssh_bootstrap_bare_repo(&control_path, target, &bare_repo_dir, &target_info.platform)
        .await?;
    push_vixen_repo_over_ssh(
        &repo_root,
        target,
        &control_path,
        &bare_repo_dir,
        &target_info.platform,
        &commit,
    )
    .await?;

    if let Some(paths) = &windows_checkout {
        prepare_ssh_bootstrap_windows_checkout(
            &control_path,
            target,
            &paths.bare_repo_dir,
            &commit,
            &paths.checkout_dir,
            &paths.vx_home,
            &paths.store_root,
        )
        .await?;
    }

    let mut command = moire::process::Command::new("ssh");
    command
        .arg("-S")
        .arg(&control_path)
        .arg("-o")
        .arg("ExitOnForwardFailure=yes")
        .arg("-o")
        .arg("ServerAliveInterval=30")
        .arg("-o")
        .arg("ServerAliveCountMax=3")
        .arg("-L")
        .arg(ssh_forward_spec(&local_store_endpoint, &store_remote)?)
        .arg("-L")
        .arg(ssh_forward_spec(&local_runner_endpoint, &runner_remote)?)
        .arg(target)
        .arg(remote_command)
        .stdin(Stdio::null())
        .stdout(Stdio::from(ssh_stdout_file))
        .stderr(Stdio::from(ssh_stderr_file))
        .kill_on_drop(true);
    let child = command.spawn()?;
    emit_ssh_bootstrap_progress(
        boot_tx,
        &progress_listener,
        class_label,
        format!("waiting for remote store and runner sockets on {target}"),
    )
    .await;

    let (store_caller, store_session_handle) = connect_endpoint_with_backoff(
        &format!("vx-store[{class_label}]"),
        &local_store_endpoint,
        SSH_BOOTSTRAP_CONNECT_TIMEOUT,
    )
    .await?;

    let (runner_caller, runner_session_handle) = connect_endpoint_with_backoff(
        &format!("vx-runner[{class_label}]"),
        &local_runner_endpoint,
        SSH_BOOTSTRAP_CONNECT_TIMEOUT,
    )
    .await?;

    Ok(SshBootstrapConnection {
        runner_handle: ServiceHandle {
            child: Some(child),
            caller: runner_caller,
            session_handle: runner_session_handle,
        },
        store_handle: ServiceHandle {
            child: Some(master_child),
            caller: store_caller,
            session_handle: store_session_handle,
        },
    })
}

#[cfg(unix)]
async fn bootstrap_ssh_executor(
    vx_home: &Utf8PathBuf,
    class_label: &str,
    target: &str,
    boot_tx: Option<&BootTx>,
    progress_listener: Option<Arc<ProgressListenerClient>>,
) -> Result<crate::service::SshBootstrapExecutor> {
    emit_ssh_bootstrap_progress(
        boot_tx,
        &progress_listener,
        class_label,
        "starting ssh bootstrap",
    )
    .await;
    let connection = connect_ssh_bootstrap_runner(
        vx_home,
        class_label,
        target,
        boot_tx,
        progress_listener.clone(),
    )
    .await?;
    let store_handle = Arc::new(connection.store_handle);
    let runner_handle = Arc::new(connection.runner_handle);
    emit_ssh_bootstrap_progress(
        boot_tx,
        &progress_listener,
        class_label,
        "waiting for remote vx-store boot",
    )
    .await;
    let store_virtual = store_handle.boot_and_connect().await?;
    emit_ssh_bootstrap_progress(
        boot_tx,
        &progress_listener,
        class_label,
        "waiting for remote vx-runner boot",
    )
    .await;
    let runner_virtual = runner_handle.boot_and_connect().await?;
    emit_ssh_bootstrap_progress(
        boot_tx,
        &progress_listener,
        class_label,
        "remote executor ready",
    )
    .await;
    Ok(crate::service::SshBootstrapExecutor {
        id: format!("ssh_bootstrap:{class_label}"),
        backend: vx_eval::ExecutorBackendKind::SshBootstrap,
        runner: Arc::new(RunnerClient::new(runner_virtual)),
        store: Some(Arc::new(StoreClient::new(store_virtual))),
        runner_handle: Some(runner_handle),
        store_handle: Some(store_handle),
    })
}

/// Resolve CLI args + env vars + defaults into concrete Args.
fn resolve_args(cli: CliArgs) -> Result<Args> {
    let vx_home = cli
        .vx_home
        .or_else(|| std::env::var("VX_HOME").ok())
        .map(Utf8PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").expect("HOME not set");
            Utf8PathBuf::from(format!("{home}/.vx"))
        });

    // VX_STORE set or --store → connect to existing store. Unset → spawn one.
    let store_endpoint = cli
        .store
        .or_else(|| std::env::var("VX_STORE").ok())
        .map(|v| Endpoint::parse(&v))
        .transpose()?;

    // VX_RUNNER set or --runner → connect to existing runner. Unset → spawn one.
    let exec_endpoint = cli
        .runner
        .or_else(|| std::env::var("VX_RUNNER").ok())
        .map(|v| Endpoint::parse(&v))
        .transpose()?;

    // --listen only (no VXD env — that's for vx connect mode)
    let bind = match cli.listen {
        Some(v) => Endpoint::parse(&v)?,
        None => vx_io::net::default_local_endpoint(&vx_home, "vxd"),
    };

    let build_vx_executor_classes = std::env::var("VX_BUILD_EXECUTOR_CLASSES")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| parse_build_vx_executor_classes(&value))
        .transpose()?
        .unwrap_or_default();

    Ok(Args {
        vx_home,
        store_endpoint,
        exec_endpoint,
        bind,
        build_vx_executor_classes,
    })
}

/// Tracks spawned child services
#[derive(Default)]
pub struct SpawnTracker {
    store: Option<Child>,
    runner: Option<Child>,
}

impl SpawnTracker {
    pub fn kill_all(&mut self) {
        if let Some(ref mut child) = self.store {
            let pid = child.id().unwrap_or(0);
            tracing::info!("Killing spawned vx-store (pid: {pid})");
            if let Err(e) = child.start_kill() {
                tracing::warn!("Failed to kill vx-store (pid: {pid}): {e}");
            }
        }
        self.store.take();
        if let Some(ref mut child) = self.runner {
            let pid = child.id().unwrap_or(0);
            tracing::info!("Killing spawned vx-runner (pid: {pid})");
            if let Err(e) = child.start_kill() {
                tracing::warn!("Failed to kill vx-runner (pid: {pid}): {e}");
            }
        }
        self.runner.take();
    }
}

struct ServiceClients {
    store: Arc<StoreClient>,
    build_vx_executors: crate::service::BuildVxExecutors,
    store_handle: Arc<ServiceHandle>,
}

#[derive(Clone)]
struct VxdVirtualAcceptor {
    boot_service: vx_io::boot::BootService<Arc<VxdService>>,
}

impl ConnectionAcceptor for VxdVirtualAcceptor {
    fn accept(
        &self,
        _conn_id: roam_types::ConnectionId,
        peer_settings: &ConnectionSettings,
        _metadata: &[roam_types::MetadataEntry],
    ) -> std::result::Result<AcceptedConnection, Metadata<'static>> {
        let Some(daemon) = self.boot_service.gate().service().cloned() else {
            return Err(vec![]);
        };

        let parity = peer_settings.parity.other();
        let session = daemon.new_session();
        Ok(AcceptedConnection {
            settings: ConnectionSettings {
                parity,
                max_concurrent_requests: peer_settings.max_concurrent_requests,
            },
            metadata: vec![],
            setup: Box::new(move |handle| {
                let session = session.clone();
                moire::task::spawn(
                    async move {
                        let dispatcher = VxdDispatcher::new(session.clone());
                        let mut driver = Driver::new(handle, dispatcher);
                        let progress_listener =
                            Arc::new(vxd_proto::ProgressListenerClient::new(driver.caller()));
                        session.set_progress_listener(Some(progress_listener)).await;
                        driver.run().await;
                        session.cancel_active_build().await;
                    }
                    .named("vxd_virtual_driver"),
                );
            }),
        })
    }
}

async fn connect_local_runner(
    args: &Args,
    spawn_tracker: &Arc<SyncMutex<SpawnTracker>>,
) -> Result<crate::service::BuildVxExecutor> {
    use vx_io::spawn::ServiceBuilder;

    let store_endpoint = args
        .store_endpoint
        .clone()
        .unwrap_or_else(|| vx_io::net::default_local_endpoint(&args.vx_home, "store"));
    let exec_endpoint = args
        .exec_endpoint
        .clone()
        .unwrap_or_else(|| vx_io::net::default_local_endpoint(&args.vx_home, "runner"));
    let vfsd_endpoint = vx_io::net::default_local_endpoint(&args.vx_home, "vfsd");

    let mut runner_builder = ServiceBuilder::new("vx-runner", exec_endpoint.clone())
        .caller_name("vxd")
        .args(&[
            "--store",
            &store_endpoint.display(),
            "--vfsd",
            &vfsd_endpoint.display(),
            "--vx-home",
            args.vx_home.as_str(),
        ])
        .log_path(args.vx_home.join("runner.log"))
        .timeout(Duration::from_secs(30));
    if args.exec_endpoint.is_some() {
        runner_builder = runner_builder.connect_only();
    }

    let mut runner_handle = runner_builder.connect().await?;
    {
        let mut tracker = spawn_tracker.lock();
        tracker.runner = runner_handle.child.take();
    }
    let runner_virtual = runner_handle.boot_and_connect().await?;
    Ok(crate::service::BuildVxExecutor::local(
        Arc::new(RunnerClient::new(runner_virtual)),
        Some(Arc::new(runner_handle)),
    ))
}

/// Ensure services are running (spawning them if necessary), run the boot
/// protocol on each, and return connected roam clients.
async fn ensure_services(
    args: &Args,
    spawn_tracker: &Arc<SyncMutex<SpawnTracker>>,
    boot_tx: &BootTx,
) -> Result<ServiceClients> {
    use vx_io::spawn::ServiceBuilder;

    let store_endpoint = args
        .store_endpoint
        .clone()
        .unwrap_or_else(|| vx_io::net::default_local_endpoint(&args.vx_home, "store"));
    let store_root = args.vx_home.join("store");

    let mut store_builder = ServiceBuilder::new("vx-store", store_endpoint.clone())
        .caller_name("vxd")
        .args(&["--root", store_root.as_str()])
        .log_path(args.vx_home.join("store.log"));
    if args.store_endpoint.is_some() {
        store_builder = store_builder.connect_only();
    }

    boot_tx.phase("connecting to vx-store");
    let mut store_handle = store_builder.connect().await?;

    {
        let mut tracker = spawn_tracker.lock();
        tracker.store = store_handle.child.take();
    }

    boot_tx.phase("waiting for vx-store boot");
    let store_virtual = store_handle.boot_and_connect().await?;

    let local_store = Arc::new(StoreClient::new(store_virtual));
    boot_tx.phase("preparing local build executor registry");
    let mut build_vx_executors = crate::service::BuildVxExecutors::local_lazy({
        let args = args.clone();
        let spawn_tracker = Arc::clone(spawn_tracker);
        let local_store = Arc::clone(&local_store);
        move |_listener| {
            let args = args.clone();
            let spawn_tracker = Arc::clone(&spawn_tracker);
            let local_store = Arc::clone(&local_store);
            Box::pin(async move {
                let mut executor = connect_local_runner(&args, &spawn_tracker)
                    .await
                    .map_err(|error| error.to_string())?;
                executor.attach_store(Arc::clone(&local_store));
                Ok(executor)
            })
        }
    });
    #[cfg(unix)]
    {
        let vx_home = args.vx_home.clone();
        build_vx_executors =
            build_vx_executors.with_ssh_bootstrap_factory(move |class_label, target, listener| {
                let vx_home = vx_home.clone();
                Box::pin(async move {
                    bootstrap_ssh_executor(&vx_home, &class_label, &target, None, listener)
                        .await
                        .map_err(|error| error.to_string())
                })
            });
    }
    for config in &args.build_vx_executor_classes {
        match config.backend {
            vx_eval::ExecutorBackendKind::Local => {
                boot_tx.phase(format!(
                    "registering build executor class:{} -> local",
                    config.class_label
                ));
                build_vx_executors
                    .register_class_alias(config.class_label.clone(), "local")
                    .map_err(eyre::Report::msg)?;
            }
            vx_eval::ExecutorBackendKind::Daemon => {
                let target = config
                    .target
                    .as_deref()
                    .expect("daemon executor target should be present");
                boot_tx.phase(format!(
                    "connecting configured build executor class:{} ({target})",
                    config.class_label
                ));
                let endpoint = Endpoint::parse(
                    config
                        .target
                        .as_deref()
                        .expect("daemon executor target should be present"),
                )?;
                let handle =
                    ServiceBuilder::new(format!("vx-runner[{}]", config.class_label), endpoint)
                        .caller_name("vxd")
                        .timeout(Duration::from_secs(30))
                        .connect_only()
                        .connect()
                        .await?;
                let virtual_handle = handle.boot_and_connect().await?;
                let runner = Arc::new(RunnerClient::new(virtual_handle));
                build_vx_executors.register_class_executor(
                    config.class_label.clone(),
                    crate::service::BuildVxExecutor::daemon(
                        format!("daemon:{}", config.class_label),
                        runner,
                        Some(Arc::new(handle)),
                    ),
                );
            }
            vx_eval::ExecutorBackendKind::SshBootstrap => {
                let target = config
                    .target
                    .as_deref()
                    .expect("ssh bootstrap executor target should be present");
                boot_tx.phase(format!(
                    "starting configured ssh bootstrap for class:{} -> {target}",
                    config.class_label
                ));
                #[cfg(unix)]
                {
                    let executor = bootstrap_ssh_executor(
                        &args.vx_home,
                        &config.class_label,
                        target,
                        Some(boot_tx),
                        None,
                    )
                    .await?;
                    build_vx_executors
                        .register_class_executor(config.class_label.clone(), executor.into());
                }
                #[cfg(not(unix))]
                {
                    build_vx_executors.register_class_unavailable(
                        config.class_label.clone(),
                        format!("ssh_bootstrap:{}", config.class_label),
                        vx_eval::ExecutorBackendKind::SshBootstrap,
                        format!(
                            "build.vx executor ssh_bootstrap:{} is configured for {target}, but SSH bootstrap is only implemented on unix hosts",
                            config.class_label
                        ),
                    );
                }
            }
            vx_eval::ExecutorBackendKind::Class => unreachable!(),
        }
    }

    Ok(ServiceClients {
        store: local_store,
        build_vx_executors,
        store_handle: Arc::new(store_handle),
    })
}

fn main() -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    rt.block_on(async_main())
}

#[cfg(unix)]
fn maybe_raise_nofile_limit() {
    let mut limits: libc::rlimit = unsafe { std::mem::zeroed() };
    let get_rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) };
    if get_rc != 0 {
        tracing::warn!(
            error = %std::io::Error::last_os_error(),
            "failed to read RLIMIT_NOFILE",
        );
        return;
    }

    let initial_soft = limits.rlim_cur as u64;
    let hard = limits.rlim_max as u64;
    if initial_soft >= RECOMMENDED_NOFILE_SOFT_LIMIT {
        return;
    }

    let desired = RECOMMENDED_NOFILE_SOFT_LIMIT.min(hard);
    if desired > initial_soft {
        limits.rlim_cur = desired as libc::rlim_t;
        let set_rc = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limits) };
        if set_rc != 0 {
            tracing::debug!(
                error = %std::io::Error::last_os_error(),
                initial_soft,
                desired_soft = desired,
                hard_limit = hard,
                "failed to raise RLIMIT_NOFILE",
            );
        }
    }

    let mut after: libc::rlimit = unsafe { std::mem::zeroed() };
    let after_rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut after) };
    if after_rc != 0 {
        tracing::warn!(
            error = %std::io::Error::last_os_error(),
            "failed to re-read RLIMIT_NOFILE after adjustment",
        );
        return;
    }

    let current_soft = after.rlim_cur as u64;
    let current_hard = after.rlim_max as u64;
    if current_soft < RECOMMENDED_NOFILE_SOFT_LIMIT {
        tracing::warn!(
            soft_limit = current_soft,
            hard_limit = current_hard,
            recommended_soft_limit = RECOMMENDED_NOFILE_SOFT_LIMIT,
            "vxd may hit file-descriptor limits under load; consider increasing max open files (shell: `ulimit -n 65536`; launchd: `sudo launchctl limit maxfiles 65536 200000`; systemd: `LimitNOFILE=65536`)",
        );
    }
}

#[cfg(not(unix))]
fn maybe_raise_nofile_limit() {}

async fn async_main() -> Result<()> {
    // Initialize tracing (logs will go to ~/.vx/vxd.log via spawn_service redirection)
    vx_tracing::init_service("vxd=info");
    maybe_raise_nofile_limit();

    let in_test = std::env::var("VX_TEST").is_ok();

    // The daemon outlives its parent process (vx CLI) and shuts down
    // after an idle timeout instead (2s in test mode, 2h in production).

    let cli: CliArgs = args::Driver::new(
        args::builder::<CliArgs>()?
            .cli(|cli| cli.args(std::env::args().skip(1)))
            .help(|h| h.program_name("vxd").version(env!("CARGO_PKG_VERSION")))
            .build(),
    )
    .run()
    .unwrap();
    let args = resolve_args(cli)?;

    // Acquire exclusive lock to prevent multiple vxd instances from racing.
    // The lock is held for the daemon's lifetime and released on exit.
    let lock_path = args.vx_home.join("vxd.lock");
    std::fs::create_dir_all(&args.vx_home)?;
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let fd = lock_file.as_raw_fd();
        // Try to acquire exclusive lock (non-blocking)
        let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                tracing::debug!("Another vxd is already running (lock held)");
                std::process::exit(0);
            }
            eyre::bail!("Failed to acquire lock on {}: {}", lock_path, err);
        }
        tracing::debug!("Acquired exclusive lock on {}", lock_path);
    }

    // Keep lock_file alive for the daemon's lifetime
    let _lock_guard = lock_file;

    let spawn_tracker = Arc::new(SyncMutex::new("vxd/spawn_tracker", SpawnTracker::default()));

    // Resolve host triple early — this is cheap and can fail fast.
    let exec_host_triple = match std::env::var("VX_RUNNER_HOST_TRIPLE") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => {
            if let Some(ep) = &args.exec_endpoint
                && !ep.is_local()
            {
                eyre::bail!(
                    "VX_RUNNER points to a non-local endpoint ({ep}) but VX_RUNNER_HOST_TRIPLE is not set.\n\
                        vxd must know the runner host triple to request the correct Rust toolchain from store.\n\
                        Set VX_RUNNER_HOST_TRIPLE (e.g. x86_64-unknown-linux-gnu) and retry.",
                );
            }

            vx_toolchain::detect_host_triple()
                .map_err(|e| eyre::eyre!("failed to detect host triple: {e}"))?
        }
    };

    // Bind listener BEFORE booting services — so vx CLI can connect immediately.
    // RPC calls will wait for boot to complete (or return the boot error).
    let mut listener = Listener::bind(&args.bind).await?;
    tracing::info!("Daemon listening on {}", args.bind);

    // Set up boot gate — root connections get Boot service only.
    let (gate, boot_tx) = BootGate::<Arc<VxdService>>::new();

    // Shared flag so the idle watchdog can see boot failure immediately.
    let boot_failed = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Spawn service boot in background — listener is already accepting connections.
    {
        let args_for_boot = Args {
            vx_home: args.vx_home.clone(),
            store_endpoint: args.store_endpoint.clone(),
            exec_endpoint: args.exec_endpoint.clone(),
            bind: args.bind.clone(),
            build_vx_executor_classes: args.build_vx_executor_classes.clone(),
        };
        let spawn_tracker = spawn_tracker.clone();
        let gate = Arc::clone(&gate);
        let vx_home = args.vx_home.clone();
        let exec_host_triple = exec_host_triple.clone();
        let boot_failed = boot_failed.clone();
        moire::task::spawn(
            async move {
                tracing::info!("Booting services (vx-store, lazy local runner)");
                boot_tx.phase("starting vx-store");
                match ensure_services(&args_for_boot, &spawn_tracker, &boot_tx).await {
                    Ok(clients) => {
                        tracing::info!("Services booted successfully");
                        boot_tx.phase("finalizing vxd");
                        let daemon = Arc::new(
                            VxdService::new(
                                vx_home,
                                exec_host_triple,
                                VxdServiceDeps {
                                    spawn_tracker,
                                    store: clients.store,
                                    build_vx_executors: clients.build_vx_executors,
                                    store_handle: clients.store_handle,
                                },
                            )
                            .await,
                        );
                        gate.set_service(daemon);
                        boot_tx.ready();
                    }
                    Err(e) => {
                        tracing::error!(e = %format!("{e:#}"), "Service boot failed");
                        boot_failed.store(true, std::sync::atomic::Ordering::Relaxed);
                        boot_tx.failed(format!("{e:#}"));
                    }
                }
            }
            .named("service_boot"),
        );
    }

    let boot_service = gate.boot_service();

    // Track active connections and shut down after idle timeout.
    // Normal: 2 hours, test mode: 2s, boot failed: 2s.
    let active_connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let last_disconnect = Arc::new(SyncMutex::new(
        "vxd/last_disconnect",
        Option::<tokio::time::Instant>::None,
    ));

    {
        let normal_idle_timeout = if in_test {
            Duration::from_secs(2)
        } else {
            Duration::from_secs(2 * 60 * 60)
        };
        let active_connections = active_connections.clone();
        let last_disconnect = last_disconnect.clone();
        let boot_failed = boot_failed.clone();
        moire::task::spawn(
            async move {
                tracing::trace!(
                    idle_timeout = ?normal_idle_timeout,
                    "idle watchdog started"
                );
                let mut idle_timeout = normal_idle_timeout;
                let mut saw_boot_failure = false;
                loop {
                    tokio::time::sleep(Duration::from_millis(400)).await;

                    // If boot failed, shrink idle timeout so we exit quickly
                    // once the client disconnects after receiving the error.
                    if !saw_boot_failure && boot_failed.load(std::sync::atomic::Ordering::Relaxed) {
                        tracing::trace!("boot failed, shrinking idle timeout");
                        idle_timeout = Duration::from_secs(1);
                        saw_boot_failure = true;
                    }

                    let count = active_connections.load(std::sync::atomic::Ordering::Relaxed);
                    if count == 0 {
                        let idle_long_enough = match *last_disconnect.lock() {
                            Some(when) => {
                                let elapsed = when.elapsed();
                                tracing::trace!(
                                    ?elapsed,
                                    ?idle_timeout,
                                    "idle watchdog tick: no active connections"
                                );
                                elapsed >= idle_timeout
                            }
                            // No client ever connected — only shut down if boot failed.
                            None => {
                                tracing::trace!(
                                    saw_boot_failure,
                                    "idle watchdog tick: no connections ever established"
                                );
                                saw_boot_failure
                            }
                        };
                        if idle_long_enough {
                            tracing::info!("no connections for {idle_timeout:?}, shutting down");
                            std::process::exit(0);
                        }
                    } else {
                        tracing::trace!(
                            active_connections = count,
                            "idle watchdog tick: connections active"
                        );
                    }
                }
            }
            .named("idle_shutdown_monitor"),
        );
    }

    tracing::trace!("Starting accept loop");
    loop {
        let (stream, peer_addr) = listener.accept().await?;
        let active_connections = active_connections.clone();
        let last_disconnect = last_disconnect.clone();

        let count = active_connections.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        tracing::trace!(
            %peer_addr,
            active_connections = count,
            "accepted new TCP connection"
        );

        let boot_service = boot_service.clone();

        let conn_name = format!("vxd_connection_{peer_addr}");
        moire::task::spawn(
            async move {
                tracing::debug!("New connection from {}", peer_addr);

                // Root connection gets Boot service only — clients must call boot(),
                // then open a virtual connection for the real Vxd service.
                let (reader, writer) = tokio::io::split(stream);
                let conduit = BareConduit::<roam_types::MessageFamily, _>::new(StreamLink::new(
                    reader, writer,
                ));
                let boot_dispatcher = BootDispatcher::new(boot_service.clone());
                let (caller, session_handle) = match acceptor(conduit)
                    .on_connection(VxdVirtualAcceptor {
                        boot_service: boot_service.clone(),
                    })
                    .establish::<roam_core::NoopCaller>(boot_dispatcher)
                    .await
                {
                    Ok(parts) => parts,
                    Err(e) => {
                        tracing::warn!("Handshake error from {}: {e}", peer_addr);
                        let count = active_connections
                            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed)
                            - 1;
                        tracing::trace!(
                            %peer_addr,
                            active_connections = count,
                            "connection dropped after handshake error"
                        );
                        *last_disconnect.lock() = Some(tokio::time::Instant::now());
                        return;
                    }
                };

                tracing::debug!(
                    %peer_addr,
                    active_connections = active_connections.load(std::sync::atomic::Ordering::Relaxed),
                    "connection established"
                );
                vx_io::net::park_session(caller, session_handle).await;

                let count = active_connections
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed)
                    - 1;
                tracing::trace!(
                    %peer_addr,
                    active_connections = count,
                    "connection closed"
                );
                *last_disconnect.lock() = Some(tokio::time::Instant::now());
            }
            .named(conn_name),
        );
    }
}
