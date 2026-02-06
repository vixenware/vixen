//! vx-rhea - Worker daemon
//!
//! Implements the Exec Roam service trait.
//! A remote-capable compilation service - all inputs/outputs go through CAS.
//!
//! Responsibilities:
//! - Materialize toolchains from CAS (cached locally)
//! - Materialize source trees from CAS
//! - Materialize dependency artifacts from CAS
//! - Run rustc/zig
//! - Ingest outputs to CAS
//! - Return output manifest hashes

pub(crate) mod error;
pub(crate) mod extract;
pub(crate) mod registry;
pub(crate) mod service;
pub(crate) mod service_vfs;
pub(crate) mod toolchain;
pub(crate) mod vfs;

use error::{Result as RheaResult, RheaError};

use camino::Utf8PathBuf;
use eyre::Result;
use fs_kitty_proto::{
    CreateResult, GetAttributesResult, ItemId, ItemType, LookupResult, ReadDirResult, ReadResult,
    SetAttributesParams, Vfs, VfsResult, WriteResult,
};
use roam_stream::{HandshakeConfig, NoDispatcher, accept};
use std::collections::HashMap;
use std::sync::Arc;
use vx_cass_proto::{Blake3Hash, CassClient};
use vx_io::net::{Endpoint, Listener};
use vx_rhea_proto::RheaDispatcher;
use vx_vfs::{VfsConfig, VfsStart};

use crate::registry::RegistryMaterializer;
use crate::vfs::CasVfs;

pub type CasCaller = roam_stream::Client<vx_io::net::EndpointConnector, NoDispatcher>;

/// Type alias for inflight materialization tracking
type InflightMaterializations = Arc<
    tokio::sync::Mutex<HashMap<Blake3Hash, Arc<tokio::sync::OnceCell<RheaResult<Utf8PathBuf>>>>>,
>;

pub(crate) fn rhea_mount_base() -> Utf8PathBuf {
    if let Ok(path) = std::env::var("VX_RHEA_MOUNT") {
        return Utf8PathBuf::from(path);
    }
    if let Ok(vx_home) = std::env::var("VX_HOME") {
        return Utf8PathBuf::from(vx_home).join("fskitty").join("rhea");
    }
    if let Ok(home) = std::env::var("HOME") {
        return Utf8PathBuf::from(home)
            .join(".vx")
            .join("fskitty")
            .join("rhea");
    }
    Utf8PathBuf::from("/tmp/vx/fskitty/rhea")
}

#[derive(Debug)]
struct Args {
    /// Toolchains directory
    toolchains_dir: Utf8PathBuf,

    /// Registry cache directory
    registry_cache_dir: Utf8PathBuf,

    /// CAS endpoint
    cas_endpoint: Endpoint,

    /// Bind endpoint for Rhea RPC service
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
        let vx_home = Utf8PathBuf::from(&vx_home);

        let toolchains_dir = vx_home.join("toolchains");
        let registry_cache_dir = vx_home.join("registry");

        // Parse CAS endpoint (defaults to Unix socket)
        let cas_endpoint = match std::env::var("VX_CASS") {
            Ok(v) => Endpoint::parse(&v)?,
            Err(_) => {
                #[cfg(unix)]
                {
                    vx_io::net::default_unix_endpoint(&vx_home, "cass")
                }
                #[cfg(not(unix))]
                {
                    Endpoint::parse("127.0.0.1:9002")?
                }
            }
        };

        // Parse bind endpoint (defaults to Unix socket)
        let bind = match std::env::var("VX_RHEA") {
            Ok(v) => Endpoint::parse(&v)?,
            Err(_) => {
                #[cfg(unix)]
                {
                    vx_io::net::default_unix_endpoint(&vx_home, "rhea")
                }
                #[cfg(not(unix))]
                {
                    Endpoint::parse("127.0.0.1:9003")?
                }
            }
        };

        Ok(Args {
            toolchains_dir,
            registry_cache_dir,
            cas_endpoint,
            bind,
        })
    }
}

