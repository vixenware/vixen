//! VFS daemon for Vixen build system.
//!
//! Manages a FUSE-backed virtual filesystem that serves content from CAS
//! (vx-store) and collects build outputs.

mod protocol;
mod service;
mod vfs;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

use camino::Utf8PathBuf;
use facet::Facet;
use figue as args;
use roam_stream::{HandshakeConfig, NoDispatcher, accept};
use std::sync::Arc;
use vixenfs_proto::{
    CreateResult, GetAttributesResult, ItemId, LookupResult, ReadDirResult, ReadResult,
    SetAttributesParams, Vfs, VfsResult, WriteResult,
};
use vx_io::net::{Endpoint, Listener};
use vx_store_proto::StoreClient;
use vx_vfs::{VfsConfig, VfsStart};

use crate::protocol::VfsdDispatcher;
use crate::service::VfsdService;
use crate::vfs::CasVfs;

pub type CasCaller = roam_stream::Client<vx_io::net::EndpointConnector, NoDispatcher>;

/// Wrapper for CasVfs that implements Vfs by delegating to the inner CasVfs
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
        item_type: vixenfs_proto::ItemType,
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

    async fn ping(&self, _cx: &roam::Context) -> String {
        "vx-vfsd".to_string()
    }
}

/// vx-vfsd - VFS daemon for Vixen build system
#[derive(Facet, Debug)]
struct Args {
    /// Sentinel mode: clean up mount on parent death
    #[facet(args::named)]
    sentinel: Option<Utf8PathBuf>,

    /// RPC listen address (e.g., "127.0.0.1:0" for random port)
    #[facet(args::named, default = "127.0.0.1:0")]
    listen: String,

    /// FUSE mount point
    #[facet(args::named)]
    mount: Utf8PathBuf,

    /// vx-store RPC address
    #[facet(args::named)]
    store: String,

    /// Standard CLI options (--help, --version, --completions)
    #[facet(flatten)]
    builtins: args::FigueBuiltins,
}

