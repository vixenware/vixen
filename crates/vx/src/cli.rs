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

    /// Mount a store-backed tree into the shared Vixen mount
    Mount(MountArgs),

    /// Start the vx daemon (auto-started by other commands if not running)
    #[facet(args::short = 'd')]
    Daemon(DaemonArgs),

    /// Start the language server (communicates over stdin/stdout)
    Lsp(LspArgs),
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

    /// Evaluation backend: `interpreter` or `cranelift`
    #[facet(args::named, default = "cranelift")]
    pub(crate) backend: String,

    /// Print the strict fragment IR to stderr and include it in the response
    #[facet(args::named)]
    pub(crate) strict_fragment_dump: bool,

    /// Print the Cranelift IR to stderr and include it in the response
    #[facet(args::named)]
    pub(crate) clif_dump: bool,

    /// Function arguments in order. Use `u64:42`, `bool:true`, `string:hello`.
    /// Bare values are treated as strings.
    #[facet(args::positional)]
    pub(crate) args: Option<Vec<String>>,
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

    /// Evaluation backend: `interpreter` or `cranelift`
    #[facet(args::named, default = "interpreter")]
    pub(crate) backend: String,

    /// Print the strict fragment IR to stderr and include it in the response
    #[facet(args::named)]
    pub(crate) strict_fragment_dump: bool,

    /// Print the Cranelift IR to stderr and include it in the response
    #[facet(args::named)]
    pub(crate) clif_dump: bool,

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
    #[facet(args::named, default = "/tmp/vx/vfs")]
    pub(crate) vfs_mount: camino::Utf8PathBuf,
}

#[derive(Facet, Debug)]
pub(crate) struct LspArgs {}
