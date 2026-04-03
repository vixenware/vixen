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

    /// Lower and evaluate one strict function through the orchestrator
    Eval(EvalArgs),

    /// Start the vx daemon (auto-started by other commands if not running)
    #[facet(args::short = 'd')]
    Daemon(DaemonArgs),

    /// Start the language server (communicates over stdin/stdout)
    Lsp(LspArgs),
}

#[derive(Facet, Debug)]
pub(crate) struct HealthArgs {}

#[derive(Facet, Debug)]
pub(crate) struct EvalArgs {
    /// Source file containing the function to evaluate
    #[facet(args::named)]
    pub(crate) file: camino::Utf8PathBuf,

    /// Function name to evaluate
    #[facet(args::named)]
    pub(crate) function: String,

    /// Backend to use: interpreter or kajit
    #[facet(args::named, default = "interpreter")]
    pub(crate) backend: String,

    /// Include kernel IR dump in the response
    #[facet(args::named)]
    pub(crate) kernel_dump: bool,

    /// Include Kajit HIR dump in the response
    #[facet(args::named)]
    pub(crate) hir: bool,

    /// Include Kajit IR dump in the response
    #[facet(args::named)]
    pub(crate) ir: bool,

    /// Function arguments in order. Use `u64:42`, `bool:true`, `string:hello`.
    /// Bare values are treated as strings.
    pub(crate) args: Vec<String>,
}

#[derive(Facet, Debug)]
pub(crate) struct DaemonArgs {
    /// Storage root directory
    #[facet(args::named)]
    pub(crate) store_root: camino::Utf8PathBuf,

    /// VFS mount point
    #[facet(args::named, default = "/tmp/vx/vfs")]
    pub(crate) vfs_mount: camino::Utf8PathBuf,
}

#[derive(Facet, Debug)]
pub(crate) struct LspArgs {}
