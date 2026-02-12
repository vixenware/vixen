//! vx - Build execution engine with deterministic caching
//!
//! This is a thin CLI client that talks to the vx daemon.

mod build_json;
mod cli;
mod progress_listener;
mod tui;
mod types;

use camino::Utf8PathBuf;
use eyre::{Result, bail};
use figue as args;
use owo_colors::OwoColorize;
use roam_stream::ConnectionHandle;
use tui::emit_osc_progress;
use vx_io::spawn::ServiceBuilder;
use vx_report::ReportStore;
use vx_store_proto::Blake3Hash;
use vx_tracing::init_cli_stderr;
use vxd_proto::{BuildRequest, VxdClient};

use crate::types::{CliContext, CliMode, DaemonStrategy};
use crate::{
    cli::{BuildArgs, CleanArgs, Cli, CliCommand, DebugArgs, KillArgs, StartArgs},
    progress_listener::ProgressListenerImpl,
};

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
    rt.block_on(async_main());
}

async fn async_main() {
    let cli: Cli = args::Driver::new(
        args::builder::<Cli>()
            .expect("failed to build CLI")
            .cli(|cli| cli.args(std::env::args().skip(1)))
            .help(|h| {
                h.program_name(env!("CARGO_PKG_NAME"))
                    .version(env!("CARGO_PKG_VERSION"))
            })
            .build(),
    )
    .run()
    .unwrap();

    let mut cx = CliContext::from_cli(&cli).expect("failed to create context");

    // Initialize tracing - with TUI layer, or plain output to stderr.
    // In JSON mode, tracing goes to stderr so stdout stays clean NDJSON.
    if let CliMode::Tui(ref tui) = cx.mode {
        init_tracing_with_tui(tui.clone());
    } else {
        init_cli_stderr();
    }

    let result = match cli.command {
        CliCommand::Build(args) => cx.cmd_build(args).await,
        CliCommand::Start(args) => cx.cmd_start(args).await,
        CliCommand::Ping => cx.cmd_ping().await,
        CliCommand::Kill(args) => cx.cmd_kill(args).await,
        CliCommand::Clean(args) => cx.cmd_clean(args).await,
        CliCommand::Doctor(_args) => cx.cmd_doctor().await,
        CliCommand::Debug(args) => cx.cmd_debug(args).await,
    };

    if let Err(e) = result {
        if let CliMode::Tui(tui) = &cx.mode {
            tui.shutdown().await;
        }
        print_fatal_error(&e);
        std::process::exit(1);
    }
}

fn print_fatal_error(error: &eyre::Report) {
    use std::fmt::Write;

    // Collect the error chain, stripping the repetitive "boot failed:" wrappers.
    let mut causes: Vec<String> = Vec::new();
    for cause in error.chain() {
        let msg = cause.to_string();
        if msg.starts_with("boot failed") {
            continue;
        }
        causes.push(msg);
    }
    if causes.is_empty() {
        causes.push(format!("{error:#}"));
    }

    let mut body = String::new();
    let _ = writeln!(body, "{}", causes[0]);
    for cause in &causes[1..] {
        let _ = writeln!(body, "  caused by: {cause}");
    }

    let lines: Vec<&str> = body.lines().collect();
    let max_width = lines.iter().map(|l| l.len()).max().unwrap_or(40).max(40);
    let bar = "─".repeat(max_width + 2);

    eprintln!();
    eprintln!("{}", format!("┌{bar}┐").red().bold());
    eprintln!(
        "{}",
        format!("│ {:max_width$} │", "FATAL ERROR").red().bold()
    );
    eprintln!("{}", format!("├{bar}┤").red().bold());
    for line in &lines {
        eprintln!(
            "{}{}{}",
            "│ ".red().bold(),
            format!("{line:max_width$}"),
            " │".red().bold(),
        );
    }
    eprintln!("{}", format!("└{bar}┘").red().bold());
    eprintln!();
}

// ─── CliContext methods ─────────────────────────────────────────────────────

impl CliContext {
    /// Connect to the daemon, spawning it if necessary.
    ///
    /// The progress dispatcher is created here because it's tightly coupled
    /// to the vx CLI's display logic.
    async fn ensure_vxd(&self) -> Result<VxdClient<ConnectionHandle>> {
        use roam_stream::NoDispatcher;

        let vx_home = vx_io::vx_home();

        // Connect to daemon — root connection only has the Boot service.
        let mut builder = ServiceBuilder::new("vxd", self.strat.endpoint().clone(), NoDispatcher)
            .caller_name("vx");

        builder = match &self.strat {
            DaemonStrategy::Connect(_) => builder.connect_only(),
            DaemonStrategy::Bootstrap(_) => builder
                .env("VX_HOME", vx_home.as_str())
                .log_path(vx_home.join("vxd.log")),
        };

        let handle = builder.connect().await?;

        // Boot protocol: call boot(), then open virtual connection with
        // ProgressListener dispatcher for callbacks from the daemon.
        let progress_listener = ProgressListenerImpl::new(self.mode.clone());
        let dispatcher = vxd_proto::ProgressListenerDispatcher::new(progress_listener);
        let virtual_handle = handle.boot_and_connect(Some(Box::new(dispatcher))).await?;

        Ok(VxdClient::new(virtual_handle))
    }

    // ─── Commands ───────────────────────────────────────────────────────

