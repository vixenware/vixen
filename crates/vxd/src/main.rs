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
mod activity;
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
use eyre::{Result, WrapErr};
use facet::Facet;
use figue as args;
use flate2::Compression as GzipCompression;
use flate2::write::GzEncoder;
use ignore::WalkBuilder;
use moire::task::FutureExt;
use roam_core::acceptor;
use roam_core::{AcceptedConnection, BareConduit, ConnectionAcceptor, Driver};
use roam_stream::StreamLink;
use roam_types::{ConnectionSettings, Metadata};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tar::Builder as TarBuilder;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use vx_bootstrap::{
    BeginSourceArchiveRequest, BootstrapProgress, BootstrapRequest, BootstrapResponse,
    BuildSourceTreeRequest, GetCachedSourceManifestRequest, PlanSourceSyncRequest,
    SourceArchiveChunkRequest, SourceFileEntry, file_modified_unix_millis,
};
use vx_io::boot::{BootDispatcher, BootGate, BootTx};
use vx_io::net::{Endpoint, Listener};
use vx_io::spawn::ServiceHandle;
use vx_runner_proto::RunnerClient;
use vx_store_proto::StoreClient;
use vxd_proto::{ProgressListenerClient, VxdDispatcher};

use crate::activity::{ActivityHandle, ActivityRegistry};
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
    arch: String,
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
    remote_home: &str,
    class_label: &str,
    store_port: u16,
    vfsd_port: u16,
    runner_port: u16,
) -> SshBootstrapWindowsPaths {
    let class_slug = ssh_bootstrap_label_slug(class_label);
    let base = format!(r"{bootstrap_root}\{class_slug}");
    let state_base = format!(r"{remote_home}\.cache\vixen\ssh-bootstrap\{class_slug}");
    let vx_home = format!(r"{state_base}\vx-home");
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
struct SshBootstrapChromeTrace {
    start: Instant,
    origin_micros: u64,
    process_name: String,
    thread_name: String,
    events: Vec<JsonValue>,
}

#[cfg(unix)]
impl SshBootstrapChromeTrace {
    fn new(process_name: impl Into<String>, thread_name: impl Into<String>) -> Self {
        let process_name = process_name.into();
        let thread_name = thread_name.into();
        let origin_micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
        let mut trace = Self {
            start: Instant::now(),
            origin_micros,
            process_name: process_name.clone(),
            thread_name: thread_name.clone(),
            events: Vec::new(),
        };
        trace.instant("thread_name", serde_json::json!({ "name": thread_name }));
        trace.instant("process_name", serde_json::json!({ "name": process_name }));
        trace
    }

    fn now_micros(&self) -> u64 {
        self.origin_micros + self.start.elapsed().as_micros() as u64
    }

    fn instant(&mut self, name: &str, args: JsonValue) {
        self.events.push(serde_json::json!({
            "name": name,
            "cat": "bootstrap",
            "ph": "i",
            "s": "t",
            "pid": 1,
            "tid": 1,
            "ts": self.now_micros(),
            "args": args,
        }));
    }

    fn complete<T>(&mut self, name: &str, args: JsonValue, f: impl FnOnce() -> T) -> T {
        let ts = self.now_micros();
        let start = Instant::now();
        let result = f();
        let dur = start.elapsed().as_micros() as u64;
        self.events.push(serde_json::json!({
            "name": name,
            "cat": "bootstrap",
            "ph": "X",
            "pid": 1,
            "tid": 1,
            "ts": ts,
            "dur": dur,
            "args": args,
        }));
        result
    }

    async fn complete_async<T>(
        &mut self,
        name: &str,
        args: JsonValue,
        future: impl std::future::Future<Output = T>,
    ) -> T {
        let ts = self.now_micros();
        let start = Instant::now();
        let result = future.await;
        let dur = start.elapsed().as_micros() as u64;
        self.events.push(serde_json::json!({
            "name": name,
            "cat": "bootstrap",
            "ph": "X",
            "pid": 1,
            "tid": 1,
            "ts": ts,
            "dur": dur,
            "args": args,
        }));
        result
    }

    fn write_to(&self, path: &Utf8Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).wrap_err_with(|| format!("failed to create {}", parent))?;
        }
        let payload = serde_json::json!({
            "displayTimeUnit": "ms",
            "otherData": {
                "process_name": self.process_name,
                "thread_name": self.thread_name,
            },
            "traceEvents": self.events,
        });
        let bytes = serde_json::to_vec_pretty(&payload)?;
        fs::write(path, bytes).wrap_err_with(|| format!("failed to write {}", path))?;
        Ok(())
    }
}

#[cfg(unix)]
async fn emit_ssh_bootstrap_progress(
    boot_tx: Option<&BootTx>,
    listener: &Option<Arc<ProgressListenerClient>>,
    class_label: &str,
    message: impl Into<String>,
    activity: Option<&ActivityHandle>,
) {
    let message = message.into();
    if let Some(activity) = activity {
        activity.update_remote(message.clone());
    }
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
async fn with_ssh_bootstrap_heartbeat<F, T>(
    boot_tx: Option<&BootTx>,
    listener: &Option<Arc<ProgressListenerClient>>,
    class_label: &str,
    base_message: &str,
    interval: Duration,
    activity: Option<&ActivityHandle>,
    future: F,
) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    tokio::pin!(future);
    let start = tokio::time::Instant::now();
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await;

    loop {
        tokio::select! {
            result = &mut future => return result,
            _ = ticker.tick() => {
                let elapsed = start.elapsed().as_secs();
                emit_ssh_bootstrap_progress(
                    boot_tx,
                    listener,
                    class_label,
                    format!("{base_message} ({elapsed}s elapsed)"),
                    activity,
                )
                .await;
            }
        }
    }
}

#[cfg(unix)]
const SSH_BOOTSTRAP_REMOTE_LAUNCH_SCRIPT: &str = r#"set -euo pipefail
checkout_dir="$1"
vx_home="$2"
store_root="$3"
store_socket="$4"
vfsd_socket="$5"
runner_socket="$6"

mkdir -p "$(dirname "$checkout_dir")" "$vx_home" "$store_root"
pkill -9 -f "$checkout_dir/target/release/vx-runner" || true
pkill -9 -f "$checkout_dir/target/release/vx-store" || true
pkill -9 -f "$checkout_dir/target/release/vx-vfsd" || true
while pgrep -f "$checkout_dir/target/release/vx-runner" >/dev/null; do sleep 0.1; done
while pgrep -f "$checkout_dir/target/release/vx-store" >/dev/null; do sleep 0.1; done
while pgrep -f "$checkout_dir/target/release/vx-vfsd" >/dev/null; do sleep 0.1; done
rm -f "$store_socket" "$vfsd_socket" "$runner_socket"
trap 'jobs -p | xargs -r kill || true' EXIT
"$checkout_dir/target/release/vx-store" --root "$store_root" --listen "unix:$store_socket" &
for _ in $(seq 1 200); do
  [ -S "$store_socket" ] && break
  sleep 0.1
done
[ -S "$store_socket" ] || { echo "vx-store did not create $store_socket" >&2; exit 1; }
export VX_HOME="$vx_home"
exec "$checkout_dir/target/release/vx-runner" \
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
    let wrapped = format!("$ProgressPreference = 'SilentlyContinue'; {script}");
    let encoded = powershell_encoded_command(&wrapped);
    format!("powershell -NoProfile -ExecutionPolicy Bypass -EncodedCommand {encoded}")
}