/// Connect to CAS and return a client handle
async fn connect_to_cas(endpoint: &Endpoint) -> Result<vx_cass_proto::CassClient<CasCaller>> {
    let caller = vx_io::net::reconnecting_client(endpoint.clone(), NoDispatcher);
    caller
        .handle()
        .await
        .map_err(|e| eyre::eyre!("failed to connect to CAS at {}: {}", endpoint, e))?;
    Ok(vx_cass_proto::CassClient::new(caller))
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
    vx_tracing::init_service("vx_rhea=info");

    let args = Args::from_env()?;

    // Check if another rhea instance is already running
    #[cfg(unix)]
    {
        if let Endpoint::Local(ref path) = args.bind
            && path.exists()
            && is_socket_in_use(path).await
        {
            return Err(eyre::eyre!(
                "Another vx-rhea instance is already running (socket {} is in use).\n\
                 Kill the existing instance or use a different VX_RHEA endpoint.",
                path
            ));
        }
    }

    // Connect to CAS
    tracing::info!("Connecting to CAS at {}", args.cas_endpoint);
    let cas = connect_to_cas(&args.cas_endpoint).await?;
    tracing::info!("Connected to CAS");

    // Initialize services
    tracing::info!("Initializing Rhea services");
    tracing::info!("  Toolchains: {}", args.toolchains_dir);
    tracing::info!("  Registry:   {}", args.registry_cache_dir);

    // Ensure directories exist
    tokio::fs::create_dir_all(&args.toolchains_dir).await?;
    tokio::fs::create_dir_all(&args.registry_cache_dir).await?;

    let cas = Arc::new(cas);

    // Create VFS (shared between VFS server and RheaService)
    let vfs = Arc::new(CasVfs::new(cas.clone()));

    // Create Rhea service (will use VFS for action execution)
    let exec = RheaService::new(
        cas.clone(),
        args.toolchains_dir,
        args.registry_cache_dir,
        vfs.clone(),
    );

    // Bind Rhea RPC socket (but don't start accepting yet - wait for VFS mount)
    let rhea_listener = match Listener::bind(&args.bind).await {
        Ok(l) => l,
        Err(e) => {
            return Err(eyre::eyre!(
                "Failed to bind Rhea RPC socket at {}: {}\n\
                 Another instance may be running, or the socket file is stale.",
                args.bind,
                e
            ));
        }
    };
    tracing::info!(
        "Rhea RPC bound on {} (will accept after VFS mount)",
        args.bind
    );

    // Mount VFS via vx-vfs (includes platform-specific behavior).
    match vx_vfs::mount(
        VfsConfig {
            mount_path: rhea_mount_base(),
        },
        SharedVfs(vfs.clone()),
    )
    .await
    {
        Ok(VfsStart::Mounted { mount_path }) => {
            tracing::info!("VFS mounted at {}", mount_path);
        }
        Ok(VfsStart::Unsupported { platform }) => {
            tracing::warn!("VFS not supported on platform {}", platform);
        }
        Err(error) => {
            tracing::error!("Failed to mount VFS: {}", error);
            tracing::error!("Cannot start rhea without VFS");
            std::process::exit(1);
        }
    }

    // NOW start accepting Rhea RPC connections (VFS is ready)
    tracing::info!("Starting Rhea RPC server (VFS is ready)");
    loop {
        match rhea_listener.accept().await {
            Ok((stream, peer_addr)) => {
                let exec = exec.clone();
                tokio::spawn(async move {
                    tracing::debug!("Rhea connection from {}", peer_addr);
                    let dispatcher = RheaDispatcher::new(exec);
                    let Ok((_handle, _incoming, driver)) =
                        accept(stream, HandshakeConfig::default(), dispatcher).await
                    else {
                        tracing::warn!("Rhea handshake error from {}", peer_addr);
                        return;
                    };
                    if let Err(e) = driver.run().await {
                        tracing::warn!("Rhea connection error from {}: {}", peer_addr, e);
                    }
                    tracing::debug!("Rhea connection from {} closed", peer_addr);
                });
            }
            Err(e) => {
                tracing::error!("Rhea accept error: {}", e);
            }
        }
    }
}

/// Inner Exec service implementation
pub struct RheaServiceInner {
    /// CAS client for storing outputs and fetching toolchains
    pub(crate) cas: Arc<CassClient<crate::CasCaller>>,

