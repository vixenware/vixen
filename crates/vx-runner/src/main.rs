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
pub(crate) mod vfs;

use error::{Result as RunnerResult, RunnerError};

use camino::Utf8PathBuf;
use eyre::Result;
use roam_stream::{HandshakeConfig, NoDispatcher, accept};
use std::collections::HashMap;
use std::sync::Arc;
use vixenfs_proto::{
    CreateResult, GetAttributesResult, ItemId, ItemType, LookupResult, ReadDirResult, ReadResult,
    SetAttributesParams, Vfs, VfsResult, WriteResult,
};
use vx_io::net::{Endpoint, Listener};
use vx_runner_proto::RunnerDispatcher;
use vx_store_proto::{Blake3Hash, StoreClient};
use vx_vfs::{VfsAction, VfsConfig, VfsDiagnostic, VfsIssueCode, VfsStart};

use crate::vfs::CasVfs;

pub type CasCaller = roam_stream::Client<vx_io::net::EndpointConnector, NoDispatcher>;

#[derive(Debug, Clone)]
pub(crate) struct VfsStartupFailure {
    pub(crate) reason: String,
    pub(crate) diagnostics: Vec<VfsDiagnostic>,
}

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
    /// CAS endpoint
    cas_endpoint: Endpoint,

    /// Bind endpoint for Runner RPC service
    bind: Endpoint,
}

#[derive(Clone)]
struct SharedVfs(Arc<CasVfs>);

impl Vfs for SharedVfs {
    async fn lookup(&self, cx: &roam::Context, parent_id: ItemId, name: String) -> LookupResult {
        self.0.lookup(cx, parent_id, name).await
    }

    async fn get_attributes(&self, cx: &roam::Context, item_id: ItemId) -> GetAttributesResult {
        self.0.get_attributes(cx, item_id).await
    }

    async fn read_dir(&self, cx: &roam::Context, item_id: ItemId, cursor: u64) -> ReadDirResult {
        self.0.read_dir(cx, item_id, cursor).await
    }

    async fn read(&self, cx: &roam::Context, item_id: ItemId, offset: u64, len: u64) -> ReadResult {
        self.0.read(cx, item_id, offset, len).await
    }

    async fn write(
        &self,
        cx: &roam::Context,
        item_id: ItemId,
        offset: u64,
        data: Vec<u8>,
    ) -> WriteResult {
        self.0.write(cx, item_id, offset, data).await
    }

    async fn create(
        &self,
        cx: &roam::Context,
        parent_id: ItemId,
        name: String,
        item_type: ItemType,
    ) -> CreateResult {
        self.0.create(cx, parent_id, name, item_type).await
    }

    async fn delete(&self, cx: &roam::Context, item_id: ItemId) -> VfsResult {
        self.0.delete(cx, item_id).await
    }

    async fn rename(
        &self,
        cx: &roam::Context,
        item_id: ItemId,
        new_parent_id: ItemId,
        new_name: String,
    ) -> VfsResult {
        self.0.rename(cx, item_id, new_parent_id, new_name).await
    }

    async fn set_attributes(
        &self,
        cx: &roam::Context,
        item_id: ItemId,
        params: SetAttributesParams,
    ) -> VfsResult {
        self.0.set_attributes(cx, item_id, params).await
    }

    async fn ping(&self, cx: &roam::Context) -> String {
        self.0.ping(cx).await
    }
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

        Ok(Args { cas_endpoint, bind })
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
    let cas = connect_to_cas(&args.cas_endpoint).await?;
    tracing::info!("Connected to CAS");

    // Initialize services
    tracing::info!("Initializing Runner services");
    let cas = Arc::new(cas);

    // Create VFS (shared between VFS server and runner service)
    let vfs = Arc::new(CasVfs::new(cas.clone()));

    // Create Runner service (will use VFS for action execution)
    let exec = RunnerService::new(cas.clone(), vfs.clone());

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