    async fn cmd_build(&mut self, args: BuildArgs) -> Result<()> {
        let cwd = Utf8PathBuf::try_from(std::env::current_dir()?)?;

        // Emit indeterminate OSC 9;4 progress while connecting to daemon
        // (TUI mode emits this from its display task; Text mode needs it here)
        // Skip in JSON mode — CI doesn't need terminal progress sequences.
        if matches!(self.mode, CliMode::Text) {
            emit_osc_progress(tui::OscProgressState::Indeterminate, None);
        }

        let daemon = self.ensure_vxd().await?;

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
                if let CliMode::Tui(tui) = &self.mode {
                    tui.shutdown().await;
                }
                emit_osc_progress(tui::OscProgressState::Remove, None);
                eprintln!("Build interrupted");
                std::process::exit(130);
            }
        };

        // Handle build outcome based on mode
        match &self.mode {
            CliMode::Tui(tui) => {
                tui.set_build_outcome(result.success).await;
                tui.shutdown().await;
            }
            CliMode::Text => {
                if result.success {
                    emit_osc_progress(tui::OscProgressState::Remove, None);
                } else {
                    emit_osc_progress(tui::OscProgressState::Error, None);
                }
            }
            CliMode::Json => {
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
                    if let Some(ref error) = result.error
                        && let Some(first_line) = error.lines().next()
                    {
                        eprintln!("::error::{first_line}");
                    }
                    std::process::exit(1);
                }

                return Ok(());
            }
        }

        // Text/TUI: print summary or error
        if result.success {
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

            let duration_str = if result.duration_ms < 1000 {
                format!("{}ms", result.duration_ms)
            } else {
                format!("{:.2}s", result.duration_ms as f64 / 1000.0)
            };
            println!("{} {}", "Duration:".dimmed(), duration_str.yellow());

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
                eprintln!("{error}");
                std::process::exit(1);
            }
            bail!("{}", result.message)
        }
    }

    async fn cmd_start(&mut self, _args: StartArgs) -> Result<()> {
        let daemon = self.ensure_vxd().await?;

        tracing::info!("✓ daemon is running");

        // Ping to verify connection
        let version = daemon.version().await?;

        tracing::info!("✓ connected to vxd");
        tracing::info!("   {}", version);

        Ok(())
    }

    async fn cmd_ping(&mut self) -> Result<()> {
        // Ping only connects, never spawns
        self.strat = self.strat.clone().connect_only();
        let daemon = self.ensure_vxd().await?;
        let version = daemon.version().await?;
        println!("{version}");
        Ok(())
    }

    async fn cmd_kill(&mut self, _args: KillArgs) -> Result<()> {
        use roam_stream::NoDispatcher;
        use vx_io::boot::BootClient;
        use vx_io::spawn::ServiceBuilder;

        // Connect to the root connection only — don't boot, don't open virtual connections.
        // shutdown() lives on the Boot service, so it's always reachable even if boot failed.
        let endpoint = self.strat.endpoint().clone();
        let builder = ServiceBuilder::new("vxd", endpoint.clone(), NoDispatcher)
            .caller_name("vx")
            .connect_only();

        match builder.connect().await {
            Ok(handle) => {
                tracing::info!("{} daemon at {}", "Stopping".green().bold(), endpoint);
                let root_handle = handle.caller.handle().await?;
                let boot_client = BootClient::new(root_handle);
                if let Err(e) = boot_client.shutdown().await {
                    // shutdown() calls process::exit(0) on the daemon side, so the
                    // connection closing is expected — not an error.
                    tracing::debug!("shutdown RPC returned: {e} (expected — daemon exited)");
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                tracing::info!("{} daemon stopped", "✓".green().bold());
            }
            Err(_) => {
                tracing::info!(
                    "note: daemon not reachable (child processes use die-with-parent, so they'll exit on their own)",
                );
            }
        }

        Ok(())
    }

    async fn cmd_clean(&mut self, args: CleanArgs) -> Result<()> {
        if args.home && args.cache {
            bail!("--home and --cache are mutually exclusive");
        }

        let vx_home = vx_io::vx_home();

        if args.cache {
            return self.cmd_clean_cache(&vx_home).await;
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

    /// Invalidate cache entries from the last build while preserving toolchains
    /// and registry crates.
    async fn cmd_clean_cache(&self, vx_home: &Utf8PathBuf) -> Result<()> {
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
        let daemon = self.ensure_vxd().await?;

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

    async fn cmd_doctor(&mut self) -> Result<()> {
        // Daemon status — check if vxd socket exists
        let vx_home = vx_io::vx_home();
        println!("{} {}", "home:".bold(), vx_home);
        let daemon_sock = vx_home.join("vxd.sock");
        if daemon_sock.exists() {
            println!(
                "{} {} (socket exists at {})",
                "daemon:".bold(),
                "likely running".green(),
                daemon_sock,
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
            // there used to be stuff here, we might put it back at some point.
        }

        Ok(())
    }

    async fn cmd_debug(&mut self, args: DebugArgs) -> Result<()> {
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

        #[cfg(unix)]
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
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Returns true if this node kind represents infrastructure (toolchains, registry)
/// that should be preserved during cache invalidation.
fn is_infrastructure_node(kind: &str) -> bool {
    kind.starts_with("toolchain.")
        || kind.starts_with("zig.toolchain.")
        || kind == "registry.acquire"
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
