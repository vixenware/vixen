//! vx - Build execution engine with deterministic caching
//!
//! This is a thin CLI client that talks to the vx daemon.

mod build_json;
mod cli;
mod diagnostics;
mod progress_listener;
mod reporting;
mod tui;
mod types;

use camino::Utf8PathBuf;
use eyre::{Result, bail, eyre};
use figue as args;
use owo_colors::OwoColorize;
use tui::emit_osc_progress;
use vox_core::Driver;
use vox_types::{ConnectionSettings, Parity};
use vx_services::{BuildRequest, PsResult, VxdClient};
use vx_tracing::init_cli_stderr;

use crate::reporting::print_fatal_error;
use crate::types::{CliContext, CliMode, DaemonStrategy};
use crate::{
    cli::{BuildArgs, CleanArgs, Cli, CliCommand, DebugArgs, EvalArgs, KillArgs, StartArgs},
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
        CliCommand::Ps => cx.cmd_ps().await,
        CliCommand::Clean(args) => cx.cmd_clean(args).await,
        CliCommand::Doctor(_args) => cx.cmd_doctor().await,
        CliCommand::Debug(args) => cx.cmd_debug(args).await,
        CliCommand::Eval(args) => cx.cmd_eval(args).await,
    };

    if let Err(e) = result {
        if let CliMode::Tui(tui) = &cx.mode {
            tui.shutdown().await;
        }
        print_fatal_error(&e);
        std::process::exit(1);
    }
}

struct ConnectedVxd {
    handle: ServiceHandle,
    client: VxdClient,
}

