use std::fs::create_dir_all;
use std::path::Path;
use std::sync::OnceLock;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const LOG_FILE_NAME: &str = "vixenfs-extension.log";

static TRACING_INIT: OnceLock<()> = OnceLock::new();
static TRACING_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

pub(super) fn init_logging(app_group_path: &Path, is_debug: bool) {
    let _ = TRACING_INIT.get_or_init(|| {
        let filter = if is_debug {
            EnvFilter::new("trace")
        } else {
            EnvFilter::new("info,vixen_fskit=debug")
        };
        let log_dir = app_group_path.join("Logs");
        let _ = create_dir_all(&log_dir);
        let file_appender = tracing_appender::rolling::weekly(&log_dir, LOG_FILE_NAME);
        let (writer, guard) = tracing_appender::non_blocking(file_appender);
        let _ = TRACING_GUARD.set(guard);
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(writer)
            .with_target(true)
            .with_file(true)
            .with_line_number(true);

        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .init();
    });
    log_event("info", "vixen-fskit tracing initialized");
}

pub(super) fn log_event(level: &str, message: impl AsRef<str>) {
    let message = message.as_ref();
    match level {
        "trace" => tracing::trace!("{message}"),
        "debug" => tracing::debug!("{message}"),
        "info" => tracing::info!("{message}"),
        "warn" => tracing::warn!("{message}"),
        "error" => tracing::error!("{message}"),
        other => tracing::info!(target: "vx_fskit::legacy_log", level = other, "{message}"),
    }
}
