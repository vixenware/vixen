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
pub(crate) mod service;
pub(crate) mod service_vfs;
pub(crate) mod vfsd_client;

use error::{Result as RunnerResult, RunnerError};

use camino::Utf8PathBuf;
use eyre::Result;
use roam_stream::{HandshakeConfig, NoDispatcher, accept};
use std::collections::HashMap;
use std::sync::Arc;
use vx_io::net::{Endpoint, Listener};
use vx_runner_proto::RunnerDispatcher;
use vx_store_proto::{Blake3Hash, StoreClient};

use crate::vfsd_client::VfsdClient as VfsClient;

pub type CasCaller = roam_stream::Client<vx_io::net::EndpointConnector, NoDispatcher>;
pub type VfsdCaller = roam_stream::Client<vx_io::net::EndpointConnector, NoDispatcher>;

pub(crate) fn runner_mount_base() -> Utf8PathBuf {
    if let Ok(path) = std::env::var("VX_RUNNER_MOUNT") {
        return Utf8PathBuf::from(path);
    }
    if let Ok(vx_home) = std::env::var("VX_HOME") {
        return Utf8PathBuf::from(vx_home).join("vixen").join("runner");
    }
    if let Ok(home) = std::env::var("HOME") {
        return Utf8PathBuf::from(home)
            .join(".vx")
            .join("vixen")
            .join("runner");
    }
    Utf8PathBuf::from("/tmp/vx/vixen/runner")
}

#[derive(Debug)]
struct Args {
    /// VX_HOME directory
    vx_home: Utf8PathBuf,

    /// CAS endpoint
    cas_endpoint: Endpoint,

    /// Bind endpoint for Runner RPC service
    bind: Endpoint,

    /// vx-vfsd endpoint
    vfsd_endpoint: Endpoint,
}

impl Args {
    fn from_env() -> Result<Self> {
        let vx_home = std::env::var("VX_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").expect("HOME not set");
            format!("{}/.vx", home)
        });
        let vx_home = Utf8PathBuf::from(vx_home);

        // Parse CAS endpoint (defaults to Unix socket)
        let cas_endpoint = match std::env::var("VX_STORE") {
            Ok(v) => Endpoint::parse(&v)?,
            Err(_) => {
                #[cfg(unix)]
                {
                    vx_io::net::default_unix_endpoint(&vx_home, "store")
                }
                #[cfg(not(unix))]
                {
                    Endpoint::parse("127.0.0.1:9002")?
                }
            }
        };

        // Parse bind endpoint (defaults to Unix socket)
        let bind = match std::env::var("VX_RUNNER") {
            Ok(v) => Endpoint::parse(&v)?,
            Err(_) => {
                #[cfg(unix)]
                {
                    vx_io::net::default_unix_endpoint(&vx_home, "runner")
                }
                #[cfg(not(unix))]
                {
                    Endpoint::parse("127.0.0.1:9003")?
                }
            }
        };

        // Parse vfsd endpoint (defaults to Unix socket)
        let vfsd_endpoint = match std::env::var("VX_VFSD") {
            Ok(v) => Endpoint::parse(&v)?,
            Err(_) => {
                #[cfg(unix)]
                {
                    vx_io::net::default_unix_endpoint(&vx_home, "vfsd")
                }
                #[cfg(not(unix))]
                {
                    Endpoint::parse("127.0.0.1:9004")?
                }
            }
        };

        Ok(Args {
            vx_home,
            cas_endpoint,
            bind,
            vfsd_endpoint,
        })
    }
}

/// Connect to CAS and return a client handle
async fn connect_to_cas(endpoint: &Endpoint) -> Result<vx_store_proto::StoreClient<CasCaller>> {
    let caller = vx_io::net::reconnecting_client(endpoint.clone(), NoDispatcher);
    caller
        .handle()
        .await
        .map_err(|e| eyre::eyre!("failed to connect to CAS at {}: {}", endpoint, e))?;
    Ok(vx_store_proto::StoreClient::new(caller))
}

