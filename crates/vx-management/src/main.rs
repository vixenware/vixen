//! The Vixen Management daemon entrypoint.
//!
//! Two ways in:
//! - **launchd** (production): a file landed in a `QueueDirectories` queue, so launchd
//!   started us. We repair the queue directories (`RunAtLoad`), run whichever button(s)
//!   fired, drain their queues, and exit. No `KeepAlive`, so we stay down until the
//!   next trigger.
//! - **direct CLI**: `vixen-management repair-extension` removes stale
//!   LaunchServices/PlugInKit state for the current effective user, while
//!   `vixen-management <button>` runs one privileged capability immediately for
//!   manual testing.

use std::fs::OpenOptions;
use std::path::Path;

use eyre::{Result, bail};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use vx_management::{Button, queue, status};

const LOG_DIR: &str = "/Library/Application Support/Vixen/management/logs";
const LOG_FILE_NAME: &str = "vixen-management.log";

fn main() -> Result<()> {
    init_tracing();
    tracing::info!(
        pid = std::process::id(),
        uid = unsafe { libc::geteuid() },
        "vixen-management starting"
    );

    match std::env::args().nth(1) {
        // Direct CLI: repair user-session registration or run one button now.
        Some(arg) => {
            tracing::info!(arg, "vixen-management direct CLI command received");
            if arg == "ensure-queues" {
                let outcome = queue::ensure_queues();
                tracing::info!(ok = outcome.is_ok(), "ensure-queues finished");
                return outcome;
            }
            if arg == "repair-extension" {
                let outcome = vx_management::extension::repair_installed_extension();
                tracing::info!(ok = outcome.is_ok(), "repair-extension finished");
                return outcome;
            }
            if arg == "probe-vfs" {
                let outcome = vx_management::probe::probe_vfs();
                tracing::info!(ok = outcome.is_ok(), "probe-vfs finished");
                return outcome;
            }
            if arg == "repair" {
                let outcome = vx_management::repair::repair_vixenfs();
                tracing::info!(ok = outcome.is_ok(), "repair finished");
                return outcome;
            }
            let Some(button) = Button::from_name(&arg) else {
                tracing::warn!(arg, "unknown direct CLI command");
                bail!(
                    "unknown command {arg:?}; expected mount|unmount|remount|kill-fskitd|repair|repair-extension|probe-vfs"
                );
            };
            tracing::info!(button = ?button, "executing direct management button");
            let outcome = button.execute();
            if let Err(err) = status::record(button, &outcome) {
                tracing::warn!(button = ?button, "failed to record direct action status: {err:#}");
            }
            tracing::info!(button = ?button, ok = outcome.is_ok(), "direct management button finished");
            outcome
        }
        // launchd mode: a queue fired (or RunAtLoad).
        None => {
            tracing::info!("vixen-management entered launchd queue mode");
            run_launchd()
        }
    }
}

fn init_tracing() {
    if unsafe { libc::geteuid() } == 0 && can_write_log_file() {
        let file_appender = tracing_appender::rolling::never(LOG_DIR, LOG_FILE_NAME);
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(file_appender)
            .with_target(true)
            .with_file(true)
            .with_line_number(true);

        tracing_subscriber::registry()
            .with(env_filter())
            .with(fmt_layer)
            .init();
    } else {
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(std::io::stderr)
            .with_target(true)
            .with_file(true)
            .with_line_number(true);

        tracing_subscriber::registry()
            .with(env_filter())
            .with(fmt_layer)
            .init();
    }
}

fn env_filter() -> EnvFilter {
    EnvFilter::new(
        std::env::var("VIXEN_MANAGEMENT_LOG")
            .unwrap_or_else(|_| "info,vx_management=debug,vixen_management=debug".to_string()),
    )
}

fn can_write_log_file() -> bool {
    if std::fs::create_dir_all(LOG_DIR).is_err() {
        return false;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(Path::new(LOG_DIR).join(LOG_FILE_NAME))
        .is_ok()
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
        tracing::info!(button = ?button, "trigger fired");
        let outcome = button.execute();
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