impl std::ops::Deref for ConnectedVxd {
    type Target = VxdClient;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

// ─── CliContext methods ─────────────────────────────────────────────────────

impl CliContext {
    /// Connect to the daemon, spawning it if necessary.
    ///
    /// The progress dispatcher is created here because it's tightly coupled
    /// to the vx CLI's display logic.
    async fn ensure_vxd(&self) -> Result<ConnectedVxd> {
        let vx_home = vx_io::vx_home();

        // Connect to daemon — root connection only has the Boot service.
        let mut builder =
            ServiceBuilder::new("vxd", self.strat.endpoint().clone()).caller_name("vx");

        builder = match &self.strat {
            DaemonStrategy::Connect(_) => builder.connect_only(),
            DaemonStrategy::Bootstrap(_) => builder
                .env("VX_HOME", vx_home.as_str())
                .log_path(vx_home.join("vxd.log")),
        };

        let handle = builder.connect().await?;

        // Boot protocol: call boot(), then open virtual connection with
        // ProgressListener dispatcher for callbacks from the daemon.
        let boot_client = vx_io::boot::BootClient::new(handle.caller.clone());
        let mut last_phase = None;
        loop {
            let status = boot_client
                .status()
                .await
                .map_err(|e| eyre!("boot status RPC failed: {e:?}"))?;
            match status.state {
                BootStatusState::Booting => {
                    if status.phase != last_phase {
                        let phase = status
                            .phase
                            .clone()
                            .unwrap_or_else(|| "booting services".to_string());
                        tracing::info!("vxd: {phase}");
                        last_phase = status.phase;
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
                BootStatusState::Ready => break,
                BootStatusState::Failed => {
                    let msg = status
                        .error
                        .unwrap_or_else(|| "unknown boot error".to_string());
                    bail!("{msg}");
                }
            }
        }

        let progress_listener = ProgressListenerImpl::new(self.mode.clone());
        let dispatcher = vx_services::ProgressListenerDispatcher::new(progress_listener);
        let virtual_handle = handle
            .session_handle
            .open_connection(
                ConnectionSettings {
                    parity: Parity::Odd,
                    max_concurrent_requests: 64,
                },
                vec![],
            )
            .await?;
        let mut driver = Driver::new(virtual_handle, dispatcher);
        let caller = driver.caller();
        tokio::spawn(async move { driver.run().await });

        Ok(ConnectedVxd {
            handle,
            client: VxdClient::new(caller),
        })
    }

    // ─── Commands ───────────────────────────────────────────────────────

    async fn cmd_eval(&mut self, _args: EvalArgs) -> Result<()> {
        bail!("vx eval is temporarily unavailable during the evaluator purge");
    }

    async fn cmd_build(&mut self, args: BuildArgs) -> Result<()> {
        let cwd = Utf8PathBuf::try_from(std::env::current_dir()?)?;
        let build_file_path = args.file.as_ref().map(|file| {
            let path = Utf8PathBuf::from(file);
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        });

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
            daemon
                .set_session(session_id)
                .await
                .map_err(|e| eyre!("set_session RPC failed: {e:?}"))?;
        }

        let mut build_env = Vec::new();
        for entry in &args.env {
            if let Some((key, value)) = entry.split_once('=') {
                build_env.push((key.to_string(), value.to_string()));
            } else {
                let value = std::env::var(entry)
                    .map_err(|_| eyre!("--env {entry}: not set in environment"))?;
                build_env.push((entry.clone(), value));
            }
        }

        let mut request = BuildRequest::new(cwd.clone())
            .release(args.release)
            .env(build_env);
        if let Some(path) = build_file_path {
            request = request.build_file_path(path);
        }

        // Race the build against Ctrl-C. When the user hits Ctrl-C, we shut down
        // the TUI cleanly and exit. The daemon detects the dropped connection and
        // cancels the in-flight build on its own.
        let result = tokio::select! {
            result = daemon.build(request) => result

                .map_err(|e| eyre!("build RPC failed: {e:?}"))?,
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
                tui.set_build_outcome(result.success);
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
            if let Some(failure) = &result.failure {
                match diagnostics::render_build_failure(failure, args.full_stack) {
                    Ok(rendered) => eprintln!("{rendered}"),
                    Err(render_error) => {
                        tracing::warn!(error = %render_error, "failed to render build diagnostics");
                        eprintln!("{}", failure.message);
                    }
                }
                std::process::exit(1);
            }
            if let Some(error) = &result.error {
                eprintln!("{error}");
                std::process::exit(1);
            }
            bail!("{}", result.message)
        }
    }

    async fn cmd_start(&mut self, _args: StartArgs) -> Result<()> {
        let mut daemon = self.ensure_vxd().await?;

        tracing::info!("✓ daemon is running");

        // Ping to verify connection
        let version = daemon
            .version()
            .await
            .map_err(|e| eyre!("version RPC failed: {e:?}"))?;

        tracing::info!("✓ connected to vxd");
        tracing::info!("   {}", version);

        if let Some(mut child) = daemon.handle.child.take() {
            let status = child
                .wait()
                .await
                .map_err(|e| eyre!("vxd wait failed: {e}"))?;
            if !status.success() {
                bail!("vxd exited with {status}");
            }
        }

        Ok(())
    }

    async fn cmd_ping(&mut self) -> Result<()> {
        // Ping only connects, never spawns
        self.strat = self.strat.clone().connect_only();
        let daemon = self.ensure_vxd().await?;
        let version = daemon
            .version()
            .await
            .map_err(|e| eyre!("version RPC failed: {e:?}"))?;
        println!("{version}");
        Ok(())
    }

    async fn cmd_ps(&mut self) -> Result<()> {
        self.strat = self.strat.clone().connect_only();
        let daemon = self.ensure_vxd().await?;
        let snapshot = daemon
            .ps()
            .await
            .map_err(|e| eyre!("ps RPC failed: {e:?}"))?;
        print_ps(snapshot);
        Ok(())
    }

    async fn cmd_kill(&mut self, _args: KillArgs) -> Result<()> {
        use vx_io::boot::BootClient;
        use vx_io::spawn::ServiceBuilder;

        // Connect to the root connection only — don't boot, don't open virtual connections.
        // shutdown() lives on the Boot service, so it's always reachable even if boot failed.
        let endpoint = self.strat.endpoint().clone();
        let builder = ServiceBuilder::new("vxd", endpoint.clone())
            .caller_name("vx")
            .timeout(tokio::time::Duration::from_millis(200))
            .connect_only();

        match builder.connect().await {
            Ok(handle) => {
                tracing::info!("Stopping daemon at {}", endpoint);
                let boot_client = BootClient::new(handle.caller.clone());
                if let Err(e) = boot_client.shutdown().await {
                    // shutdown() calls process::exit(0) on the daemon side, so the
                    // connection closing is expected — not an error.
                    tracing::debug!("shutdown RPC returned: {e:?} (expected — daemon exited)");
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                tracing::info!("Daemon stopped");
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
    async fn cmd_clean_cache(&self, _vx_home: &Utf8PathBuf) -> Result<()> {
        bail!("vx clean --cache is temporarily unavailable during the daemon/proto merge")
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

    async fn cmd_debug(&mut self, _args: DebugArgs) -> Result<()> {
        // Launch moire dashboard server CLI.
        let status = moire::process::Command::new("moire")
            .status()
            .await
            .map_err(|e| eyre::eyre!("failed to launch `moire` dashboard CLI: {e}"))?;
        if !status.success() {
            eyre::bail!("`moire` exited with status: {status}");
        }
        Ok(())
    }
}

fn print_ps(snapshot: PsResult) {
    if snapshot.entries.is_empty() {
        println!("no in-flight activity");
        return;
    }

    for entry in snapshot.entries {
        let executor = entry.executor.unwrap_or_else(|| "-".to_string());
        let last_message = entry.last_message.unwrap_or_else(|| "-".to_string());
        println!(
            "[{}] {} {} age={} idle={} {}",
            entry.id,
            entry.kind,
            executor,
            format_duration_ms(entry.age_ms),
            format_duration_ms(entry.idle_ms),
            entry.label
        );
        println!("    last: {last_message}");
        match (entry.remote_idle_ms, entry.last_remote_message) {
            (Some(remote_idle_ms), Some(last_remote_message)) => {
                println!(
                    "    remote: {} ago: {}",
                    format_duration_ms(remote_idle_ms),
                    last_remote_message
                );
            }
            _ => {
                println!(
                    "    remote: none yet ({})",
                    format_duration_ms(entry.age_ms)
                );
            }
        }
    }
}

fn format_duration_ms(ms: u64) -> String {
    if ms >= 1_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        format!("{ms}ms")
    }
}
