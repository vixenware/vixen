use std::ffi::CStr;
use std::fs::create_dir_all;
use std::path::PathBuf;
use std::sync::OnceLock;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const APP_GROUP_ID: &str = "B2N6FSRTPV.vixenfs";
const LOG_FILE_NAME: &str = "vixenfs-extension.log";

static TRACING_INIT: OnceLock<()> = OnceLock::new();
static TRACING_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

pub(super) fn init_logging() {
    let _ = TRACING_INIT.get_or_init(|| {
        let filter = EnvFilter::new("trace");
        let (log_dir, file_name) = log_destination();
        let _ = create_dir_all(&log_dir);
        let file_appender = tracing_appender::rolling::hourly(log_dir, file_name);
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

pub(super) fn real_home_dir() -> PathBuf {
    let uid = unsafe { libc::getuid() };
    let passwd = unsafe { libc::getpwuid(uid) };
    if !passwd.is_null() {
        let dir = unsafe { CStr::from_ptr((*passwd).pw_dir) };
        return PathBuf::from(dir.to_string_lossy().into_owned());
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    if let Some(prefix) = home.split("/Library/Containers/").next()
        && prefix.starts_with("/Users/")
    {
        return PathBuf::from(prefix);
    }
    PathBuf::from(home)
}

fn log_file_path() -> PathBuf {
    if let Ok(path) = std::env::var("VIXEN_FS_LOG_PATH") {
        return PathBuf::from(path);
    }

    real_home_dir()
        .join("Library")
        .join("Group Containers")
        .join(APP_GROUP_ID)
        .join("Logs")
        .join(LOG_FILE_NAME)
}

fn log_destination() -> (PathBuf, String) {
    let log_path = log_file_path();
    let directory = log_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let file_name = log_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(LOG_FILE_NAME)
        .to_owned();
    (directory, file_name)
}
