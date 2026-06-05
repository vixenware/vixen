use facet::Facet;
use figue as args;

/// vixen - Management CLI for the current daemon shell
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
    /// Run a package-defined command
    Run(RunArgs),

    /// Quick health check
    Health(HealthArgs),

    /// Stop a running vx daemon
    Kill(KillArgs),

    /// Lower and evaluate one strict function through the orchestrator
    Eval(EvalArgs),

    /// Discover and run `.vx` tests through the daemon, streaming results
    Test(TestArgs),

    /// Mount a store-backed tree into the shared Vixen mount
    Mount(MountArgs),

    /// Start the vx daemon (auto-started by other commands if not running)
    #[facet(args::short = 'd')]
    Daemon(DaemonArgs),
}

#[derive(Facet, Debug)]
pub(crate) struct ClientTargetArgs {
    /// Daemon address to connect to
    #[facet(args::named)]
    pub(crate) daemon: Option<String>,

    /// Daemon configuration file in STYX format
    #[facet(args::named)]
    pub(crate) config: Option<camino::Utf8PathBuf>,
}

#[derive(Facet, Debug)]
pub(crate) struct HealthArgs {
    #[facet(flatten)]
    pub(crate) target: ClientTargetArgs,
}

#[derive(Facet, Debug)]
pub(crate) struct KillArgs {
    #[facet(flatten)]
    pub(crate) target: ClientTargetArgs,
}

#[derive(Facet, Debug)]
pub(crate) struct EvalArgs {
    #[facet(flatten)]
    pub(crate) target: ClientTargetArgs,

    /// Stop and restart the local auto-started daemon before evaluation
    #[facet(args::named)]
    pub(crate) fresh_daemon: bool,

    /// Source file containing the function to evaluate
    #[facet(args::named)]
    pub(crate) file: camino::Utf8PathBuf,

    /// Function name to evaluate
    #[facet(args::named)]
    pub(crate) function: String,

    /// Function arguments in order. Use `u64:42`, `bool:true`, `string:hello`.
    /// Bare values are treated as strings.
    #[facet(args::positional)]
    pub(crate) args: Option<Vec<String>>,
}

#[derive(Facet, Debug)]
pub(crate) struct TestArgs {
    #[facet(flatten)]
    pub(crate) target: ClientTargetArgs,

    /// Stop and restart the local auto-started daemon before running tests
    #[facet(args::named)]
    pub(crate) fresh_daemon: bool,

    /// File or directory to search for `.vx` tests. Defaults to the current
    /// directory, searched recursively.
    #[facet(args::positional)]
    pub(crate) path: Option<String>,
}

#[derive(Facet, Debug)]
pub(crate) struct RunArgs {
    #[facet(flatten)]
    pub(crate) target: ClientTargetArgs,

    /// Stop and restart the local auto-started daemon before running
    #[facet(args::named)]
    pub(crate) fresh_daemon: bool,

    /// Source file containing the requested command
    #[facet(args::named)]
    pub(crate) file: Option<camino::Utf8PathBuf>,

    /// Package-defined command to run
    #[facet(args::positional)]
    pub(crate) command: String,

    /// Command arguments in order. Use `u64:42`, `bool:true`, `string:hello`.
    /// Bare values are treated as strings.
    #[facet(args::positional)]
    pub(crate) args: Option<Vec<String>>,
}

#[derive(Facet, Debug)]
pub(crate) struct MountArgs {
    #[facet(flatten)]
    pub(crate) target: ClientTargetArgs,

    /// Tree hash to mount. Accepts `tree:<hash>` or bare hex.
    #[facet(args::positional)]
    pub(crate) tree: String,

    /// Override the mounted directory name under the shared VFS root
    #[facet(args::named)]
    pub(crate) name: Option<String>,
}

#[derive(Facet, Debug)]
pub(crate) struct DaemonArgs {
    /// Daemon configuration file in STYX format
    #[facet(args::named)]
    pub(crate) config: Option<camino::Utf8PathBuf>,

    /// Storage root directory
    #[facet(args::named)]
    pub(crate) store_root: Option<camino::Utf8PathBuf>,

    /// VFS mount point
    #[facet(args::named, default = vx_vfsd::DEFAULT_CONFIGURED_VFS_MOUNT_PATH)]
    pub(crate) vfs_mount: camino::Utf8PathBuf,
}