/// Check for stuck FUSE mount and clean up if needed
async fn cleanup_stuck_mount(mount_path: &Utf8PathBuf) -> Result<()> {
    // Check if mount exists in /proc/mounts
    let mounts = match std::fs::read_to_string("/proc/mounts") {
        Ok(content) => content,
        Err(_) => return Ok(()), // Not Linux or can't read mounts, skip cleanup
    };

    if !mounts.contains(mount_path.as_str()) {
        // Not mounted, nothing to clean up
        return Ok(());
    }

    // Try to stat the mount point to see if it's stuck
    match std::fs::metadata(mount_path.as_std_path()) {
        Ok(_) => {
            // stat succeeded, mount is healthy
            return Ok(());
        }
        Err(e) if e.raw_os_error() == Some(107) => {
            // ENOTCONN - Transport endpoint is not connected
            // This is a stuck mount, proceed with cleanup
            tracing::warn!("Detected stuck FUSE mount at {}, attempting cleanup", mount_path);
        }
        Err(e) => {
            // Some other error, let it fail naturally
            return Err(e.into());
        }
    }

    // Verify this is our mount (matches pattern */vixen/runner)
    if !mount_path.as_str().contains("/vixen/runner") {
        return Err(eyre::eyre!(
            "Refusing to clean up mount at {} - doesn't match vixen pattern",
            mount_path
        ));
    }

    // Level 1: Try gentle unmount
    tracing::info!("Attempting gentle unmount: fusermount3 -u {}", mount_path);
    let status = std::process::Command::new("fusermount3")
        .arg("-u")
        .arg(mount_path.as_str())
        .status()?;

    if status.success() {
        tracing::info!("Successfully unmounted stuck FUSE mount");
        return Ok(());
    }

    // Level 2: Try lazy unmount
    tracing::warn!("Gentle unmount failed, trying lazy unmount: fusermount3 -uz {}", mount_path);
    let status = std::process::Command::new("fusermount3")
        .arg("-uz")
        .arg(mount_path.as_str())
        .status()?;

    if status.success() {
        tracing::info!("Successfully unmounted with lazy unmount");
        return Ok(());
    }

    // Level 3: Nuclear option - abort FUSE connections then unmount
    tracing::error!("Lazy unmount failed, attempting nuclear cleanup");

    // Abort all FUSE connections
    if let Ok(entries) = std::fs::read_dir("/sys/fs/fuse/connections") {
        for entry in entries.flatten() {
            let abort_path = entry.path().join("abort");
            if abort_path.exists() {
                let _ = std::fs::write(&abort_path, b"1");
            }
        }
    }

    // Try unmount again after abort
    let status = std::process::Command::new("fusermount3")
        .arg("-uz")
        .arg(mount_path.as_str())
        .status()?;

    if status.success() {
        tracing::warn!("Nuclear cleanup succeeded");
        Ok(())
    } else {
        Err(eyre::eyre!(
            "Failed to clean up stuck FUSE mount at {} after all attempts",
            mount_path
        ))
    }
}

/// Spawn vx-vfsd if not already running, return any startup errors
async fn ensure_vfsd(
    vfsd_endpoint: &Endpoint,
    vx_home: &Utf8PathBuf,
    cas_endpoint: &Endpoint,
) -> Result<()> {
    use std::process::Stdio;

    // Check if already running
    if vx_io::net::try_connect(vfsd_endpoint).await?.is_some() {
        tracing::debug!("vx-vfsd already running at {}", vfsd_endpoint);
        return Ok(());
    }

    tracing::info!("Spawning vx-vfsd");

    // Find vx-vfsd binary (sibling to vx-runner)
    let binary_path = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("vx-vfsd")))
        .filter(|candidate| candidate.exists())
        .unwrap_or_else(|| std::path::PathBuf::from("vx-vfsd"));

    let mount_path = runner_mount_base();

    // Check for stuck FUSE mount and clean up if needed
    cleanup_stuck_mount(&mount_path).await?;

    // Ensure mount directory exists
    std::fs::create_dir_all(&mount_path)?;

    let mut cmd = std::process::Command::new(&binary_path);
    cmd.arg("--listen").arg(vfsd_endpoint.display());
    cmd.arg("--mount").arg(mount_path.as_str());
    cmd.arg("--store").arg(cas_endpoint.display());
    cmd.env("VX_HOME", vx_home.as_str());

    // Capture stderr for error messages
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());

    let mut child = ur_taking_me_with_you::spawn_dying_with_parent(cmd)?;

    // Capture stderr in background for error reporting
    let stderr = child.stderr.take().expect("stderr was piped");
    let stderr_buffer = Arc::new(std::sync::Mutex::new(Vec::new()));
    let stderr_buffer_clone = stderr_buffer.clone();

    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut stderr = tokio::io::BufReader::new(tokio::process::ChildStderr::from_std(stderr).unwrap());
        let mut buf = [0u8; 4096];
        loop {
            match stderr.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    stderr_buffer_clone.lock().unwrap().extend_from_slice(&buf[..n]);
                }
                Err(_) => break,
            }
        }
    });

    // Wait for vx-vfsd to be ready with exponential backoff
    let backoff_ms = [10, 50, 100, 250, 500, 1000, 2000];
    for (attempt, delay) in backoff_ms.iter().enumerate() {
        tokio::time::sleep(tokio::time::Duration::from_millis(*delay)).await;

        // Check if process died
        if let Ok(Some(status)) = child.try_wait() {
            let stderr_data = stderr_buffer.lock().unwrap().clone();
            let stderr_output = String::from_utf8_lossy(&stderr_data);
            return Err(eyre::eyre!(
                "vx-vfsd exited with status {} during startup.\n\nStderr:\n{}",
                status,
                stderr_output
            ));
        }

        // Try to connect
        if vx_io::net::try_connect(vfsd_endpoint).await?.is_some() {
            tracing::info!("vx-vfsd started successfully (attempt {})", attempt + 1);
            return Ok(());
        }
    }

    // Final check - if still not connectable, report error
    let stderr_data = stderr_buffer.lock().unwrap().clone();
    let stderr_output = String::from_utf8_lossy(&stderr_data);
    Err(eyre::eyre!(
        "vx-vfsd failed to start after {} attempts.\n\nStderr:\n{}",
        backoff_ms.len(),
        stderr_output
    ))
}