#[cfg(unix)]
fn ssh_bootstrap_windows_loader_command(bootstrap_path: &str, compressed_path: &str) -> String {
    let script = format!(
        concat!(
            "$ErrorActionPreference = 'Stop'; ",
            "$dest = '{bootstrap_path}'; ",
            "$srcGz = '{compressed_path}'; ",
            "New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dest) | Out-Null; ",
            "$tmpGz = $dest + '.tmp.' + $PID + '.gz'; ",
            "$tmpExe = $dest + '.tmp.' + $PID + '.exe'; ",
            "[Console]::Error.WriteLine('vx-bootstrap-loader: staging gzip payload'); ",
            "Move-Item -Force $srcGz $tmpGz; ",
            "[Console]::Error.WriteLine('vx-bootstrap-loader: decompressing bootstrap executable'); ",
            "$gzipIn = [System.IO.File]::OpenRead($tmpGz); ",
            "$exeOut = [System.IO.File]::Open($tmpExe, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None); ",
            "try {{ ",
            "  $gzip = New-Object System.IO.Compression.GzipStream($gzipIn, [System.IO.Compression.CompressionMode]::Decompress); ",
            "  try {{ $gzip.CopyTo($exeOut) }} finally {{ $gzip.Dispose() }} ",
            "}} finally {{ $exeOut.Dispose(); $gzipIn.Dispose() }}; ",
            "Remove-Item -Force $tmpGz; ",
            "Move-Item -Force $tmpExe $dest; ",
            "[Console]::Error.WriteLine('vx-bootstrap-loader: starting bootstrap'); ",
            "& $dest --stdio"
        ),
        bootstrap_path = powershell_single_quote(bootstrap_path),
        compressed_path = powershell_single_quote(compressed_path),
    );
    powershell_command_line(&script)
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
            "$storeExe = Join-Path $checkoutDir 'target\\release\\vx-store.exe'; ",
            "$runnerExe = Join-Path $checkoutDir 'target\\release\\vx-runner.exe'; ",
            "$storeProc = Start-Process -FilePath $storeExe -ArgumentList @('--root', $storeRoot, '--listen', $storeEndpoint) -PassThru; ",
            "$storeReady = $false; ",
            "for ($i = 0; $i -lt 200; $i++) {{ ",
            "  try {{ ",
            "    $client = [System.Net.Sockets.TcpClient]::new(); ",
            "    try {{ $client.Connect('127.0.0.1', [int]($storeEndpoint.Split(':')[1])); $storeReady = $true; break }} ",
            "    finally {{ $client.Dispose() }} ",
            "  }} catch {{ Start-Sleep -Milliseconds 100 }} ",
            "}}; ",
            "if (-not $storeReady) {{ throw ('vx-store did not start listening on ' + $storeEndpoint) }}; ",
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

fn resolve_vixen_repo_root() -> Result<Utf8PathBuf> {
    let manifest_dir = Utf8Path::new(VIXEN_REPO_MANIFEST_DIR);
    let repo_root = manifest_dir
        .parent()
        .and_then(Utf8Path::parent)
        .ok_or_else(|| eyre::eyre!("failed to resolve Vixen repo root from {manifest_dir}"))?;
    Ok(repo_root.to_path_buf())
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
fn ssh_scp_windows_dest(target: &str, remote_path: &str) -> String {
    let normalized = remote_path.replace('\\', "/");
    format!("{target}:/{normalized}")
}

#[cfg(unix)]
fn ssh_scp_unix_dest(target: &str, remote_path: &str) -> String {
    format!("{target}:{remote_path}")
}

#[cfg(unix)]
async fn ssh_control_scp_file(
    control_path: &Utf8Path,
    local_path: &Utf8Path,
    remote_dest: &str,
) -> Result<()> {
    let output = moire::process::Command::new("scp")
        .arg("-o")
        .arg(format!("ControlPath={control_path}"))
        .arg("-o")
        .arg("ControlMaster=no")
        .arg(local_path.as_std_path())
        .arg(remote_dest)
        .output()
        .await?;
    if !output.status.success() {
        eyre::bail!(
            "scp to {remote_dest} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(unix)]
fn write_local_bootstrap_archive(stem: &str, compressed_bytes: &[u8]) -> Result<Utf8PathBuf> {
    let temp_dir = Utf8PathBuf::from_path_buf(std::env::temp_dir())
        .map_err(|path| eyre::eyre!("temporary directory is not utf-8: {}", path.display()))?;
    let path = temp_dir.join(format!("{stem}-{}.gz", ssh_bootstrap_unique_suffix()));
    fs::write(path.as_std_path(), compressed_bytes)
        .wrap_err_with(|| format!("failed to write {}", path))?;
    Ok(path)
}

#[cfg(unix)]
const SOURCE_ARCHIVE_CHUNK_BYTES: usize = 4 * 1024 * 1024;

#[cfg(unix)]
struct SourceArchiveChunkWriter {
    sender: mpsc::UnboundedSender<Result<Vec<u8>, String>>,
    buffer: Vec<u8>,
}

#[cfg(unix)]
impl SourceArchiveChunkWriter {
    fn new(sender: mpsc::UnboundedSender<Result<Vec<u8>, String>>) -> Self {
        Self {
            sender,
            buffer: Vec::with_capacity(SOURCE_ARCHIVE_CHUNK_BYTES),
        }
    }

    fn emit_full_chunks(&mut self) -> io::Result<()> {
        while self.buffer.len() >= SOURCE_ARCHIVE_CHUNK_BYTES {
            let tail = self.buffer.split_off(SOURCE_ARCHIVE_CHUNK_BYTES);
            let chunk = std::mem::replace(&mut self.buffer, tail);
            self.sender.send(Ok(chunk)).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "bootstrap archive receiver dropped",
                )
            })?;
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Write for SourceArchiveChunkWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        self.emit_full_chunks()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buffer.is_empty() {
            let chunk = std::mem::take(&mut self.buffer);
            self.sender.send(Ok(chunk)).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "bootstrap archive receiver dropped",
                )
            })?;
        }
        Ok(())
    }
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
                "$envInfo = [ordered]@{ ",
                "  HOME = [Environment]::GetFolderPath('UserProfile'); ",
                "  ARCH = (Get-VxEnv 'PROCESSOR_ARCHITECTURE'); ",
                "  CARGO_HOME = (Get-VxEnv 'CARGO_HOME'); ",
                "  RUSTUP_HOME = (Get-VxEnv 'RUSTUP_HOME'); ",
                "  TMP = (Get-VxEnv 'TMP'); ",
                "  TEMP = (Get-VxEnv 'TEMP') ",
                "}; ",
                "[Console]::Out.Write(($envInfo | ConvertTo-Json -Compress))"
            ),
        )
        .await?;
        let env_dump = serde_json::from_str::<JsonValue>(&env_dump)
            .map_err(|error| eyre::eyre!("failed to parse Windows target env dump: {error}"))?;
        let json_string = |key: &str| {
            env_dump
                .get(key)
                .and_then(JsonValue::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        };
        let home = json_string("HOME");
        let cargo_home = json_string("CARGO_HOME");
        let rustup_home = json_string("RUSTUP_HOME");
        let tmp = json_string("TMP");
        let temp = json_string("TEMP");
        let arch = json_string("ARCH").unwrap_or_else(|| "unknown".to_string());
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
            arch,
            windows_bootstrap_root: Some(bootstrap_root),
        });
    }

    let home = ssh_control_output_unix(control_path, target, &["printenv", "HOME"]).await?;
    let arch = ssh_control_output_unix(control_path, target, &["uname", "-m"]).await?;
    if home.is_empty() {
        eyre::bail!("ssh bootstrap target {target} returned an empty HOME");
    }

    Ok(SshBootstrapTargetInfo {
        platform: SshBootstrapTargetPlatform::Unix,
        home,
        arch,
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
async fn write_bootstrap_request_frame<W>(writer: &mut W, request: &BootstrapRequest) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let payload = facet_postcard::to_vec(request)?;
    writer
        .write_all(&(payload.len() as u32).to_le_bytes())
        .await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(unix)]
async fn read_bootstrap_response_frame<R>(reader: &mut R) -> Result<Option<BootstrapResponse>>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0_u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0_u8; len];
    reader.read_exact(&mut payload).await?;
    Ok(Some(facet_postcard::from_slice(&payload)?))
}

