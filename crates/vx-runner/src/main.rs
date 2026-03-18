//! vx-runner - Worker daemon
//!
//! Implements the Exec Roam service trait.
//! A remote-capable compilation service - all inputs/outputs go through CAS.
//!
//! Responsibilities:
//! - Populate VFS prefixes with toolchains/sources/deps from CAS metadata
//! - Run rustc/zig
//! - Ingest outputs to CAS
//! - Return output manifest hashes

pub(crate) mod eval_boundary;
pub(crate) mod sandbox;
pub(crate) mod service;
pub(crate) mod service_vfs;
pub(crate) mod vfsd_client;

use camino::Utf8PathBuf;
use eyre::Result;
use facet::Facet;
use figue as args;
use moire::task::FutureExt;
use roam_core::acceptor;
use roam_core::{AcceptedConnection, BareConduit, ConnectionAcceptor, Driver};
use roam_stream::StreamLink;
use roam_types::{ConnectionSettings, Metadata};
use std::collections::HashMap;
use std::sync::Arc;
use vx_io::boot::{BootDispatcher, BootGate};
use vx_io::net::{Endpoint, Listener};
use vx_io::spawn::ServiceHandle;
use vx_runner_proto::RunnerDispatcher;
use vx_runner_proto::TransportValue;
use vx_store_proto::StoreClient;

use crate::vfsd_client::VfsdClient as VfsClient;

pub type CasCaller = roam_core::DriverCaller;
pub type VfsdCaller = roam_core::DriverCaller;

#[derive(Clone)]
struct RunnerVirtualAcceptor {
    boot_service: vx_io::boot::BootService<RunnerService>,
}

impl ConnectionAcceptor for RunnerVirtualAcceptor {
    fn accept(
        &self,
        _conn_id: roam_types::ConnectionId,
        peer_settings: &ConnectionSettings,
        _metadata: &[roam_types::MetadataEntry],
    ) -> std::result::Result<AcceptedConnection, Metadata<'static>> {
        let Some(exec) = self.boot_service.gate().service().cloned() else {
            return Err(vec![]);
        };

        let parity = peer_settings.parity.other();
        Ok(AcceptedConnection {
            settings: ConnectionSettings {
                parity,
                max_concurrent_requests: peer_settings.max_concurrent_requests,
            },
            metadata: vec![],
            setup: Box::new(move |handle| {
                let dispatcher = RunnerDispatcher::new(exec);
                moire::task::spawn(
                    async move {
                        let mut driver = Driver::new(handle, dispatcher);
                        driver.run().await;
                    }
                    .named("runner_virtual_driver"),
                );
            }),
        })
    }
}

pub(crate) fn runner_mount_base() -> Utf8PathBuf {
    if cfg!(target_os = "macos") {
        // Global long-lived VixenFS mount path. vx-vfsd owns mount lifecycle.
        Utf8PathBuf::from("/tmp/vx/vfs")
    } else if let Ok(vx_home) = std::env::var("VX_HOME") {
        Utf8PathBuf::from(vx_home).join("vfs")
    } else {
        Utf8PathBuf::from("/tmp/vx/vfs")
    }
}

/// vx-runner - Worker daemon
#[derive(Facet, Debug)]
struct Args {
    /// VX home directory
    #[facet(args::named)]
    vx_home: Utf8PathBuf,

    /// CAS (vx-store) endpoint
    #[facet(args::named)]
    store: String,

    /// Listen endpoint for Runner RPC service
    #[facet(args::named)]
    listen: String,

    /// vx-vfsd endpoint
    #[facet(args::named)]
    vfsd: String,

    /// Standard CLI options (--help, --version, --completions)
    #[facet(flatten)]
    builtins: args::FigueBuiltins,
}

