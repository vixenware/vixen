/// Initialize tracing for a daemon/service process.
/// Uses uptime timer, writes to stderr, respects RUST_LOG.
pub fn init_service(default_filter: &str) {
    tracing_subscriber::fmt()
        .with_timer(tracing_subscriber::fmt::time::uptime())
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter)),
        )
        .init();
}

/// Build the default EnvFilter for CLI processes that talk to multiple vx crates.
pub fn cli_env_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(
            "warn,vx=info,vx_aether=info,vx_cass=info,vx_toolchain=info,vx_rhea=info",
        )
    })
}

/// Initialize tracing for CLI with plain stderr output (non-TUI mode).
pub fn init_cli_stderr() {
    tracing_subscriber::fmt()
        .with_timer(tracing_subscriber::fmt::time::uptime())
        .with_env_filter(cli_env_filter())
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_thread_ids(false)
        .init();
}