#[tokio::main]
async fn main() -> Result<()> {
    vx_tracing::init_service("vx-vfsd=debug");

    // Die when parent dies
    ur_taking_me_with_you::die_with_parent();

    let args: Args = args::Driver::new(
        args::builder::<Args>()?
            .cli(|cli| cli.args(std::env::args().skip(1)))
            .help(|h| h.program_name("vx-vfsd").version(env!("CARGO_PKG_VERSION")))
            .build(),
    )
    .run()
    .unwrap();

    if let Some(mount_path) = args.sentinel {
        // Sentinel mode: poll parent PID, unmount when it dies
        tracing::info!(?mount_path, "running in sentinel mode");

        let parent_pid = unsafe { libc::getppid() };
        tracing::info!(parent_pid, "watching parent process");

        // Poll parent with kill(pid, 0) until it dies
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            // Check if parent is still alive
            let alive = unsafe { libc::kill(parent_pid, 0) == 0 };
            if !alive {
                tracing::info!(parent_pid, "parent died, unmounting");
                break;
            }
        }

        // Unmount the FUSE filesystem
        // FIXME: no cfg code in the main path!! move to per-platform submodules
        #[cfg(unix)]
        {
            let status = std::process::Command::new("fusermount3")
                .arg("-uz")
                .arg(mount_path.as_str())
                .status();
            match status {
                Ok(s) if s.success() => tracing::info!("unmounted successfully"),
                Ok(s) => tracing::error!("unmount failed with status: {}", s),
                Err(e) => tracing::error!("failed to run fusermount3: {}", e),
            }
        }

        return Ok(());
    }

    tracing::info!(
        listen = %args.listen,
        mount = %args.mount,
        store = %args.store,
        "starting vx-vfsd"
    );

    // 1. Connect to vx-store
    tracing::info!("Connecting to vx-store at {}", args.store);
    let cas = connect_to_store(&args.store).await?;
    tracing::info!("Connected to vx-store");

    let cas = Arc::new(cas);

    // 2. Create VFS with optional L2 disk blob cache
    let disk_cache_dir = std::env::var("VX_HOME")
        .ok()
        .map(|vx_home| Utf8PathBuf::from(vx_home).join("cache/blobs/blake3"));
    if let Some(ref dir) = disk_cache_dir {
        tracing::info!(%dir, "L2 disk blob cache enabled");
    }
    let vfs = Arc::new(CasVfs::new(cas.clone(), disk_cache_dir));

    // 3. Bind RPC listener before FUSE mount
    let listen_endpoint = Endpoint::parse(&args.listen)?;
    let vfsd_listener = Listener::bind(&listen_endpoint).await.map_err(|e| {
        format!(
            "Failed to bind vx-vfsd RPC socket at {}: {}",
            listen_endpoint, e
        )
    })?;
    let actual_addr = vfsd_listener.local_addr()?;
    tracing::info!("vx-vfsd RPC bound on {}", actual_addr);

    // Print the actual address to stdout so vxd can parse it
    println!("{}", actual_addr);

    // 4. Clean up stuck mounts BEFORE creating directory
    #[cfg(target_os = "linux")]
    cleanup_stuck_fuse_mount(&args.mount).await?;
    #[cfg(target_os = "macos")]
    cleanup_stuck_fskit_mount(&args.mount).await?;

    // 5. Ensure mount directory exists (skip for /Volumes - system creates it)
    if !args.mount.starts_with("/Volumes/") {
        tokio::fs::create_dir_all(args.mount.as_std_path())
            .await
            .map_err(|e| format!("failed to create mount directory {}: {}", args.mount, e))?;
    }

    // 6. Mount VFS
    tracing::info!("Mounting VFS at {}", args.mount);
    match vx_vfs::mount(
        VfsConfig {
            mount_path: args.mount.clone(),
        },
        SharedVfs(vfs.clone()),
    )
    .await
    {
        Ok(VfsStart::Mounted {
            mount_path,
            mut vfs_dead,
            ..
        }) => {
            tracing::info!("VFS mounted at {}", mount_path);

            // Set up SIGUSR1 handler for diagnostics
            #[cfg(unix)]
            {
                let vfs_for_signal = vfs.clone();
                tokio::spawn(async move {
                    use tokio::signal::unix::{SignalKind, signal};
                    let mut sigusr1 = signal(SignalKind::user_defined1())
                        .expect("failed to register SIGUSR1 handler");
                    loop {
                        sigusr1.recv().await;
                        tracing::info!("SIGUSR1 received, dumping diagnostics");
                        dump_kernel_fuse_state();
                        vfs_for_signal.dump_all_state();
                    }
                });
            }

            // Spawn watcher for VFS death
            let vfs_mount_path = mount_path.clone();
            tokio::spawn(async move {
                // Wait for VFS to report death
                loop {
                    if vfs_dead.changed().await.is_err() || *vfs_dead.borrow() {
                        break;
                    }
                }
                tracing::error!("VFS session died unexpectedly at {}", vfs_mount_path);
                std::process::exit(1);
            });
        }
        Ok(VfsStart::Unsupported { platform }) => {
            return Err(format!("VFS not supported on platform {}", platform).into());
        }
        Err(error) => {
            eprintln!("VFS mount failed: {}", error);
            for diagnostic in error.diagnostics() {
                eprintln!("  {}: {}", diagnostic.code, diagnostic.summary);
                if let Some(ref detail) = diagnostic.detail {
                    eprintln!("    {}", detail);
                }
            }
            return Err(format!("VFS mount failed: {}", error).into());
        }
    }

    // 7. Start accepting RPC connections
    tracing::info!("Starting vx-vfsd RPC server");
    let vfsd_service = VfsdService::new(vfs);

    loop {
        match vfsd_listener.accept().await {
            Ok((stream, peer_addr)) => {
                let service = vfsd_service.clone();
                tokio::spawn(async move {
                    tracing::debug!("vx-vfsd connection from {}", peer_addr);
                    let dispatcher = VfsdDispatcher::new(service);
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
                        tracing::warn!("Failed to accept vx-vfsd connection from {}", peer_addr);
                        return;
                    };
                    tracing::debug!("vx-vfsd session established with {}", peer_addr);
                    if let Err(e) = driver.run().await {
                        tracing::debug!("vx-vfsd session error with {}: {}", peer_addr, e);
                    }
                    tracing::debug!("vx-vfsd session closed with {}", peer_addr);
                });
            }
            Err(e) => {
                tracing::error!("vx-vfsd accept error: {}", e);
            }
        }
    }
}

