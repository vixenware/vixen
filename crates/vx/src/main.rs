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
use tui::emit_osc_progress;
use vx_io::net::Endpoint;
use vx_io::spawn::ServiceBuilder;
use vx_report::ReportStore;
use vxd_proto::{BuildRequest, VxdClient};

use crate::types::CliMode;
use crate::{
    cli::{BuildArgs, CleanArgs, Cli, CliCommand, DebugArgs, KillArgs, StartArgs},
    progress_listener::ProgressListenerImpl,
};
use progress_listener::VxdCaller;

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
        CliCommand::Ping => cmd_ping().await,
        // Other commands use stderr tracing
        CliCommand::Kill(args) => {
            vx_tracing::init_cli_stderr();
            cmd_kill(args).await
        }
        CliCommand::Clean(args) => {
            vx_tracing::init_cli_stderr();
            cmd_clean(args).await
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

// ─── Daemon connection ──────────────────────────────────────────────────────

/// Connect to the daemon, spawning it if necessary.
///
/// The progress dispatcher is created here because it's tightly coupled
/// to the vx CLI's display logic. The mode is cloned into the progress
/// listener and returned for post-build use.
async fn ensure_vxd(mode: CliMode) -> Result<(VxdClient<VxdCaller>, CliMode)> {
    let vx_home = vx_io::vx_home();

    let progress_listener = ProgressListenerImpl::new(mode.clone());
    let dispatcher = vxd_proto::ProgressListenerDispatcher::new(progress_listener);

    // VXD env var: connect to an existing daemon (no spawn).
    // Otherwise: spawn at the default local endpoint.
    if let Ok(vxd) = std::env::var("VXD") {
        let endpoint = Endpoint::parse(&vxd)?;
        let builder = ServiceBuilder::new("vxd", endpoint, dispatcher)
            .caller_name("vx")
            .connect_only();

        match builder.connect().await {
            Ok(handle) => Ok((VxdClient::new(handle.caller), mode)),
            Err(e) => {
                if let CliMode::Tui(tui) = &mode {
                    tui.shutdown().await;
                }
                Err(e)
            }
        }
    } else {
        let endpoint = vx_io::net::default_local_endpoint(&vx_home, "vxd");
        let builder = ServiceBuilder::new("vxd", endpoint.clone(), dispatcher)
            .caller_name("vx")
            .env("VX_HOME", vx_home.as_str())
            .log_path(vx_home.join("vxd.log"));

        match builder.connect().await {
            Ok(handle) => Ok((VxdClient::new(handle.caller), mode)),
            Err(e) => {
                if let CliMode::Tui(tui) = &mode {
                    tui.shutdown().await;
                }
                Err(e)
            }
        }
    }
}

// ─── Commands ───────────────────────────────────────────────────────────────

async fn cmd_build(args: BuildArgs) -> Result<()> {
    let cwd = Utf8PathBuf::try_from(std::env::current_dir()?)?;

    let mode = if args.json {
        CliMode::Json
    } else if args.no_tui {
        CliMode::Text
    } else {
        CliMode::Tui(tui::TuiHandle::new())
    };

    // Emit indeterminate OSC 9;4 progress while connecting to daemon
    // (TUI mode emits this from its display task; Text mode needs it here)
    // Skip in JSON mode — CI doesn't need terminal progress sequences.
    if matches!(mode, CliMode::Text) {
        emit_osc_progress(tui::OscProgressState::Indeterminate, None);
    }

    let (daemon, mode) = ensure_vxd(mode).await?;

    // Initialize tracing - with TUI layer, or plain output to stderr.
    // In JSON mode, tracing goes to stderr so stdout stays clean NDJSON.
    if let CliMode::Tui(ref tui) = mode {
        init_tracing_with_tui(tui.clone());
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
            if let CliMode::Tui(tui) = &mode {
                tui.shutdown().await;
            }
            emit_osc_progress(tui::OscProgressState::Remove, None);
            eprintln!("Build interrupted");
            std::process::exit(130);
        }
    };

    // Handle build outcome based on mode
    match &mode {
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

async fn cmd_start(_args: StartArgs) -> Result<()> {
    let (daemon, _mode) = ensure_vxd(CliMode::Text).await?;

    tracing::info!("{} daemon is running", "✓".green().bold());

    // Ping to verify connection
    let version = daemon.version().await?;

    tracing::info!("{} connected to vxd", "✓".green().bold());
    tracing::info!("   {}", version);

    Ok(())
}

async fn cmd_ping() -> Result<()> {}

async fn cmd_kill(_args: KillArgs) -> Result<()> {
    use vx_io::net::Endpoint;

    let endpoint = match std::env::var("VXD") {
        Ok(v) => Endpoint::parse(&v)?,
        Err(_) => vx_io::net::default_local_endpoint(&vx_io::vx_home(), "vxd"),
    };

    // Try graceful shutdown via RPC first
    let kill_result = {
        use vx_io::spawn::ServiceBuilder;
        let progress_listener = ProgressListenerImpl::new(CliMode::Text);
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

    let vx_home = vx_io::vx_home();

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
    let (daemon, _mode) = ensure_vxd(CliMode::Text).await?;

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
