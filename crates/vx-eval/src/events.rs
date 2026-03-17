//! Per-engine event recording for tracing, profiling, and test assertions.
//!
//! The `EventSink` trait is implemented by the engine's generic parameter.
//! In production, `()` is used — all methods are no-ops that optimize away.
//! In tests, `EventRecorder` captures events for assertions.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

pub type EventArgs = facet_value::Value;

/// Trait for recording structured engine events.
///
/// Generic over the Engine so each engine instance can have its own sink.
/// The associated `Span` type is returned by `begin_span` and emits an
/// end event on drop.
pub trait EventSink: Send + Sync + 'static {
    type Span: Send;

    /// Start a duration span. The returned guard emits an end event on drop.
    fn begin_span(&self, name: &'static str) -> Self::Span;

    /// Start a duration span with structured arguments attached to the begin event.
    fn begin_span_with_args(&self, name: &'static str, _args: EventArgs) -> Self::Span {
        self.begin_span(name)
    }

    /// Record a point-in-time event.
    fn instant(&self, name: &'static str);

    /// Record a point-in-time event with structured arguments.
    fn instant_with_args(&self, name: &'static str, _args: EventArgs) {
        self.instant(name);
    }

    /// Record a dependency: the current context caused `target` to be demanded.
    fn flow(&self, from: &'static str, to: &'static str);
}

/// No-op sink — zero cost. All methods inline to nothing.
impl EventSink for () {
    type Span = ();
    #[inline(always)]
    fn begin_span(&self, _name: &'static str) {}
    #[inline(always)]
    fn instant(&self, _name: &'static str) {}
    #[inline(always)]
    fn flow(&self, _from: &'static str, _to: &'static str) {}
}

/// A recorded event.
#[derive(Debug, Clone)]
pub struct RecordedEvent {
    pub name: &'static str,
    pub phase: EventPhase,
    pub timestamp_us: u64,
    pub duration_us: Option<u64>,
    pub args: Option<EventArgs>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventPhase {
    Complete,
    Instant,
    Flow { target: &'static str },
}

/// Shared state for the event recorder.
struct RecorderInner {
    start: Instant,
    events: Mutex<Vec<RecordedEvent>>,
}

impl RecorderInner {
    fn now_us(&self) -> u64 {
        self.start.elapsed().as_micros() as u64
    }

    fn push(
        &self,
        name: &'static str,
        phase: EventPhase,
        timestamp_us: u64,
        duration_us: Option<u64>,
        args: Option<EventArgs>,
    ) {
        self.events.lock().unwrap().push(RecordedEvent {
            name,
            phase,
            timestamp_us,
            duration_us,
            args,
        });
    }

    fn push_now(&self, name: &'static str, phase: EventPhase, args: Option<EventArgs>) {
        self.push(name, phase, self.now_us(), None, args);
    }
}

/// Recording sink for tests and diagnostics.
#[derive(Clone)]
pub struct EventRecorder {
    inner: Arc<RecorderInner>,
}

impl EventRecorder {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RecorderInner {
                start: Instant::now(),
                events: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Take a snapshot of all recorded events.
    pub fn events(&self) -> Vec<RecordedEvent> {
        self.inner.events.lock().unwrap().clone()
    }

    /// Count how many times a span with the given name was started.
    pub fn count_spans(&self, name: &'static str) -> usize {
        self.inner
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.name == name && e.phase == EventPhase::Complete)
            .count()
    }

    /// Count how many instant events with the given name were recorded.
    pub fn count_instants(&self, name: &'static str) -> usize {
        self.inner
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.name == name && e.phase == EventPhase::Instant)
            .count()
    }
}

impl Default for EventRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl EventRecorder {
    /// Write all recorded events as a Chrome Trace JSON file.
    ///
    /// The output can be loaded in `chrome://tracing` or Perfetto.
    pub fn write_chrome_trace(&self, path: &std::path::Path) -> std::io::Result<()> {
        use std::io::Write;
        let mut events = self.inner.events.lock().unwrap().clone();
        events.sort_by_key(|event| event.timestamp_us);
        let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
        write!(f, "{{\"displayTimeUnit\":\"ms\",\"traceEvents\":[")?;
        let mut first = true;
        for event in events.iter() {
            if !first {
                write!(f, ",")?;
            }
            first = false;
            match event.phase {
                EventPhase::Complete => {
                    let duration_us = event.duration_us.unwrap_or_default();
                    write!(
                        f,
                        "{{\"ph\":\"X\",\"name\":\"{}\",\"cat\":\"eval\",\"ts\":{},\"dur\":{},\"pid\":1,\"tid\":1",
                        event.name, event.timestamp_us, duration_us
                    )?;
                    write_trace_args(&mut f, event.args.as_ref())?;
                    write!(f, "}}")?;
                }
                EventPhase::Instant => {
                    write!(
                        f,
                        "{{\"ph\":\"i\",\"name\":\"{}\",\"cat\":\"eval\",\"s\":\"t\",\"ts\":{},\"pid\":1,\"tid\":1",
                        event.name, event.timestamp_us
                    )?;
                    write_trace_args(&mut f, event.args.as_ref())?;
                    write!(f, "}}")?;
                }
                EventPhase::Flow { target } => {
                    write!(
                        f,
                        "{{\"ph\":\"i\",\"name\":\"{}→{}\",\"cat\":\"flow\",\"s\":\"t\",\"ts\":{},\"pid\":1,\"tid\":1}}",
                        event.name, target, event.timestamp_us
                    )?;
                }
            }
        }
        write!(f, "]}}")?;
        f.flush()
    }
}

/// Guard that records an end event on drop.
pub struct RecorderSpan {
    inner: Arc<RecorderInner>,
    name: &'static str,
    started_us: u64,
    args: Option<EventArgs>,
}

impl Drop for RecorderSpan {
    fn drop(&mut self) {
        let ended_us = self.inner.now_us();
        self.inner.push(
            self.name,
            EventPhase::Complete,
            self.started_us,
            Some(ended_us.saturating_sub(self.started_us)),
            self.args.take(),
        );
    }
}

impl EventSink for EventRecorder {
    type Span = RecorderSpan;

    fn begin_span(&self, name: &'static str) -> RecorderSpan {
        RecorderSpan {
            inner: Arc::clone(&self.inner),
            name,
            started_us: self.inner.now_us(),
            args: None,
        }
    }

    fn begin_span_with_args(&self, name: &'static str, args: EventArgs) -> RecorderSpan {
        RecorderSpan {
            inner: Arc::clone(&self.inner),
            name,
            started_us: self.inner.now_us(),
            args: Some(args),
        }
    }

    fn instant(&self, name: &'static str) {
        self.inner.push_now(name, EventPhase::Instant, None);
    }

    fn instant_with_args(&self, name: &'static str, args: EventArgs) {
        self.inner.push_now(name, EventPhase::Instant, Some(args));
    }

    fn flow(&self, from: &'static str, to: &'static str) {
        self.inner
            .push_now(from, EventPhase::Flow { target: to }, None);
    }
}

// --- Type-erased wrapper ---

/// Type-erased event sink. The span is a boxed trait object.
/// Slightly more overhead than the generic version, but avoids
/// making Engine generic.
pub trait ErasedEventSink: Send + Sync + 'static {
    fn begin_span(&self, name: &'static str) -> ErasedSpan;
    fn begin_span_with_args(&self, name: &'static str, args: EventArgs) -> ErasedSpan;
    fn instant(&self, name: &'static str);
    fn instant_with_args(&self, name: &'static str, args: EventArgs);
    fn flow(&self, from: &'static str, to: &'static str);
}

/// A type-erased span guard. Calls the inner drop on drop.
pub struct ErasedSpan(Option<Box<dyn Send>>);

impl Drop for ErasedSpan {
    fn drop(&mut self) {
        // The inner value's Drop impl records the end event.
        drop(self.0.take());
    }
}

impl<S: EventSink> ErasedEventSink for S
where
    S::Span: 'static,
{
    fn begin_span(&self, name: &'static str) -> ErasedSpan {
        ErasedSpan(Some(Box::new(EventSink::begin_span(self, name))))
    }

    fn begin_span_with_args(&self, name: &'static str, args: EventArgs) -> ErasedSpan {
        ErasedSpan(Some(Box::new(EventSink::begin_span_with_args(
            self, name, args,
        ))))
    }

    fn instant(&self, name: &'static str) {
        EventSink::instant(self, name);
    }

    fn instant_with_args(&self, name: &'static str, args: EventArgs) {
        EventSink::instant_with_args(self, name, args);
    }

    fn flow(&self, from: &'static str, to: &'static str) {
        EventSink::flow(self, from, to);
    }
}

fn write_trace_args(f: &mut impl std::io::Write, args: Option<&EventArgs>) -> std::io::Result<()> {
    let Some(args) = args else {
        return Ok(());
    };
    let json = facet_json::to_string(args).map_err(|error| {
        std::io::Error::other(format!("failed to serialize trace args: {error}"))
    })?;
    write!(f, ",\"args\":{json}")
}

#[cfg(test)]
mod tests {
    use facet_value::value;

    use super::*;

    #[test]
    fn chrome_trace_includes_span_args() {
        let recorder = EventRecorder::new();
        let span = EventSink::begin_span_with_args(
            &recorder,
            "node.execute",
            value!({
                "node_id": 7,
                "description": "compile:cc"
            }),
        );
        drop(span);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("trace.json");
        recorder
            .write_chrome_trace(&path)
            .expect("write chrome trace");

        let trace = std::fs::read_to_string(path).expect("read trace");
        assert!(
            trace.contains("\"name\":\"node.execute\""),
            "trace: {trace}"
        );
        assert!(trace.contains("\"ph\":\"X\""), "trace: {trace}");
        assert!(trace.contains("\"args\":{\"node_id\":7"), "trace: {trace}");
        assert!(
            trace.contains("\"description\":\"compile:cc\""),
            "trace: {trace}"
        );
    }

    #[test]
    fn complete_events_do_not_emit_begin_end_pairs() {
        let recorder = EventRecorder::new();
        let outer = EventSink::begin_span(&recorder, "outer");
        let inner = EventSink::begin_span(&recorder, "inner");
        drop(outer);
        drop(inner);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("trace.json");
        recorder
            .write_chrome_trace(&path)
            .expect("write chrome trace");

        let trace = std::fs::read_to_string(path).expect("read trace");
        assert!(trace.contains("\"ph\":\"X\""), "trace: {trace}");
        assert!(!trace.contains("\"ph\":\"B\""), "trace: {trace}");
        assert!(!trace.contains("\"ph\":\"E\""), "trace: {trace}");
    }
}
