//! Per-engine event recording for tracing, profiling, and test assertions.
//!
//! The `EventSink` trait is implemented by the engine's generic parameter.
//! In production, `()` is used — all methods are no-ops that optimize away.
//! In tests, `EventRecorder` captures events for assertions.

use std::cell::RefCell;
use std::future::Future;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub type EventArgs = facet_value::Value;

tokio::task_local! {
    static TRACE_TRACK_STACK: RefCell<Vec<u64>>;
}

thread_local! {
    static FALLBACK_TRACE_TRACK_STACK: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

pub async fn with_fresh_trace_context<T>(future: impl Future<Output = T>) -> T {
    TRACE_TRACK_STACK
        .scope(RefCell::new(Vec::new()), future)
        .await
}

/// Trait for recording structured engine events.
///
/// Generic over the Engine so each engine instance can have its own sink.
/// The associated `Span` type is returned by `begin_span` and emits an
/// end event on drop.
pub trait EventSink: Send + Sync + 'static {
    type Span: Send;

    /// Whether this sink wants structured span arguments computed.
    fn supports_args(&self) -> bool {
        true
    }

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
    fn supports_args(&self) -> bool {
        false
    }
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
    pub track_id: u64,
    pub timestamp_us: u64,
    pub sequence: u64,
    pub args: Option<EventArgs>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventPhase {
    Begin,
    End,
    Instant,
    Flow { target: &'static str },
}

/// Shared state for the event recorder.
struct RecorderInner {
    start: Instant,
    next_track_id: AtomicU64,
    next_sequence: AtomicU64,
    free_track_ids: Mutex<Vec<u64>>,
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
        track_id: u64,
        timestamp_us: u64,
        args: Option<EventArgs>,
    ) {
        self.events.lock().unwrap().push(RecordedEvent {
            name,
            phase,
            track_id,
            timestamp_us,
            sequence: self.next_sequence.fetch_add(1, Ordering::Relaxed),
            args,
        });
    }

    fn push_now(
        &self,
        name: &'static str,
        phase: EventPhase,
        track_id: u64,
        args: Option<EventArgs>,
    ) {
        self.push(name, phase, track_id, self.now_us(), args);
    }

    fn acquire_track_id(&self) -> u64 {
        self.free_track_ids
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| self.next_track_id.fetch_add(1, Ordering::Relaxed))
    }

    fn release_track_id(&self, track_id: u64) {
        if track_id == 0 {
            return;
        }
        self.free_track_ids.lock().unwrap().push(track_id);
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
                next_track_id: AtomicU64::new(1),
                next_sequence: AtomicU64::new(0),
                free_track_ids: Mutex::new(Vec::new()),
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
            .filter(|e| e.name == name && e.phase == EventPhase::Begin)
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

    pub fn begin_detached_span(&self, track_id: u64, name: &'static str) -> DetachedRecorderSpan {
        self.inner.push_now(name, EventPhase::Begin, track_id, None);
        DetachedRecorderSpan {
            inner: Arc::clone(&self.inner),
            name,
            track_id,
        }
    }

    pub fn begin_detached_span_with_args(
        &self,
        track_id: u64,
        name: &'static str,
        args: EventArgs,
    ) -> DetachedRecorderSpan {
        self.inner
            .push_now(name, EventPhase::Begin, track_id, Some(args));
        DetachedRecorderSpan {
            inner: Arc::clone(&self.inner),
            name,
            track_id,
        }
    }

    pub fn instant_on_track(&self, track_id: u64, name: &'static str) {
        self.inner
            .push_now(name, EventPhase::Instant, track_id, None);
    }

    pub fn instant_on_track_with_args(&self, track_id: u64, name: &'static str, args: EventArgs) {
        self.inner
            .push_now(name, EventPhase::Instant, track_id, Some(args));
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
        events.sort_by_key(|event| (event.timestamp_us, event.sequence));
        let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
        write!(f, "{{\"displayTimeUnit\":\"ms\",\"traceEvents\":[")?;
        let mut first = true;
        for track_id in track_ids(&events) {
            if !first {
                write!(f, ",")?;
            }
            first = false;
            let track_name = if track_id == 0 {
                "global".to_string()
            } else {
                format!("lane {track_id}")
            };
            write!(
                f,
                "{{\"ph\":\"M\",\"name\":\"thread_name\",\"pid\":1,\"tid\":{},\"args\":{{\"name\":\"{}\"}}}}",
                track_id, track_name
            )?;
        }
        for event in events.iter() {
            if !first {
                write!(f, ",")?;
            }
            first = false;
            match event.phase {
                EventPhase::Begin => {
                    write!(
                        f,
                        "{{\"ph\":\"B\",\"name\":\"{}\",\"cat\":\"eval\",\"ts\":{},\"pid\":1,\"tid\":{}",
                        event.name, event.timestamp_us, event.track_id
                    )?;
                    write_trace_args(&mut f, event.args.as_ref())?;
                    write!(f, "}}")?;
                }
                EventPhase::End => {
                    write!(
                        f,
                        "{{\"ph\":\"E\",\"name\":\"{}\",\"cat\":\"eval\",\"ts\":{},\"pid\":1,\"tid\":{}}}",
                        event.name, event.timestamp_us, event.track_id
                    )?;
                }
                EventPhase::Instant => {
                    write!(
                        f,
                        "{{\"ph\":\"i\",\"name\":\"{}\",\"cat\":\"eval\",\"s\":\"t\",\"ts\":{},\"pid\":1,\"tid\":{}",
                        event.name, event.timestamp_us, event.track_id
                    )?;
                    write_trace_args(&mut f, event.args.as_ref())?;
                    write!(f, "}}")?;
                }
                EventPhase::Flow { target } => {
                    write!(
                        f,
                        "{{\"ph\":\"i\",\"name\":\"{}→{}\",\"cat\":\"flow\",\"s\":\"t\",\"ts\":{},\"pid\":1,\"tid\":{}}}",
                        event.name, target, event.timestamp_us, event.track_id
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
    track_id: u64,
    owns_track: bool,
}

impl Drop for RecorderSpan {
    fn drop(&mut self) {
        pop_track(self.track_id);
        self.inner
            .push_now(self.name, EventPhase::End, self.track_id, None);
        if self.owns_track {
            self.inner.release_track_id(self.track_id);
        }
    }
}

pub struct DetachedRecorderSpan {
    inner: Arc<RecorderInner>,
    name: &'static str,
    track_id: u64,
}

impl Drop for DetachedRecorderSpan {
    fn drop(&mut self) {
        self.inner
            .push_now(self.name, EventPhase::End, self.track_id, None);
    }
}

impl EventSink for EventRecorder {
    type Span = RecorderSpan;

    fn begin_span(&self, name: &'static str) -> RecorderSpan {
        let (track_id, owns_track) = push_track(&self.inner);
        self.inner.push_now(name, EventPhase::Begin, track_id, None);
        RecorderSpan {
            inner: Arc::clone(&self.inner),
            name,
            track_id,
            owns_track,
        }
    }

    fn begin_span_with_args(&self, name: &'static str, args: EventArgs) -> RecorderSpan {
        let (track_id, owns_track) = push_track(&self.inner);
        self.inner
            .push_now(name, EventPhase::Begin, track_id, Some(args));
        RecorderSpan {
            inner: Arc::clone(&self.inner),
            name,
            track_id,
            owns_track,
        }
    }

    fn instant(&self, name: &'static str) {
        self.inner
            .push_now(name, EventPhase::Instant, current_track_id(), None);
    }

    fn instant_with_args(&self, name: &'static str, args: EventArgs) {
        self.inner
            .push_now(name, EventPhase::Instant, current_track_id(), Some(args));
    }

    fn flow(&self, from: &'static str, to: &'static str) {
        self.inner.push_now(
            from,
            EventPhase::Flow { target: to },
            current_track_id(),
            None,
        );
    }
}

// --- Type-erased wrapper ---

/// Type-erased event sink. The span is a boxed trait object.
/// Slightly more overhead than the generic version, but avoids
/// making Engine generic.
pub trait ErasedEventSink: Send + Sync + 'static {
    fn supports_args(&self) -> bool;
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
    fn supports_args(&self) -> bool {
        EventSink::supports_args(self)
    }

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

pub fn current_trace_track_id() -> u64 {
    current_track_id()
}

fn current_track_id() -> u64 {
    TRACE_TRACK_STACK
        .try_with(|stack| stack.borrow().last().copied().unwrap_or(0))
        .unwrap_or_else(|_| {
            FALLBACK_TRACE_TRACK_STACK.with(|stack| stack.borrow().last().copied().unwrap_or(0))
        })
}

fn push_track(inner: &RecorderInner) -> (u64, bool) {
    TRACE_TRACK_STACK
        .try_with(|stack| {
            let mut stack = stack.borrow_mut();
            let (track_id, owns_track) = match stack.last().copied() {
                Some(track_id) => (track_id, false),
                None => (inner.acquire_track_id(), true),
            };
            stack.push(track_id);
            (track_id, owns_track)
        })
        .unwrap_or_else(|_| {
            FALLBACK_TRACE_TRACK_STACK.with(|stack| {
                let mut stack = stack.borrow_mut();
                let (track_id, owns_track) = match stack.last().copied() {
                    Some(track_id) => (track_id, false),
                    None => (inner.acquire_track_id(), true),
                };
                stack.push(track_id);
                (track_id, owns_track)
            })
        })
}

fn pop_track(expected_track_id: u64) {
    TRACE_TRACK_STACK
        .try_with(|stack| {
            let popped = stack.borrow_mut().pop();
            debug_assert_eq!(popped, Some(expected_track_id));
        })
        .unwrap_or_else(|_| {
            FALLBACK_TRACE_TRACK_STACK.with(|stack| {
                let popped = stack.borrow_mut().pop();
                debug_assert_eq!(popped, Some(expected_track_id));
            });
        });
}

fn track_ids(events: &[RecordedEvent]) -> Vec<u64> {
    let mut track_ids = events
        .iter()
        .map(|event| event.track_id)
        .collect::<Vec<_>>();
    track_ids.sort_unstable();
    track_ids.dedup();
    track_ids
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
        assert!(trace.contains("\"ph\":\"B\""), "trace: {trace}");
        assert!(trace.contains("\"ph\":\"E\""), "trace: {trace}");
        assert!(trace.contains("\"args\":{\"node_id\":7"), "trace: {trace}");
        assert!(
            trace.contains("\"description\":\"compile:cc\""),
            "trace: {trace}"
        );
    }

    #[test]
    fn nested_spans_emit_begin_end_pairs() {
        let recorder = EventRecorder::new();
        let outer = EventSink::begin_span(&recorder, "outer");
        let inner = EventSink::begin_span(&recorder, "inner");
        drop(inner);
        drop(outer);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("trace.json");
        recorder
            .write_chrome_trace(&path)
            .expect("write chrome trace");

        let trace = std::fs::read_to_string(path).expect("read trace");
        assert!(trace.contains("\"ph\":\"B\""), "trace: {trace}");
        assert!(trace.contains("\"ph\":\"E\""), "trace: {trace}");
        assert!(!trace.contains("\"ph\":\"X\""), "trace: {trace}");
    }

    #[test]
    fn sequential_top_level_spans_reuse_track_ids() {
        let recorder = EventRecorder::new();

        let first = EventSink::begin_span(&recorder, "first");
        drop(first);
        let second = EventSink::begin_span(&recorder, "second");
        drop(second);

        let begin_track_ids = recorder
            .events()
            .into_iter()
            .filter(|event| event.phase == EventPhase::Begin)
            .map(|event| event.track_id)
            .collect::<Vec<_>>();

        assert_eq!(begin_track_ids, vec![1, 1]);
    }

    #[test]
    fn detached_spans_can_share_the_current_track() {
        let recorder = EventRecorder::new();

        let outer = EventSink::begin_span(&recorder, "outer");
        let track_id = current_trace_track_id();
        let inner = recorder.begin_detached_span(track_id, "inner");
        drop(inner);
        drop(outer);

        let begin_track_ids = recorder
            .events()
            .into_iter()
            .filter(|event| event.phase == EventPhase::Begin)
            .map(|event| event.track_id)
            .collect::<Vec<_>>();

        assert_eq!(begin_track_ids, vec![1, 1]);
    }
}
