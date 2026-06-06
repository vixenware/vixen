//! The Vixen Management daemon entrypoint.
//!
//! Two ways in:
//! - **launchd** (production): a file landed in a `QueueDirectories` queue, so launchd
//!   started us. We repair the queue directories (`RunAtLoad`), run whichever button(s)
//!   fired, drain their queues, and exit. No `KeepAlive`, so we stay down until the
//!   next trigger.
//! - **direct CLI**: `vixen-management repair-extension` repairs the current user's
//!   LaunchServices/PlugInKit state, while `vixen-management <button>` runs one
//!   privileged capability immediately for manual testing.

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
            if arg == "ensure-queues" {
                return queue::ensure_queues();
            }
            if arg == "repair-extension" {
                return vx_management::extension::repair_installed_extension();
            }
            let Some(button) = Button::from_name(&arg) else {
                bail!(
                    "unknown button {arg:?}; expected mount|unmount|remount|kill-fskitd|repair-extension"
                );
            };
            let outcome = button.execute();
            if let Err(err) = status::record(button, &outcome) {
                tracing::warn!(button = ?button, "failed to record direct action status: {err:#}");
            }
            outcome
        }
        // launchd mode: a queue fired (or RunAtLoad).
        None => run_launchd(),
    }
}

fn init_tracing() {
    let filter = EnvFilter::new(
        std::env::var("VIXEN_MANAGEMENT_LOG")
            .unwrap_or_else(|_| "info,vx_management=debug,vixen_management=debug".to_string()),
    );
    let _ = std::fs::create_dir_all(LOG_DIR);
    let file_appender = tracing_appender::rolling::never(LOG_DIR, LOG_FILE_NAME);
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(file_appender)
        .with_target(true)
        .with_file(true)
        .with_line_number(true);

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .init();
}

/// launchd-invoked: repair the queue perms, then for each fired button run it once and
/// drain. Draining happens **after** the action (even on failure) so the trigger's
/// removal acks completion to the firing doctor, while still guaranteeing launchd won't
/// relaunch us in a loop.
fn run_launchd() -> Result<()> {
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
    Ok(())
}
