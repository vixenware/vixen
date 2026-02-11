//! vxd - Build orchestration daemon
//!
//! The daemon orchestrates builds but never touches the filesystem directly.
//! All file I/O and compilation is delegated to vx-runner.
//!
//! Responsibilities:
//! - Own the picante incremental computation database
//! - Compute cache keys
//! - Check CAS for cache hits
//! - Send compile requests to execd
//! - Materialize final outputs locally (via execd or direct CAS fetch)

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
use eyre::{Result, WrapErr};
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

    /// CAS endpoint
    cas_endpoint: Endpoint,

    /// Exec endpoint
    exec_endpoint: Endpoint,

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
            Utf8PathBuf::from(format!("{}/.vx", home))
        });

    // CAS endpoint: --store > VX_STORE > default local socket
    let cas_endpoint = match cli.store.or_else(|| std::env::var("VX_STORE").ok()) {
        Some(v) => Endpoint::parse(&v)?,
        None => vx_io::net::default_unix_endpoint(&vx_home, "store"),
    };

    // Runner endpoint: --runner > VX_RUNNER > default local socket
    let exec_endpoint = match cli.runner.or_else(|| std::env::var("VX_RUNNER").ok()) {
        Some(v) => Endpoint::parse(&v)?,
        None => vx_io::net::default_unix_endpoint(&vx_home, "runner"),
    };

    // Bind endpoint: --listen > VXD > default local socket
    let bind = match cli.listen.or_else(|| std::env::var("VXD").ok()) {
        Some(v) => Endpoint::parse(&v)?,
        None => vx_io::net::default_unix_endpoint(&vx_home, "vxd"),
    };

    Ok(Args {
        vx_home,
        cas_endpoint,
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

/// Spawn a service binary and return the child process
///
/// When `VX_INHERIT_STDIO` is set, stdout/stderr are inherited from the parent
/// (so daemon logs appear inline in test output). Otherwise they go to `log_path`.
fn spawn_service(
    binary_name: &str,
    log_path: &camino::Utf8Path,
    cli_args: &[&str],
) -> Result<Child> {
    use std::process::Stdio;

    let vx_test = std::env::var("VX_TEST").is_ok();

    // Prefer spawning sibling binaries next to the currently running vxd.
    // This keeps test runs using the same target/debug artifacts instead of any
    // globally installed binaries found on PATH.
    let binary_path = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(binary_name)))
        .filter(|candidate| candidate.exists())
        .unwrap_or_else(|| std::path::PathBuf::from(binary_name));

    tracing::debug!(%vx_test, ?cli_args, "spawning {} from {}", binary_name, binary_path.display());

    let mut cmd = std::process::Command::new(&binary_path);
    cmd.args(cli_args);

    if vx_test {
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());
    } else {
        use std::fs::OpenOptions;

        // Ensure parent directory exists for log file
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;

        cmd.stdout(Stdio::from(log_file.try_clone()?));
        cmd.stderr(Stdio::from(log_file));
    }

    ur_taking_me_with_you::spawn_dying_with_parent(cmd).map_err(Into::into)
}

