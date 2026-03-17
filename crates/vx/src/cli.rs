use facet::Facet;
use figue as args;

/// vx - Build execution engine with deterministic caching
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
    #[facet(args::named, args::short = 'e')]
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

    /// Invalidate cache entries from the last build (keeps toolchains)
    #[facet(args::named)]
    pub(crate) cache: bool,
}

/// Arguments for the `debug` subcommand
#[derive(Facet, Debug)]
pub(crate) struct DebugArgs {}

/// Arguments for the `doctor` subcommand
#[derive(Facet, Debug)]
pub(crate) struct DoctorArgs {}
