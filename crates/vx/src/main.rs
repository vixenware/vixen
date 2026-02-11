//! vx - Build execution engine with deterministic caching
//!
//! This is a thin CLI client that talks to the vx daemon.

mod build_json;
mod tui;

use camino::Utf8PathBuf;
use eyre::{Result, bail};
use facet::Facet;
use figue as args;
use owo_colors::OwoColorize;
use tui::emit_osc_progress;
use vx_report::{CacheOutcome, ReportStore, RunDiff};
use vx_store_proto::Blake3Hash;
use vxd_proto::{ActionProgress, ActionType, BuildRequest, ProgressListener, VxdClient};

/// vx - Build execution engine with deterministic caching
#[derive(Facet, Debug)]
struct Cli {
    /// Command to run
    #[facet(args::subcommand)]
    command: CliCommand,

    /// Standard CLI options (--help, --version, --completions)
    #[facet(flatten)]
    builtins: args::FigueBuiltins,
}

/// Arguments for the `build` subcommand
#[derive(Facet, Debug)]
struct BuildArgs {
    /// Build in release mode
    #[facet(args::named, args::short = 'r')]
    release: bool,

    /// Don't auto-spawn daemon if not running (for tests)
    #[facet(args::named)]
    no_spawn: bool,

    /// Disable TUI and use plain log output
    #[facet(args::named)]
    no_tui: bool,

    /// Emit NDJSON build events to stdout (implies --no-tui)
    #[facet(args::named)]
    json: bool,
}

/// Start daemons
#[derive(Facet, Debug)]
struct StartArgs {}

/// Arguments for the `kill` subcommand
#[derive(Facet, Debug)]
struct KillArgs {}

/// Arguments for the `clean` subcommand
#[derive(Facet, Debug)]
struct CleanArgs {
    /// Remove VX_HOME instead of project .vx/ (asks for confirmation)
    #[facet(args::named)]
    home: bool,

    /// Invalidate cache entries from the last build (keeps toolchains)
    #[facet(args::named)]
    cache: bool,
}

/// Arguments for the `debug` subcommand
#[derive(Facet, Debug)]
struct DebugArgs {
    /// Print aggregated text dump to stderr (no web dashboard)
    #[facet(args::named)]
    dump: bool,

    /// Print aggregated JSON to stdout (no web dashboard)
    #[facet(args::named)]
    json: bool,
}

/// Arguments for the `doctor` subcommand
#[derive(Facet, Debug)]
struct DoctorArgs {}

/// Arguments for the `explain` subcommand
#[derive(Facet, Debug)]
struct ExplainArgs {
    /// Show all nodes including cache hits
    #[facet(args::named)]
    all: bool,

    /// Show verbose output (full hashes, unchanged deps)
    #[facet(args::named, args::short = 'v')]
    verbose: bool,

    /// Explain only this specific node ID
    #[facet(args::named)]
    node: Option<String>,

    /// Show only fanout analysis
    #[facet(args::named)]
    fanout: bool,

    /// Output in JSON format
    #[facet(args::named)]
    json: bool,

    /// Compare last two builds and show what changed
    #[facet(args::named)]
    diff: bool,

    /// Show details of the last cache miss
    #[facet(args::named)]
    last_miss: bool,

    /// Open interactive build graph in browser
    #[facet(args::named)]
    web: bool,
}

/// Options for displaying the explain report (internal)
#[derive(Debug, Clone, Copy)]
struct ReportDisplayOptions {
    /// Show all nodes including cache hits
    show_all: bool,

    /// Show verbose output (full hashes, unchanged deps)
    verbose: bool,

    /// Show only fanout analysis
    fanout_only: bool,
}

#[derive(Facet, Debug)]
#[repr(u8)]
enum CliCommand {
    /// Execute a build
    Build(BuildArgs),

    /// Just start daemons
    Start(StartArgs),

    /// Stop the daemon process
    Kill(KillArgs),

    /// Stop the daemon and remove .vx/ directory
    Clean(CleanArgs),

    /// Explain the last build
    Explain(ExplainArgs),

    /// Quick health check
    Doctor(DoctorArgs),

    /// Collect diagnostic dumps from all vixen processes
    Debug(DebugArgs),
}

/// Initialize tracing with TUI layer
fn init_tracing_with_tui(tui: tui::TuiHandle) {
    use tracing_subscriber::prelude::*;

    tracing_subscriber::registry()
        .with(vx_tracing::cli_env_filter())
        .with(tui::TuiLayer::new(tui))
        .init();
}

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    match rt.block_on(async_main()) {
        Ok(()) => {}
        Err(e) => {
            // Build failures already call process::exit(1) directly,
            // so any error reaching here is an internal error (exit code 2).
            eprintln!("vx: {e:#}");
            std::process::exit(2);
        }
    }
}

async fn async_main() -> Result<()> {
    let cli: Cli = args::Driver::new(
        args::builder::<Cli>()?
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
        // Build command gets TUI-based tracing (initialized in cmd_build)
        CliCommand::Build(args) => cmd_build(args).await,
        CliCommand::Start(args) => {
            vx_tracing::init_cli_stderr();
            cmd_start(args).await
        }
        // Other commands use stderr tracing
        CliCommand::Kill(args) => {
            vx_tracing::init_cli_stderr();
            cmd_kill(args).await
        }
        CliCommand::Clean(args) => {
            vx_tracing::init_cli_stderr();
            cmd_clean(args).await
        }
        CliCommand::Explain(args) => {
            vx_tracing::init_cli_stderr();
            cmd_explain(args)
        }
        CliCommand::Doctor(_args) => {
            vx_tracing::init_cli_stderr();
            cmd_doctor().await
        }
        CliCommand::Debug(args) => {
            vx_tracing::init_cli_stderr();
            cmd_debug(args).await
        }
    }
}

/// Display a hash in short form (first 8 hex chars) unless verbose
fn short_hex(hash: &str, verbose: bool) -> String {
    if verbose || hash.len() <= 8 {
        hash.to_string()
    } else {
        format!("{}…", &hash[..8])
    }
}

/// Progress listener that drives the TUI (or logs to console if TUI is disabled).
/// When `json` is true, emits NDJSON events to stdout instead.
/// In non-JSON modes, OSC 9;4 terminal progress sequences are emitted.
struct ProgressListenerImpl {
    tui: Option<tui::TuiHandle>,
    json: bool,
    next_action_id: std::sync::atomic::AtomicU64,
    /// Tracks total/completed for OSC progress in no-tui mode
    total_actions: std::sync::atomic::AtomicU64,
    completed_actions: std::sync::atomic::AtomicU64,
}

type VxdCaller = roam_stream::Client<
    vx_io::net::EndpointConnector,
    vxd_proto::ProgressListenerDispatcher<ProgressListenerImpl>,
>;

