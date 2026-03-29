use facet::Facet;
use figue as args;

/// vx - Thin CLI for the current daemon shell
#[derive(Facet, Debug)]
pub(crate) struct Cli {
    /// Disable TUI and use plain log output
    #[facet(args::named)]
    pub(crate) no_tui: bool,

    /// Emit NDJSON build events to stdout (implies --no-tui)
    #[facet(args::named)]
    pub(crate) json: bool,

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
    /// Just start daemons
    Start(StartArgs),

    /// Just ping daemons
    Ping,

    /// Execute a build
    Build(BuildArgs),

    /// Stop the daemon process
    Kill(KillArgs),

    /// Show in-flight daemon activity
    Ps,

    /// Stop the daemon and remove .vx/ directory
    Clean(CleanArgs),

    /// Quick health check
    Doctor(DoctorArgs),

    /// Collect live diagnostic snapshots from all vixen processes
    Debug(DebugArgs),

    /// Placeholder eval entrypoint
    Eval(EvalArgs),
}

/// Arguments for the `build` subcommand
#[derive(Facet, Debug)]
pub(crate) struct BuildArgs {
    /// Build file to evaluate instead of `build.vx`
    #[facet(args::named, args::short = 'f')]
    pub(crate) file: Option<String>,

    /// Show the full evaluation stack for build failures
    #[facet(args::named)]
    pub(crate) full_stack: bool,

    /// Build in release mode
    #[facet(args::named, args::short = 'r')]
    pub(crate) release: bool,

    /// Pass an environment variable to the build (KEY=VALUE)
    #[facet(args::named, args::short = 'e', default)]
    pub(crate) env: Vec<String>,
}

/// Start daemons
#[derive(Facet, Debug)]
pub(crate) struct StartArgs {}

/// Arguments for the `kill` subcommand
#[derive(Facet, Debug)]
pub(crate) struct KillArgs {}

/// Arguments for the `clean` subcommand
#[derive(Facet, Debug)]
pub(crate) struct CleanArgs {
    /// Remove VX_HOME instead of project .vx/ (asks for confirmation)
    #[facet(args::named)]
    pub(crate) home: bool,

    /// Invalidate cache entries from the last build
    #[facet(args::named)]
    pub(crate) cache: bool,
}

/// Arguments for the `debug` subcommand
#[derive(Facet, Debug)]
pub(crate) struct DebugArgs {}

/// Arguments for the `doctor` subcommand
#[derive(Facet, Debug)]
pub(crate) struct DoctorArgs {}

/// Arguments for the `eval` subcommand.
///
/// The command is intentionally unavailable during the evaluator purge, but we
/// still accept `-f/--file` so existing invocations fail cleanly.
#[derive(Facet, Debug)]
pub(crate) struct EvalArgs {
    /// Vixen source file to evaluate, or `-` for stdin
    #[facet(args::named, args::short = 'f')]
    pub(crate) file: Option<String>,
}
