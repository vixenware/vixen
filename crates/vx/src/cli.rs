use facet::Facet;
use figue as args;

/// vixen — CLI husk.
///
/// The language runtime and daemon are being rebuilt atop the primitive layer
/// (fuse3, vx-vfs, vx-store, vx-fskit, vx-management, …). Until that rebuild
/// lands, the binary only answers the standard CLI builtins.
#[derive(Facet, Debug)]
pub(crate) struct Cli {
    /// Standard CLI options (--help, --version, --completions).
    #[facet(flatten)]
    pub(crate) builtins: args::FigueBuiltins,
}
