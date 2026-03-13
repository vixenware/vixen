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
use camino::Utf8PathBuf;
use eyre::Result;
use facet::Facet;
use figue as args;
use moire::task::FutureExt;
use roam_core::acceptor;
use roam_core::{AcceptedConnection, BareConduit, ConnectionAcceptor, Driver};
use roam_stream::StreamLink;
use roam_types::{ConnectionSettings, Metadata};
use std::fs::OpenOptions;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vx_io::boot::{BootDispatcher, BootGate};
use vx_io::net::{Endpoint, Listener};
use vx_io::spawn::ServiceHandle;
use vx_runner_proto::RunnerClient;
use vx_store_proto::StoreClient;
use vxd_proto::VxdDispatcher;

use crate::service::VxdServiceDeps;
pub use db::Database;
pub use inputs::*;
pub use queries::*;
pub use service::VxdService;

pub type UpstreamCaller = roam_core::DriverCaller;

const RECOMMENDED_NOFILE_SOFT_LIMIT: u64 = 65_536;

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

#[derive(Debug)]
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
    checkout_dir: String,
    vx_home: String,
    store_root: String,
    store_socket: String,
    vfsd_socket: String,
    runner_socket: String,
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
fn ssh_bootstrap_paths(remote_home: &str, class_label: &str) -> SshBootstrapPaths {
    let base = format!("{remote_home}/.cache/vixen/ssh-bootstrap/{class_label}");
    let vx_home = format!("{base}/vx-home");
    SshBootstrapPaths {
        checkout_dir: format!("{base}/repo"),
        store_root: format!("{vx_home}/store"),
        store_socket: format!("{vx_home}/store.sock"),
        vfsd_socket: format!("{vx_home}/vfsd.sock"),
        runner_socket: format!("{vx_home}/runner.sock"),
        vx_home,
    }
}

#[cfg(unix)]
fn ssh_bootstrap_local_endpoint(vx_home: &Utf8PathBuf, class_label: &str) -> Endpoint {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    Endpoint::Local(vx_home.join(format!(
        "ssh-bootstrap-{class_label}-{}-{unique}.sock",
        std::process::id()
    )))
}

#[cfg(unix)]
const SSH_BOOTSTRAP_REMOTE_SCRIPT: &str = r#"set -euo pipefail
repo_url="$1"
commit="$2"
checkout_dir="$3"
vx_home="$4"
store_root="$5"
store_socket="$6"
vfsd_socket="$7"
runner_socket="$8"

mkdir -p "$(dirname "$checkout_dir")" "$vx_home" "$store_root"
if [ ! -d "$checkout_dir/.git" ]; then
  rm -rf "$checkout_dir"
  git clone "$repo_url" "$checkout_dir"
fi
git -C "$checkout_dir" fetch --all --tags --prune
git -C "$checkout_dir" checkout --detach "$commit"
cargo build --manifest-path "$checkout_dir/Cargo.toml" -p vx-store -p vx-vfsd -p vx-runner
rm -f "$store_socket" "$vfsd_socket" "$runner_socket"
trap 'jobs -p | xargs -r kill || true' EXIT
"$checkout_dir/target/debug/vx-store" --root "$store_root" --listen "unix:$store_socket" &
exec "$checkout_dir/target/debug/vx-runner" \
  --vx-home "$vx_home" \
  --store "unix:$store_socket" \
  --vfsd "unix:$vfsd_socket" \
  --listen "unix:$runner_socket"
