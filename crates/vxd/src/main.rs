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
use std::sync::Arc;
use std::time::Duration;
use vx_io::boot::{BootDispatcher, BootGate};
use vx_io::net::{Endpoint, Listener};
use vx_runner_proto::RunnerClient;
use vx_store_proto::StoreClient;
use vxd_proto::VxdDispatcher;

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

    Ok(Args {
        vx_home,
        store_endpoint,
        exec_endpoint,
        bind,
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

    Ok(ServiceClients {
        store: Arc::new(StoreClient::new(store_virtual)),
        exec: Arc::new(RunnerClient::new(runner_virtual)),
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
                                spawn_tracker,
                                clients.store,
                                clients.exec,
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
                let (_client, _session_handle) = match acceptor(conduit)
                    .on_connection(VxdVirtualAcceptor {
                        boot_service: boot_service.clone(),
                    })
                    .establish::<()>(boot_dispatcher)
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

                let count =
                    active_connections.fetch_sub(1, std::sync::atomic::Ordering::Relaxed) - 1;
                tracing::debug!(
                    %peer_addr,
                    active_connections = count,
                    "connection established"
                );
                *last_disconnect.lock() = Some(tokio::time::Instant::now());
            }
            .named(conn_name),
        );
    }
}