#[cfg(unix)]
async fn read_bootstrap_final_response_frame<R>(
    reader: &mut R,
    boot_tx: Option<&BootTx>,
    progress_listener: &Option<Arc<ProgressListenerClient>>,
    class_label: &str,
    activity: Option<&ActivityHandle>,
) -> Result<Option<BootstrapResponse>>
where
    R: AsyncRead + Unpin,
{
    loop {
        let Some(response) = read_bootstrap_response_frame(reader).await? else {
            return Ok(None);
        };
        match response {
            BootstrapResponse::Progress(progress) => {
                handle_bootstrap_progress_event(
                    boot_tx,
                    progress_listener,
                    class_label,
                    progress,
                    activity,
                )
                .await;
            }
            other => return Ok(Some(other)),
        }
    }
}

#[cfg(unix)]
async fn handle_bootstrap_progress_event(
    boot_tx: Option<&BootTx>,
    progress_listener: &Option<Arc<ProgressListenerClient>>,
    class_label: &str,
    progress: BootstrapProgress,
    activity: Option<&ActivityHandle>,
) {
    let suffix = match (progress.current, progress.total) {
        (Some(current), Some(total)) => format!(" ({current}/{total})"),
        _ => String::new(),
    };
    emit_ssh_bootstrap_progress(
        boot_tx,
        progress_listener,
        class_label,
        format!(
            "vx-bootstrap {}: {}{}",
            progress.phase, progress.message, suffix
        ),
        activity,
    )
    .await;
}

#[cfg(unix)]
fn produce_source_archive_chunks(
    repo_root: Utf8PathBuf,
    missing_paths: Vec<String>,
    sender: mpsc::UnboundedSender<Result<Vec<u8>, String>>,
) -> Result<()> {
    let sender_for_error = sender.clone();
    let result = (|| -> Result<()> {
        let writer = SourceArchiveChunkWriter::new(sender);
        let encoder = GzEncoder::new(writer, GzipCompression::default());
        let mut builder = TarBuilder::new(encoder);
        for relpath in &missing_paths {
            let source_path = repo_root.join(relpath);
            builder
                .append_path_with_name(source_path.as_std_path(), relpath)
                .wrap_err_with(|| format!("failed to append {} to source archive", source_path))?;
        }
        let encoder = builder
            .into_inner()
            .wrap_err("failed to finish streamed source tar archive")?;
        let mut writer = encoder
            .finish()
            .wrap_err("failed to finish streamed source gzip archive")?;
        writer
            .flush()
            .wrap_err("failed to flush streamed source archive")?;
        Ok(())
    })();
    if let Err(error) = &result {
        let _ = sender_for_error.send(Err(format!("{error:#}")));
    }
    result
}

#[cfg(unix)]
async fn stream_source_archive_over_bootstrap<W, R>(
    stdin: &mut W,
    stdout: &mut R,
    context: SourceArchiveStreamContext<'_>,
    trace: &mut SshBootstrapChromeTrace,
) -> Result<(usize, usize)>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let stream_start = Instant::now();
    let stream_start_ts = trace.now_micros();
    if context.missing_paths.is_empty() {
        trace.instant(
            "remote_stream_source_archive_summary",
            serde_json::json!({
                "uploaded_files": 0,
                "uploaded_bytes": 0,
                "uploaded_chunks": 0,
            }),
        );
        return Ok((0, 0));
    }

    let files = context
        .missing_paths
        .iter()
        .map(|path| {
            context
                .local_source_map
                .get(path)
                .cloned()
                .ok_or_else(|| eyre::eyre!("vx-bootstrap requested unknown source file {path}"))
        })
        .collect::<Result<Vec<_>>>()?;

    trace
        .complete_async(
            "remote_begin_source_archive",
            serde_json::json!({
                "file_count": files.len(),
            }),
            async {
                write_bootstrap_request_frame(
                    stdin,
                    &BootstrapRequest::BeginSourceArchive(BeginSourceArchiveRequest {
                        checkout_dir: context.checkout_dir.to_string(),
                        files,
                    }),
                )
                .await?;
                read_bootstrap_final_response_frame(
                    stdout,
                    context.progress.boot_tx,
                    context.progress.progress_listener,
                    context.progress.class_label,
                    context.progress.activity,
                )
                .await
            },
        )
        .await?
        .ok_or_else(|| eyre::eyre!("vx-bootstrap closed before begin_source_archive response"))
        .and_then(|response| match response {
            BootstrapResponse::BeginSourceArchiveReady => Ok(()),
            BootstrapResponse::Error(error) => eyre::bail!(
                "vx-bootstrap begin_source_archive failed: {}: {}",
                error.kind,
                error.message
            ),
            other => {
                eyre::bail!("unexpected vx-bootstrap begin_source_archive response: {other:?}")
            }
        })?;

    let (sender, mut receiver) = mpsc::unbounded_channel::<Result<Vec<u8>, String>>();
    let missing_count = context.missing_paths.len();
    let repo_root = context.repo_root.to_path_buf();
    let missing_paths = context.missing_paths.to_vec();
    let producer = tokio::task::spawn_blocking(move || {
        produce_source_archive_chunks(repo_root, missing_paths, sender)
    });

    let mut uploaded_bytes = 0usize;
    let mut uploaded_chunks = 0usize;
    while let Some(item) = receiver.recv().await {
        let chunk = match item {
            Ok(chunk) => chunk,
            Err(message) => eyre::bail!("failed to produce source archive stream: {message}"),
        };
        uploaded_bytes += chunk.len();
        uploaded_chunks += 1;
        write_bootstrap_request_frame(
            stdin,
            &BootstrapRequest::SourceArchiveChunk(SourceArchiveChunkRequest { bytes: chunk }),
        )
        .await?;
    }
    producer
        .await
        .map_err(|error| eyre::eyre!("source archive producer task failed: {error}"))??;

    let applied_files = trace
        .complete_async(
            "remote_finish_source_archive",
            serde_json::json!({
                "uploaded_bytes": uploaded_bytes,
                "uploaded_chunks": uploaded_chunks,
            }),
            async {
                write_bootstrap_request_frame(stdin, &BootstrapRequest::FinishSourceArchive)
                    .await?;
                read_bootstrap_final_response_frame(
                    stdout,
                    context.progress.boot_tx,
                    context.progress.progress_listener,
                    context.progress.class_label,
                    context.progress.activity,
                )
                .await
            },
        )
        .await?
        .ok_or_else(|| eyre::eyre!("vx-bootstrap closed before finish_source_archive response"))
        .and_then(|response| match response {
            BootstrapResponse::SourceArchiveApplied(response) => Ok(response.applied_files),
            BootstrapResponse::Error(error) => eyre::bail!(
                "vx-bootstrap finish_source_archive failed: {}: {}",
                error.kind,
                error.message
            ),
            other => {
                eyre::bail!("unexpected vx-bootstrap finish_source_archive response: {other:?}")
            }
        })?;

    trace.instant(
        "remote_stream_source_archive_summary",
        serde_json::json!({
            "uploaded_files": applied_files,
            "uploaded_bytes": uploaded_bytes,
            "uploaded_chunks": uploaded_chunks,
        }),
    );
    trace.events.push(serde_json::json!({
        "name": "remote_stream_source_archive",
        "cat": "bootstrap",
        "ph": "X",
        "pid": 1,
        "tid": 1,
        "ts": stream_start_ts,
        "dur": stream_start.elapsed().as_micros() as u64,
        "args": {
            "missing_count": missing_count,
            "uploaded_chunks": uploaded_chunks,
            "uploaded_bytes": uploaded_bytes,
        },
    }));

    Ok((applied_files as usize, uploaded_bytes))
}

