//! vx - Build execution engine with deterministic caching
//!
//! This is a thin CLI client that talks to the vx daemon.

mod cli;

use figue as args;

use crate::cli::{Cli, CliCommand};

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    rt.block_on(async_main());
}

async fn async_main() {
    let cli: Cli = args::Driver::new(
        args::builder::<Cli>()
            .expect("failed to build CLI")
            .cli(|cli| cli.args(std::env::args().skip(1)))
            .help(|h| {
                h.program_name(env!("CARGO_PKG_NAME"))
                    .version(env!("CARGO_PKG_VERSION"))
            })
            .build(),
    )
    .run()
    .unwrap();

    match cli.command {
        CliCommand::Doctor(_args) => {
            eprintln!("All is looking good.");
        }
    };
}
