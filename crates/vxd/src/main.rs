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
mod nextest_watcher;
mod queries;
mod service;

#[cfg(test)]
mod tests;

use camino::Utf8PathBuf;
use eyre::Result;
use facet::Facet;
use figue as args;
use peeps::locks::DiagnosticAsyncMutex;
use roam_stream::{HandshakeConfig, NoDispatcher, accept};
use std::process::Child;
use std::sync::Arc;
use std::time::Duration;
use vx_io::net::{Endpoint, Listener};
use vx_runner_proto::RunnerClient;
use vx_store_proto::StoreClient;
use vxd_proto::VxdDispatcher;

pub use db::Database;
pub use inputs::*;
pub use queries::*;
pub use service::VxdService;

pub type UpstreamCaller = roam_stream::Client<vx_io::net::EndpointConnector, NoDispatcher>;

/// vxd - Build orchestration daemon
#[derive(Facet, Debug)]
struct CliArgs {
    /// Listen endpoint (default: $VX_HOME/vxd.sock)
    #[facet(default, args::named)]
    listen: Option<String>,

    /// VX home directory (default: ~/.vx)
    #[facet(default, args::named)]
    vx_home: Option<String>,

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

    // VX_STORE set → connect to existing store. Unset → spawn one.
    let store_endpoint = std::env::var("VX_STORE")
        .ok()
        .map(|v| Endpoint::parse(&v))
        .transpose()?;

    // VX_RUNNER set → connect to existing runner. Unset → spawn one.
    let exec_endpoint = std::env::var("VX_RUNNER")
        .ok()
        .map(|v| Endpoint::parse(&v))
        .transpose()?;

    // --listen only (no VXD env — that's for vx connect mode)
    let bind = match cli.listen {
        Some(v) => Endpoint::parse(&v)?,
        None => vx_io::net::default_unix_endpoint(&vx_home, "vxd"),
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
        if let Some(mut child) = self.store.take() {
            tracing::info!("Killing spawned vx-store (pid: {})", child.id());
            if let Err(e) = child.kill() {
                tracing::warn!("Failed to kill vx-store (pid: {}): {e}", child.id());
            }
        }
        if let Some(mut child) = self.runner.take() {
            tracing::info!("Killing spawned vx-runner (pid: {})", child.id());
            if let Err(e) = child.kill() {
                tracing::warn!("Failed to kill vx-runner (pid: {}): {e}", child.id());
            }
        }
    }
}

struct ServiceClients {
    store: Arc<StoreClient<UpstreamCaller>>,
    exec: Arc<RunnerClient<UpstreamCaller>>,
}

/// Ensure services are running (spawning them if necessary) and return
/// connected roam clients. Store and runner are started in parallel.
async fn ensure_services(
    args: &Args,
    spawn_tracker: &Arc<DiagnosticAsyncMutex<SpawnTracker>>,
) -> Result<ServiceClients> {
    use vx_io::spawn::ServiceBuilder;

    let store_endpoint = args
        .store_endpoint
        .clone()
        .unwrap_or_else(|| vx_io::net::default_unix_endpoint(&args.vx_home, "store"));
    let exec_endpoint = args
        .exec_endpoint
        .clone()
        .unwrap_or_else(|| vx_io::net::default_unix_endpoint(&args.vx_home, "runner"));
    let vfsd_endpoint = vx_io::net::default_unix_endpoint(&args.vx_home, "vfsd");
    let store_root = args.vx_home.join("store");

    let mut store_builder = ServiceBuilder::new("vx-store", store_endpoint.clone(), NoDispatcher)
        .caller_name("vxd")
        .args(&[
            "--root",
            store_root.as_str(),
            "--vx-home",
            args.vx_home.as_str(),
        ])
        .log_path(args.vx_home.join("store.log"));
    if args.store_endpoint.is_some() {
        store_builder = store_builder.connect_only();
    }

    let mut runner_builder = ServiceBuilder::new("vx-runner", exec_endpoint.clone(), NoDispatcher)
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

    let (store, runner) = tokio::try_join!(store_builder.connect(), runner_builder.connect(),)?;

    {
        let mut tracker = spawn_tracker.lock().await;
        tracker.store = store.child;
        tracker.runner = runner.child;
    }

    Ok(ServiceClients {
        store: Arc::new(StoreClient::new(store.caller)),
        exec: Arc::new(RunnerClient::new(runner.caller)),
    })
}