#[cfg(unix)]
#[derive(Clone, Copy)]
struct SshBootstrapProgressContext<'a> {
    boot_tx: Option<&'a BootTx>,
    progress_listener: &'a Option<Arc<ProgressListenerClient>>,
    class_label: &'a str,
    activity: Option<&'a ActivityHandle>,
}

#[cfg(unix)]
struct SourceArchiveStreamContext<'a> {
    repo_root: &'a Utf8Path,
    checkout_dir: &'a str,
    local_source_map: &'a BTreeMap<String, SourceFileEntry>,
    missing_paths: &'a [String],
    progress: SshBootstrapProgressContext<'a>,
}

#[cfg(unix)]
fn spawn_bootstrap_stderr_drain(
    child: &mut moire::process::Child,
) -> Result<tokio::task::JoinHandle<std::io::Result<Vec<u8>>>> {
    let mut stderr = child
        .stderr()
        .take()
        .ok_or_else(|| eyre::eyre!("failed to capture ssh bootstrap stderr"))?;
    Ok(tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).await?;
        Ok(bytes)
    }))
}

#[cfg(unix)]
async fn finish_bootstrap_child(
    mut child: moire::process::Child,
    stderr_task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
) -> Result<std::process::Output> {
    let status = child.wait().await?;
    let stderr = stderr_task
        .await
        .map_err(|error| eyre::eyre!("failed to join ssh bootstrap stderr task: {error}"))??;
    Ok(std::process::Output {
        status,
        stdout: Vec::new(),
        stderr,
    })
}

#[cfg(unix)]
fn unix_bootstrap_target_triple(arch: &str) -> Result<&'static str> {
    match arch.to_ascii_lowercase().as_str() {
        "aarch64" | "arm64" => Ok("aarch64-unknown-linux-gnu"),
        "x86_64" | "amd64" => Ok("x86_64-unknown-linux-gnu"),
        other => eyre::bail!("unsupported unix ssh bootstrap architecture: {other}"),
    }
}

#[cfg(unix)]
fn windows_bootstrap_target_triple(arch: &str) -> Result<&'static str> {
    match arch.to_ascii_lowercase().as_str() {
        "x86_64" | "amd64" => Ok("x86_64-pc-windows-msvc"),
        other => eyre::bail!("unsupported windows ssh bootstrap architecture: {other}"),
    }
}

#[cfg(unix)]
fn local_vx_bootstrap_binary_path(
    repo_root: &Utf8Path,
    target_triple: &str,
    binary_name: &str,
) -> Utf8PathBuf {
    repo_root
        .join("target")
        .join(target_triple)
        .join("release")
        .join(binary_name)
}

#[cfg(unix)]
fn local_vx_bootstrap_stamp_path(binary_path: &Utf8Path) -> Utf8PathBuf {
    binary_path.with_extension(format!(
        "{}.stamp",
        binary_path.extension().unwrap_or("bin")
    ))
}