    /// Toolchain materialization directory
    pub(crate) toolchains_dir: Utf8PathBuf,

    /// In-flight toolchain materializations (keyed by manifest_hash)
    materializing: InflightMaterializations,

    /// Registry crate materializer
    pub(crate) registry_materializer: RegistryMaterializer,

    /// VFS for hermetic action execution
    pub(crate) vfs: Arc<CasVfs>,
}

/// Exec service handle - cloneable wrapper around shared inner state
#[derive(Clone)]
pub struct RheaService {
    inner: Arc<RheaServiceInner>,
}

impl std::ops::Deref for RheaService {
    type Target = RheaServiceInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl RheaService {
    pub fn new(
        cas: Arc<CassClient<crate::CasCaller>>,
        toolchains_dir: Utf8PathBuf,
        registry_cache_dir: Utf8PathBuf,
        vfs: Arc<CasVfs>,
    ) -> Self {
        Self {
            inner: Arc::new(RheaServiceInner {
                registry_materializer: RegistryMaterializer::new(cas.clone(), registry_cache_dir),
                cas,
                toolchains_dir,
                materializing: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
                vfs,
            }),
        }
    }

    /// Ensures a toolchain is materialized locally, returns the materialized directory.
    /// Uses async locking to prevent concurrent materializations of the same toolchain.
    pub(crate) async fn ensure_materialized(
        &self,
        manifest_hash: Blake3Hash,
    ) -> RheaResult<Utf8PathBuf> {
        // Check if already materializing
        let cell = {
            let mut map = self.materializing.lock().await;
            map.entry(manifest_hash)
                .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new()))
                .clone()
        };