/// Connect to vx-vfsd and return a client handle
async fn connect_to_vfsd(
    endpoint: &Endpoint,
    cas: Arc<StoreClient<CasCaller>>,
) -> Result<VfsClient> {
    let caller: VfsdCaller = vx_io::net::reconnecting_client(endpoint.clone(), NoDispatcher);
    caller
        .handle()
        .await
        .map_err(|e| eyre::eyre!("failed to connect to vx-vfsd at {}: {}", endpoint, e))?;
    let protocol_client = vx_vfsd::protocol::VfsdClient::new(caller);
    Ok(VfsClient::new(protocol_client, cas))
}

/// Check if a socket endpoint is already in use by trying to connect
#[cfg(unix)]
async fn is_socket_in_use(path: &Utf8PathBuf) -> bool {
    use tokio::net::UnixStream;
    // Try to connect - if successful, someone is listening
    UnixStream::connect(path.as_std_path()).await.is_ok()
}

#[tokio::main]
async fn main() -> Result<()> {
    // If spawned by parent, die when parent dies
    ur_taking_me_with_you::die_with_parent();

    // Initialize tracing
    vx_tracing::init_service("vx_runner=info");

    tracing::info!("hello from vixen-runner");

    let args = Args::from_env()?;

    // Check if another runner instance is already running
    #[cfg(unix)]
    {
        if let Endpoint::Local(ref path) = args.bind
            && path.exists()
            && is_socket_in_use(path).await
        {
            return Err(eyre::eyre!(
                "Another vx-runner instance is already running (socket {} is in use).\n\
                 Kill the existing instance or use a different VX_RUNNER endpoint.",
                path
            ));
        }
    }

    // Connect to CAS
    tracing::info!("Connecting to CAS at {}", args.cas_endpoint);
    let cas = Arc::new(connect_to_cas(&args.cas_endpoint).await?);
    tracing::info!("Connected to CAS");

    // Ensure vx-vfsd is running
    ensure_vfsd(&args.vfsd_endpoint, &args.vx_home, &args.cas_endpoint).await?;

    // Connect to vx-vfsd
    tracing::info!("Connecting to vx-vfsd at {}", args.vfsd_endpoint);
    let vfs = connect_to_vfsd(&args.vfsd_endpoint, cas.clone()).await?;
    tracing::info!("Connected to vx-vfsd");

    // Create Runner service (will use VFS via RPC)
    let exec = RunnerService::new(cas.clone(), Arc::new(vfs));

    // Bind Runner RPC socket before VFS mount attempt.
    let runner_listener = match Listener::bind(&args.bind).await {
        Ok(l) => l,
        Err(e) => {
            return Err(eyre::eyre!(
                "Failed to bind Runner RPC socket at {}: {}\n\
                 Another instance may be running, or the socket file is stale.",
                args.bind,
                e
            ));
        }
    };
    tracing::info!("Runner RPC bound on {}", args.bind);

    // VFS is now mounted by vx-vfsd, not by vx-runner
    tracing::info!("VFS provided by vx-vfsd at {}", runner_mount_base());

    // Start accepting Runner RPC connections
    tracing::info!("Starting Runner RPC server");
    loop {
        match runner_listener.accept().await {
            Ok((stream, peer_addr)) => {
                let exec = exec.clone();
                tokio::spawn(async move {
                    tracing::debug!("Runner connection from {}", peer_addr);
                    let dispatcher = RunnerDispatcher::new(exec);
                    let Ok((_handle, _incoming, driver)) = accept(
                        stream,
                        HandshakeConfig {
                            max_payload_size: 256 * 1024 * 1024, // 256 MiB
                            ..Default::default()
                        },
                        dispatcher,
                    )
                    .await
                    else {
                        tracing::debug!("Runner handshake error from {}", peer_addr);
                        return;
                    };
                    if let Err(e) = driver.run().await {
                        tracing::debug!("Runner connection error from {}: {}", peer_addr, e);
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
        tokio::sync::Mutex<HashMap<Blake3Hash, Arc<tokio::sync::OnceCell<Result<String, String>>>>>,

    /// Track created prefix IDs to avoid collisions
    created_prefixes: std::sync::Mutex<std::collections::HashSet<String>>,
}

/// Exec service handle - cloneable wrapper around shared inner state
#[derive(Clone)]
pub struct RunnerService {
    inner: Arc<RunnerServiceInner>,
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
                toolchain_flights: tokio::sync::Mutex::new(HashMap::new()),
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
            self.created_prefixes.lock().unwrap().insert(prefix_id.clone());

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
            let mut flights = self.toolchain_flights.lock().await;
            flights
                .entry(toolchain_manifest_hash)
                .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new()))
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
            .get_toolchain_manifest(toolchain_manifest_hash)
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
                tree_manifest = %component.tree_manifest,
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
                .mount_tree(&prefix_id, mount_path, component.tree_manifest)
                .await
                .map_err(|e| RunnerError::VfsMount(format!(
                    "failed to mount toolchain component {}: {}",
                    component.name, e
                )))?;
        }

        tracing::info!(
            prefix_id = %prefix_id,
            toolchain_manifest = %toolchain_manifest_hash,
            "toolchain prefix created"
        );

        Ok(prefix_id)
    }

    /// Add a source tree to an action prefix
    ///
    /// Populates `<prefix>/src/` with the source tree's files from CAS.
    pub(crate) async fn add_source_tree_to_prefix(
        &self,
        prefix_id: &str,
        source_manifest_hash: Blake3Hash,
    ) -> RunnerResult<()> {
        tracing::debug!(
            source_manifest = %source_manifest_hash,
            "loading source tree manifest"
        );

        // Get the source tree manifest from CAS (source trees are stored via ingest_tree)
        let manifest = self
            .cas
            .get_tree_manifest(source_manifest_hash)
            .await
            .map_err(|e| {
                tracing::error!(
                    source_manifest = %source_manifest_hash,
                    error = ?e,
                    "failed to fetch source tree manifest"
                );
                RunnerError::CasFetch(format!("{:?}", e))
            })?
            .ok_or_else(|| {
                tracing::error!(
                    source_manifest = %source_manifest_hash,
                    "source tree manifest missing"
                );
                RunnerError::CasFetch(format!(
                    "source manifest {} not found",
                    source_manifest_hash
                ))
            })?;

        tracing::debug!(
            source_manifest = %source_manifest_hash,
            entry_count = manifest.entries.len(),
            "mounting source tree in VFS"
        );

        // Mount the entire source tree at src/
        self.vfs
            .mount_tree(prefix_id, "src", source_manifest_hash)
            .await
            .map_err(|e| RunnerError::VfsMount(format!("failed to mount source tree: {}", e)))?;

        Ok(())
    }

    /// Add a dependency artifact to an action prefix
    ///
    /// Places the dependency's rlib at `<prefix>/deps/<extern_name>.rlib`
    pub(crate) async fn add_dep_to_prefix(
        &self,
        prefix_id: &str,
        extern_name: &str,
        manifest_hash: Blake3Hash,
    ) -> RunnerResult<()> {
        // Get the dependency manifest
        let manifest = self
            .cas
            .get_manifest(manifest_hash)
            .await?
            .ok_or(RunnerError::DependencyManifestNotFound(manifest_hash))?;

        // Find the rlib output
        let rlib = manifest
            .outputs
            .iter()
            .find(|o| o.logical == "rlib")
            .ok_or_else(|| {
                RunnerError::CasFetch(format!("no rlib in manifest for {}", extern_name))
            })?;

        let path = format!("deps/lib{}.rlib", extern_name);
        self.vfs
            .add_file_to_prefix(prefix_id, &path, rlib.blob, rlib.size, false)
            .await
            .map_err(|e| RunnerError::VfsMount(format!("failed to add dependency {}: {}", extern_name, e)))?;

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

    /// Get the VFS mount path for an action prefix
    ///
    /// This is the path that should be passed to sandboxed processes.
    /// Format: <mount-base>/<prefix_id>/ where mount-base defaults to ~/.vx/vixen/runner.
    #[allow(dead_code)] // Will be used for sandbox integration
    pub(crate) fn get_prefix_mount_path(&self, prefix_id: &str) -> Utf8PathBuf {
        runner_mount_base().join(prefix_id)
    }
}

