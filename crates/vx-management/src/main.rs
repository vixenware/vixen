//! The Vixen Management daemon entrypoint.
//!
//! Two ways in:
//! - **launchd** (production): a file landed in a `QueueDirectories` queue, so launchd
//!   started us. We repair the queue directories (`RunAtLoad`), run whichever button(s)
//!   fired, drain their queues, and exit. No `KeepAlive`, so we stay down until the
//!   next trigger.
//! - **direct CLI** (testing, under `sudo`): `vixen-management <button>` runs one
//!   capability immediately — e.g. `sudo vixen-management mount` to bring up
//!   the VixenFS volume before the launchd plumbing is installed.

use eyre::{Result, bail};
use vx_management::{Button, queue};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    match std::env::args().nth(1) {
        // Direct CLI: run one button now (manual / sudo testing).
        Some(arg) => {
            if arg == "ensure-queues" {
                return queue::ensure_queues();
            }
            let Some(button) = Button::from_name(&arg) else {
                bail!("unknown button {arg:?}; expected mount|unmount|remount|kill-fskitd");
            };
            button.execute()
        }
        // launchd mode: a queue fired (or RunAtLoad).
        None => run_launchd(),
    }
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
        queue::drain(button)?;
        match outcome {
            Ok(()) => tracing::info!(button = ?button, "done"),
            Err(e) => tracing::error!(button = ?button, "failed: {e:#}"),
        }
    }
    if !fired_any {
        tracing::info!("launched with no fired queue (RunAtLoad perm-repair only)");
    }
    Ok(())
}