impl Clone for ProgressListenerImpl {
    fn clone(&self) -> Self {
        Self {
            tui: self.tui.clone(),
            json: self.json,
            next_action_id: std::sync::atomic::AtomicU64::new(
                self.next_action_id
                    .load(std::sync::atomic::Ordering::SeqCst),
            ),
            total_actions: std::sync::atomic::AtomicU64::new(
                self.total_actions.load(std::sync::atomic::Ordering::SeqCst),
            ),
            completed_actions: std::sync::atomic::AtomicU64::new(
                self.completed_actions
                    .load(std::sync::atomic::Ordering::SeqCst),
            ),
        }
    }
}

impl ProgressListenerImpl {
    fn new(tui: Option<tui::TuiHandle>, json: bool) -> Self {
        Self {
            tui,
            json,
            next_action_id: std::sync::atomic::AtomicU64::new(1),
            total_actions: std::sync::atomic::AtomicU64::new(0),
            completed_actions: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

impl ProgressListener for ProgressListenerImpl {
    async fn set_total(&self, _cx: &roam::Context, total: u64) {
        if self.json {
            build_json::emit(&build_json::BuildStarted {
                event: "build_started".into(),
                total_actions: total,
            });
        } else if let Some(tui) = &self.tui {
            tui.set_total(total as usize).await;
        } else {
            self.total_actions
                .store(total, std::sync::atomic::Ordering::SeqCst);
            // Emit OSC progress (0%) now that we know the total
            emit_osc_progress(tui::OscProgressState::Set, Some(0));
            tracing::info!("Build: {} actions total", total);
        }
    }

    async fn start_action(&self, _cx: &roam::Context, action_type: ActionType) -> u64 {
        if self.json {
            let id = self
                .next_action_id
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            build_json::emit(&build_json::ActionStarted {
                event: "action_started".into(),
                action_id: id,
                action: action_type.display_name(),
            });
            id
        } else if let Some(tui) = &self.tui {
            tui.start_action(action_type).await
        } else {
            let id = self
                .next_action_id
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tracing::info!("[{}] Starting: {:?}", id, action_type);
            id
        }
    }

    async fn update_progress(&self, _cx: &roam::Context, action_id: u64, progress: ActionProgress) {
        if self.json {
            build_json::emit(&build_json::ActionProgress {
                event: "action_progress".into(),
                action_id,
                message: progress.message,
                current: progress.current,
                total: progress.total,
            });
        } else if let Some(tui) = &self.tui {
            tui.update_progress(action_id, progress).await;
        } else if let (Some(current), Some(total)) = (progress.current, progress.total) {
            let pct = if total > 0 {
                current as f64 / total as f64 * 100.0
            } else {
                0.0
            };
            tracing::info!(
                "[{}] {}: {:.0}% ({}/{})",
                action_id,
                progress.message,
                pct,
                current,
                total
            );
        } else {
            tracing::info!("[{}] {}", action_id, progress.message);
        }
    }

    async fn complete_action(&self, _cx: &roam::Context, action_id: u64) {
        if self.json {
            build_json::emit(&build_json::ActionCompleted {
                event: "action_completed".into(),
                action_id,
            });
        } else if let Some(tui) = &self.tui {
            tui.complete_action(action_id).await;
        } else {
            let completed = self
                .completed_actions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            let total = self.total_actions.load(std::sync::atomic::Ordering::SeqCst);
            if total > 0 {
                let pct = (completed as f64 / total as f64 * 100.0).min(100.0) as u8;
                emit_osc_progress(tui::OscProgressState::Set, Some(pct));
            }
            tracing::info!("[{}] Complete", action_id);
        }
    }

    async fn log_message(&self, _cx: &roam::Context, message: String) {
        if self.json {
            build_json::emit(&build_json::LogMessage {
                event: "log".into(),
                message,
            });
        } else if let Some(tui) = &self.tui {
            tui.log_message(message).await;
        } else {
            tracing::info!("{}", message);
        }
    }
}

/// Connect to the daemon, spawning it if necessary.
///
/// The TUI and progress dispatcher are created here because they're
/// tightly coupled to the vx CLI's display logic.
async fn get_or_spawn_daemon(
    no_spawn: bool,
    no_tui: bool,
    json: bool,
) -> Result<(VxdClient<VxdCaller>, Option<tui::TuiHandle>)> {
    use vx_io::net::Endpoint;
    use vx_io::spawn::ServiceBuilder;

    let vx_home = std::env::var("VX_HOME")
        .unwrap_or_else(|_| format!("{}/.vx", std::env::var("HOME").expect("HOME not set")));
    let vx_home = camino::Utf8PathBuf::from(&vx_home);

    // Parse endpoint (defaults to Unix socket in VX_HOME)
    let endpoint = match std::env::var("VXD") {
        Ok(v) => Endpoint::parse(&v)?,
        Err(_) => {
            #[cfg(unix)]
            {
                vx_io::net::default_unix_endpoint(&vx_home, "vxd")
            }
            #[cfg(not(unix))]
            {
                Endpoint::parse("127.0.0.1:9001")?
            }
        }
    };

    // Create TUI and progress listener
    let tui = if no_tui {
        None
    } else {
        Some(tui::TuiHandle::new())
    };
    let progress_listener = ProgressListenerImpl::new(tui.clone(), json);
    let dispatcher = vxd_proto::ProgressListenerDispatcher::new(progress_listener);

    let mut builder = ServiceBuilder::new("vxd", endpoint.clone(), dispatcher)
        .caller_name("vx")
        .env("VX_HOME", vx_home.as_str())
        .log_path(vx_home.join("vxd.log"));

    if no_spawn || std::env::var("VXD").is_ok() {
        builder = builder.connect_only();
    } else if !endpoint.is_local() {
        // Shut down TUI before bailing
        if let Some(tui) = tui {
            tui.shutdown().await;
        }
        eyre::bail!(
            "failed to connect to vxd at {}.\n\
            Auto-spawn is only supported for local endpoints.\n\
            Start vxd on the remote host, or point VXD to a local endpoint.",
            endpoint
        );
    }

    match builder.connect().await {
        Ok(handle) => {
            let client = VxdClient::new(handle.caller);
            Ok((client, tui))
        }
        Err(e) => {
            if let Some(tui) = tui {
                tui.shutdown().await;
            }
            Err(e)
        }
    }
}

async fn cmd_build(args: BuildArgs) -> Result<()> {
    let cwd = Utf8PathBuf::try_from(std::env::current_dir()?)?;
    let json_mode = args.json;
    let no_tui = args.no_tui || json_mode;

    // Emit indeterminate OSC 9;4 progress while connecting to daemon
    // (TUI mode emits this from its display task; no-tui mode needs it here)
    // Skip in JSON mode — CI doesn't need terminal progress sequences.
    if no_tui && !json_mode {
        emit_osc_progress(tui::OscProgressState::Indeterminate, None);
    }

    // Connect to daemon (spawning if necessary) and set up TUI
    let (daemon, tui) = get_or_spawn_daemon(args.no_spawn, no_tui, json_mode).await?;

    // Initialize tracing - with TUI layer, or plain output to stderr.
    // In JSON mode, tracing goes to stderr so stdout stays clean NDJSON.
    if let Some(ref tui) = tui {
        init_tracing_with_tui(tui.clone());
    } else {
        vx_tracing::init_cli_stderr();
    }

    // When running under the test harness, set the session ID so cache keys
    // are namespaced per-test (isolating parallel tests from each other).
    if let Ok(session_id) = std::env::var("VX_TEST_SESSION") {
        daemon.set_session(session_id).await?;
    }

    let request = BuildRequest {
        project_path: cwd.clone(),
        release: args.release,
    };

    // Race the build against Ctrl-C. When the user hits Ctrl-C, we shut down
    // the TUI cleanly and exit. The daemon detects the dropped connection and
    // cancels the in-flight build on its own.
    let result = tokio::select! {
        result = daemon.build(request) => result?,
        _ = tokio::signal::ctrl_c() => {
            if let Some(tui) = tui {
                tui.shutdown().await;
            }
            // Clear terminal progress on interrupt
            emit_osc_progress(tui::OscProgressState::Remove, None);
            eprintln!("Build interrupted");
            std::process::exit(130);
        }
    };

    // Signal the build outcome and shutdown the TUI
    if let Some(tui) = tui {
        tui.set_build_outcome(result.success).await;
        tui.shutdown().await;
    } else if !json_mode {
        // No TUI — emit final OSC 9;4 state directly
        // (skip in JSON mode — CI doesn't need terminal progress sequences)
        if result.success {
            emit_osc_progress(tui::OscProgressState::Remove, None);
        } else {
            emit_osc_progress(tui::OscProgressState::Error, None);
        }
    }

    // JSON mode: emit BuildFinished event and exit with appropriate code
    if json_mode {
        build_json::emit(&build_json::BuildFinished {
            event: "build_finished".into(),
            success: result.success,
            cached: result.cached,
            duration_ms: result.duration_ms,
            total_actions: result.total_actions as u64,
            cache_hits: result.cache_hits as u64,
            rebuilt: result.rebuilt as u64,
            cancelled: result.cancelled as u64,
            output_path: result.output_path.map(|p| p.to_string()),
            error: result.error.clone(),
        });

        if !result.success {
            // Emit GitHub Actions annotation to stderr
            if let Some(ref error) = result.error
                && let Some(first_line) = error.lines().next()
            {
                eprintln!("::error::{first_line}");
            }
            std::process::exit(1);
        }

        return Ok(());
    }

    if result.success {
        // Print nice summary box (TUI has already cleaned up)
        println!("─────────────────────────────────────────────────────────────");
        println!("{} Build Complete", "✓".green().bold());
        println!("─────────────────────────────────────────────────────────────");

        if result.cached {
            println!(
                "{} {}",
                "Status:".dimmed(),
                "Cached (no rebuild needed)".cyan()
            );
        } else {
            println!("{} {}", "Status:".dimmed(), "Success".green());
        }

        // Format duration nicely
        let duration_str = if result.duration_ms < 1000 {
            format!("{}ms", result.duration_ms)
        } else {
            format!("{:.2}s", result.duration_ms as f64 / 1000.0)
        };
        println!("{} {}", "Duration:".dimmed(), duration_str.yellow());

        // Show action stats
        let executed = result
            .total_actions
            .saturating_sub(result.cache_hits)
            .saturating_sub(result.cancelled);
        if result.cancelled > 0 {
            println!(
                "{} {} total, {} cached, {} executed, {} cancelled",
                "Actions:".dimmed(),
                result.total_actions.to_string().yellow(),
                result.cache_hits.to_string().cyan(),
                executed.to_string().green(),
                result.cancelled.to_string().red()
            );
        } else {
            println!(
                "{} {} total, {} cached, {} executed",
                "Actions:".dimmed(),
                result.total_actions.to_string().yellow(),
                result.cache_hits.to_string().cyan(),
                executed.to_string().green()
            );
        }

        if let Some(output_path) = &result.output_path {
            // Make path relative to cwd if possible
            let display_path = if let Ok(rel) = output_path.strip_prefix(&cwd) {
                rel.to_string()
            } else {
                output_path.to_string()
            };
            println!("{} {}", "Binary:".dimmed(), display_path.cyan());
        }

        println!("{} {}", "Project:".dimmed(), cwd.to_string().dimmed());
        println!("─────────────────────────────────────────────────────────────");

        Ok(())
    } else {
        if let Some(error) = &result.error {
            // Error already contains formatted diagnostic, just print and exit
            eprintln!("{}", error);
            std::process::exit(1);
        }
        bail!("{}", result.message)
    }
}

async fn cmd_start(_args: StartArgs) -> Result<()> {
    let (daemon, _tui) = get_or_spawn_daemon(false, true, false).await?;

    tracing::info!("{} daemon is running", "✓".green().bold());

    // Ping to verify connection
    let version = daemon.version().await?;

    tracing::info!("{} connected to vxd", "✓".green().bold());
    tracing::info!("   {}", version);

    Ok(())
}

async fn cmd_kill(_args: KillArgs) -> Result<()> {
    use vx_io::net::Endpoint;

    let endpoint = match std::env::var("VXD") {
        Ok(v) => Endpoint::parse(&v)?,
        Err(_) => {
            #[cfg(unix)]
            {
                let vx_home = std::env::var("VX_HOME").unwrap_or_else(|_| {
                    format!("{}/.vx", std::env::var("HOME").expect("HOME not set"))
                });
                let vx_home = camino::Utf8PathBuf::from(&vx_home);
                vx_io::net::default_unix_endpoint(&vx_home, "vxd")
            }
            #[cfg(not(unix))]
            {
                Endpoint::parse("127.0.0.1:9001")?
            }
        }
    };

    // Try graceful shutdown via RPC first
    let kill_result = {
        use vx_io::spawn::ServiceBuilder;
        let progress_listener = ProgressListenerImpl::new(None, false);
        let dispatcher = vxd_proto::ProgressListenerDispatcher::new(progress_listener);
        ServiceBuilder::new("vxd", endpoint.clone(), dispatcher)
            .caller_name("vx")
            .connect_only()
            .connect()
            .await
    };
    match kill_result {
        Ok(handle) => {
            let daemon = VxdClient::new(handle.caller);
            tracing::info!("{} daemon at {}", "Stopping".green().bold(), endpoint);
            if let Err(e) = daemon.shutdown().await {
                // shutdown() calls process::exit(0) on the daemon side, so the
                // connection closing is expected — not an error.
                tracing::debug!("shutdown RPC returned: {e} (expected — daemon exited)");
            }
            // Give it a moment to finish cleanup
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            tracing::info!("{} daemon stopped", "✓".green().bold());
        }
        Err(_) => {
            tracing::info!(
                "{} daemon not reachable (child processes use die-with-parent, so they'll exit on their own)",
                "Note:".yellow().bold()
            );
        }
    }

    Ok(())
}

async fn cmd_clean(args: CleanArgs) -> Result<()> {
    if args.home && args.cache {
        bail!("--home and --cache are mutually exclusive");
    }

    let vx_home = std::env::var("VX_HOME")
        .unwrap_or_else(|_| format!("{}/.vx", std::env::var("HOME").expect("HOME not set")));
    let vx_home = Utf8PathBuf::from(&vx_home);

    if args.cache {
        return cmd_clean_cache(&vx_home).await;
    }

    // Determine which directory to remove
    let vx_dir = if args.home {
        // Remove VX_HOME (~/.vx)
        vx_home.clone()
    } else {
        // Remove project .vx/
        let cwd = Utf8PathBuf::try_from(std::env::current_dir()?)?;
        cwd.join(".vx")
    };

    if args.home {
        use std::io::{self, Write};

        print!("Remove VX_HOME at {}? [y/N] ", vx_dir);
        io::stdout().flush()?;

        let mut response = String::new();
        io::stdin().read_line(&mut response)?;
        let confirmed = matches!(response.trim().to_ascii_lowercase().as_str(), "y" | "yes");
        if !confirmed {
            println!("{}", "Aborted.".yellow().bold());
            return Ok(());
        }
    }

    // Then remove the directory if it exists
    if vx_dir.exists() {
        std::fs::remove_dir_all(&vx_dir)?;
        println!("{} {}", "Removed".green().bold(), vx_dir);
    } else {
        println!("{} {} does not exist", "Note:".yellow().bold(), vx_dir);
    }

    Ok(())
}

/// Returns true if this node kind represents infrastructure (toolchains, registry)
/// that should be preserved during cache invalidation.
fn is_infrastructure_node(kind: &str) -> bool {
    kind.starts_with("toolchain.")
        || kind.starts_with("zig.toolchain.")
        || kind == "registry.acquire"
}

/// Invalidate cache entries from the last build while preserving toolchains
/// and registry crates.
async fn cmd_clean_cache(vx_home: &Utf8PathBuf) -> Result<()> {
    let cwd = Utf8PathBuf::try_from(std::env::current_dir()?)?;
    let report_store = ReportStore::new(&cwd);

    let Some(report) = report_store.load_latest()? else {
        println!(
            "{} No build reports found. Run {} first.",
            "Error:".red().bold(),
            "vx build".cyan()
        );
        return Ok(());
    };

    // Collect cache keys for non-infrastructure nodes
    let mut cache_keys = Vec::new();
    let mut skipped = 0u32;

    for node in &report.nodes {
        if is_infrastructure_node(&node.kind) {
            skipped += 1;
            continue;
        }

        // Use cas_cache_key (the actual store cache key) when available,
        // fall back to cache_key for older reports
        let hex = node.cas_cache_key.as_deref().unwrap_or(&node.cache_key);
        if let Some(key) = Blake3Hash::from_hex(hex) {
            cache_keys.push(key);
        }
    }

    if cache_keys.is_empty() {
        println!(
            "{} No cache entries to invalidate.",
            "Note:".yellow().bold()
        );
        return Ok(());
    }

    // Connect to daemon (spawns it if needed), unpublish via RPC
    let (daemon, _tui) = get_or_spawn_daemon(false, true, false).await?;

    let result = daemon.unpublish_cache_keys(cache_keys).await?;

    // Clear picante cache so the daemon recomputes everything on next build
    let picante_cache = vx_home.join("picante.cache");
    let picante_wal = vx_home.join("picante.wal");
    for path in [&picante_cache, &picante_wal] {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }

    println!(
        "{} Invalidated {} cache entries ({} infrastructure preserved, {} already absent)",
        "Done.".green().bold(),
        result.removed,
        skipped,
        result.not_found,
    );
    if result.removed > 0 {
        println!(
            "{}",
            "Next build will re-execute these nodes from scratch.".dimmed()
        );
    }

    Ok(())
}

async fn cmd_doctor() -> Result<()> {
    // Daemon status — check if vxd socket exists
    let vx_home = std::env::var("VX_HOME")
        .unwrap_or_else(|_| format!("{}/.vx", std::env::var("HOME").expect("HOME not set")));
    println!("{} {}", "home:".bold(), vx_home);
    let daemon_sock = std::path::PathBuf::from(&vx_home).join("vxd.sock");
    if daemon_sock.exists() {
        println!(
            "{} {} (socket exists at {})",
            "daemon:".bold(),
            "likely running".green(),
            daemon_sock.display(),
        );
    } else {
        println!(
            "{} {} (will auto-spawn on next build)",
            "daemon:".bold(),
            "not running".dimmed(),
        );
    }

    #[cfg(target_os = "macos")]
    {
        let app_installed = vx_vfs::is_app_installed().await;
        let health = vx_vfs::check_extension_health().await;

        if matches!(health.as_ref(), Some(h) if h.extension_enabled) {
            println!("{} {}", "filesystem extension:".bold(), "enabled".green());
        } else {
            println!(
                "{} {}",
                "filesystem extension:".bold(),
                "not ready".yellow()
            );

            if app_installed {
                match std::process::Command::new("open")
                    .arg(vx_vfs::VIXEN_APP_PATH)
                    .status()
                {
                    Ok(status) if status.success() => {
                        println!(
                            "{} Opened Vixen.app to guide filesystem extension setup.",
                            "note:".bold()
                        );
                    }
                    Ok(status) => {
                        println!(
                            "{} Failed to open Vixen.app automatically (exit {}).",
                            "note:".bold(),
                            status
                        );
                    }
                    Err(error) => {
                        println!(
                            "{} Failed to open Vixen.app automatically: {}",
                            "note:".bold(),
                            error
                        );
                    }
                }
            } else {
                println!(
                    "{} {} is missing; install it first.",
                    "note:".bold(),
                    vx_vfs::VIXEN_APP_PATH
                );
            }
        }
    }

    Ok(())
}
async fn cmd_debug(args: DebugArgs) -> Result<()> {
    // 1. Find vixen process PIDs
    let pids = find_vixen_pids()?;
    if pids.is_empty() {
        eprintln!("No vixen processes found. Is the daemon running?");
        eprintln!("Start it with: vx build");
        std::process::exit(1);
    }

    eprintln!(
        "Found {} vixen process(es): {}",
        pids.len(),
        pids.iter()
            .map(|(name, pid)| format!("{name}({pid})"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // 2. Clean stale dumps, send SIGUSR1 to each
    peeps::clean_dumps();
    std::fs::create_dir_all(peeps::DUMP_DIR)?;

    for (_name, pid) in &pids {
        unsafe {
            libc::kill(*pid as i32, libc::SIGUSR1);
        }
    }

    // 3. Poll for JSON dumps to appear
    let expected = pids.len();
    let mut collected = 0;
    for _ in 0..20 {
        // 100ms intervals, 2s max
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        collected = count_dump_files();
        if collected >= expected {
            break;
        }
    }

    if collected == 0 {
        eprintln!("No dumps collected. Processes may not have the dump handler yet.");
        std::process::exit(1);
    }

    if collected < expected {
        eprintln!(
            "Warning: only collected {collected}/{expected} dumps (some processes may lack the handler)"
        );
    }

    // 4. Read all dumps
    let dumps = peeps::read_all_dumps();

    if args.json {
        // --json: raw JSON array to stdout
        let json = facet_json::to_string(&dumps)?;
        println!("{json}");
        return Ok(());
    }

    if args.dump {
        // --dump: human-readable text to stderr
        print_text_dump(&dumps);
        return Ok(());
    }

    // Default: serve web dashboard
    peeps::serve_dashboard(dumps).await?;
    Ok(())
}

/// Find running vixen processes via pgrep.
fn find_vixen_pids() -> Result<Vec<(String, u32)>> {
    let output = std::process::Command::new("pgrep")
        .args(["-l", "-f", "vxd|vx-store|vx-runner|vx-vfsd"])
        .output()?;

    let mut pids = Vec::new();
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        if parts.len() == 2
            && let Ok(pid) = parts[0].parse::<u32>()
        {
            let name = parts[1].to_string();
            // Filter out ourselves and grep itself
            if !name.contains("pgrep") && !name.contains("vx debug") {
                pids.push((name, pid));
            }
        }
    }
    Ok(pids)
}

/// Count JSON files in the dump directory.
fn count_dump_files() -> usize {
    let dir = std::path::Path::new(peeps::DUMP_DIR);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .count()
}

/// Print a human-readable text dump of all process diagnostics.
fn print_text_dump(dumps: &[peeps::ProcessDump]) {
    for dump in dumps {
        eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        eprintln!("Process: {} (pid {})", dump.process_name, dump.pid);
        eprintln!("Timestamp: {}", dump.timestamp);
        eprintln!();

        // Roam connections
        if let Some(ref roam) = dump.roam {
            if roam.connections.is_empty() {
                eprintln!("  Connections: (none)");
            } else {
                eprintln!("  Connections ({}):", roam.connections.len());
                for conn in &roam.connections {
                    let peer = conn.peer_name.as_deref().unwrap_or("unknown");
                    eprintln!(
                        "    {} ↔ {} (age {:.1}s, {} completed, {} in-flight)",
                        conn.name,
                        peer,
                        conn.age_secs,
                        conn.total_completed,
                        conn.in_flight.len()
                    );

                    for req in &conn.in_flight {
                        let method = req.method_name.as_deref().unwrap_or("?");
                        let dir = match req.direction {
                            peeps::types::Direction::Outgoing => "→",
                            peeps::types::Direction::Incoming => "←",
                        };
                        eprintln!(
                            "      {dir} {method} (id={}, {:.3}s)",
                            req.request_id, req.elapsed_secs
                        );
                    }
                }
            }
        }

        // SHM
        if let Some(shm) = &dump.shm {
            eprintln!();
            eprintln!("  SHM segments ({}):", shm.segments.len());
            for seg in &shm.segments {
                let path = seg.segment_path.as_deref().unwrap_or("?");
                eprintln!(
                    "    {} ({}/{} bytes, {} peers)",
                    path,
                    seg.current_size,
                    seg.total_size,
                    seg.peers.len()
                );
            }
        }

        // Locks
        if let Some(locks) = &dump.locks {
            eprintln!();
            eprintln!("  Locks ({}):", locks.locks.len());
            for lock in &locks.locks {
                eprintln!(
                    "    \"{}\" (acquires={}, releases={}, {} holders, {} waiters)",
                    lock.name,
                    lock.acquires,
                    lock.releases,
                    lock.holders.len(),
                    lock.waiters.len()
                );
            }
        }

        // Thread stacks
        if !dump.threads.is_empty() {
            eprintln!();
            eprintln!("  Threads ({}):", dump.threads.len());
            for t in &dump.threads {
                let status = if t.responded == 0 {
                    "UNRESPONSIVE"
                } else if t.samples > 1
                    && t.same_location_count as f64 / (t.samples - 1) as f64 >= 0.7
                {
                    "STUCK"
                } else {
                    "active"
                };
                eprintln!(
                    "    {} [{status}] (responded {}/{}, same_loc {}/{})",
                    t.name,
                    t.responded,
                    t.samples,
                    t.same_location_count,
                    t.samples.saturating_sub(1)
                );
                if let Some(ref frame) = t.dominant_frame {
                    eprintln!("      dominant: {frame}");
                }
            }
        }

        // Custom
        if !dump.custom.is_empty() {
            eprintln!();
            eprintln!("  Custom:");
            for (k, v) in &dump.custom {
                eprintln!("    {k}: {v}");
            }
        }

        eprintln!();
    }
}

fn cmd_explain(args: ExplainArgs) -> Result<()> {
    let cwd = Utf8PathBuf::try_from(std::env::current_dir()?)?;
    let store = ReportStore::new(&cwd);

    // Load the latest report
    let Some(report) = store.load_latest()? else {
        println!(
            "{} No build reports found. Run {} first.",
            "Error:".red().bold(),
            "vx build".cyan()
        );
        return Ok(());
    };

    // Handle --web: open interactive graph in browser
    if args.web {
        return cmd_explain_web(&report);
    }

    // Handle --diff: compare last two builds
    if args.diff {
        let Some(prev_report) = store.load_previous()? else {
            println!(
                "{} Need at least two builds to show diff. Run {} again.",
                "Error:".red().bold(),
                "vx build".cyan()
            );
            return Ok(());
        };
        return cmd_explain_diff(&prev_report, &report);
    }

    // Handle --last-miss: show details of the last cache miss
    if args.last_miss {
        return cmd_explain_last_miss(&report);
    }

    // Load previous report for diffing deps
    let prev_report = store.load_previous().ok().flatten();

    if args.json {
        cmd_explain_json(&report, prev_report.as_ref(), args.node.clone())
    } else {
        let options = ReportDisplayOptions {
            show_all: args.all,
            verbose: args.verbose,
            fanout_only: args.fanout,
        };
        cmd_explain_report(&report, prev_report.as_ref(), args.node.clone(), options)
    }
}

fn cmd_explain_report(
    report: &vx_report::BuildReport,
    prev_report: Option<&vx_report::BuildReport>,
    node_filter: Option<String>,
    options: ReportDisplayOptions,
) -> Result<()> {
    use std::collections::HashMap;

    // =========================================================================
    // 1. RUN HEADER
    // =========================================================================
    let status = if report.success {
        "SUCCESS".green().bold().to_string()
    } else {
        "FAILED".red().bold().to_string()
    };

    let duration_s = report.duration_ms() as f64 / 1000.0;
    println!(
        "{} {} in {:.2}s",
        "Build:".bold(),
        status,
        duration_s.to_string().dimmed()
    );

    println!("  {} {}", "Workspace:".dimmed(), report.workspace_root);
    println!("  {} {}", "Profile:".dimmed(), report.profile);
    println!("  {} {}", "Target:".dimmed(), report.target_triple);

    // Show toolchains
    if let Some(ref rust_tc) = report.toolchains.rust {
        let version_str = rust_tc.version.as_deref().unwrap_or("unknown");
        let id_short = short_hex(&rust_tc.id, options.verbose);
        println!("  {} {} ({})", "Rust:".dimmed(), version_str, id_short);
    }
    if let Some(ref zig_tc) = report.toolchains.zig {
        let version_str = zig_tc.version.as_deref().unwrap_or("unknown");
        let id_short = short_hex(&zig_tc.id, options.verbose);
        println!("  {} {} ({})", "Zig:".dimmed(), version_str, id_short);
    }

    println!();

    // =========================================================================
    // 2. NODE LIST
    // =========================================================================

    // Index previous run's nodes by node_id for dep diffing
    let prev_nodes: HashMap<&str, &vx_report::NodeReport> = prev_report
        .map(|r| r.nodes.iter().map(|n| (n.node_id.as_str(), n)).collect())
        .unwrap_or_default();

    // Handle fanout-only mode
    if options.fanout_only {
        print_fanout_analysis(report, options.verbose);
        return Ok(());
    }

    // Separate nodes by outcome
    let mut failures = Vec::new();
    let mut misses = Vec::new();
    let mut hits = Vec::new();

    for node in &report.nodes {
        // Apply node filter if specified
        if let Some(ref filter) = node_filter
            && &node.node_id != filter
        {
            continue;
        }

        match &node.cache {
            CacheOutcome::Miss { .. } => {
                if node
                    .invocation
                    .as_ref()
                    .map(|i| i.exit_code != 0)
                    .unwrap_or(false)
                {
                    failures.push(node);
                } else {
                    misses.push(node);
                }
            }
            CacheOutcome::Hit { .. } => hits.push(node),
        }
    }

    // Print failures first
    if !failures.is_empty() {
        println!("{}", "Failures:".red().bold());
        for node in &failures {
            print_node(node, None, options.verbose);
        }
        println!();
    }

    // Print misses with explanations
    if !misses.is_empty() {
        println!("{}", "=== WHY REBUILT ===".yellow().bold());
        println!();
        for node in &misses {
            let prev_node = prev_nodes.get(node.node_id.as_str()).copied();
            print_node_explanation(node, prev_node, options.verbose);
        }
        println!();
    }

    // Compute and print fanout
    if !misses.is_empty() {
        print_fanout_analysis(report, options.verbose);
    }

    // Summary of hits (unless --all)
    if !options.show_all && !hits.is_empty() {
        println!("{} {} cached", "Cache hits:".green().bold(), hits.len());
    } else if options.show_all && !hits.is_empty() {
        println!("{}", "Cache hits:".green().bold());
        for node in &hits {
            print_node(node, None, options.verbose);
        }
    }

    // Error message if failed
    if let Some(error) = &report.error {
        println!();
        println!("{}", "Error:".red().bold());
        for line in error.lines().take(10) {
            println!("  {}", line);
        }
        if error.lines().count() > 10 {
            println!(
                "  {} ({} more lines)",
                "...".dimmed(),
                error.lines().count() - 10
            );
        }
    }

    Ok(())
}

/// Print a node explanation using Lane 2 analysis
fn print_node_explanation(
    node: &vx_report::NodeReport,
    prev_node: Option<&vx_report::NodeReport>,
    verbose: bool,
) {
    use vx_report::NodeRebuildExplanation;

    // Compute explanation
    let Some(explanation) = NodeRebuildExplanation::compute(node, prev_node) else {
        // Cache hit - shouldn't happen in this context
        return;
    };

    let duration_ms = node.timing.execute_ms;
    let duration_str = if duration_ms >= 1000 {
        format!("{:.2}s", duration_ms as f64 / 1000.0)
    } else {
        format!("{}ms", duration_ms)
    };

    // Print node header with observed reason
    println!(
        "  {} ({}) {}",
        node.node_id.cyan().bold(),
        explanation.observed_reason.to_string().yellow(),
        duration_str.dimmed()
    );

    // Print input changes
    if !explanation.input_changes.is_empty() {
        println!("    {}:", "Inputs".dimmed());
        for change in &explanation.input_changes {
            match (&change.old_value, &change.new_value) {
                (Some(old), Some(new)) => {
                    let old_short = short_hex(old, verbose);
                    let new_short = short_hex(new, verbose);
                    println!(
                        "      {} {} → {}",
                        change.label,
                        old_short.dimmed(),
                        new_short
                    );
                }
                (None, Some(new)) => {
                    let new_short = short_hex(new, verbose);
                    println!("      {} {} (added)", change.label, new_short);
                }
                (Some(old), None) => {
                    let old_short = short_hex(old, verbose);
                    println!("      {} {} (removed)", change.label, old_short.dimmed());
                }
                (None, None) => {}
            }
        }
    }

    // Print dependency changes
    if !explanation.dep_changes.is_empty() {
        println!("    {}:", "Dependencies".dimmed());
        for change in &explanation.dep_changes {
            use vx_report::DepChangeKind;

            let crate_id_short = short_hex(&change.crate_id, verbose);

            match &change.change {
                DepChangeKind::Added {
                    rlib_hash,
                    manifest_hash,
                } => {
                    let rlib_short = short_hex(rlib_hash, verbose);
                    let manifest_short = short_hex(manifest_hash, verbose);
                    println!(
                        "      {} (crate {}): rlib {} (manifest {})",
                        change.extern_name.cyan(),
                        crate_id_short.dimmed(),
                        rlib_short,
                        manifest_short.dimmed()
                    );
                }
                DepChangeKind::Removed {
                    rlib_hash,
                    manifest_hash,
                } => {
                    let rlib_short = short_hex(rlib_hash, verbose);
                    let manifest_short = short_hex(manifest_hash, verbose);
                    println!(
                        "      {} (crate {}): {} (removed)",
                        change.extern_name.cyan(),
                        crate_id_short.dimmed(),
                        format!("rlib {} manifest {}", rlib_short, manifest_short).dimmed()
                    );
                }
                DepChangeKind::RlibChanged {
                    old_rlib,
                    new_rlib,
                    old_manifest,
                    new_manifest,
                } => {
                    let old_rlib_short = short_hex(old_rlib, verbose);
                    let new_rlib_short = short_hex(new_rlib, verbose);
                    let old_manifest_short = short_hex(old_manifest, verbose);
                    let new_manifest_short = short_hex(new_manifest, verbose);

                    print!(
                        "      {} (crate {}): rlib {} → {}",
                        change.extern_name.cyan(),
                        crate_id_short.dimmed(),
                        old_rlib_short.dimmed(),
                        new_rlib_short
                    );
                    if verbose || old_manifest != new_manifest {
                        print!(
                            " (manifest {} → {})",
                            old_manifest_short.dimmed(),
                            new_manifest_short
                        );
                    }
                    println!();
                }
                DepChangeKind::ManifestChanged {
                    rlib_hash,
                    old_manifest,
                    new_manifest,
                } => {
                    let rlib_short = short_hex(rlib_hash, verbose);
                    let old_manifest_short = short_hex(old_manifest, verbose);
                    let new_manifest_short = short_hex(new_manifest, verbose);
                    println!(
                        "      {} (crate {}): rlib {} manifest {} → {}",
                        change.extern_name.cyan(),
                        crate_id_short.dimmed(),
                        rlib_short.dimmed(),
                        old_manifest_short.dimmed(),
                        new_manifest_short
                    );
                }
            }
        }
    }

    // If verbose, also show the reported reason from daemon
    if verbose {
        println!(
            "    {} {}",
            "Reported reason:".dimmed(),
            explanation.reported_reason.to_string().dimmed()
        );
    }

    println!();
}

/// Print the old-style node display (for cache hits and failures)
fn print_node(
    node: &vx_report::NodeReport,
    _prev_node: Option<&vx_report::NodeReport>,
    verbose: bool,
) {
    let outcome_str = match &node.cache {
        CacheOutcome::Hit { .. } => "HIT ".green().to_string(),
        CacheOutcome::Miss { reason } => format!("MISS ({})", reason).yellow().to_string(),
    };

    let duration_ms = node.timing.execute_ms;
    let duration_str = if duration_ms >= 1000 {
        format!("{:.2}s", duration_ms as f64 / 1000.0)
    } else {
        format!("{}ms", duration_ms)
    };

    println!(
        "  {} {:<15} {}",
        node.node_id,
        outcome_str,
        duration_str.dimmed()
    );

    if verbose {
        let cache_key_short = short_hex(&node.cache_key, verbose);
        println!("    cache_key: {}", cache_key_short.dimmed());
    }
}

/// Print fanout analysis showing which rebuilds triggered downstream rebuilds
fn print_fanout_analysis(report: &vx_report::BuildReport, verbose: bool) {
    use vx_report::FanoutAnalysis;

    let fanout = FanoutAnalysis::compute(report);

    // Only print if there's actual fanout
    if fanout.fanout_map.is_empty() {
        return;
    }

    println!("{}", "=== FANOUT ===".cyan().bold());
    println!();

    // Sort crate IDs for stable output
    let mut crate_ids: Vec<_> = fanout.fanout_map.keys().collect();
    crate_ids.sort();

    for crate_id in crate_ids {
        let dependents = fanout.get_dependents(crate_id);
        if dependents.is_empty() {
            continue;
        }

        let crate_id_short = short_hex(crate_id, verbose);
        println!("  crate {}:", crate_id_short.cyan());

        for dependent in dependents {
            println!("    ↳ {}", dependent.dimmed());
        }
        println!();
    }
}

fn cmd_explain_last_miss(report: &vx_report::BuildReport) -> Result<()> {
    // Find the first miss
    let miss = report
        .nodes
        .iter()
        .find(|n| matches!(n.cache, CacheOutcome::Miss { .. }));

    let Some(node) = miss else {
        println!("{} All nodes hit cache!", "Note:".green().bold());
        return Ok(());
    };

    println!("{}", "First cache miss:".bold());
    println!("  {} {}", "Node:".dimmed(), node.node_id);

    if let CacheOutcome::Miss { reason } = &node.cache {
        println!("  {} {}", "Reason:".dimmed(), reason);
    }

    println!("  {} {}", "Cache key:".dimmed(), &node.cache_key[..16]);

    println!();
    println!("{}", "Inputs:".bold());
    for input in &node.inputs {
        let value_display = if input.value.len() > 16 {
            format!("{}...", &input.value[..16])
        } else {
            input.value.clone()
        };
        println!("  {:<30} {}", input.label, value_display.dimmed());
    }

    Ok(())
}

fn cmd_explain_diff(before: &vx_report::BuildReport, after: &vx_report::BuildReport) -> Result<()> {
    let diff = RunDiff::compute(before, after);

    let before_short = if diff.before.0.len() >= 10 {
        &diff.before.0[..10]
    } else {
        &diff.before.0
    };
    let after_short = if diff.after.0.len() >= 10 {
        &diff.after.0[..10]
    } else {
        &diff.after.0
    };
    println!(
        "{} {} {} {}",
        "Comparing:".bold(),
        before_short.dimmed(),
        "→".dimmed(),
        after_short
    );

    // Toolchain changes
    if !diff.toolchain_changes.is_empty() {
        println!();
        println!("{}", "Toolchain changes:".bold());
        for tc in &diff.toolchain_changes {
            println!(
                "  {} {} → {}",
                tc.name,
                tc.old.as_deref().unwrap_or("(none)").dimmed(),
                tc.new.as_deref().unwrap_or("(none)")
            );
        }
    }

    // Nodes that flipped to miss
    if !diff.flipped_to_miss.is_empty() {
        println!();
        println!("{}", "Nodes that flipped hit → miss:".yellow().bold());
        for node_diff in &diff.flipped_to_miss {
            println!("  {}", node_diff.node_id);

            if !node_diff.changed_inputs.is_empty() {
                println!("    {}:", "Changed inputs".dimmed());
                for (label, old, new) in &node_diff.changed_inputs {
                    let old_short = if old.len() > 12 {
                        format!("{}...", &old[..12])
                    } else {
                        old.clone()
                    };
                    let new_short = if new.len() > 12 {
                        format!("{}...", &new[..12])
                    } else {
                        new.clone()
                    };
                    println!(
                        "      {}: {} → {}",
                        label,
                        old_short.dimmed(),
                        new_short.yellow()
                    );
                }
            }
        }
    }

    // Nodes that flipped to hit
    if !diff.flipped_to_hit.is_empty() {
        println!();
        println!("{}", "Nodes that flipped miss → hit:".green().bold());
        for node_id in &diff.flipped_to_hit {
            println!("  {}", node_id);
        }
    }

    // Summary
    if diff.flipped_to_miss.is_empty()
        && diff.flipped_to_hit.is_empty()
        && diff.toolchain_changes.is_empty()
    {
        println!();
        println!("{} No significant changes between runs.", "Note:".dimmed());
    }

    Ok(())
}

// JSON output types for cmd_explain_json
mod explain_json {
    use facet::Facet;
    use std::collections::HashMap;

    #[derive(Facet)]
    pub struct ExplainOutput {
        pub run_id: String,
        pub prev_run_id: Option<String>,
        pub workspace_root: String,
        pub profile: String,
        pub target_triple: String,
        pub success: bool,
        pub duration_ms: u64,
        pub nodes: Vec<NodeJson>,
        pub fanout: HashMap<String, Vec<String>>,
    }

    #[derive(Facet)]
    pub struct NodeJson {
        pub node_id: String,
        pub kind: String,
        pub cache: String,
        pub execute_ms: u64,
        #[facet(default)]
        pub reported_reason: Option<String>,
        #[facet(default)]
        pub observed_reason: Option<String>,
        #[facet(default)]
        pub input_changes: Vec<InputChangeJson>,
        #[facet(default)]
        pub dep_changes: Vec<DepChangeJson>,
    }

    #[derive(Facet)]
    pub struct InputChangeJson {
        pub label: String,
        pub old_value: Option<String>,
        pub new_value: Option<String>,
    }

    #[derive(Facet)]
    pub struct DepChangeJson {
        pub extern_name: String,
        pub crate_id: String,
        pub change: DepChangeKindJson,
    }

    #[derive(Facet)]
    #[repr(u8)]
    #[allow(dead_code)]
    pub enum DepChangeKindJson {
        Added {
            rlib_hash: String,
            manifest_hash: String,
        },
        Removed {
            rlib_hash: String,
            manifest_hash: String,
        },
        RlibChanged {
            old_rlib: String,
            new_rlib: String,
            old_manifest: String,
            new_manifest: String,
        },
        ManifestChanged {
            rlib_hash: String,
            old_manifest: String,
            new_manifest: String,
        },
    }
}

/// Output explanation in JSON format
fn cmd_explain_json(
    report: &vx_report::BuildReport,
    prev_report: Option<&vx_report::BuildReport>,
    node_filter: Option<String>,
) -> Result<()> {
    use explain_json::*;
    use std::collections::HashMap;
    use vx_report::{CacheOutcome, FanoutAnalysis, NodeRebuildExplanation};

    let prev_nodes: HashMap<&str, &vx_report::NodeReport> = prev_report
        .map(|r| r.nodes.iter().map(|n| (n.node_id.as_str(), n)).collect())
        .unwrap_or_default();

    let mut nodes_json = Vec::new();

    for node in &report.nodes {
        // Apply node filter if specified
        if let Some(ref filter) = node_filter
            && &node.node_id != filter
        {
            continue;
        }

        let prev_node = prev_nodes.get(node.node_id.as_str()).copied();

        // For cache hits, include minimal info
        if matches!(node.cache, CacheOutcome::Hit { .. }) {
            nodes_json.push(NodeJson {
                node_id: node.node_id.clone(),
                kind: node.kind.clone(),
                cache: "hit".to_string(),
                execute_ms: node.timing.execute_ms,
                reported_reason: None,
                observed_reason: None,
                input_changes: vec![],
                dep_changes: vec![],
            });
            continue;
        }

        // For cache misses, compute explanation
        let explanation = NodeRebuildExplanation::compute(node, prev_node);

        if let Some(expl) = explanation {
            let input_changes: Vec<InputChangeJson> = expl
                .input_changes
                .iter()
                .map(|change| InputChangeJson {
                    label: change.label.clone(),
                    old_value: change.old_value.clone(),
                    new_value: change.new_value.clone(),
                })
                .collect();

            let dep_changes: Vec<DepChangeJson> = expl
                .dep_changes
                .iter()
                .map(|change| {
                    use vx_report::DepChangeKind;

                    let change_kind = match &change.change {
                        DepChangeKind::Added {
                            rlib_hash,
                            manifest_hash,
                        } => DepChangeKindJson::Added {
                            rlib_hash: rlib_hash.clone(),
                            manifest_hash: manifest_hash.clone(),
                        },
                        DepChangeKind::Removed {
                            rlib_hash,
                            manifest_hash,
                        } => DepChangeKindJson::Removed {
                            rlib_hash: rlib_hash.clone(),
                            manifest_hash: manifest_hash.clone(),
                        },
                        DepChangeKind::RlibChanged {
                            old_rlib,
                            new_rlib,
                            old_manifest,
                            new_manifest,
                        } => DepChangeKindJson::RlibChanged {
                            old_rlib: old_rlib.clone(),
                            new_rlib: new_rlib.clone(),
                            old_manifest: old_manifest.clone(),
                            new_manifest: new_manifest.clone(),
                        },
                        DepChangeKind::ManifestChanged {
                            rlib_hash,
                            old_manifest,
                            new_manifest,
                        } => DepChangeKindJson::ManifestChanged {
                            rlib_hash: rlib_hash.clone(),
                            old_manifest: old_manifest.clone(),
                            new_manifest: new_manifest.clone(),
                        },
                    };

                    DepChangeJson {
                        extern_name: change.extern_name.clone(),
                        crate_id: change.crate_id.clone(),
                        change: change_kind,
                    }
                })
                .collect();

            nodes_json.push(NodeJson {
                node_id: expl.node_id.clone(),
                kind: node.kind.clone(),
                cache: "miss".to_string(),
                execute_ms: node.timing.execute_ms,
                reported_reason: Some(expl.reported_reason.to_string()),
                observed_reason: Some(expl.observed_reason.to_string()),
                input_changes,
                dep_changes,
            });
        }
    }

    // Compute fanout
    let fanout = FanoutAnalysis::compute(report);

    let output = ExplainOutput {
        run_id: report.run_id.to_string(),
        prev_run_id: prev_report.map(|r| r.run_id.to_string()),
        workspace_root: report.workspace_root.clone(),
        profile: report.profile.clone(),
        target_triple: report.target_triple.clone(),
        success: report.success,
        duration_ms: report.duration_ms(),
        nodes: nodes_json,
        fanout: fanout.fanout_map.clone(),
    };

    println!(
        "{}",
        facet_json::to_string_pretty(&output)
            .map_err(|e| eyre::eyre!("failed to serialize explain output: {e}"))?
    );
    Ok(())
}

/// Open an interactive build graph in the browser.
fn cmd_explain_web(report: &vx_report::BuildReport) -> Result<()> {
    use std::io::{Read, Write};

    if report.graph.is_none() {
        println!(
            "{} This build report has no graph topology data.",
            "Error:".red().bold()
        );
        println!(
            "Graph topology was added in schema version 2. Re-run {} to generate a report with graph data.",
            "vx build".cyan()
        );
        return Ok(());
    }

    let report_json = facet_json::to_string(report)
        .map_err(|e| eyre::eyre!("failed to serialize report: {e}"))?;

    let html_template = include_str!("graph.html");
    let html = html_template.replace("__REPORT_DATA__", &report_json);

    // Start a tiny HTTP server on a random port
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let url = format!("http://127.0.0.1:{port}");

    println!("{} {}", "Build graph:".green().bold(), url.cyan());
    println!("{}", "Press Ctrl+C to stop".dimmed());

    // Open browser
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
    }

    // Serve requests until interrupted
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Read request (we only serve one page, ignore the actual request path)
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            html.len(),
            html
        );
        let _ = stream.write_all(response.as_bytes());
    }

    Ok(())
}
