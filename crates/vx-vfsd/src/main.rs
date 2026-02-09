//! VFS daemon for Vixen build system.
//!
//! Manages a FUSE-backed virtual filesystem that serves content from CAS
//! (vx-store) and collects build outputs.

mod protocol;
mod service;
mod vfs;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

use std::sync::Arc;
use facet::Facet;
use figue as args;
use roam_stream::{HandshakeConfig, NoDispatcher, accept};
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

    async fn write(&self, cx: &roam::Context, item_id: ItemId, offset: u64, data: Vec<u8>) -> WriteResult {
        self.0.write(cx, item_id, offset, data).await
    }

    async fn create(&self, cx: &roam::Context, parent_id: ItemId, name: String, item_type: vixenfs_proto::ItemType) -> CreateResult {
        self.0.create(cx, parent_id, name, item_type).await
    }

    async fn delete(&self, cx: &roam::Context, item_id: ItemId) -> VfsResult {
        self.0.delete(cx, item_id).await
    }

    async fn rename(&self, cx: &roam::Context, item_id: ItemId, new_parent_id: ItemId, new_name: String) -> VfsResult {
        self.0.rename(cx, item_id, new_parent_id, new_name).await
    }

    async fn set_attributes(&self, cx: &roam::Context, item_id: ItemId, params: SetAttributesParams) -> VfsResult {
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
    sentinel: Option<camino::Utf8PathBuf>,

    /// RPC listen address (e.g., "127.0.0.1:0" for random port)
    #[facet(args::named, default = "127.0.0.1:0")]
    listen: String,

    /// FUSE mount point
    #[facet(args::named)]
    mount: camino::Utf8PathBuf,

    /// vx-store RPC address
    #[facet(args::named)]
    store: String,

    /// Standard CLI options (--help, --version, --completions)
    #[facet(flatten)]
    builtins: args::FigueBuiltins,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    // Die when parent dies
    ur_taking_me_with_you::die_with_parent();

    let args: Args = args::Driver::new(
        args::builder::<Args>()?
            .cli(|cli| cli.args(std::env::args().skip(1)))
            .help(|h| {
                h.program_name("vx-vfsd")
                    .version(env!("CARGO_PKG_VERSION"))
            })
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

    // 2. Create VFS
    let vfs = Arc::new(CasVfs::new(cas.clone()));

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

    // 4. Mount VFS
    tracing::info!("Mounting VFS at {}", args.mount);
    match vx_vfs::mount(
        VfsConfig {
            mount_path: args.mount.clone(),
        },
        SharedVfs(vfs.clone()),
    )
    .await
    {
        Ok(VfsStart::Mounted { mount_path, mut vfs_dead, .. }) => {
            tracing::info!("VFS mounted at {}", mount_path);

            // Set up SIGUSR1 handler for diagnostics
            #[cfg(unix)]
            {
                let vfs_for_signal = vfs.clone();
                tokio::spawn(async move {
                    use tokio::signal::unix::{signal, SignalKind};
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
            return Err(format!("VFS mount failed: {}", error).into());
        }
    }

    // 5. Start accepting RPC connections
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
                eprintln!(
                    "  {}: waiting={}",
                    entry.path().display(),
                    val.trim()
                );
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
                eprintln!(
                    "  Thread {} ({}):",
                    tid.to_string_lossy(),
                    comm.trim()
                );
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
