//! vx-runner - Worker daemon entry point.
//!
//! Thin shim: parses CLI args and delegates to [`vx_runner::serve`].

use eyre::Result;
use facet::Facet;
use figue as args;

/// vx-runner - Worker daemon
#[derive(Facet, Debug)]
struct Args {
    /// Listen endpoint for Runner RPC service
    #[facet(args::named)]
    listen: String,

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

    vx_runner::serve(&args.listen).await
}
