//! vx-vfsd - VFS daemon entry point.
//!
//! Thin shim: parses CLI args and delegates to [`vx_vfsd::boot`] + [`vox::serve`].

use camino::Utf8PathBuf;
use eyre::Result;
use facet::Facet;
use figue as args;

use vx_vfsd::{VfsdAcceptor, effective_mount_path};

/// vx-vfsd - VFS daemon for Vixen build system
#[derive(Facet, Debug)]
struct Args {
    /// Sentinel mode: clean up mount on parent death
    #[facet(args::named)]
    sentinel: Option<Utf8PathBuf>,

    /// RPC listen address (e.g., "127.0.0.1:0" for random port)
    #[facet(args::named, default = "127.0.0.1:0")]
    listen: String,

    /// VFS mount point (defaults to stable global path on macOS)
    #[facet(args::named, default = "/tmp/vx/vfs")]
    mount: Utf8PathBuf,

    /// vx-store (or vx-daemon) RPC address
    #[facet(args::named)]
    store: String,

    /// Standard CLI options (--help, --version, --completions)
    #[facet(flatten)]
    builtins: args::FigueBuiltins,
}

fn main() -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<()> {
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
        vx_vfsd::platform::run_sentinel(&mount_path).await?;
        return Ok(());
    }

    let mount = effective_mount_path(&args.mount);
    if cfg!(target_os = "macos") && mount != args.mount {
        tracing::info!(
            configured_mount = %args.mount,
            %mount,
            "ignoring configured mount path on macOS; using stable global mount path",
        );
    }

    tracing::debug!(
        listen = %args.listen,
        %mount,
        store = %args.store,
        "starting vx-vfsd"
    );

    let service = vx_vfsd::boot(&args.store, &mount).await?;
    let acceptor = VfsdAcceptor::new(service);
    vox::serve(&args.listen, acceptor).await?;
    Ok(())
}
