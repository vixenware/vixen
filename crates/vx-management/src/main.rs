//! The Vixen Management daemon entrypoint.
//!
//! Two ways in:
//! - **launchd** (production): a file landed in a `QueueDirectories` queue, so launchd
//!   started us. We repair the queue directories (`RunAtLoad`), run whichever button(s)
//!   fired, drain their queues, and exit. No `KeepAlive`, so we stay down until the
//!   next trigger.
//! - **direct CLI**: `vixen-management repair-app-records` removes stale
//!   LaunchServices app state for the current effective user,
//!   `vixen-management repair-extension` records/repairs deeper extension state for
//!   diagnostics, `vixen-management repair` runs the mount primitive,
//!   `vixen-management uninstall` removes Vixen-owned privileged support state, and
//!   `vixen-management <button>` runs one privileged capability immediately for
//!   manual testing.

use std::io::Write;
use std::path::PathBuf;

use eyre::{Result, WrapErr, bail};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use vx_management::{Button, queue, status};

/// Root daemon log dir (legacy path, used only when running as root).
const ROOT_LOG_DIR: &str = "/Library/Application Support/Vixen/management/logs";
/// User-relative log dir (under `$HOME`) — where the user-side helper logs now go.
const USER_LOG_DIR_REL: &str = "Library/Logs/Vixen";
const LOG_FILE_NAME: &str = "vixen-management.log";

fn main() -> Result<()> {
    init_tracing();
    tracing::info!(
        pid = std::process::id(),
        uid = unsafe { libc::geteuid() },
        "vixen-management starting"
    );

    match std::env::args().nth(1) {
        // Direct CLI: run a diagnostic helper or one fixed button now.
        Some(arg) => {
            tracing::info!(arg, "vixen-management direct CLI command received");
            if arg == "ensure-queues" {
                let outcome = queue::ensure_queues();
                tracing::info!(ok = outcome.is_ok(), "ensure-queues finished");
                report_direct_command(&arg, &outcome)?;
                return outcome;
            }
            if arg == "repair-app-records" {
                let outcome = vx_management::extension::repair_installed_app_records();
                tracing::info!(ok = outcome.is_ok(), "repair-app-records finished");
                report_direct_command(&arg, &outcome)?;
                return outcome;
            }
            if arg == "repair-extension" {
                let outcome = vx_management::extension::repair_installed_extension();
                tracing::info!(ok = outcome.is_ok(), "repair-extension finished");
                report_direct_command(&arg, &outcome)?;
                return outcome;
            }
            if arg == "probe-vfs" {
                let outcome = vx_management::probe::probe_vfs();
                tracing::info!(ok = outcome.is_ok(), "probe-vfs finished");
                if outcome.is_err() {
                    report_direct_command(&arg, &outcome)?;
                }
                return outcome;
            }
            if arg == "setup-mountpoint" {
                let outcome = vx_management::mount::setup_mountpoint();
                tracing::info!(ok = outcome.is_ok(), "setup-mountpoint finished");
                report_direct_command(&arg, &outcome)?;
                return outcome;
            }
            if arg == "repair" {
                let outcome = vx_management::mount::establish();
                tracing::info!(ok = outcome.is_ok(), "repair finished");
                report_direct_command(&arg, &outcome)?;
                return outcome;
            }
            if arg == "uninstall" {
                let outcome = vx_management::uninstall::uninstall_vixenfs();
                tracing::info!(ok = outcome.is_ok(), "uninstall finished");
                report_direct_command(&arg, &outcome)?;
                return outcome;
            }
            let Some(button) = Button::from_name(&arg) else {
                tracing::warn!(arg, "unknown direct CLI command");
                bail!(
                    "unknown command {arg:?}; expected mount|unmount|remount|kill-fskitd|repair|uninstall|repair-app-records|repair-extension|probe-vfs"
                );
            };
            tracing::info!(button = ?button, "executing direct management button");
            let outcome = button.execute();
            if let Err(err) = status::record(button, &outcome) {
                tracing::warn!(button = ?button, "failed to record direct action status: {err:#}");
            }
            tracing::info!(button = ?button, ok = outcome.is_ok(), "direct management button finished");
            report_button_result(button, &outcome)?;
            outcome
        }
        // launchd mode: a queue fired (or RunAtLoad).
        None => {
            tracing::info!("vixen-management entered launchd queue mode");
            run_launchd()
        }
    }
}