fn dump_kernel_fuse_state() {
    eprintln!("\n--- Kernel FUSE state ---");

    if let Ok(entries) = std::fs::read_dir("/sys/fs/fuse/connections") {
        for entry in entries.flatten() {
            let waiting_path = entry.path().join("waiting");
            if let Ok(val) = std::fs::read_to_string(&waiting_path) {
                eprintln!("  {}: waiting={}", entry.path().display(), val.trim());
            }
        }
    }

    eprintln!("\n--- Thread stacks ---");
    if let Ok(tasks) = std::fs::read_dir("/proc/self/task") {
        for task in tasks.flatten() {
            let tid = task.file_name();
            let comm = std::fs::read_to_string(task.path().join("comm")).unwrap_or_default();
            let stack = std::fs::read_to_string(task.path().join("stack")).unwrap_or_default();
            if !stack.trim().is_empty() {
                eprintln!("  Thread {} ({}):", tid.to_string_lossy(), comm.trim());
                for line in stack.lines() {
                    eprintln!("    {line}");
                }
            }
        }
    }
}

async fn connect_to_store(endpoint_str: &str) -> Result<StoreClient<CasCaller>> {
    let endpoint = Endpoint::parse(endpoint_str)?;
    let caller = vx_io::net::reconnecting_client(endpoint.clone(), NoDispatcher);

    // Wait for initial connection
    caller
        .handle()
        .await
        .map_err(|e| format!("failed to connect to vx-store at {}: {}", endpoint, e))?;

    Ok(StoreClient::new(caller))
}

/// Clean up stuck FUSE mount if needed (Linux only)
#[cfg(target_os = "linux")]
async fn cleanup_stuck_fuse_mount(mount_path: &Utf8PathBuf) -> Result<()> {
    // Check if mount exists in /proc/mounts
    let mounts = match std::fs::read_to_string("/proc/mounts") {
        Ok(content) => content,
        Err(_) => return Ok(()), // Can't read mounts, skip cleanup
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
            tracing::warn!(
                mount_path = %mount_path,
                "detected stuck FUSE mount, attempting cleanup"
            );
        }
        Err(_) => {
            // Some other error, let it fail naturally
            return Ok(());
        }
    }

    // Level 1: Try gentle unmount
    tracing::info!(mount_path = %mount_path, "attempting gentle unmount: fusermount3 -u");
    let status = std::process::Command::new("fusermount3")
        .arg("-u")
        .arg(mount_path.as_str())
        .status()
        .map_err(|e| format!("failed to run fusermount3: {}", e))?;

    if status.success() {
        tracing::info!("successfully unmounted stuck FUSE mount");
        return Ok(());
    }

    // Level 2: Try lazy unmount
    tracing::warn!(mount_path = %mount_path, "gentle unmount failed, trying lazy unmount: fusermount3 -uz");
    let status = std::process::Command::new("fusermount3")
        .arg("-uz")
        .arg(mount_path.as_str())
        .status()
        .map_err(|e| format!("failed to run fusermount3: {}", e))?;

    if status.success() {
        tracing::info!("successfully unmounted with lazy unmount");
        return Ok(());
    }

    // Level 3: Nuclear option - abort FUSE connections then unmount
    tracing::error!("lazy unmount failed, attempting nuclear cleanup");

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
        .status()
        .map_err(|e| format!("failed to run fusermount3: {}", e))?;

    if status.success() {
        tracing::warn!("nuclear cleanup succeeded");
        Ok(())
    } else {
        Err(format!(
            "failed to clean up stuck FUSE mount at {} after all attempts",
            mount_path
        )
        .into())
    }
}