fn main() -> Result<()> {
    // On Windows, detect MSVC toolchain and inject env vars (LIB, INCLUDE, etc.)
    // so that rustc can find link.exe even when not launched from a Developer Command Prompt.
    // Must happen before tokio runtime starts (set_var requires single-threaded context).
    #[cfg(windows)]
    detect_msvc_environment();

    // Create a new process group for vx-runner and all its children.
    // The reaper watches our PID and kills this entire group if we die.
    #[cfg(target_os = "macos")]
    spawn_reaper();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // If spawned by parent, die when parent dies
    ur_taking_me_with_you::die_with_parent();

    // Initialize tracing
    vx_tracing::init_service("vx_runner=info");

    tracing::info!("hello from vixen-runner");

    let args: Args = args::Driver::new(
        args::builder::<Args>()?
            .cli(|cli| cli.args(std::env::args().skip(1)))
            .help(|h| {
                h.program_name("vx-runner")
                    .version(env!("CARGO_PKG_VERSION"))
            })
            .build(),
    )
    .run()
    .unwrap();

    let bind_endpoint = Endpoint::parse(&args.listen)?;

    // Bind listener BEFORE booting — so parent can connect immediately.
    let mut runner_listener = Listener::bind(&bind_endpoint).await?;
    tracing::info!("Runner RPC bound on {}", bind_endpoint);

    // Set up boot gate — root connections get Boot service only.
    let (gate, boot_tx) = BootGate::<RunnerService>::new();

    // Boot in background: connect to store, spawn vfsd, create runner service.
    {
        let gate = Arc::clone(&gate);
        let store_addr = args.store.clone();
        let vfsd_addr = args.vfsd.clone();
        let vx_home = args.vx_home.clone();
        moire::task::spawn(
            async move {
                tracing::info!("Booting vx-runner");
                match boot_runner(&store_addr, &vfsd_addr, &vx_home).await {
                    Ok(service) => {
                        tracing::info!("vx-runner boot succeeded");
                        gate.set_service(service);
                        boot_tx.ready();
                    }
                    Err(e) => {
                        tracing::error!("vx-runner boot failed: {e:#}");
                        boot_tx.failed(format!("{e:#}"));
                    }
                }
            }
            .named("runner_boot"),
        );
    }

    let boot_service = gate.boot_service();

    loop {
        match runner_listener.accept().await {
            Ok((stream, peer_addr)) => {
                let boot_service = boot_service.clone();
                moire::task::spawn(
                    async move {
                        tracing::debug!("Runner connection from {}", peer_addr);

                        // Root connection gets Boot service only.
                        let (reader, writer) = tokio::io::split(stream);
                        let conduit = BareConduit::<roam_types::MessageFamily, _>::new(
                            StreamLink::new(reader, writer),
                        );
                        let boot_dispatcher = BootDispatcher::new(boot_service.clone());
                        let (caller, session_handle) = match acceptor(conduit)
                            .on_connection(RunnerVirtualAcceptor {
                                boot_service: boot_service.clone(),
                            })
                            .establish::<roam_core::NoopCaller>(boot_dispatcher)
                            .await
                        {
                            Ok(parts) => parts,
                            Err(e) => {
                                tracing::debug!("Runner handshake error from {}: {e}", peer_addr);
                                return;
                            }
                        };

                        tracing::debug!("Runner connection from {} established", peer_addr);
                        vx_io::net::park_session(caller, session_handle).await;
                    }
                    .named("runner_connection"),
                );
            }
            Err(e) => {
                tracing::debug!("Runner accept error: {}", e);
            }
        }
    }
}

