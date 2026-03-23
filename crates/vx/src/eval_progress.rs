use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use vx_eval2::SourceSpan;

use crate::types::CliMode;

#[derive(Clone, Debug)]
pub(crate) struct EvalProgressReporter {
    next_call_id: Arc<AtomicU64>,
    active_calls: Arc<AtomicUsize>,
}

pub(crate) struct EvalRuntimeCallProgress {
    call_id: u64,
    label: &'static str,
    detail: Option<String>,
    source: SourceSpan,
    started: Instant,
    finished: bool,
    active_calls: Arc<AtomicUsize>,
}

impl Default for EvalProgressReporter {
    fn default() -> Self {
        Self {
            next_call_id: Arc::new(AtomicU64::new(0)),
            active_calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl EvalProgressReporter {
    pub(crate) fn maybe_new(mode: &CliMode) -> Option<Self> {
        if matches!(mode, CliMode::Json) {
            None
        } else {
            Some(Self::default())
        }
    }

    pub(crate) fn begin_runtime_call(
        &self,
        label: &'static str,
        detail: Option<String>,
        source: &SourceSpan,
    ) -> EvalRuntimeCallProgress {
        let call_id = self.next_call_id.fetch_add(1, Ordering::SeqCst) + 1;
        let active = self.active_calls.fetch_add(1, Ordering::SeqCst) + 1;
        eprintln!(
            "{} {}{} started{}",
            format_prefix(call_id, active),
            label,
            format_detail(detail.as_deref()),
            format_source_suffix(source)
        );
        EvalRuntimeCallProgress {
            call_id,
            label,
            detail,
            source: source.clone(),
            started: Instant::now(),
            finished: false,
            active_calls: self.active_calls.clone(),
        }
    }
}

impl EvalRuntimeCallProgress {
    pub(crate) fn report_phase(&self, phase: &'static str, elapsed: Duration) {
        let active = self.active_calls.load(Ordering::SeqCst);
        eprintln!(
            "{} {}{} {} in {}{}",
            format_prefix(self.call_id, active),
            self.label,
            format_detail(self.detail.as_deref()),
            phase,
            format_elapsed(elapsed),
            format_source_suffix(&self.source)
        );
    }

    pub(crate) fn finish_ready(mut self) {
        self.finish("finished");
    }

    pub(crate) fn finish_failed(mut self) {
        self.finish("failed");
    }

    fn finish(&mut self, outcome: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        let active = self.active_calls.fetch_sub(1, Ordering::SeqCst) - 1;
        eprintln!(
            "{} {}{} {} in {}{}",
            format_prefix(self.call_id, active),
            self.label,
            format_detail(self.detail.as_deref()),
            outcome,
            format_elapsed(self.started.elapsed()),
            format_source_suffix(&self.source)
        );
    }
}

impl Drop for EvalRuntimeCallProgress {
    fn drop(&mut self) {
        self.finish("failed");
    }
}

fn format_detail(detail: Option<&str>) -> String {
    let Some(detail) = detail else {
        return String::new();
    };
    format!(" {}", summarize_detail(detail))
}

fn format_prefix(call_id: u64, active_calls: usize) -> String {
    format!("[eval #{call_id} active={active_calls}]")
}

fn summarize_detail(detail: &str) -> &str {
    Path::new(detail)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(detail)
}

fn format_source_suffix(source: &SourceSpan) -> String {
    let label = Path::new(source.source_id.as_str())
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(source.source_id.as_str());
    format!(" at {}:{}", label, source.start.line.0 + 1)
}

fn format_elapsed(elapsed: Duration) -> String {
    if elapsed.as_secs_f64() >= 1.0 {
        format!("{:.2}s", elapsed.as_secs_f64())
    } else if elapsed.as_millis() >= 1 {
        format!("{}ms", elapsed.as_millis())
    } else {
        format!("{}us", elapsed.as_micros())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vx_eval2::{ByteOffset, ColumnIndex, LineIndex, SourcePoint};

    #[test]
    fn maybe_new_skips_json_mode() {
        assert!(EvalProgressReporter::maybe_new(&CliMode::Json).is_none());
    }

    #[test]
    fn format_prefix_includes_call_id_and_active_count() {
        assert_eq!(format_prefix(7, 3), "[eval #7 active=3]");
    }

    #[test]
    fn summarizes_detail_to_basename() {
        assert_eq!(
            summarize_detail("https://static.crates.io/crates/sha2/sha2-0.10.9.crate"),
            "sha2-0.10.9.crate"
        );
        assert_eq!(summarize_detail("/usr/bin/rustc"), "rustc");
        assert_eq!(summarize_detail("sha2 (cache)"), "sha2 (cache)");
    }

    #[test]
    fn formats_source_suffix_with_basename_and_one_based_line() {
        let source = SourceSpan {
            source_id: "/tmp/workspace/zoo/cargo-resolve.vx".into(),
            start: SourcePoint {
                byte: ByteOffset(0),
                line: LineIndex(2262),
                column: ColumnIndex(16),
            },
            end: SourcePoint {
                byte: ByteOffset(0),
                line: LineIndex(2262),
                column: ColumnIndex(20),
            },
        };
        assert_eq!(format_source_suffix(&source), " at cargo-resolve.vx:2263");
    }

    #[test]
    fn runtime_calls_track_unique_ids_and_active_counts() {
        let reporter = EvalProgressReporter::default();
        let source = SourceSpan {
            source_id: "/tmp/workspace/zoo/cargo-resolve.vx".into(),
            start: SourcePoint {
                byte: ByteOffset(0),
                line: LineIndex(0),
                column: ColumnIndex(0),
            },
            end: SourcePoint {
                byte: ByteOffset(0),
                line: LineIndex(0),
                column: ColumnIndex(0),
            },
        };

        let first = reporter.begin_runtime_call("download", Some("one".into()), &source);
        assert_eq!(first.call_id, 1);
        assert_eq!(reporter.active_calls.load(Ordering::SeqCst), 1);

        let second = reporter.begin_runtime_call("download", Some("two".into()), &source);
        assert_eq!(second.call_id, 2);
        assert_eq!(reporter.active_calls.load(Ordering::SeqCst), 2);

        second.finish_ready();
        assert_eq!(reporter.active_calls.load(Ordering::SeqCst), 1);

        first.finish_failed();
        assert_eq!(reporter.active_calls.load(Ordering::SeqCst), 0);
    }
}
