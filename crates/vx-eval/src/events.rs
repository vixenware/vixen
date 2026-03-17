//! Per-engine event recording for tracing, profiling, and test assertions.
//!
//! The `EventSink` trait is implemented by the engine's generic parameter.
//! In production, `()` is used — all methods are no-ops that optimize away.
//! In tests, `EventRecorder` captures events for assertions.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

/// Trait for recording structured engine events.
///
/// Generic over the Engine so each engine instance can have its own sink.
/// The associated `Span` type is returned by `begin_span` and emits an
/// end event on drop.
pub trait EventSink: Send + Sync + 'static {
    type Span: Send;

    /// Start a duration span. The returned guard emits an end event on drop.
    fn begin_span(&self, name: &'static str) -> Self::Span;

    /// Record a point-in-time event.
    fn instant(&self, name: &'static str);

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
    events: Mutex<Vec<RecordedEvent>>,
}

impl RecorderInner {
    fn now_us(&self) -> u64 {
        self.start.elapsed().as_micros() as u64
    }

    fn push(&self, name: &'static str, phase: EventPhase) {
        self.events.lock().unwrap().push(RecordedEvent {
            name,
            phase,
            timestamp_us: self.now_us(),
        });
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
}

impl Default for EventRecorder {
    fn default() -> Self {
        Self::new()
    }
}

/// Guard that records an end event on drop.
pub struct RecorderSpan {
    inner: Arc<RecorderInner>,
    name: &'static str,
}

impl Drop for RecorderSpan {
    fn drop(&mut self) {
        self.inner.push(self.name, EventPhase::End);
    }
}

impl EventSink for EventRecorder {
    type Span = RecorderSpan;

    fn begin_span(&self, name: &'static str) -> RecorderSpan {
        self.inner.push(name, EventPhase::Begin);
        RecorderSpan {
            inner: Arc::clone(&self.inner),
            name,
        }
    }

    fn instant(&self, name: &'static str) {
        self.inner.push(name, EventPhase::Instant);
    }

    fn flow(&self, from: &'static str, to: &'static str) {
        self.inner.push(from, EventPhase::Flow { target: to });
    }
}

// --- Type-erased wrapper ---

/// Type-erased event sink. The span is a boxed trait object.
/// Slightly more overhead than the generic version, but avoids
/// making Engine generic.
pub trait ErasedEventSink: Send + Sync + 'static {
    fn begin_span(&self, name: &'static str) -> ErasedSpan;
    fn instant(&self, name: &'static str);
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

    fn instant(&self, name: &'static str) {
        EventSink::instant(self, name);
    }

    fn flow(&self, from: &'static str, to: &'static str) {
        EventSink::flow(self, from, to);
    }
}
