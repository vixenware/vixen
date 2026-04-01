use facet::Facet;
use figue as args;

/// vx - Thin CLI for the current daemon shell
#[derive(Facet, Debug)]
pub(crate) struct Cli {
    /// Command to run
    #[facet(args::subcommand)]
    pub(crate) command: CliCommand,

    /// Standard CLI options (--help, --version, --completions)
    #[facet(flatten)]
    pub(crate) builtins: args::FigueBuiltins,
}

#[derive(Facet, Debug)]
#[repr(u8)]
pub(crate) enum CliCommand {
    /// Quick health check
    Health(HealthArgs),

    /// Start the vx daemon (auto-started by other commands if not running)
    #[facet(args::short = 'd')]
    Daemon(DaemonArgs),
}

#[derive(Facet, Debug)]
pub(crate) struct HealthArgs {}

#[derive(Facet, Debug)]
pub(crate) struct DaemonArgs {}