        // Wait for or perform materialization
        cell.get_or_init(|| async { self.materialize_toolchain(manifest_hash).await })
            .await
            .clone()
    }

    /// Materialize a toolchain from CAS to local directory
    async fn materialize_toolchain(&self, manifest_hash: Blake3Hash) -> RheaResult<Utf8PathBuf> {
        self.materialize_toolchain_impl(manifest_hash).await
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
        // Generate a unique prefix ID using timestamp + random suffix
        let ts = Timestamp::now().as_millisecond();
        let prefix_id = format!("build-{:x}-{:04x}", ts, rand_u16());
        let (id, _item_id) = self.vfs.create_prefix(prefix_id).await;
        id
    }

    /// Add a toolchain to an action prefix
    ///
    /// Populates `<prefix>/toolchain/` with the toolchain's files from CAS.
    pub(crate) async fn add_toolchain_to_prefix(
        &self,
        prefix_id: &str,
        toolchain_manifest_hash: Blake3Hash,
    ) -> RheaResult<()> {
        tracing::info!(
            toolchain_manifest = %toolchain_manifest_hash,
            "add_toolchain_to_prefix: starting"
        );

        // Get the toolchain manifest from CAS
        let toolchain_manifest = self
            .cas
            .get_toolchain_manifest(toolchain_manifest_hash)
            .await
            .map_err(|e| RheaError::CasFetch(format!("{:?}", e)))?
            .ok_or_else(|| {
                RheaError::CasFetch(format!(
                    "toolchain manifest {} not found",
                    toolchain_manifest_hash
                ))
            })?;

        tracing::info!(
            component_count = toolchain_manifest.components.len(),
            "add_toolchain_to_prefix: got toolchain manifest"
        );

        // Add each component's tree to the VFS prefix under toolchain/
        for component in &toolchain_manifest.components {
            tracing::info!(
                component_name = %component.name,
                tree_manifest = %component.tree_manifest,
                "add_toolchain_to_prefix: fetching component tree manifest"
            );

            // Fetch the tree manifest for this component
            let tree_manifest = self
                .cas
                .get_tree_manifest(component.tree_manifest)
                .await
                .map_err(|e| {
                    tracing::error!(
                        component_name = %component.name,
                        tree_manifest = %component.tree_manifest,
                        error = ?e,
                        "add_toolchain_to_prefix: get_tree_manifest RPC error"
                    );
                    RheaError::CasFetch(format!("{:?}", e))
                })?
                .ok_or_else(|| {
                    tracing::error!(
                        component_name = %component.name,
                        tree_manifest = %component.tree_manifest,
                        "add_toolchain_to_prefix: get_tree_manifest returned None"
                    );
                    RheaError::CasFetch(format!(
                        "tree manifest {} not found for component {}",
                        component.tree_manifest, component.name
                    ))
                })?;

            tracing::info!(
                component_name = %component.name,
                entry_count = tree_manifest.entries.len(),
                "add_toolchain_to_prefix: got tree manifest, adding files"
            );

            // Add all files from the tree
            for entry in &tree_manifest.entries {
                // Only handle files (not symlinks yet)
                if let vx_cass_proto::TreeEntryKind::File {
                    blob,
                    size,
                    executable,
                } = &entry.kind
                {
                    let path = if component.name == "rustc" {
                        // rustc files go directly under toolchain/
                        format!("toolchain/{}", entry.path)
                    } else if component.name == "rust-std" {
                        // rust-std files go under toolchain/sysroot/
                        format!("toolchain/sysroot/{}", entry.path)
                    } else if matches!(component.name.as_str(), "zig-exe" | "zig-lib") {
                        format!("toolchain/{}", entry.path)
                    } else {
                        format!("toolchain/{}/{}", component.name, entry.path)
                    };

                    self.vfs
                        .add_file_to_prefix(prefix_id, &path, *blob, *size, *executable)
                        .await;
                }
            }
        }

        Ok(())
    }

    /// Add a source tree to an action prefix
    ///
    /// Populates `<prefix>/src/` with the source tree's files from CAS.
    pub(crate) async fn add_source_tree_to_prefix(
        &self,
        prefix_id: &str,
        source_manifest_hash: Blake3Hash,
    ) -> RheaResult<()> {
        use vx_cass_proto::TreeEntryKind;

        tracing::info!(
            source_manifest = %source_manifest_hash,
            "add_source_tree_to_prefix: calling get_tree_manifest"
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
                    "add_source_tree_to_prefix: RPC error from get_tree_manifest"
                );
                RheaError::CasFetch(format!("{:?}", e))
            })?
            .ok_or_else(|| {
                tracing::error!(
                    source_manifest = %source_manifest_hash,
                    "add_source_tree_to_prefix: get_tree_manifest returned None"
                );
                RheaError::CasFetch(format!(
                    "source manifest {} not found",
                    source_manifest_hash
                ))
            })?;

        tracing::info!(
            source_manifest = %source_manifest_hash,
            entry_count = manifest.entries.len(),
            "add_source_tree_to_prefix: got manifest, adding files"
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
    ) -> RheaResult<()> {
        // Get the dependency manifest
        let manifest = self
            .cas
            .get_manifest(manifest_hash)
            .await
            .map_err(|e| RheaError::CasFetch(format!("{:?}", e)))?
            .ok_or_else(|| {
                RheaError::CasFetch(format!("dependency manifest {} not found", manifest_hash))
            })?;

        // Find the rlib output
        let rlib = manifest
            .outputs
            .iter()
            .find(|o| o.logical == "rlib")
            .ok_or_else(|| {
                RheaError::CasFetch(format!("no rlib in manifest for {}", extern_name))
            })?;

        let path = format!("deps/lib{}.rlib", extern_name);
        self.vfs
            .add_file_to_prefix(prefix_id, &path, rlib.blob, 0, false)
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
    ) -> HashMap<String, Blake3Hash> {
        self.vfs.flush_prefix_to_cas(prefix_id).await
    }

    /// Clean up an action prefix
    pub(crate) async fn remove_action_prefix(&self, prefix_id: &str) {
        self.vfs.remove_prefix(prefix_id).await;
    }

    /// Get the VFS mount path for an action prefix
    ///
    /// This is the path that should be passed to sandboxed processes.
    /// Format: <mount-base>/<prefix_id>/ where mount-base defaults to ~/.vx/fskitty/rhea.
    #[allow(dead_code)] // Will be used for sandbox integration
    pub(crate) fn get_prefix_mount_path(&self, prefix_id: &str) -> Utf8PathBuf {
        rhea_mount_base().join(prefix_id)
    }
}

/// Generate a random u16 for prefix uniqueness
fn rand_u16() -> u16 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    (nanos & 0xFFFF) as u16
}