/// Full boot sequence: connect to store, spawn vfsd, create runner service.
async fn boot_runner(
    store_addr: &str,
    vfsd_addr: &str,
    vx_home: &Utf8PathBuf,
) -> Result<RunnerService> {
    use vx_io::spawn::ServiceBuilder;

    let store_endpoint = Endpoint::parse(store_addr)?;
    let vfsd_endpoint = Endpoint::parse(vfsd_addr)?;

    let store_builder = ServiceBuilder::new("vx-store", store_endpoint.clone())
        .caller_name("vx-runner")
        .connect_only();

    let vfsd_builder = ServiceBuilder::new("vx-vfsd", vfsd_endpoint.clone())
        .caller_name("vx-runner")
        .args(&[
            "--store",
            &store_endpoint.display(),
            "--mount",
            runner_mount_base().as_str(),
        ])
        .log_path(vx_home.join("vfsd.log"))
        .env("VX_HOME", vx_home.as_str());

    // Connect to store and spawn vfsd in parallel.
    let (store_handle, mut vfsd_handle) =
        tokio::try_join!(store_builder.connect(), vfsd_builder.connect(),)?;

    // Run boot protocol on both services.
    let (store_virtual, vfsd_virtual) = tokio::try_join!(
        store_handle.boot_and_connect(),
        vfsd_handle.boot_and_connect(),
    )?;

    // Watch for vx-vfsd dying after it has booted successfully — if it does, we're useless.
    if let Some(mut vfsd_child) = vfsd_handle.child.take() {
        moire::task::spawn(
            async move {
                let status = vfsd_child.wait().await;
                tracing::error!(?status, "vx-vfsd exited unexpectedly");
                std::process::exit(1);
            }
            .named("vfsd_death_watcher"),
        );
    }

    let store = Arc::new(StoreClient::new(store_virtual));
    let vfsd_protocol = vx_vfsd::protocol::VfsdClient::new(vfsd_virtual);
    let vfs = VfsClient::new(vfsd_protocol, Arc::clone(&store));
    let vfs_access_path = Utf8PathBuf::from(
        vfs.access_path()
            .await
            .map_err(|e| eyre::eyre!("failed to get VFS access path from vx-vfsd: {e}"))?,
    );

    let exec = RunnerService::new(
        store,
        Arc::new(vfs),
        vfs_access_path.clone(),
        store_handle,
        vfsd_handle,
    );

    tracing::info!("VFS provided by vx-vfsd at {}", vfs_access_path);

    Ok(exec)
}

/// Inner Exec service implementation
pub struct RunnerServiceInner {
    /// CAS client for storing outputs and fetching toolchains
    pub(crate) cas: Arc<StoreClient>,

    /// VFS client for hermetic action execution (via vx-vfsd RPC)
    pub(crate) vfs: Arc<VfsClient>,

    /// Real filesystem access path for the mounted VFS session.
    pub(crate) vfs_access_path: Utf8PathBuf,

    /// Retain the upstream root handles for the lifetime of the runner service.
    _store_handle: ServiceHandle,
    _vfsd_handle: ServiceHandle,

    /// Track created prefix IDs to avoid collisions
    created_prefixes: std::sync::Mutex<std::collections::HashSet<String>>,

    /// Cache committed discovery results while this daemon stays alive.
    discover_cache: std::sync::Mutex<HashMap<String, TransportValue>>,
}

/// Exec service handle - cloneable wrapper around shared inner state
#[derive(Clone)]
pub struct RunnerService {
    inner: Arc<RunnerServiceInner>,
}

impl vx_io::boot::GracefulShutdown for RunnerService {
    async fn graceful_shutdown(&self) {}
}