#[cfg(unix)]
fn vx_bootstrap_binary_fingerprint(path: &Utf8Path) -> Result<String> {
    let bytes = fs::read(path.as_std_path())
        .wrap_err_with(|| format!("failed to read vx-bootstrap binary at {path}"))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

#[cfg(unix)]
async fn ensure_local_host_vx_bootstrap_binary(repo_root: &Utf8Path) -> Result<Utf8PathBuf> {
    let binary_path = repo_root
        .join("target")
        .join("release")
        .join("vx-bootstrap");
    let output = moire::process::Command::new("cargo")
        .arg("build")
        .arg("-p")
        .arg("vx-bootstrap")
        .arg("--release")
        .current_dir(repo_root)
        .output()
        .await?;
    if !output.status.success() {
        eyre::bail!(
            "cargo build for host vx-bootstrap failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if !binary_path.exists() {
        eyre::bail!(
            "cargo build for host vx-bootstrap completed, but {} was not produced",
            binary_path
        );
    }
    Ok(binary_path)
}

#[cfg(unix)]
async fn ensure_local_unix_vx_bootstrap_binary(
    repo_root: &Utf8Path,
    arch: &str,
) -> Result<Utf8PathBuf> {
    let target_triple = unix_bootstrap_target_triple(arch)?;
    let binary_path = local_vx_bootstrap_binary_path(repo_root, target_triple, "vx-bootstrap");
    let output = moire::process::Command::new("cargo")
        .arg("zigbuild")
        .arg("-p")
        .arg("vx-bootstrap")
        .arg("--release")
        .arg("--target")
        .arg(target_triple)
        .current_dir(repo_root)
        .output()
        .await?;
    if !output.status.success() {
        eyre::bail!(
            "cargo zigbuild for vx-bootstrap ({target_triple}) failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if !binary_path.exists() {
        eyre::bail!(
            "cargo zigbuild for vx-bootstrap completed, but {} was not produced",
            binary_path
        );
    }
    Ok(binary_path)
}

#[cfg(unix)]
async fn ensure_local_windows_vx_bootstrap_binary(
    repo_root: &Utf8Path,
    arch: &str,
) -> Result<Utf8PathBuf> {
    let host_binary = ensure_local_host_vx_bootstrap_binary(repo_root).await?;
    let host_fingerprint = vx_bootstrap_binary_fingerprint(&host_binary)?;
    let target_triple = windows_bootstrap_target_triple(arch)?;
    let binary_path = local_vx_bootstrap_binary_path(repo_root, target_triple, "vx-bootstrap.exe");
    let stamp_path = local_vx_bootstrap_stamp_path(&binary_path);
    let current_stamp = fs::read_to_string(stamp_path.as_std_path())
        .ok()
        .map(|stamp| stamp.trim().to_string());
    if binary_path.exists() && current_stamp.as_deref() == Some(host_fingerprint.as_str()) {
        return Ok(binary_path);
    }
    let output = moire::process::Command::new("cargo")
        .arg("xwin")
        .arg("build")
        .arg("-p")
        .arg("vx-bootstrap")
        .arg("--release")
        .arg("--target")
        .arg(target_triple)
        .current_dir(repo_root)
        .output()
        .await?;
    if !output.status.success() {
        eyre::bail!(
            "cargo xwin build for vx-bootstrap ({target_triple}) failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if !binary_path.exists() {
        eyre::bail!(
            "cargo xwin build for vx-bootstrap completed, but {} was not produced",
            binary_path
        );
    }
    fs::write(stamp_path.as_std_path(), format!("{host_fingerprint}\n"))
        .wrap_err_with(|| format!("failed to write bootstrap stamp {}", stamp_path))?;
    Ok(binary_path)
}

#[cfg(unix)]
fn ssh_bootstrap_unix_loader_command(bootstrap_path: &str, compressed_path: &str) -> String {
    format!(
        "sh -lc {} sh {} {}",
        shell_quote(
            r#"set -euo pipefail
dest="$1"
src="$2"
mkdir -p "$(dirname "$dest")"
tmp="${dest}.tmp.$$"
gzip -dc "$src" > "$tmp"
chmod +x "$tmp"
mv "$tmp" "$dest"
rm -f "$src"
exec "$dest" --stdio
"#
        ),
        shell_quote(bootstrap_path),
        shell_quote(compressed_path),
    )
}

#[cfg(unix)]
fn should_skip_local_bootstrap_source_path(relpath: &Utf8Path) -> bool {
    if relpath.starts_with("apps/mac/xcode/build") || relpath.starts_with("apps/mac/dist") {
        return true;
    }
    relpath.components().enumerate().any(|(index, component)| {
        if matches!(component.as_str(), ".build" | ".git" | "target") {
            return true;
        }
        index == 0
            && matches!(
                component.as_str(),
                ".DS_Store"
                    | ".claude"
                    | ".git"
                    | ".handoffs"
                    | ".ship"
                    | ".tracey"
                    | ".vx"
                    | ".zed"
                    | "target"
            )
    })
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, Default)]
struct LocalBootstrapSourceManifestStats {
    reused_count: usize,
    hashed_count: usize,
}

#[cfg(unix)]
fn collect_local_bootstrap_source_files(
    repo_root: &Utf8Path,
    cached_manifest: &BTreeMap<String, SourceFileEntry>,
    files: &mut Vec<SourceFileEntry>,
    stats: &mut LocalBootstrapSourceManifestStats,
) -> Result<()> {
    let walker = WalkBuilder::new(repo_root)
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .ignore(true)
        .build();
    for entry in walker {
        let entry = entry.wrap_err_with(|| format!("failed to walk {}", repo_root))?;
        let path = Utf8PathBuf::from_path_buf(entry.into_path()).map_err(|path| {
            eyre::eyre!("non-utf8 path in Vixen source tree: {}", path.display())
        })?;
        if path == repo_root {
            continue;
        }
        let relpath = path
            .strip_prefix(repo_root)
            .map_err(|error| eyre::eyre!("failed to strip {} from {}: {error}", repo_root, path))?;
        if should_skip_local_bootstrap_source_path(relpath) {
            continue;
        }
        let metadata =
            fs::symlink_metadata(&path).wrap_err_with(|| format!("failed to stat {}", path))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let relpath_string = relpath.as_str().to_string();
        let executable = metadata.permissions().mode() & 0o111 != 0;
        let modified_unix_millis = file_modified_unix_millis(&metadata);
        if let Some(cached_entry) = cached_manifest.get(&relpath_string)
            && cached_entry.size == metadata.len()
            && cached_entry.executable == executable
            && modified_unix_millis.is_some()
            && cached_entry.modified_unix_millis == modified_unix_millis
        {
            files.push(cached_entry.clone());
            stats.reused_count += 1;
            continue;
        }
        let contents = fs::read(&path).wrap_err_with(|| format!("failed to read {}", path))?;
        files.push(SourceFileEntry {
            path: relpath_string,
            blake3_hex: blake3::hash(&contents).to_hex().to_string(),
            executable,
            size: metadata.len(),
            modified_unix_millis,
        });
        stats.hashed_count += 1;
    }
    Ok(())
}

#[cfg(unix)]
fn local_bootstrap_source_files(
    repo_root: &Utf8Path,
    cached_manifest: &BTreeMap<String, SourceFileEntry>,
) -> Result<(Vec<SourceFileEntry>, LocalBootstrapSourceManifestStats)> {
    let mut files = Vec::new();
    let mut stats = LocalBootstrapSourceManifestStats::default();
    collect_local_bootstrap_source_files(repo_root, cached_manifest, &mut files, &mut stats)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok((files, stats))
}

#[cfg(unix)]
async fn prepare_ssh_bootstrap_unix_checkout_via_bootstrap(
    control_path: &Utf8Path,
    target: &str,
    repo_root: &Utf8Path,
    target_info: &SshBootstrapTargetInfo,
    progress: SshBootstrapProgressContext<'_>,
    trace: &mut SshBootstrapChromeTrace,
) -> Result<String> {
    let local_bootstrap = trace
        .complete_async(
            "local_vx_bootstrap_build",
            serde_json::json!({
                "arch": target_info.arch,
            }),
            ensure_local_unix_vx_bootstrap_binary(repo_root, &target_info.arch),
        )
        .await?;
    let bootstrap_bytes = trace.complete(
        "read_local_vx_bootstrap_binary",
        serde_json::json!({
            "path": local_bootstrap.as_str(),
        }),
        || {
            fs::read(local_bootstrap.as_std_path()).wrap_err_with(|| {
                format!("failed to read local vx-bootstrap binary at {local_bootstrap}")
            })
        },
    )?;
    let compressed_bootstrap_bytes = trace.complete(
        "compress_local_vx_bootstrap_binary",
        serde_json::json!({
            "raw_bytes": bootstrap_bytes.len(),
        }),
        || -> Result<Vec<u8>> {
            let mut encoder = GzEncoder::new(Vec::new(), GzipCompression::default());
            encoder.write_all(&bootstrap_bytes)?;
            Ok(encoder.finish()?)
        },
    )?;
    let paths = ssh_bootstrap_paths(&target_info.home, progress.class_label);
    let remote_bootstrap_path = format!("{}/vx-bootstrap", paths.vx_home);
    let remote_compressed_bootstrap_path = format!("{}/vx-bootstrap.gz", paths.vx_home);
    let local_bootstrap_archive = trace.complete(
        "write_local_vx_bootstrap_archive",
        serde_json::json!({
            "compressed_bytes": compressed_bootstrap_bytes.len(),
        }),
        || write_local_bootstrap_archive("vx-bootstrap-unix", &compressed_bootstrap_bytes),
    )?;
    let upload_result = trace
        .complete_async(
            "upload_vx_bootstrap_binary",
            serde_json::json!({
                "compressed_bytes": compressed_bootstrap_bytes.len(),
                "raw_bytes": bootstrap_bytes.len(),
            }),
            ssh_control_scp_file(
                control_path,
                &local_bootstrap_archive,
                &ssh_scp_unix_dest(target, &remote_compressed_bootstrap_path),
            ),
        )
        .await;
    let _ = fs::remove_file(local_bootstrap_archive.as_std_path());
    upload_result?;
    let remote_command = ssh_bootstrap_unix_loader_command(
        &remote_bootstrap_path,
        &remote_compressed_bootstrap_path,
    );

    let mut command = ssh_control_command(control_path, target);
    command
        .arg(remote_command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = trace.complete(
        "spawn_ssh_bootstrap_loader",
        serde_json::json!({
            "target": target,
        }),
        || command.spawn(),
    )?;
    let mut stdin = child
        .stdin()
        .take()
        .ok_or_else(|| eyre::eyre!("failed to capture ssh bootstrap stdin"))?;
    let mut stdout = child
        .stdout()
        .take()
        .ok_or_else(|| eyre::eyre!("failed to capture ssh bootstrap stdout"))?;
    let stderr_task = spawn_bootstrap_stderr_drain(&mut child)?;

    trace
        .complete_async("bootstrap_hello_roundtrip", serde_json::json!({}), async {
            write_bootstrap_request_frame(&mut stdin, &BootstrapRequest::Hello).await?;
            read_bootstrap_final_response_frame(
                &mut stdout,
                progress.boot_tx,
                progress.progress_listener,
                progress.class_label,
                progress.activity,
            )
            .await
        })
        .await?
        .ok_or_else(|| eyre::eyre!("vx-bootstrap closed before hello response"))
        .and_then(|response| match response {
            BootstrapResponse::Hello(_) => Ok(()),
            BootstrapResponse::Error(error) => eyre::bail!(
                "vx-bootstrap hello failed: {}: {}",
                error.kind,
                error.message
            ),
            other => eyre::bail!("unexpected vx-bootstrap hello response: {other:?}"),
        })?;
    let cached_manifest_entries = trace
        .complete_async(
            "remote_get_cached_source_manifest",
            serde_json::json!({}),
            async {
                write_bootstrap_request_frame(
                    &mut stdin,
                    &BootstrapRequest::GetCachedSourceManifest(GetCachedSourceManifestRequest {
                        checkout_dir: paths.checkout_dir.clone(),
                    }),
                )
                .await?;
                read_bootstrap_final_response_frame(
                    &mut stdout,
                    progress.boot_tx,
                    progress.progress_listener,
                    progress.class_label,
                    progress.activity,
                )
                .await
            },
        )
        .await?
        .ok_or_else(|| {
            eyre::eyre!("vx-bootstrap closed before get_cached_source_manifest response")
        })
        .and_then(|response| match response {
            BootstrapResponse::GetCachedSourceManifest(response) => Ok(response.files),
            BootstrapResponse::Error(error) => eyre::bail!(
                "vx-bootstrap get_cached_source_manifest failed: {}: {}",
                error.kind,
                error.message
            ),
            other => eyre::bail!(
                "unexpected vx-bootstrap get_cached_source_manifest response: {other:?}"
            ),
        })?;
    let cached_manifest = cached_manifest_entries
        .into_iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    let (local_source_files, manifest_stats) = trace.complete(
        "walk_local_source_manifest",
        serde_json::json!({
            "repo_root": repo_root.as_str(),
            "cached_entries": cached_manifest.len(),
        }),
        || local_bootstrap_source_files(repo_root, &cached_manifest),
    )?;
    trace.instant(
        "walk_local_source_manifest_summary",
        serde_json::json!({
            "reused_count": manifest_stats.reused_count,
            "hashed_count": manifest_stats.hashed_count,
        }),
    );
    let local_source_map = local_source_files
        .iter()
        .cloned()
        .map(|entry| (entry.path.clone(), entry))
        .collect::<BTreeMap<_, _>>();

    let request = PlanSourceSyncRequest {
        checkout_dir: paths.checkout_dir.clone(),
        files: local_source_files,
    };
    let plan_file_count = request.files.len();
    let plan_response = trace
        .complete_async(
            "remote_plan_source_sync",
            serde_json::json!({
                "file_count": plan_file_count,
            }),
            async {
                write_bootstrap_request_frame(
                    &mut stdin,
                    &BootstrapRequest::PlanSourceSync(request),
                )
                .await?;
                read_bootstrap_final_response_frame(
                    &mut stdout,
                    progress.boot_tx,
                    progress.progress_listener,
                    progress.class_label,
                    progress.activity,
                )
                .await
            },
        )
        .await?
        .ok_or_else(|| eyre::eyre!("vx-bootstrap closed before plan_source_sync response"))?;
    let missing_paths = match plan_response {
        BootstrapResponse::PlanSourceSync(response) => {
            for deleted in &response.deleted_paths {
                tracing::debug!(
                    target,
                    class_label = progress.class_label,
                    path = deleted,
                    "vx-bootstrap deleted stale source file"
                );
            }
            trace.instant(
                "remote_plan_source_sync_summary",
                serde_json::json!({
                    "missing_count": response.missing_paths.len(),
                    "deleted_count": response.deleted_paths.len(),
                }),
            );
            response.missing_paths
        }
        BootstrapResponse::Error(error) => eyre::bail!(
            "vx-bootstrap plan_source_sync failed: {}: {}",
            error.kind,
            error.message
        ),
        other => eyre::bail!("unexpected vx-bootstrap plan_source_sync response: {other:?}"),
    };

    let (uploaded_files, uploaded_bytes) = stream_source_archive_over_bootstrap(
        &mut stdin,
        &mut stdout,
        SourceArchiveStreamContext {
            repo_root,
            checkout_dir: &paths.checkout_dir,
            local_source_map: &local_source_map,
            missing_paths: &missing_paths,
            progress,
        },
        trace,
    )
    .await?;
    if uploaded_files > 0 {
        emit_ssh_bootstrap_progress(
            progress.boot_tx,
            progress.progress_listener,
            progress.class_label,
            format!(
                "vx-bootstrap upload progress: {uploaded_files} files ({uploaded_bytes} bytes)"
            ),
            progress.activity,
        )
        .await;
    }

    let request = BuildSourceTreeRequest {
        checkout_dir: paths.checkout_dir.clone(),
        build_manifest_relpath: "Cargo.toml".to_string(),
        build_packages: vec![
            "vx-store".to_string(),
            "vx-vfsd".to_string(),
            "vx-runner".to_string(),
            "landlock-exec".to_string(),
        ],
        scrub_files: vec![
            "crates/vx/Cargo.toml".to_string(),
            "crates/vxd/Cargo.toml".to_string(),
        ],
        scrub_line_contains: vec!["tree-sitter-vixen = ".to_string()],
    };
    let package_count = request.build_packages.len();
    let checkout_dir = trace
        .complete_async(
            "remote_build_source_tree",
            serde_json::json!({
                "package_count": package_count,
            }),
            async {
                write_bootstrap_request_frame(
                    &mut stdin,
                    &BootstrapRequest::BuildSourceTree(request),
                )
                .await?;
                read_bootstrap_final_response_frame(
                    &mut stdout,
                    progress.boot_tx,
                    progress.progress_listener,
                    progress.class_label,
                    progress.activity,
                )
                .await
            },
        )
        .await?
        .ok_or_else(|| eyre::eyre!("vx-bootstrap closed before build_source_tree response"))
        .and_then(|response| match response {
            BootstrapResponse::BuildSourceTree(response) => Ok(response.checkout_dir),
            BootstrapResponse::Error(error) => eyre::bail!(
                "vx-bootstrap build_source_tree failed: {}: {}",
                error.kind,
                error.message
            ),
            other => eyre::bail!("unexpected vx-bootstrap build_source_tree response: {other:?}"),
        })?;
    trace
        .complete_async(
            "bootstrap_shutdown_roundtrip",
            serde_json::json!({}),
            async {
                write_bootstrap_request_frame(&mut stdin, &BootstrapRequest::Shutdown).await?;
                read_bootstrap_final_response_frame(
                    &mut stdout,
                    progress.boot_tx,
                    progress.progress_listener,
                    progress.class_label,
                    progress.activity,
                )
                .await
            },
        )
        .await?;
    drop(stdin);
    let output = trace
        .complete_async(
            "wait_for_ssh_bootstrap_exit",
            serde_json::json!({}),
            finish_bootstrap_child(child, stderr_task),
        )
        .await?;
    if !output.status.success() {
        eyre::bail!(
            "vx-bootstrap ssh session failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(checkout_dir)
}

#[cfg(unix)]
async fn prepare_ssh_bootstrap_windows_checkout_via_bootstrap(
    control_path: &Utf8Path,
    target: &str,
    repo_root: &Utf8Path,
    target_info: &SshBootstrapTargetInfo,
    paths: &SshBootstrapWindowsPaths,
    progress: SshBootstrapProgressContext<'_>,
    trace: &mut SshBootstrapChromeTrace,
) -> Result<String> {
    let local_bootstrap = trace
        .complete_async(
            "local_vx_bootstrap_build",
            serde_json::json!({
                "arch": target_info.arch,
            }),
            ensure_local_windows_vx_bootstrap_binary(repo_root, &target_info.arch),
        )
        .await?;
    let bootstrap_bytes = trace.complete(
        "read_local_vx_bootstrap_binary",
        serde_json::json!({
            "path": local_bootstrap.as_str(),
        }),
        || {
            fs::read(local_bootstrap.as_std_path()).wrap_err_with(|| {
                format!("failed to read local vx-bootstrap binary at {local_bootstrap}")
            })
        },
    )?;
    let compressed_bootstrap_bytes = trace.complete(
        "compress_local_vx_bootstrap_binary",
        serde_json::json!({
            "raw_bytes": bootstrap_bytes.len(),
        }),
        || -> Result<Vec<u8>> {
            let mut encoder = GzEncoder::new(Vec::new(), GzipCompression::default());
            encoder.write_all(&bootstrap_bytes)?;
            Ok(encoder.finish()?)
        },
    )?;
    let local_bootstrap_archive = trace.complete(
        "write_local_vx_bootstrap_archive",
        serde_json::json!({
            "compressed_bytes": compressed_bootstrap_bytes.len(),
        }),
        || write_local_bootstrap_archive("vx-bootstrap-windows", &compressed_bootstrap_bytes),
    )?;
    let remote_bootstrap_path = format!(r"{}\vx-bootstrap.exe", paths.vx_home);
    let remote_compressed_bootstrap_path = format!(r"{}\vx-bootstrap.exe.gz", paths.vx_home);
    let upload_result = trace
        .complete_async(
            "upload_vx_bootstrap_binary",
            serde_json::json!({
                "compressed_bytes": compressed_bootstrap_bytes.len(),
                "raw_bytes": bootstrap_bytes.len(),
            }),
            ssh_control_scp_file(
                control_path,
                &local_bootstrap_archive,
                &ssh_scp_windows_dest(target, &remote_compressed_bootstrap_path),
            ),
        )
        .await;
    let _ = fs::remove_file(local_bootstrap_archive.as_std_path());
    upload_result?;
    let remote_command = ssh_bootstrap_windows_loader_command(
        &remote_bootstrap_path,
        &remote_compressed_bootstrap_path,
    );

    let mut command = ssh_control_command(control_path, target);
    command
        .arg(remote_command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = trace.complete(
        "spawn_ssh_bootstrap_loader",
        serde_json::json!({
            "target": target,
        }),
        || command.spawn(),
    )?;
    let mut stdin = child
        .stdin()
        .take()
        .ok_or_else(|| eyre::eyre!("failed to capture ssh bootstrap stdin"))?;
    let mut stdout = child
        .stdout()
        .take()
        .ok_or_else(|| eyre::eyre!("failed to capture ssh bootstrap stdout"))?;
    let stderr_task = spawn_bootstrap_stderr_drain(&mut child)?;

    trace
        .complete_async("bootstrap_hello_roundtrip", serde_json::json!({}), async {
            write_bootstrap_request_frame(&mut stdin, &BootstrapRequest::Hello).await?;
            read_bootstrap_final_response_frame(
                &mut stdout,
                progress.boot_tx,
                progress.progress_listener,
                progress.class_label,
                progress.activity,
            )
            .await
        })
        .await?
        .ok_or_else(|| eyre::eyre!("vx-bootstrap closed before hello response"))
        .and_then(|response| match response {
            BootstrapResponse::Hello(_) => Ok(()),
            BootstrapResponse::Error(error) => eyre::bail!(
                "vx-bootstrap hello failed: {}: {}",
                error.kind,
                error.message
            ),
            other => eyre::bail!("unexpected vx-bootstrap hello response: {other:?}"),
        })?;
    let cached_manifest_entries = trace
        .complete_async(
            "remote_get_cached_source_manifest",
            serde_json::json!({}),
            async {
                write_bootstrap_request_frame(
                    &mut stdin,
                    &BootstrapRequest::GetCachedSourceManifest(GetCachedSourceManifestRequest {
                        checkout_dir: paths.checkout_dir.clone(),
                    }),
                )
                .await?;
                read_bootstrap_final_response_frame(
                    &mut stdout,
                    progress.boot_tx,
                    progress.progress_listener,
                    progress.class_label,
                    progress.activity,
                )
                .await
            },
        )
        .await?
        .ok_or_else(|| {
            eyre::eyre!("vx-bootstrap closed before get_cached_source_manifest response")
        })
        .and_then(|response| match response {
            BootstrapResponse::GetCachedSourceManifest(response) => Ok(response.files),
            BootstrapResponse::Error(error) => eyre::bail!(
                "vx-bootstrap get_cached_source_manifest failed: {}: {}",
                error.kind,
                error.message
            ),
            other => eyre::bail!(
                "unexpected vx-bootstrap get_cached_source_manifest response: {other:?}"
            ),
        })?;
    let cached_manifest = cached_manifest_entries
        .into_iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    let (local_source_files, manifest_stats) = trace.complete(
        "walk_local_source_manifest",
        serde_json::json!({
            "repo_root": repo_root.as_str(),
            "cached_entries": cached_manifest.len(),
        }),
        || local_bootstrap_source_files(repo_root, &cached_manifest),
    )?;
    trace.instant(
        "walk_local_source_manifest_summary",
        serde_json::json!({
            "reused_count": manifest_stats.reused_count,
            "hashed_count": manifest_stats.hashed_count,
        }),
    );
    let local_source_map = local_source_files
        .iter()
        .cloned()
        .map(|entry| (entry.path.clone(), entry))
        .collect::<BTreeMap<_, _>>();

    let request = PlanSourceSyncRequest {
        checkout_dir: paths.checkout_dir.clone(),
        files: local_source_files,
    };
    let plan_file_count = request.files.len();
    let plan_response = trace
        .complete_async(
            "remote_plan_source_sync",
            serde_json::json!({
                "file_count": plan_file_count,
            }),
            async {
                write_bootstrap_request_frame(
                    &mut stdin,
                    &BootstrapRequest::PlanSourceSync(request),
                )
                .await?;
                read_bootstrap_final_response_frame(
                    &mut stdout,
                    progress.boot_tx,
                    progress.progress_listener,
                    progress.class_label,
                    progress.activity,
                )
                .await
            },
        )
        .await?
        .ok_or_else(|| eyre::eyre!("vx-bootstrap closed before plan_source_sync response"))?;
    let missing_paths = match plan_response {
        BootstrapResponse::PlanSourceSync(response) => {
            for deleted in &response.deleted_paths {
                tracing::debug!(
                    target,
                    class_label = progress.class_label,
                    path = deleted,
                    "vx-bootstrap deleted stale source file"
                );
            }
            trace.instant(
                "remote_plan_source_sync_summary",
                serde_json::json!({
                    "missing_count": response.missing_paths.len(),
                    "deleted_count": response.deleted_paths.len(),
                }),
            );
            response.missing_paths
        }
        BootstrapResponse::Error(error) => eyre::bail!(
            "vx-bootstrap plan_source_sync failed: {}: {}",
            error.kind,
            error.message
        ),
        other => eyre::bail!("unexpected vx-bootstrap plan_source_sync response: {other:?}"),
    };

    let (uploaded_files, uploaded_bytes) = stream_source_archive_over_bootstrap(
        &mut stdin,
        &mut stdout,
        SourceArchiveStreamContext {
            repo_root,
            checkout_dir: &paths.checkout_dir,
            local_source_map: &local_source_map,
            missing_paths: &missing_paths,
            progress,
        },
        trace,
    )
    .await?;
    if uploaded_files > 0 {
        emit_ssh_bootstrap_progress(
            progress.boot_tx,
            progress.progress_listener,
            progress.class_label,
            format!(
                "vx-bootstrap upload progress: {uploaded_files} files ({uploaded_bytes} bytes)"
            ),
            progress.activity,
        )
        .await;
    }

    let request = BuildSourceTreeRequest {
        checkout_dir: paths.checkout_dir.clone(),
        build_manifest_relpath: "Cargo.toml".to_string(),
        build_packages: vec![
            "vx-store".to_string(),
            "vx-vfsd".to_string(),
            "vx-runner".to_string(),
            "landlock-exec".to_string(),
        ],
        scrub_files: vec![
            "crates/vx/Cargo.toml".to_string(),
            "crates/vxd/Cargo.toml".to_string(),
        ],
        scrub_line_contains: vec!["tree-sitter-vixen = ".to_string()],
    };
    let package_count = request.build_packages.len();
    let checkout_dir = trace
        .complete_async(
            "remote_build_source_tree",
            serde_json::json!({
                "package_count": package_count,
            }),
            async {
                write_bootstrap_request_frame(
                    &mut stdin,
                    &BootstrapRequest::BuildSourceTree(request),
                )
                .await?;
                read_bootstrap_final_response_frame(
                    &mut stdout,
                    progress.boot_tx,
                    progress.progress_listener,
                    progress.class_label,
                    progress.activity,
                )
                .await
            },
        )
        .await?
        .ok_or_else(|| eyre::eyre!("vx-bootstrap closed before build_source_tree response"))
        .and_then(|response| match response {
            BootstrapResponse::BuildSourceTree(response) => Ok(response.checkout_dir),
            BootstrapResponse::Error(error) => eyre::bail!(
                "vx-bootstrap build_source_tree failed: {}: {}",
                error.kind,
                error.message
            ),
            other => eyre::bail!("unexpected vx-bootstrap build_source_tree response: {other:?}"),
        })?;
    trace
        .complete_async(
            "bootstrap_shutdown_roundtrip",
            serde_json::json!({}),
            async {
                write_bootstrap_request_frame(&mut stdin, &BootstrapRequest::Shutdown).await?;
                read_bootstrap_final_response_frame(
                    &mut stdout,
                    progress.boot_tx,
                    progress.progress_listener,
                    progress.class_label,
                    progress.activity,
                )
                .await
            },
        )
        .await?;
    drop(stdin);
    let output = trace
        .complete_async(
            "wait_for_ssh_bootstrap_exit",
            serde_json::json!({}),
            finish_bootstrap_child(child, stderr_task),
        )
        .await?;
    if !output.status.success() {
        eyre::bail!(
            "vx-bootstrap ssh session failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(checkout_dir)
}

#[cfg(unix)]
fn ssh_forward_spec(local: &Endpoint, remote: &str) -> Result<String> {
    match local {
        Endpoint::Local(path) => Ok(format!("{}:{remote}", path.as_str())),
        Endpoint::Tcp { host, port } => Ok(format!("{host}:{port}:{remote}")),
    }
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
    activity: Option<&ActivityHandle>,
) -> Result<SshBootstrapConnection> {
    let class_slug = ssh_bootstrap_label_slug(class_label);
    let trace_path = vx_home.join(format!(
        "runner-ssh-bootstrap-{class_slug}.chrometracing.json"
    ));
    let mut trace =
        SshBootstrapChromeTrace::new(format!("ssh-bootstrap:{class_label}"), target.to_string());
    let result = async {
        emit_ssh_bootstrap_progress(
            boot_tx,
            &progress_listener,
            class_label,
            format!("checking local Vixen repo for ssh target {target}"),
            activity,
        )
        .await;
        let repo_root = resolve_vixen_repo_root()?;
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
            activity,
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
            activity,
        )
        .await;
        let target_info = detect_ssh_bootstrap_target(&control_path, target).await?;
        emit_ssh_bootstrap_progress(
            boot_tx,
            &progress_listener,
            class_label,
            format!("detected {} host", target_info.platform.as_tag()),
            activity,
        )
        .await;
        let mut windows_checkout = None;
        let (
            local_store_endpoint,
            local_runner_endpoint,
            store_remote,
            runner_remote,
            remote_command,
        ) = match target_info.platform {
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
                    "sh -lc {} sh {} {} {} {} {} {}",
                    shell_quote(SSH_BOOTSTRAP_REMOTE_LAUNCH_SCRIPT),
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
                    &target_info.home,
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
                    &target_info.home,
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
            format!("preparing local Vixen sources for {target}"),
            activity,
        )
        .await;
        emit_ssh_bootstrap_progress(
            boot_tx,
            &progress_listener,
            class_label,
            format!("syncing local Vixen sources to {target}"),
            activity,
        )
        .await;
        let bootstrap_progress = SshBootstrapProgressContext {
            boot_tx,
            progress_listener: &progress_listener,
            class_label,
            activity,
        };
        match (&target_info.platform, windows_checkout.as_ref()) {
            (SshBootstrapTargetPlatform::Unix, _) => {
                with_ssh_bootstrap_heartbeat(
                    boot_tx,
                    &progress_listener,
                    class_label,
                    &format!("syncing local Vixen sources to {target}"),
                    Duration::from_secs(5),
                    activity,
                    prepare_ssh_bootstrap_unix_checkout_via_bootstrap(
                        &control_path,
                        target,
                        &repo_root,
                        &target_info,
                        bootstrap_progress,
                        &mut trace,
                    ),
                )
                .await?;
            }
            (SshBootstrapTargetPlatform::Windows, Some(paths)) => {
                with_ssh_bootstrap_heartbeat(
                    boot_tx,
                    &progress_listener,
                    class_label,
                    &format!("syncing local Vixen sources to {target}"),
                    Duration::from_secs(5),
                    activity,
                    prepare_ssh_bootstrap_windows_checkout_via_bootstrap(
                        &control_path,
                        target,
                        &repo_root,
                        &target_info,
                        paths,
                        bootstrap_progress,
                        &mut trace,
                    ),
                )
                .await?;
            }
            (SshBootstrapTargetPlatform::Windows, None) => {
                eyre::bail!("missing Windows ssh bootstrap checkout paths for {target}");
            }
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
            activity,
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
    .await;
    if let Err(error) = trace.write_to(&trace_path) {
        tracing::warn!(
            path = %trace_path,
            error = %error,
            "failed to write ssh bootstrap chrome trace"
        );
    }
    result
}

#[cfg(unix)]
async fn bootstrap_ssh_executor(
    vx_home: &Utf8PathBuf,
    class_label: &str,
    target: &str,
    boot_tx: Option<&BootTx>,
    progress_listener: Option<Arc<ProgressListenerClient>>,
    activity: Option<&ActivityHandle>,
) -> Result<crate::service::SshBootstrapExecutor> {
    emit_ssh_bootstrap_progress(
        boot_tx,
        &progress_listener,
        class_label,
        "starting ssh bootstrap",
        activity,
    )
    .await;
    let connection = connect_ssh_bootstrap_runner(
        vx_home,
        class_label,
        target,
        boot_tx,
        progress_listener.clone(),
        activity,
    )
    .await?;
    let store_handle = Arc::new(connection.store_handle);
    let runner_handle = Arc::new(connection.runner_handle);
    emit_ssh_bootstrap_progress(
        boot_tx,
        &progress_listener,
        class_label,
        "waiting for remote vx-store boot",
        activity,
    )
    .await;
    let store_virtual = store_handle.boot_and_connect().await?;
    emit_ssh_bootstrap_progress(
        boot_tx,
        &progress_listener,
        class_label,
        "waiting for remote vx-runner boot",
        activity,
    )
    .await;
    let runner_virtual = runner_handle.boot_and_connect().await?;
    emit_ssh_bootstrap_progress(
        boot_tx,
        &progress_listener,
        class_label,
        "remote executor ready",
        activity,
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
    activity_registry: ActivityRegistry,
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
    let activity_registry = ActivityRegistry::new();
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
        let activity_registry = activity_registry.clone();
        build_vx_executors =
            build_vx_executors.with_ssh_bootstrap_factory(move |class_label, target, listener| {
                let vx_home = vx_home.clone();
                let activity_registry = activity_registry.clone();
                Box::pin(async move {
                    let activity = activity_registry.start(
                        "bootstrap",
                        format!("bootstrap {}", class_label),
                        Some(format!("class:{class_label}")),
                    );
                    bootstrap_ssh_executor(
                        &vx_home,
                        &class_label,
                        &target,
                        None,
                        listener,
                        Some(&activity),
                    )
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
        activity_registry,
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
                                    activity_registry: clients.activity_registry,
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