/// Clean up stuck FSKit mount if needed (macOS only)
#[cfg(target_os = "macos")]
async fn cleanup_stuck_fskit_mount(mount_path: &Utf8PathBuf) -> Result<()> {
    use std::process::Command;

    tracing::debug!(mount_path = %mount_path, "checking for stuck FSKit mount");

    // Canonicalize the mount path
    // If it doesn't exist, canonicalize the parent and append the final component
    let canonical_mount_path = match tokio::fs::canonicalize(mount_path.as_std_path()).await {
        Ok(p) => {
            let canonical = Utf8PathBuf::try_from(p).unwrap_or_else(|_| mount_path.clone());
            tracing::debug!(
                mount_path = %mount_path,
                canonical_mount_path = %canonical,
                "canonicalized mount path directly"
            );
            canonical
        }
        Err(_) => {
            // Path doesn't exist, try canonicalizing parent
            if let Some(parent) = mount_path.parent() {
                match tokio::fs::canonicalize(parent.as_std_path()).await {
                    Ok(canonical_parent) => {
                        let canonical_parent = Utf8PathBuf::try_from(canonical_parent)
                            .unwrap_or_else(|_| parent.to_path_buf());
                        let canonical = canonical_parent.join(mount_path.file_name().unwrap());
                        tracing::debug!(
                            mount_path = %mount_path,
                            canonical_mount_path = %canonical,
                            "canonicalized via parent directory"
                        );
                        canonical
                    }
                    Err(e) => {
                        tracing::debug!(
                            mount_path = %mount_path,
                            error = %e,
                            "parent does not exist, using mount path as-is"
                        );
                        mount_path.clone()
                    }
                }
            } else {
                tracing::debug!(
                    mount_path = %mount_path,
                    "no parent directory, using mount path as-is"
                );
                mount_path.clone()
            }
        }
    };

    // Run `df` to check for existing mounts
    tracing::debug!("running df to check for existing mounts");
    let output = Command::new("df")
        .output()
        .map_err(|e| format!("failed to run df: {}", e))?;

    if !output.status.success() {
        tracing::warn!("df command failed, skipping mount cleanup");
        return Ok(()); // df failed, skip cleanup
    }

    let df_output = String::from_utf8_lossy(&output.stdout);
    tracing::trace!(df_output = %df_output, "df output");

    // Look for a mount at our target path
    // df output format: "Filesystem ... Mounted on"
    // Example: "vixen://session?sid=... 8000000 0 4000000 0% 50000 50000 50% /private/tmp/.vx-test/..."
    let mut is_mounted = false;
    let mut found_mount_at: Option<String> = None;
    for line in df_output.lines().skip(1) {
        // Last field is the mount point
        if let Some(mounted_on) = line.split_whitespace().last() {
            // Canonicalize both paths for comparison (handles /tmp -> /private/tmp, symlinks, etc.)
            let canonical_df_path = match std::fs::canonicalize(mounted_on) {
                Ok(p) => Utf8PathBuf::try_from(p).ok(),
                Err(_) => None,
            };

            if let Some(df_path) = canonical_df_path {
                if df_path == canonical_mount_path {
                    tracing::info!(
                        mount_path = %mount_path,
                        canonical_mount_path = %canonical_mount_path,
                        df_entry = %mounted_on,
                        "found existing mount (matched via canonicalization)"
                    );
                    is_mounted = true;
                    found_mount_at = Some(mounted_on.to_string());
                    break;
                }
            } else {
                // If canonicalization fails, fall back to string comparison
                if mounted_on == mount_path.as_str() || mounted_on == canonical_mount_path.as_str()
                {
                    tracing::info!(
                        mount_path = %mount_path,
                        df_entry = %mounted_on,
                        "found existing mount (matched via string comparison)"
                    );
                    is_mounted = true;
                    found_mount_at = Some(mounted_on.to_string());
                    break;
                }
            }
        }
    }

    if !is_mounted {
        tracing::debug!(mount_path = %mount_path, "no existing mount found");
        return Ok(()); // Not mounted, nothing to clean up
    }

    let found_at = found_mount_at.as_deref().unwrap_or("unknown");
    tracing::info!(mount_path = %mount_path, found_at = %found_at, "proceeding with unmount");

    // Try to stat the mount point to see if it's responsive
    match tokio::fs::metadata(mount_path.as_std_path()).await {
        Ok(_) => {
            // Mount is responsive, but we still need to unmount to re-mount
            tracing::warn!(
                mount_path = %mount_path,
                "found existing mount, attempting unmount"
            );
        }
        Err(_) => {
            // Mount is stuck/unresponsive
            tracing::warn!(
                mount_path = %mount_path,
                "detected stuck FSKit mount, attempting cleanup"
            );
        }
    }

    // Use diskutil to unmount
    tracing::info!(mount_path = %mount_path, "attempting unmount: diskutil unmount");
    let status = Command::new("diskutil")
        .arg("unmount")
        .arg(canonical_mount_path.as_str())
        .status()
        .map_err(|e| format!("failed to run diskutil: {}", e))?;

    if status.success() {
        tracing::info!("successfully unmounted existing mount");
        Ok(())
    } else {
        Err(format!(
            "failed to unmount existing mount at {}",
            canonical_mount_path
        )
        .into())
    }
}
