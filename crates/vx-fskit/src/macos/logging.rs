use std::fs::{create_dir_all, metadata, read_to_string};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry, reload};

const LOG_FILE_NAME: &str = "vixenfs-extension.log";
const LOG_FILTER_FILE_NAME: &str = "vixenfs-log-filter";
const DEFAULT_FILTER: &str = "info";
const DEBUG_FILTER: &str = "info,vx_fskit=debug";
const TRACE_FILTER: &str = "trace";

static TRACING_INIT: OnceLock<()> = OnceLock::new();
static TRACING_GUARD: OnceLock<WorkerGuard> = OnceLock::new();
static TRACING_RELOAD: OnceLock<reload::Handle<EnvFilter, Registry>> = OnceLock::new();
static LOG_FILTER_CONTROL: OnceLock<Mutex<LogFilterControl>> = OnceLock::new();

#[derive(Clone, Copy, PartialEq, Eq)]
struct FilterFileSignature {
    modified: Option<SystemTime>,
    len: u64,
}

struct LogFilterControl {
    path: PathBuf,
    active_filter: String,
    last_signature: Option<FilterFileSignature>,
}

pub(super) fn init_logging(app_group_path: &Path) {
    let control = LogFilterControl::new(log_filter_path(app_group_path));
    let initial_filter = control.active_filter.clone();
    let _ = LOG_FILTER_CONTROL.set(Mutex::new(control));

    let _ = TRACING_INIT.get_or_init(|| {
        let filter = EnvFilter::new(initial_filter.clone());
        let (filter, reload_handle) = reload::Layer::new(filter);
        let _ = TRACING_RELOAD.set(reload_handle);
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
    log_event(
        "info",
        format!(
            "vixen-fskit tracing initialized filter={} control={}",
            active_filter(),
            log_filter_path(app_group_path).display()
        ),
    );
}

pub(super) fn log_filter_path(app_group_path: &Path) -> PathBuf {
    app_group_path.join(LOG_FILTER_FILE_NAME)
}

pub(super) fn log_event(level: &str, message: impl AsRef<str>) {
    refresh_filter();
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

fn active_filter() -> String {
    LOG_FILTER_CONTROL
        .get()
        .and_then(|control| {
            control
                .lock()
                .ok()
                .map(|control| control.active_filter.clone())
        })
        .unwrap_or_else(|| DEFAULT_FILTER.to_owned())
}

fn refresh_filter() {
    let Some(control) = LOG_FILTER_CONTROL.get() else {
        return;
    };
    let Some(reload_handle) = TRACING_RELOAD.get() else {
        return;
    };

    let mut control = match control.lock() {
        Ok(control) => control,
        Err(_) => return,
    };
    let Some(candidate) = control.refresh_candidate() else {
        return;
    };

    match EnvFilter::try_new(&candidate) {
        Ok(filter) => match reload_handle.reload(filter) {
            Ok(()) => {
                control.active_filter = candidate.clone();
                tracing::info!(
                    "vixen-fskit log filter changed filter={} control={}",
                    candidate,
                    control.path.display()
                );
            }
            Err(error) => {
                tracing::warn!("vixen-fskit failed to reload log filter: {error}");
            }
        },
        Err(error) => {
            tracing::warn!(
                "vixen-fskit ignoring invalid log filter control={} error={error}",
                control.path.display()
            );
        }
    }
}

impl LogFilterControl {
    fn new(path: PathBuf) -> Self {
        let (active_filter, last_signature) = filter_from_file(&path).unwrap_or_else(|| {
            (
                DEFAULT_FILTER.to_owned(),
                signature_for_path(&path).unwrap_or(None),
            )
        });
        Self {
            path,
            active_filter,
            last_signature,
        }
    }

    fn refresh_candidate(&mut self) -> Option<String> {
        let signature = match signature_for_path(&self.path) {
            Ok(signature) => signature,
            Err(error) => {
                tracing::warn!(
                    "vixen-fskit could not stat log filter control={} error={error}",
                    self.path.display()
                );
                return None;
            }
        };
        if signature == self.last_signature {
            return None;
        }
        self.last_signature = signature;

        let next = match read_filter_file(&self.path) {
            Ok(filter) => filter,
            Err(error) => {
                tracing::warn!(
                    "vixen-fskit could not read log filter control={} error={error}",
                    self.path.display()
                );
                return None;
            }
        };
        (next != self.active_filter).then_some(next)
    }
}

fn filter_from_file(path: &Path) -> Option<(String, Option<FilterFileSignature>)> {
    let signature = signature_for_path(path).ok()?;
    let filter = read_filter_file(path).ok()?;
    Some((filter, signature))
}

fn signature_for_path(path: &Path) -> std::io::Result<Option<FilterFileSignature>> {
    match metadata(path) {
        Ok(metadata) => Ok(Some(FilterFileSignature {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn read_filter_file(path: &Path) -> std::io::Result<String> {
    match read_to_string(path) {
        Ok(contents) => Ok(normalize_filter(contents.trim())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DEFAULT_FILTER.to_owned()),
        Err(error) => Err(error),
    }
}

fn normalize_filter(value: &str) -> String {
    if value.is_empty()
        || value.eq_ignore_ascii_case("off")
        || value.eq_ignore_ascii_case("default")
        || value.eq_ignore_ascii_case("info")
    {
        return DEFAULT_FILTER.to_owned();
    }

    if value == "1"
        || value.eq_ignore_ascii_case("on")
        || value.eq_ignore_ascii_case("true")
        || value.eq_ignore_ascii_case("debug")
    {
        return DEBUG_FILTER.to_owned();
    }

    if value.eq_ignore_ascii_case("trace") {
        return TRACE_FILTER.to_owned();
    }

    value.to_owned()
}
