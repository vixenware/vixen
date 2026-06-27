//! vixen/vx — CLI husk.
//!
//! The language runtime and daemon are being rebuilt on top of the primitive
//! layer. Until that rebuild lands, this binary only serves the standard CLI
//! builtins (--help, --version, --completions) so that `xtask install` keeps
//! working and there is a stable entrypoint for the future command surface.

mod cli;

use figue as args;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binary {
    Vx,
    Vixen,
}

pub fn run(binary: Binary) {
    let program_name = match binary {
        Binary::Vixen => "vixen",
        Binary::Vx => "vx",
    };
    let raw_args: Vec<String> = std::env::args().skip(1).collect();

    let _: cli::Cli = args::Driver::new(
        args::builder::<cli::Cli>()
            .expect("failed to build CLI")
            .cli(|c| c.args(raw_args))
            .help(|h| {
                h.program_name(program_name)
                    .version(env!("CARGO_PKG_VERSION"))
            })
            .build(),
    )
    .run()
    .unwrap();

    eprintln!(
        "{program_name}: the language runtime is being rebuilt; no subcommands are available yet."
    );
}