/// Ensure services are running, spawning them if necessary
async fn ensure_services(
    args: &Args,
    spawn_tracker: &Arc<DiagnosticAsyncMutex<SpawnTracker>>,
) -> Result<()> {
    // Service startup can take a few seconds on cold systems (especially runner).
    // Keep retrying long enough to avoid flaking during daemon orchestration.
    let backoff_ms = [10, 50, 100, 250, 500, 1000, 1500, 2000, 3000];

    let vx_home_str = args.vx_home.as_str();
    let cas_display = args.cas_endpoint.display();
    let store_root = args.vx_home.join("store");

    // Check Store (CAS)
    if vx_io::net::try_connect(&args.cas_endpoint).await?.is_none() {
        if !args.cas_endpoint.is_local() {
            eyre::bail!(
                "Store is not reachable at {}. Auto-spawn is only supported for local endpoints.\n\
                Start vx-store on the remote host, or point VX_STORE to a local endpoint.",
                args.cas_endpoint
            );
        }

        let store_log = args.vx_home.join("store.log");
        let child = spawn_service(
            "vx-store",
            &store_log,
            &[
                "--listen",
                &cas_display,
                "--root",
                store_root.as_str(),
                "--vx-home",
                vx_home_str,
            ],
        )?;

        spawn_tracker.lock().await.store = Some(child);

        for (attempt, delay) in backoff_ms.iter().enumerate() {
            tokio::time::sleep(tokio::time::Duration::from_millis(*delay)).await;
            if vx_io::net::try_connect(&args.cas_endpoint).await?.is_some() {
                tracing::trace!("Connected to Store after {} attempts", attempt + 1);
                break;
            }
        }

        // Final connection attempt
        vx_io::net::connect(&args.cas_endpoint)
            .await
            .wrap_err_with(|| {
                format!("Store still unreachable after spawn: {}", args.cas_endpoint)
            })?;
    } else {
        tracing::info!("Store already running at {}", args.cas_endpoint);
    }

    let exec_display = args.exec_endpoint.display();
    let vfsd_endpoint = vx_io::net::default_unix_endpoint(&args.vx_home, "vfsd");
    let vfsd_display = vfsd_endpoint.display();

    // Check Runner (worker)
    if vx_io::net::try_connect(&args.exec_endpoint)
        .await?
        .is_none()
    {
        if !args.exec_endpoint.is_local() {
            eyre::bail!(
                "Runner is not reachable at {}. Auto-spawn is only supported for local endpoints.\n\
                Start vx-runner on the remote host, or point VX_RUNNER to a local endpoint.",
                args.exec_endpoint
            );
        }

        let runner_log = args.vx_home.join("runner.log");
        let child = spawn_service(
            "vx-runner",
            &runner_log,
            &[
                "--listen",
                &exec_display,
                "--store",
                &cas_display,
                "--vfsd",
                &vfsd_display,
                "--vx-home",
                vx_home_str,
            ],
        )?;

        spawn_tracker.lock().await.runner = Some(child);

        for (attempt, delay) in backoff_ms.iter().enumerate() {
            tokio::time::sleep(tokio::time::Duration::from_millis(*delay)).await;
            if vx_io::net::try_connect(&args.exec_endpoint)
                .await?
                .is_some()
            {
                tracing::trace!("Connected to Runner after {} attempts", attempt + 1);
                break;
            }
        }

        // Final connection attempt
        vx_io::net::connect(&args.exec_endpoint)
            .await
            .wrap_err_with(|| {
                format!(
                    "Runner still unreachable after spawn: {}",
                    args.exec_endpoint
                )
            })?;
    } else {
        tracing::info!("Runner already running at {}", args.exec_endpoint);
    }

    Ok(())
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
    tracing::info!("  CAS:     {}", args.cas_endpoint);
    tracing::info!("  Exec:    {}", args.exec_endpoint);
    tracing::info!("  Bind:    {}", args.bind);

    let spawn_tracker = Arc::new(DiagnosticAsyncMutex::new(
        "vxd/spawn_tracker",
        SpawnTracker::default(),
    ));

    ensure_services(&args, &spawn_tracker).await?;

    let exec_host_triple = match std::env::var("VX_RUNNER_HOST_TRIPLE") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => {
            if !args.exec_endpoint.is_local() {
                eyre::bail!(
                    "VX_RUNNER points to a non-local endpoint ({}) but VX_RUNNER_HOST_TRIPLE is not set.\n\
                    vxd must know the runner host triple to request the correct Rust toolchain from CAS.\n\
                    Set VX_RUNNER_HOST_TRIPLE (e.g. x86_64-unknown-linux-gnu) and retry.",
                    args.exec_endpoint
                );
            }

            vx_toolchain::detect_host_triple()
                .map_err(|e| eyre::eyre!("failed to detect host triple: {}", e))?
        }
    };

    // Create reconnecting Roam clients for CAS and Exec.
    tracing::info!("Connecting to CAS at {}", args.cas_endpoint);
    let cas_caller = vx_io::net::reconnecting_client_named(
        args.cas_endpoint.clone(),
        NoDispatcher,
        Some("vxd".into()),
    );
    cas_caller
        .handle()
        .await
        .map_err(|e| eyre::eyre!("failed to connect to CAS at {}: {}", args.cas_endpoint, e))?;
    let cas_client = Arc::new(StoreClient::new(cas_caller));

    tracing::info!("Connecting to Exec at {}", args.exec_endpoint);
    let exec_caller = vx_io::net::reconnecting_client_named(
        args.exec_endpoint.clone(),
        NoDispatcher,
        Some("vxd".into()),
    );
    exec_caller
        .handle()
        .await
        .map_err(|e| eyre::eyre!("failed to connect to Exec at {}: {}", args.exec_endpoint, e))?;
    let exec_client = Arc::new(RunnerClient::new(exec_caller));

    // Initialize daemon service (singleton shared across all connections)
    tracing::info!("Initializing daemon service");
    let daemon = Arc::new(
        VxdService::new(
            cas_client,
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
            let Ok((handle, _incoming, driver)) = accept(
                stream,
                HandshakeConfig {
                    max_payload_size: 256 * 1024 * 1024, // 256 MiB
                    name: Some("vxd".into()),
                    ..Default::default()
                },
                dispatcher,
            )
            .await
            else {
                tracing::warn!("Handshake error from {}", peer_addr);
                active_connections.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                *last_disconnect.lock().await = Some(tokio::time::Instant::now());
                return;
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