fn report_direct_command(command: &str, outcome: &Result<()>) -> Result<()> {
    match outcome {
        Ok(()) => writeln!(std::io::stdout(), "vixen-management {command}: ok"),
        Err(err) => writeln!(
            std::io::stderr(),
            "vixen-management {command}: failed: {err:#}"
        ),
    }
    .wrap_err("writing direct command result")
}

fn report_button_result(button: Button, outcome: &Result<()>) -> Result<()> {
    let command = button.queue_name();
    match outcome {
        Ok(()) => {
            let detail = match button {
                Button::Mount => format!(" ({})", vx_management::mount::MOUNT_POINT),
                Button::Unmount => " (unmounted)".to_string(),
                Button::Remount => format!(" ({})", vx_management::mount::MOUNT_POINT),
                Button::KillFskitd => " (fskitd terminated if it was running)".to_string(),
                Button::Uninstall => " (removed VixenFS setup)".to_string(),
            };
            writeln!(std::io::stdout(), "vixen-management {command}: ok{detail}")
        }
        Err(err) => writeln!(
            std::io::stderr(),
            "vixen-management {command}: failed: {err:#}"
        ),
    }
    .wrap_err("writing direct button result")
}

fn init_tracing() {
    // Always log to stderr (captured by the app when it spawns us, visible from a
    // terminal). Additionally persist to a log file so failures are debuggable after the
    // fact — under the user's `~/Library/Logs/Vixen` when running as the user (the normal
    // case now), or the legacy root dir when running as root.
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_file(true)
        .with_line_number(true);

    if let Some(dir) = log_dir().filter(|dir| std::fs::create_dir_all(dir).is_ok()) {
        let file_appender = tracing_appender::rolling::never(&dir, LOG_FILE_NAME);
        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(file_appender)
            .with_target(true)
            .with_file(true)
            .with_line_number(true);
        tracing_subscriber::registry()
            .with(env_filter())
            .with(stderr_layer)
            .with(file_layer)
            .init();
        return;
    }

    tracing_subscriber::registry()
        .with(env_filter())
        .with(stderr_layer)
        .init();
}

/// The directory the helper writes its log file to, or `None` if it can't be determined.
fn log_dir() -> Option<PathBuf> {
    if unsafe { libc::geteuid() } == 0 {
        return Some(PathBuf::from(ROOT_LOG_DIR));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(USER_LOG_DIR_REL))
}

fn env_filter() -> EnvFilter {
    EnvFilter::new(
        std::env::var("VIXEN_MANAGEMENT_LOG")
            .unwrap_or_else(|_| "info,vx_management=debug,vixen_management=debug".to_string()),
    )
}

/// launchd-invoked: repair the queue perms, then for each fired button run it once and
/// drain. Draining happens **after** the action (even on failure) so the trigger's
/// removal acks completion to the firing doctor, while still guaranteeing launchd won't
/// relaunch us in a loop.
fn run_launchd() -> Result<()> {
    tracing::info!("launchd queue processing started");
    queue::ensure_queues()?;
    let mut fired_any = false;
    for button in Button::ALL {
        if !queue::is_fired(button) {
            continue;
        }
        fired_any = true;
        let caller_pid = queue::caller_pid(button);
        match caller_pid {
            Some(pid) => tracing::info!(button = ?button, pid, "trigger fired"),
            None => {
                tracing::warn!(button = ?button, "trigger fired but no caller PID found in queue")
            }
        }
        let outcome = button.execute_from_launchd(caller_pid);
        if button == Button::Uninstall && outcome.is_ok() {
            tracing::info!(button = ?button, "uninstall completed; management support removed");
            return Ok(());
        }
        if let Err(err) = status::record(button, &outcome) {
            tracing::warn!(button = ?button, "failed to record action status: {err:#}");
        }
        queue::drain(button)?;
        match outcome {
            Ok(()) => tracing::info!(button = ?button, "done"),
            Err(e) => {
                tracing::error!(button = ?button, "failed: {e:#}");
            }
        }
    }
    if !fired_any {
        tracing::info!("launched with no fired queue (RunAtLoad perm-repair only)");
    }
    tracing::info!(fired_any, "launchd queue processing finished");
    Ok(())
}