impl std::ops::Deref for RunnerService {
    type Target = RunnerServiceInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl RunnerService {
    pub fn new(
        cas: Arc<StoreClient>,
        vfs: Arc<VfsClient>,
        vfs_access_path: Utf8PathBuf,
        store_handle: ServiceHandle,
        vfsd_handle: ServiceHandle,
    ) -> Self {
        Self {
            inner: Arc::new(RunnerServiceInner {
                cas,
                vfs,
                vfs_access_path,
                _store_handle: store_handle,
                _vfsd_handle: vfsd_handle,
                created_prefixes: std::sync::Mutex::new(std::collections::HashSet::new()),
                discover_cache: std::sync::Mutex::new(HashMap::new()),
            }),
        }
    }

    // ========================================================================
    // VFS Prefix Management (for hermetic action execution)
    // ========================================================================

    /// Create a new action prefix in the VFS
    ///
    /// Returns the prefix ID (e.g., "build-01JFX...") which can be used
    /// to add files and later clean up.
    pub(crate) async fn create_action_prefix(&self) -> String {
        use jiff::Timestamp;
        let ts = Timestamp::now().as_millisecond();

        for attempt in 0..3 {
            let prefix_id = format!("build-{:x}-{:08x}", ts, fastrand::u32(..));

            // Check local HashSet for collision
            {
                let prefixes = self.created_prefixes.lock().unwrap();
                if prefixes.contains(&prefix_id) {
                    tracing::warn!(prefix_id = %prefix_id, attempt, "prefix ID collision, retrying");
                    continue;
                }
            }

            // Create the prefix via RPC
            let (id, _item_id) = self.vfs.create_prefix(prefix_id.clone()).await;

            // Track it locally
            self.created_prefixes
                .lock()
                .unwrap()
                .insert(prefix_id.clone());

            return id;
        }

        panic!("Failed to generate unique prefix ID after 3 attempts");
    }

    /// Clean up an action prefix
    pub(crate) async fn remove_action_prefix(&self, prefix_id: &str) {
        self.vfs.remove_prefix(prefix_id).await;
    }
}

/// Global handle to the reaper's stdin pipe.
/// `run_sandboxed_command` writes child pgids here after spawning.
#[cfg(target_os = "macos")]
static REAPER_STDIN: std::sync::OnceLock<std::sync::Mutex<std::process::ChildStdin>> =
    std::sync::OnceLock::new();

/// Notify the reaper about a new child process group.
#[cfg(target_os = "macos")]
pub(crate) fn register_child_pgroup(pgid: u32) {
    if let Some(stdin) = REAPER_STDIN.get() {
        use std::io::Write;
        if let Ok(mut guard) = stdin.lock() {
            let _ = writeln!(guard, "{pgid}");
            let _ = guard.flush();
        }
    }
}

/// Spawn the vx-reaper sidecar process.
///
/// Spawns vx-reaper which watches our PID via kqueue. vx-runner writes
/// child process group IDs to the reaper's stdin as it spawns them.
/// If vx-runner dies for any reason (including SIGKILL), the reaper
/// kills all known child process groups to avoid orphaned compilers.
#[cfg(target_os = "macos")]
fn spawn_reaper() {
    use std::process::Command;

    let my_pid = std::process::id();

    // Find vx-reaper next to our own binary
    let reaper_path = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("vx-reaper")))
        .filter(|path| path.exists());

    let Some(reaper_path) = reaper_path else {
        // Not a hard error — just means orphan protection is disabled
        return;
    };

    let mount_base = runner_mount_base();
    let log_file = mount_base
        .as_std_path()
        .parent()
        .unwrap_or(mount_base.as_std_path())
        .join("vx-reaper.log");

    match Command::new(&reaper_path)
        .arg("--parent-pid")
        .arg(my_pid.to_string())
        .arg("--log-file")
        .arg(&log_file)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            if let Some(stdin) = child.stdin.take() {
                let _ = REAPER_STDIN.set(std::sync::Mutex::new(stdin));
            }
            // Don't wait on it — fire-and-forget sidecar.
        }
        Err(_) => {
            // Not a hard error — just means orphan protection is disabled
        }
    }
}

/// Detect MSVC toolchain and set LIB/INCLUDE/LIBPATH/PATH in the process environment.
///
/// When vx-runner is launched from VS Code or a plain terminal (not a Developer Command Prompt),
/// the MSVC environment variables aren't set, so rustc can't find link.exe or system libraries.
/// We use `find-msvc-tools` (same detection logic as the `cc` crate) to locate the MSVC
/// installation and inject the required env vars.
#[cfg(windows)]
fn detect_msvc_environment() {
    // If LIB is already set, assume we're in a Developer Command Prompt.
    if std::env::var_os("LIB").is_some() {
        return;
    }

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86") {
        "i686"
    } else {
        eprintln!("vx-runner: unknown host architecture, skipping MSVC detection");
        return;
    };

    match find_msvc_tools::find_tool(arch, "link.exe") {
        Some(tool) => {
            for (key, value) in tool.env() {
                // SAFETY: called from main() before any threads are spawned.
                unsafe { std::env::set_var(key, value) };
            }
            eprintln!(
                "vx-runner: detected MSVC toolchain at {}",
                tool.path().display()
            );
        }
        None => {
            eprintln!(
                "vx-runner: could not detect MSVC toolchain — link.exe may not be found by rustc"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::runner_mount_base;

    #[test]
    fn runner_mount_base_is_stable_global_path() {
        if cfg!(target_os = "macos") {
            assert_eq!(runner_mount_base().as_str(), "/tmp/vx/vfs");
        }
    }
}