"#;

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
) -> Result<ServiceHandle> {
    let repo_url = command_output("git", &["config", "--get", "remote.origin.url"]).await?;
    let commit = command_output("git", &["rev-parse", "HEAD"]).await?;
    let remote_home = command_output("ssh", &[target, "sh", "-lc", "printf %s \"$HOME\""]).await?;
    if remote_home.is_empty() {
        eyre::bail!("ssh bootstrap target {target} returned an empty HOME");
    }

    let local_endpoint = ssh_bootstrap_local_endpoint(vx_home, class_label);
    let local_socket = match &local_endpoint {
        Endpoint::Local(path) => path.clone(),
        Endpoint::Tcp { .. } => unreachable!("ssh bootstrap uses local sockets on unix"),
    };
    if let Some(parent) = local_socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if local_socket.exists() {
        let _ = std::fs::remove_file(local_socket.as_std_path());
    }

    let paths = ssh_bootstrap_paths(&remote_home, class_label);
    let log_path = vx_home.join(format!("runner-ssh-bootstrap-{class_label}.log"));
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let stderr_file = log_file.try_clone()?;

    let mut command = moire::process::Command::new("ssh");
    command
        .arg("-o")
        .arg("ExitOnForwardFailure=yes")
        .arg("-o")
        .arg("ServerAliveInterval=30")
        .arg("-o")
        .arg("ServerAliveCountMax=3")
        .arg("-L")
        .arg(format!(
            "{}:{}",
            local_socket.as_str(),
            paths.runner_socket.as_str()
        ))
        .arg(target)
        .arg("sh")
        .arg("-lc")
        .arg(SSH_BOOTSTRAP_REMOTE_SCRIPT)
        .arg("ssh-bootstrap")
        .arg(repo_url.as_str())
        .arg(commit.as_str())
        .arg(paths.checkout_dir.as_str())
        .arg(paths.vx_home.as_str())
        .arg(paths.store_root.as_str())
        .arg(paths.store_socket.as_str())
        .arg(paths.vfsd_socket.as_str())
        .arg(paths.runner_socket.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(stderr_file))
        .kill_on_drop(true);
    let child = command.spawn()?;

    let (caller, session_handle) = connect_endpoint_with_backoff(
        &format!("vx-runner[{class_label}]"),
        &local_endpoint,
        Duration::from_secs(60),
    )
    .await?;

    Ok(ServiceHandle {
        child: Some(child),
        caller,
        session_handle,
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
    exec: Arc<RunnerClient>,
    build_vx_executors: crate::service::BuildVxExecutors,
    store_handle: Arc<ServiceHandle>,
    runner_handles: Arc<[Arc<ServiceHandle>]>,
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

/// Ensure services are running (spawning them if necessary), run the boot
/// protocol on each, and return connected roam clients.
async fn ensure_services(
    args: &Args,
    spawn_tracker: &Arc<SyncMutex<SpawnTracker>>,
) -> Result<ServiceClients> {
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
    let store_root = args.vx_home.join("store");

    let mut store_builder = ServiceBuilder::new("vx-store", store_endpoint.clone())
        .caller_name("vxd")
        .args(&["--root", store_root.as_str()])
        .log_path(args.vx_home.join("store.log"));
    if args.store_endpoint.is_some() {
        store_builder = store_builder.connect_only();
    }

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

    // Spawn/connect to both services in parallel.
    let (mut store_handle, mut runner_handle) =
        tokio::try_join!(store_builder.connect(), runner_builder.connect(),)?;

    {
        let mut tracker = spawn_tracker.lock();
        tracker.store = store_handle.child.take();
        tracker.runner = runner_handle.child.take();
    }

    // Run boot protocol on both services in parallel.
    let (store_virtual, runner_virtual) = tokio::try_join!(
        store_handle.boot_and_connect(),
        runner_handle.boot_and_connect(),
    )?;

    let exec = Arc::new(RunnerClient::new(runner_virtual));
    let mut build_vx_executors = crate::service::BuildVxExecutors::local_default(Arc::clone(&exec));
    let mut runner_handles = vec![Arc::new(runner_handle)];

    for config in &args.build_vx_executor_classes {
        match config.backend {
            vx_eval::ExecutorBackendKind::Local => {
                build_vx_executors
                    .register_class_alias(config.class_label.clone(), "local")
                    .map_err(eyre::Report::msg)?;
            }
            vx_eval::ExecutorBackendKind::Daemon => {
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
                build_vx_executors.register_class_runner(
                    config.class_label.clone(),
                    format!("daemon:{}", config.class_label),
                    vx_eval::ExecutorBackendKind::Daemon,
                    runner,
                );
                runner_handles.push(Arc::new(handle));
            }
            vx_eval::ExecutorBackendKind::SshBootstrap => {
                let target = config
                    .target
                    .as_deref()
                    .expect("ssh bootstrap executor target should be present");
                #[cfg(unix)]
                {
                    let handle =
                        connect_ssh_bootstrap_runner(&args.vx_home, &config.class_label, target)
                            .await?;
                    let virtual_handle = handle.boot_and_connect().await?;
                    let runner = Arc::new(RunnerClient::new(virtual_handle));
                    build_vx_executors.register_class_runner(
                        config.class_label.clone(),
                        format!("ssh_bootstrap:{}", config.class_label),
                        vx_eval::ExecutorBackendKind::SshBootstrap,
                        runner,
                    );
                    runner_handles.push(Arc::new(handle));
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
        store: Arc::new(StoreClient::new(store_virtual)),
        exec,
        build_vx_executors,
        store_handle: Arc::new(store_handle),
        runner_handles: Arc::from(runner_handles),
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
                tracing::info!("Booting services (vx-store, vx-runner)");
                match ensure_services(&args_for_boot, &spawn_tracker).await {
                    Ok(clients) => {
                        tracing::info!("Services booted successfully");
                        let daemon = Arc::new(
                            VxdService::new(
                                vx_home,
                                exec_host_triple,
                                VxdServiceDeps {
                                    spawn_tracker,
                                    store: clients.store,
                                    exec: clients.exec,
                                    build_vx_executors: clients.build_vx_executors,
                                    store_handle: clients.store_handle,
                                    runner_handles: clients.runner_handles,
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