    // Mount VFS via vx-vfs (includes platform-specific behavior).
    match vx_vfs::mount(
        VfsConfig {
            mount_path: runner_mount_base(),
        },
        SharedVfs(vfs.clone()),
    )
    .await
    {
        Ok(VfsStart::Mounted {
            mount_path,
            vfs_dead,
        }) => {
            tracing::info!("VFS mounted at {}", mount_path);
            exec.set_vfs_startup_error(None).await;
            let exec_for_watcher = exec.clone();
            tokio::spawn(async move {
                let mut vfs_dead = vfs_dead;
                while vfs_dead.changed().await.is_ok() {
                    if *vfs_dead.borrow() {
                        tracing::error!("VFS session died — marking VFS as failed");
                        exec_for_watcher
                            .set_vfs_startup_error(Some(VfsStartupFailure {
                                reason: "VFS session closed unexpectedly".into(),
                                diagnostics: vec![VfsDiagnostic {
                                    code: VfsIssueCode::MountFailed,
                                    summary: "VFS session died after mount".into(),
                                    detail: None,
                                    actions: vec![VfsAction::RetryBuild],
                                }],
                            }))
                            .await;
                        break;
                    }
                }
            });
        }
        Ok(VfsStart::Unsupported { platform }) => {
            let reason = format!("VFS not supported on platform {}", platform);
            tracing::warn!("{}", reason);
            exec.set_vfs_startup_error(Some(VfsStartupFailure {
                reason: reason.clone(),
                diagnostics: vec![VfsDiagnostic {
                    code: VfsIssueCode::UnsupportedPlatform,
                    summary: reason,
                    detail: None,
                    actions: vec![VfsAction::RetryBuild],
                }],
            }))
            .await;
        }
        Err(error) => {
            let mut diagnostics = error.diagnostics().to_vec();
            let reason = "Mount failed".to_string();
            if diagnostics.is_empty() {
                diagnostics.push(VfsDiagnostic {
                    code: VfsIssueCode::MountFailed,
                    summary: "VFS mount failed without structured diagnostics".to_string(),
                    detail: Some(reason.clone()),
                    actions: vec![VfsAction::RetryBuild],
                });
            }
            exec.set_vfs_startup_error(Some(VfsStartupFailure {
                reason,
                diagnostics,
            }))
            .await;
        }
    }

    // Start accepting Runner RPC connections regardless of VFS startup status.
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

    /// VFS for hermetic action execution
    pub(crate) vfs: Arc<CasVfs>,

    /// Startup-time VFS failure details, if any.
    pub(crate) vfs_startup_error: tokio::sync::RwLock<Option<VfsStartupFailure>>,

    /// Single-flight deduplication for toolchain prefix creation.
    /// Concurrent requests for the same toolchain coalesce into one operation.
    /// Stores Ok(prefix_id) or Err(message) — errors are cached too, since a missing
    /// toolchain manifest won't fix itself without a new runner session.
    #[allow(clippy::type_complexity)]
    toolchain_flights:
        tokio::sync::Mutex<HashMap<Blake3Hash, Arc<tokio::sync::OnceCell<Result<String, String>>>>>,
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
    pub fn new(cas: Arc<StoreClient<crate::CasCaller>>, vfs: Arc<CasVfs>) -> Self {
        Self {
            inner: Arc::new(RunnerServiceInner {
                cas,
                vfs,
                vfs_startup_error: tokio::sync::RwLock::new(None),
                toolchain_flights: tokio::sync::Mutex::new(HashMap::new()),
            }),
        }
    }

    pub(crate) async fn set_vfs_startup_error(&self, error: Option<VfsStartupFailure>) {
        *self.vfs_startup_error.write().await = error;
    }

    pub(crate) async fn get_vfs_startup_error(&self) -> Option<VfsStartupFailure> {
        self.vfs_startup_error.read().await.clone()
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
        loop {
            let prefix_id = format!("build-{:x}-{:08x}", ts, fastrand::u32(..));
            if self.vfs.has_prefix(&prefix_id).await {
                tracing::warn!(prefix_id = %prefix_id, "prefix ID collision, retrying");
                continue;
            }
            let (id, _item_id) = self.vfs.create_prefix(prefix_id).await;
            return id;
        }
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
                "loading component tree manifest"
            );

            let tree_manifest = self
                .cas
                .get_tree_manifest(component.tree_manifest)
                .await?
                .ok_or_else(|| {
                    RunnerError::CasFetch(format!(
                        "tree manifest {} not found for component {}",
                        component.tree_manifest, component.name
                    ))
                })?;

            tracing::debug!(
                component_name = %component.name,
                entry_count = tree_manifest.entries.len(),
                "registering component files in VFS"
            );

            for entry in &tree_manifest.entries {
                if let vx_store_proto::TreeEntryKind::File {
                    blob,
                    size,
                    executable,
                } = &entry.kind
                {
                    // Files go directly under the prefix root — no "toolchain/" nesting.
                    // The prefix itself IS the toolchain.
                    let path = if component.name == "rustc" {
                        entry.path.to_string()
                    } else if component.name == "rust-std" {
                        format!("sysroot/{}", entry.path)
                    } else if matches!(component.name.as_str(), "zig-exe" | "zig-lib") {
                        entry.path.to_string()
                    } else {
                        format!("{}/{}", component.name, entry.path)
                    };

                    self.vfs
                        .add_file_to_prefix(&prefix_id, &path, *blob, *size, *executable)
                        .await;
                }
            }
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
        use vx_store_proto::TreeEntryKind;

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
            "registering source files in VFS"
        );

        // Add each file to the VFS prefix under src/
        for entry in &manifest.entries {
            if let TreeEntryKind::File {
                blob,
                size,
                executable,
            } = &entry.kind
            {
                let path = format!("src/{}", entry.path);
                self.vfs
                    .add_file_to_prefix(prefix_id, &path, *blob, *size, *executable)
                    .await;
            }
        }

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
            .await;

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