fn main() -> Result<()> {
    peeps::init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .on_thread_start(|| {
            peeps::threads::register_thread(
                std::thread::current().name().unwrap_or("tokio-worker"),
            );
        })
        .on_thread_stop(|| {
            peeps::threads::deregister_thread();
        })
        .build()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // Initialize tracing (logs will go to ~/.vx/vxd.log via spawn_service redirection)
    vx_tracing::init_service("vxd=debug");

    let in_test = std::env::var("VX_TEST").is_ok();

    // The daemon outlives its parent process (vx CLI) and shuts down
    // after an idle timeout instead (5s in test mode, 2h in production).
    // In test mode, we also watch the test harness ancestor (cargo-nextest,
    // vx-fuse-stress, vx-vfs-integrity, etc.) and exit immediately when it dies.
    if in_test {
        if let Some((harness_pid, harness_name)) = nextest_watcher::find_test_harness_pid() {
            tracing::info!(harness_pid, %harness_name, "watching test harness process");
            peeps::tasks::spawn_tracked("nextest_watcher", async move {
                nextest_watcher::wait_for_exit(harness_pid).await;
                // Use libc::_exit — tracing/stdout/stderr may be broken pipes
                // after ctrl-c kills the pipeline, and writing to them blocks.
                unsafe { libc::_exit(0) };
            });
        } else {
            tracing::debug!("no test harness ancestor found (external daemon?)");
        }
    }

    // SIGUSR1: dump diagnostics
    peeps::install_sigusr1("vxd");

    let cli: CliArgs = args::Driver::new(
        args::builder::<CliArgs>()?
            .cli(|cli| cli.args(std::env::args().skip(1)))
            .help(|h| h.program_name("vxd").version(env!("CARGO_PKG_VERSION")))
            .build(),
    )
    .run()
    .unwrap();
    let args = resolve_args(cli)?;
    tracing::info!("Starting vxd");
    tracing::info!("  VX_HOME: {}", args.vx_home);
    match &args.store_endpoint {
        Some(ep) => tracing::info!("  Store:   {ep} (from VX_STORE)"),
        None => tracing::info!("  Store:   (will spawn)"),
    }
    match &args.exec_endpoint {
        Some(ep) => tracing::info!("  Exec:    {ep} (from VX_RUNNER)"),
        None => tracing::info!("  Exec:    (will spawn)"),
    }
    tracing::info!("  Bind:    {}", args.bind);

    let spawn_tracker = Arc::new(DiagnosticAsyncMutex::new(
        "vxd/spawn_tracker",
        SpawnTracker::default(),
    ));

    let clients = ensure_services(&args, &spawn_tracker).await?;

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

    let store_client = clients.store;
    let exec_client = clients.exec;

    // Initialize daemon service (singleton shared across all connections)
    tracing::info!("Initializing daemon service");
    let daemon = Arc::new(
        VxdService::new(
            store_client,
            exec_client,
            args.vx_home,
            exec_host_triple,
            spawn_tracker,
        )
        .await,
    );

    // Start server (TCP or Unix socket)
    let listener = Listener::bind(&args.bind).await?;
    tracing::info!("Daemon listening on {}", args.bind);

    // Track active connections and shut down after idle timeout.
    // Test mode: 5s, production: 2 hours.
    let active_connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let last_disconnect = Arc::new(DiagnosticAsyncMutex::new(
        "vxd/last_disconnect",
        Option::<tokio::time::Instant>::None,
    ));

    {
        let idle_timeout = if in_test {
            Duration::from_secs(5)
        } else {
            Duration::from_secs(2 * 60 * 60)
        };
        let active_connections = active_connections.clone();
        let last_disconnect = last_disconnect.clone();
        peeps::tasks::spawn_tracked("idle_shutdown_monitor", async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let count = active_connections.load(std::sync::atomic::Ordering::Relaxed);
                if count == 0
                    && let Some(when) = *last_disconnect.lock().await
                    && when.elapsed() >= idle_timeout
                {
                    tracing::info!("no connections for {idle_timeout:?}, shutting down");
                    std::process::exit(0);
                }
            }
        });
    }

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        let daemon = daemon.clone();
        let active_connections = active_connections.clone();
        let last_disconnect = last_disconnect.clone();

        active_connections.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        peeps::tasks::spawn_tracked(format!("vxd_connection_{}", peer_addr), async move {
            tracing::debug!("New connection from {}", peer_addr);

            // Create a per-connection session with fresh state
            let session = daemon.new_session();

            let dispatcher = VxdDispatcher::new(session.clone());
            let (handle, _incoming, driver) = match accept(
                stream,
                HandshakeConfig {
                    max_payload_size: 256 * 1024 * 1024, // 256 MiB
                    name: Some("vxd".into()),
                    ..Default::default()
                },
                dispatcher,
            )
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Handshake error from {}: {e}", peer_addr);
                    active_connections.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    *last_disconnect.lock().await = Some(tokio::time::Instant::now());
                    return;
                }
            };

            let progress_listener =
                Arc::new(vxd_proto::ProgressListenerClient::new(handle.clone()));

            // Set progress listener for this connection
            session.set_progress_listener(Some(progress_listener)).await;

            if let Err(e) = driver.run().await {
                tracing::warn!("Session error from {}: {}", peer_addr, e);
            }

            // Connection dropped — cancel any in-flight build for this session.
            // This is the primary graceful shutdown mechanism: when the CLI exits
            // (e.g. Ctrl-C), its connection closes, which cancels the build.
            session.cancel_active_build().await;

            tracing::debug!("Connection from {} closed", peer_addr);
            active_connections.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            *last_disconnect.lock().await = Some(tokio::time::Instant::now());
        });
    }
}
