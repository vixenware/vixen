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

pub(crate) mod error;
pub(crate) mod sandbox;
pub(crate) mod service;
pub(crate) mod service_vfs;
pub(crate) mod vfsd_client;

use error::{Result as RunnerResult, RunnerError};

use camino::Utf8PathBuf;
use eyre::Result;
use facet::Facet;
use figue as args;
use roam_stream::{HandshakeConfig, NoDispatcher, accept};
use std::collections::HashMap;
use std::sync::Arc;
use vx_io::boot::{BootDispatcher, BootGate};
use vx_io::net::{Endpoint, Listener};
use vx_runner_proto::RunnerDispatcher;
use vx_store_proto::{Blake3Hash, StoreClient};

use crate::vfsd_client::VfsdClient as VfsClient;

pub type CasCaller = roam_stream::ConnectionHandle;
pub type VfsdCaller = roam_stream::ConnectionHandle;

pub(crate) fn runner_mount_base() -> Utf8PathBuf {
    // Option 1: VX_HOME-based path (works with FSKit, but has system policy restrictions on unsigned binaries)
    let vx_home = Utf8PathBuf::from(
        std::env::var("VX_HOME")
            .expect("VX_HOME not set — vx-runner must be launched by `vx` or with VX_HOME set"),
    );

    vx_home.join("vfs")
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

    // Initialize peeps instrumentation
    peeps::init("vx-runner");

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
        peeps::spawn_tracked("runner_boot", async move {
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
        });
    }

    let boot_service = gate.boot_service();

    loop {
        match runner_listener.accept().await {
            Ok((stream, peer_addr)) => {
                let boot_service = boot_service.clone();
                peeps::spawn_tracked(format!("runner_connection_{}", peer_addr), async move {
                    tracing::debug!("Runner connection from {}", peer_addr);

                    // Root connection gets Boot service only.
                    let boot_dispatcher = BootDispatcher::new(boot_service.clone());
                    let (_handle, mut incoming, driver) = match accept(
                        stream,
                        HandshakeConfig {
                            max_payload_size: 256 * 1024 * 1024, // 256 MiB
                            name: Some("vx-runner".into()),
                            ..Default::default()
                        },
                        boot_dispatcher,
                    )
                    .await
                    {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::debug!("Runner handshake error from {}: {e}", peer_addr);
                            return;
                        }
                    };

                    // Drive the connection in the background.
                    let driver_handle =
                        peeps::spawn_tracked(format!("runner_driver_{}", peer_addr), async move {
                            driver.run().await
                        });

                    // Accept virtual connections — each gets a RunnerDispatcher.
                    while let Some(conn) =
                        peeps::peep!(incoming.recv(), "runner.incoming.recv").await
                    {
                        let Some(exec) = boot_service.gate().service() else {
                            conn.reject("boot not complete".into(), vec![]);
                            continue;
                        };
                        let dispatcher = RunnerDispatcher::new(exec.clone());
                        if let Err(e) = conn.accept(vec![], Some(Box::new(dispatcher))).await {
                            tracing::warn!(
                                "Failed to accept virtual connection from {}: {e}",
                                peer_addr
                            );
                        }
                    }

                    if let Err(e) = driver_handle.await {
                        tracing::warn!("Driver error from {}: {e:?}", peer_addr);
                    }
                    tracing::debug!("Runner connection from {} closed", peer_addr);
                });
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
    let mount_path = runner_mount_base();

    let store_builder = ServiceBuilder::new("vx-store", store_endpoint.clone(), NoDispatcher)
        .caller_name("vx-runner")
        .connect_only();

    let vfsd_builder = ServiceBuilder::new("vx-vfsd", vfsd_endpoint.clone(), NoDispatcher)
        .caller_name("vx-runner")
        .args(&[
            "--mount",
            mount_path.as_str(),
            "--store",
            &store_endpoint.display(),
        ])
        .env("VX_HOME", vx_home.as_str());

    // Connect to store and spawn vfsd in parallel.
    let (store_handle, mut vfsd_handle) =
        tokio::try_join!(store_builder.connect(), vfsd_builder.connect(),)?;

    // Watch for vx-vfsd dying — if it does, we're useless.
    if let Some(mut vfsd_child) = vfsd_handle.child.take() {
        peeps::spawn_tracked("vfsd_death_watcher", async move {
            let status = vfsd_child.wait().await;
            tracing::error!(?status, "vx-vfsd exited unexpectedly");
            std::process::exit(1);
        });
    }

    // Run boot protocol on both services.
    let (store_virtual, vfsd_virtual) = tokio::try_join!(
        store_handle.boot_and_connect(None),
        vfsd_handle.boot_and_connect(None),
    )?;

    let store = Arc::new(StoreClient::new(store_virtual));
    let vfsd_protocol = vx_vfsd::protocol::VfsdClient::new(vfsd_virtual);
    let vfs = VfsClient::new(vfsd_protocol, store.clone());

    let exec = RunnerService::new(store.clone(), Arc::new(vfs));

    tracing::info!("VFS provided by vx-vfsd at {}", runner_mount_base());

    Ok(exec)
}

/// Inner Exec service implementation
pub struct RunnerServiceInner {
    /// CAS client for storing outputs and fetching toolchains
    pub(crate) cas: Arc<StoreClient<crate::CasCaller>>,

    /// VFS client for hermetic action execution (via vx-vfsd RPC)
    pub(crate) vfs: Arc<VfsClient>,

    /// Single-flight deduplication for toolchain prefix creation.
    /// Concurrent requests for the same toolchain coalesce into one operation.
    /// Stores Ok(prefix_id) or Err(message) — errors are cached too, since a missing
    /// toolchain manifest won't fix itself without a new runner session.
    #[allow(clippy::type_complexity)]
    toolchain_flights:
        peeps::Mutex<HashMap<Blake3Hash, Arc<peeps::OnceCell<Result<String, String>>>>>,

    /// Track created prefix IDs to avoid collisions
    created_prefixes: std::sync::Mutex<std::collections::HashSet<String>>,
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
    pub fn new(cas: Arc<StoreClient<crate::CasCaller>>, vfs: Arc<VfsClient>) -> Self {
        Self {
            inner: Arc::new(RunnerServiceInner {
                cas,
                vfs,
                toolchain_flights: peeps::Mutex::new("runner/toolchain_flights", HashMap::new()),
                created_prefixes: std::sync::Mutex::new(std::collections::HashSet::new()),
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

    /// Ensure a toolchain prefix exists in the VFS.
    ///
    /// Toolchain prefixes are stable — keyed by `toolchain_manifest_hash` — and
    /// persist for the lifetime of the runner. This means the macOS FS cache stays
    /// warm across compilations that use the same toolchain.
    ///
    /// Returns the prefix ID (e.g., "tc-ab3f...").
    ///
    /// Layout under the prefix:
    /// ```text
    /// tc-{short_hex}/
    ///   bin/rustc          (from rustc component)
    ///   sysroot/lib/...    (from rust-std component)
    ///   zig                (from zig-exe component)
    ///   lib/...            (from zig-lib component)
    /// ```
    pub(crate) async fn ensure_toolchain_prefix(
        &self,
        toolchain_manifest_hash: Blake3Hash,
    ) -> RunnerResult<String> {
        // Single-flight: concurrent callers for the same toolchain coalesce into one.
        let cell = {
            let mut flights = self.toolchain_flights.lock();
            flights
                .entry(toolchain_manifest_hash)
                .or_insert_with(|| Arc::new(peeps::OnceCell::new("toolchain_prefix_flight")))
                .clone()
        };
        let result = cell
            .get_or_init(|| async {
                self.populate_toolchain_prefix(toolchain_manifest_hash)
                    .await
                    .map_err(|e| e.to_string())
            })
            .await;
        match result {
            Ok(prefix_id) => Ok(prefix_id.clone()),
            Err(msg) => Err(RunnerError::CasFetch(msg.clone())),
        }
    }

    async fn populate_toolchain_prefix(
        &self,
        toolchain_manifest_hash: Blake3Hash,
    ) -> RunnerResult<String> {
        let prefix_id = format!("tc-{}", toolchain_manifest_hash.short_hex());

        tracing::debug!(
            prefix_id = %prefix_id,
            toolchain_manifest = %toolchain_manifest_hash,
            "creating new toolchain prefix"
        );

        // Get the toolchain manifest from CAS
        let toolchain_manifest = self
            .cas
            .get_toolchain_manifest(vx_store_proto::NodeHash(toolchain_manifest_hash))
            .await?
            .ok_or(RunnerError::ToolchainManifestNotFound(
                toolchain_manifest_hash,
            ))?;

        tracing::debug!(
            component_count = toolchain_manifest.components.len(),
            "loaded toolchain manifest"
        );

        // Create the prefix
        self.vfs.create_prefix(prefix_id.clone()).await;

        // Add each component's tree to the VFS prefix
        for component in &toolchain_manifest.components {
            tracing::debug!(
                component_name = %component.name,
                tree_root = %component.tree_root,
                "mounting component tree in VFS"
            );

            // Determine mount path prefix for this component
            // Files go directly under the prefix root — no "toolchain/" nesting.
            // The prefix itself IS the toolchain.
            let mount_path = if component.name == "rustc" {
                ""
            } else if component.name == "rust-std" {
                "sysroot"
            } else if matches!(component.name.as_str(), "zig-exe" | "zig-lib") {
                ""
            } else {
                &component.name
            };

            // Mount the entire component tree at once
            self.vfs
                .mount_tree(&prefix_id, mount_path, component.tree_root.0)
                .await
                .map_err(|e| {
                    RunnerError::VfsMount(format!(
                        "failed to mount toolchain component {}: {}",
                        component.name, e
                    ))
                })?;
        }

        tracing::info!(
            prefix_id = %prefix_id,
            toolchain_manifest = %toolchain_manifest_hash,
            "toolchain prefix created"
        );

        // Verify the toolchain is actually visible at the VFS mount point.
        // FSKit mounts can be flaky; catch the problem early with a clear error.
        let has_rustc = toolchain_manifest
            .components
            .iter()
            .any(|c| c.name == "rustc");
        if has_rustc {
            let rustc_vfs_path = runner_mount_base().join(&prefix_id).join("bin/rustc");
            if !rustc_vfs_path.as_std_path().exists() {
                let mount_base = runner_mount_base();
                let mount_exists = mount_base.as_std_path().exists();
                let prefix_exists = mount_base.join(&prefix_id).as_std_path().exists();
                let bin_exists = mount_base
                    .join(&prefix_id)
                    .join("bin")
                    .as_std_path()
                    .exists();
                tracing::error!(
                    %rustc_vfs_path,
                    %mount_base,
                    mount_exists,
                    prefix_exists,
                    bin_exists,
                    "toolchain mounted but bin/rustc not visible at VFS path"
                );
                return Err(RunnerError::VfsMount(format!(
                    "toolchain prefix {} created but bin/rustc not found at {} \
                     (mount_base exists={}, prefix exists={}, bin/ exists={})",
                    prefix_id, rustc_vfs_path, mount_exists, prefix_exists, bin_exists,
                )));
            }
        }

        Ok(prefix_id)
    }

    /// Add a source tree to an action prefix
    pub(crate) async fn add_source_tree_to_prefix(
        &self,
        action: &crate::service_vfs::ActionContext,
        source_manifest_hash: Blake3Hash,
    ) -> RunnerResult<()> {
        tracing::debug!(
            source_manifest = %source_manifest_hash,
            "mounting source tree in VFS"
        );

        // Mount the entire source tree at src/
        // source_manifest_hash is a NodeHash of a DirectoryNode
        self.vfs
            .mount_tree(
                &action.prefix_id,
                crate::service_vfs::ActionContext::SOURCES_DIR,
                source_manifest_hash,
            )
            .await
            .map_err(|e| RunnerError::VfsMount(format!("failed to mount source tree: {}", e)))?;

        Ok(())
    }

    /// Add a dependency artifact to an action prefix
    pub(crate) async fn add_dep_to_prefix(
        &self,
        action: &crate::service_vfs::ActionContext,
        extern_name: &str,
        manifest_hash: Blake3Hash,
    ) -> RunnerResult<()> {
        // Get the dependency manifest from CAS (stored as CasNode::Manifest)
        let node_data = self
            .cas
            .get_node(vx_store_proto::NodeHash(manifest_hash))
            .await?
            .ok_or(RunnerError::DependencyManifestNotFound(manifest_hash))?;

        let manifest = match vx_store_proto::CasNode::from_bytes(&node_data) {
            Ok(vx_store_proto::CasNode::Manifest(m)) => m,
            Ok(_) => {
                return Err(RunnerError::CasFetch(format!(
                    "node {} is not a Manifest",
                    manifest_hash
                )));
            }
            Err(e) => {
                return Err(RunnerError::CasFetch(format!(
                    "failed to deserialize manifest {}: {}",
                    manifest_hash, e
                )));
            }
        };

        // Find the rlib output
        let rlib = manifest
            .outputs
            .iter()
            .find(|o| o.logical == "rlib")
            .ok_or_else(|| {
                RunnerError::CasFetch(format!("no rlib in manifest for {}", extern_name))
            })?;

        // Extract blob hash and size from OutputBlobRef
        let (blob_hash, blob_size) = match &rlib.blob {
            vx_store_proto::OutputBlobRef::Small { chunk, size } => (chunk.0, *size),
            vx_store_proto::OutputBlobRef::Large {
                content_hash, size, ..
            } => (content_hash.0, *size),
        };

        let path = format!("deps/lib{}.rlib", extern_name);
        self.vfs
            .add_file_to_prefix(&action.prefix_id, &path, blob_hash, blob_size, false)
            .await
            .map_err(|e| {
                RunnerError::VfsMount(format!("failed to add dependency {}: {}", extern_name, e))
            })?;

        Ok(())
    }

    /// Create the output directory in a prefix
    pub(crate) async fn create_output_dir_in_prefix(&self, prefix_id: &str) {
        self.vfs.add_dir_to_prefix(prefix_id, "out").await;
    }

    /// Flush all written files in a prefix to CAS
    ///
    /// Returns a map of relative paths (e.g., "out/libfoo.rlib") to CAS hashes.
    pub(crate) async fn flush_prefix_outputs(
        &self,
        prefix_id: &str,
    ) -> HashMap<String, (Blake3Hash, u64)> {
        self.vfs.flush_prefix_to_cas(prefix_id).await
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
/// kills all known child process groups, preventing orphaned compiler
/// subprocesses from getting stuck on a dead FSKit mount.
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

    let mount_path = runner_mount_base();
    let log_file = mount_path
        .as_std_path()
        .parent()
        .unwrap_or(mount_path.as_std_path())
        .join("vx-reaper.log");

    match Command::new(&reaper_path)
        .arg("--parent-pid")
        .arg(my_pid.to_string())
        .arg("--mount-path")
        .arg(mount_path.as_str())
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
