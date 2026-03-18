//! Per-engine event recording for tracing, profiling, and test assertions.
//!
//! The `EventSink` trait is implemented by the engine's generic parameter.
//! In production, `()` is used — all methods are no-ops that optimize away.
//! In tests, `EventRecorder` captures events for assertions.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use prost::Message;

use crate::perfetto_proto_generated::{
    BuiltinClock, DebugAnnotation, ProcessDescriptor, ThreadDescriptor, Trace, TracePacket,
    TrackDescriptor, TrackEvent, debug_annotation, trace_packet, track_descriptor, track_event,
};

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
    track_metadata: Mutex<HashMap<u64, TrackMetadata>>,
    events: Mutex<Vec<RecordedEvent>>,
}

#[derive(Debug, Clone, Default)]
struct TrackMetadata {
    name: Option<String>,
    sort_index: Option<i64>,
    parent_track_id: Option<u64>,
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
        self.track_metadata.lock().unwrap().remove(&track_id);
    }

    fn set_track_metadata(
        &self,
        track_id: u64,
        name: Option<String>,
        sort_index: Option<i64>,
        parent_track_id: Option<u64>,
    ) {
        self.track_metadata.lock().unwrap().insert(
            track_id,
            TrackMetadata {
                name,
                sort_index,
                parent_track_id,
            },
        );
    }

    fn track_metadata(&self, track_id: u64) -> TrackMetadata {
        self.track_metadata
            .lock()
            .unwrap()
            .get(&track_id)
            .cloned()
            .unwrap_or_default()
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
                track_metadata: Mutex::new(HashMap::new()),
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

    pub fn acquire_detached_track(&self) -> DetachedTrack {
        DetachedTrack {
            inner: Arc::clone(&self.inner),
            track_id: self.inner.acquire_track_id(),
            release_on_drop: true,
        }
    }

    pub fn acquire_detached_track_with_metadata(
        &self,
        name: impl Into<String>,
        sort_index: i64,
        parent_track_id: Option<u64>,
    ) -> DetachedTrack {
        let track_id = self.inner.acquire_track_id();
        self.inner.set_track_metadata(
            track_id,
            Some(name.into()),
            Some(sort_index),
            parent_track_id,
        );
        DetachedTrack {
            inner: Arc::clone(&self.inner),
            track_id,
            release_on_drop: false,
        }
    }
}

impl Default for EventRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl EventRecorder {
    /// Write all recorded events as a native Perfetto protobuf trace file.
    pub fn write_perfetto_trace(&self, path: &std::path::Path) -> std::io::Result<()> {
        let mut events = self.inner.events.lock().unwrap().clone();
        events.sort_by_key(|event| (event.timestamp_us, event.sequence));
        let mut packets = Vec::new();
        packets.push(process_descriptor_packet());
        for track_id in track_ids(&events) {
            packets.push(track_descriptor_packet(
                track_id,
                self.inner.track_metadata(track_id),
            ));
        }
        for event in &events {
            packets.push(event_packet(event));
        }

        let trace = Trace { packet: packets };
        let encoded = trace.encode_to_vec();
        std::fs::write(path, encoded)
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

pub struct DetachedTrack {
    inner: Arc<RecorderInner>,
    track_id: u64,
    release_on_drop: bool,
}

impl DetachedTrack {
    pub fn id(&self) -> u64 {
        self.track_id
    }
}

impl Drop for DetachedTrack {
    fn drop(&mut self) {
        if self.release_on_drop {
            self.inner.release_track_id(self.track_id);
        }
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

fn trace_display_name<'a>(fallback: &'a str, args: Option<&'a EventArgs>) -> Cow<'a, str> {
    args.and_then(|args| {
        args.as_object()
            .and_then(|object| object.get("display_name"))
            .and_then(|value| value.as_string())
            .map(|value| Cow::Borrowed(value.as_str()))
    })
    .unwrap_or_else(|| Cow::Borrowed(fallback))
}

fn process_descriptor_packet() -> TracePacket {
    TracePacket {
        data: Some(trace_packet::Data::TrackDescriptor(TrackDescriptor {
            uuid: Some(process_uuid()),
            process: Some(ProcessDescriptor {
                pid: Some(1),
                process_name: Some("vixen".to_string()),
                ..ProcessDescriptor::default()
            }),
            static_or_dynamic_name: Some(track_descriptor::StaticOrDynamicName::Name(
                "vixen".to_string(),
            )),
            ..TrackDescriptor::default()
        })),
        ..TracePacket::default()
    }
}

fn track_descriptor_packet(track_id: u64, metadata: TrackMetadata) -> TracePacket {
    let track_name = metadata.name.unwrap_or_else(|| {
        if track_id == 0 {
            "global".to_string()
        } else {
            format!("lane {track_id}")
        }
    });
    TracePacket {
        data: Some(trace_packet::Data::TrackDescriptor(TrackDescriptor {
            uuid: Some(track_uuid(track_id)),
            parent_uuid: Some(
                metadata
                    .parent_track_id
                    .map(track_uuid)
                    .unwrap_or_else(process_uuid),
            ),
            thread: Some(ThreadDescriptor {
                pid: Some(1),
                tid: Some(track_tid(track_id)),
                thread_name: Some(track_name.clone()),
                legacy_sort_index: Some(
                    metadata.sort_index.unwrap_or((track_id as i64) * 10) as i32
                ),
                ..ThreadDescriptor::default()
            }),
            static_or_dynamic_name: Some(track_descriptor::StaticOrDynamicName::Name(track_name)),
            disallow_merging_with_system_tracks: Some(true),
            ..TrackDescriptor::default()
        })),
        ..TracePacket::default()
    }
}

fn event_packet(event: &RecordedEvent) -> TracePacket {
    let (name, phase, args) = match event.phase {
        EventPhase::Begin => (
            trace_display_name(event.name, event.args.as_ref()).into_owned(),
            track_event::Type::SliceBegin,
            event.args.as_ref(),
        ),
        EventPhase::End => (event.name.to_string(), track_event::Type::SliceEnd, None),
        EventPhase::Instant => (
            trace_display_name(event.name, event.args.as_ref()).into_owned(),
            track_event::Type::Instant,
            event.args.as_ref(),
        ),
        EventPhase::Flow { target } => (
            format!("{}→{}", event.name, target),
            track_event::Type::Instant,
            None,
        ),
    };
    TracePacket {
        timestamp: Some(us_to_ns(event.timestamp_us)),
        timestamp_clock_id: Some(BuiltinClock::Monotonic as u32),
        data: Some(trace_packet::Data::TrackEvent(TrackEvent {
            r#type: Some(phase as i32),
            track_uuid: Some(track_uuid(event.track_id)),
            name_field: Some(track_event::NameField::Name(name)),
            debug_annotations: args.map(debug_annotations_from_args).unwrap_or_default(),
            ..TrackEvent::default()
        })),
        ..TracePacket::default()
    }
}

fn debug_annotations_from_args(args: &EventArgs) -> Vec<DebugAnnotation> {
    let Some(object) = args.as_object() else {
        return vec![debug_annotation_for_value(None, args)];
    };
    object
        .iter()
        .map(|(key, value)| debug_annotation_for_value(Some(key.as_str()), value))
        .collect()
}

fn debug_annotation_for_value(name: Option<&str>, value: &EventArgs) -> DebugAnnotation {
    let mut annotation = DebugAnnotation::default();
    if let Some(name) = name {
        annotation.name_field = Some(debug_annotation::NameField::Name(name.to_string()));
    }
    if value.is_null() {
        annotation.value = Some(debug_annotation::Value::LegacyJsonValue("null".to_string()));
        return annotation;
    }
    if let Some(boolean) = value.as_bool() {
        annotation.value = Some(debug_annotation::Value::BoolValue(boolean));
        return annotation;
    }
    if let Some(number) = value.as_number() {
        if let Some(integer) = number.to_i64() {
            annotation.value = Some(debug_annotation::Value::IntValue(integer));
            return annotation;
        }
        if let Some(unsigned) = number.to_u64() {
            annotation.value = Some(debug_annotation::Value::UintValue(unsigned));
            return annotation;
        }
        annotation.value = Some(debug_annotation::Value::DoubleValue(number.to_f64_lossy()));
        return annotation;
    }
    if let Some(string) = value.as_string() {
        annotation.value = Some(debug_annotation::Value::StringValue(
            string.as_str().to_string(),
        ));
        return annotation;
    }
    if let Some(array) = value.as_array() {
        annotation.array_values = array
            .as_slice()
            .iter()
            .map(|item| debug_annotation_for_value(None, item))
            .collect();
        return annotation;
    }
    if let Some(object) = value.as_object() {
        annotation.dict_entries = object
            .iter()
            .map(|(key, item)| debug_annotation_for_value(Some(key.as_str()), item))
            .collect();
        return annotation;
    }
    annotation.value = Some(debug_annotation::Value::LegacyJsonValue(
        facet_json::to_string(value).unwrap_or_else(|_| format!("\"{:?}\"", value.value_type())),
    ));
    annotation
}

fn process_uuid() -> u64 {
    1
}

fn track_uuid(track_id: u64) -> u64 {
    1_000 + track_id
}

fn track_tid(track_id: u64) -> i32 {
    if track_id == 0 {
        0
    } else {
        i32::try_from(track_id).unwrap_or(i32::MAX)
    }
}

fn us_to_ns(timestamp_us: u64) -> u64 {
    timestamp_us.saturating_mul(1_000)
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
    use prost::Message;

    use super::*;
    use crate::perfetto_proto_generated::{Trace, trace_packet, track_descriptor, track_event};

    fn decode_trace(path: &std::path::Path) -> Trace {
        Trace::decode(std::fs::read(path).expect("read trace").as_slice()).expect("decode trace")
    }

    #[test]
    fn perfetto_trace_includes_span_args() {
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
        let path = dir.path().join("trace.pftrace");
        recorder
            .write_perfetto_trace(&path)
            .expect("write perfetto trace");

        let trace = decode_trace(&path);
        let begin = trace
            .packet
            .iter()
            .find_map(|packet| match packet.data.as_ref() {
                Some(trace_packet::Data::TrackEvent(event))
                    if event.r#type == Some(track_event::Type::SliceBegin as i32) =>
                {
                    Some(event)
                }
                _ => None,
            })
            .expect("slice begin");
        assert_eq!(
            begin.name_field,
            Some(track_event::NameField::Name("node.execute".to_string()))
        );
        assert!(begin.debug_annotations.iter().any(|annotation| {
            annotation.name_field
                == Some(
                    crate::perfetto_proto_generated::debug_annotation::NameField::Name(
                        "node_id".to_string(),
                    ),
                )
                && annotation.value
                    == Some(crate::perfetto_proto_generated::debug_annotation::Value::IntValue(7))
        }));
    }

    #[test]
    fn perfetto_trace_uses_display_name_when_present() {
        let recorder = EventRecorder::new();
        let span = EventSink::begin_span_with_args(
            &recorder,
            "node.execute",
            value!({
                "display_name": "compile(src/lua.c)",
                "node_id": 7
            }),
        );
        drop(span);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("trace.pftrace");
        recorder
            .write_perfetto_trace(&path)
            .expect("write perfetto trace");

        let trace = decode_trace(&path);
        let begin = trace
            .packet
            .iter()
            .find_map(|packet| match packet.data.as_ref() {
                Some(trace_packet::Data::TrackEvent(event))
                    if event.r#type == Some(track_event::Type::SliceBegin as i32) =>
                {
                    Some(event)
                }
                _ => None,
            })
            .expect("slice begin");
        assert_eq!(
            begin.name_field,
            Some(track_event::NameField::Name(
                "compile(src/lua.c)".to_string()
            ))
        );
    }

    #[test]
    fn perfetto_trace_emits_slice_begin_end_pairs() {
        let recorder = EventRecorder::new();
        let outer = EventSink::begin_span(&recorder, "outer");
        let inner = EventSink::begin_span(&recorder, "inner");
        drop(inner);
        drop(outer);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("trace.pftrace");
        recorder
            .write_perfetto_trace(&path)
            .expect("write perfetto trace");

        let trace = decode_trace(&path);
        let types = trace
            .packet
            .iter()
            .filter_map(|packet| match packet.data.as_ref() {
                Some(trace_packet::Data::TrackEvent(event)) => event.r#type,
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(types.contains(&(track_event::Type::SliceBegin as i32)));
        assert!(types.contains(&(track_event::Type::SliceEnd as i32)));
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

    #[test]
    fn perfetto_trace_preserves_parent_track_relationships() {
        let recorder = EventRecorder::new();
        let outer = EventSink::begin_span(&recorder, "outer");
        let parent_track_id = current_trace_track_id();
        let child_track = recorder.acquire_detached_track_with_metadata(
            "child".to_string(),
            42,
            Some(parent_track_id),
        );
        let child = recorder.begin_detached_span(child_track.id(), "child_span");
        drop(child);
        drop(outer);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("trace.pftrace");
        recorder
            .write_perfetto_trace(&path)
            .expect("write perfetto trace");

        let trace = decode_trace(&path);
        let descriptors = trace
            .packet
            .iter()
            .filter_map(|packet| match packet.data.as_ref() {
                Some(trace_packet::Data::TrackDescriptor(descriptor)) => Some(descriptor),
                _ => None,
            })
            .collect::<Vec<_>>();
        let child_track_uuid = track_uuid(child_track.id());
        let parent_track_uuid = track_uuid(parent_track_id);
        let child_descriptor = descriptors
            .into_iter()
            .find(|descriptor| descriptor.uuid == Some(child_track_uuid))
            .expect("child descriptor");
        assert_eq!(child_descriptor.parent_uuid, Some(parent_track_uuid));
        assert_eq!(
            child_descriptor.static_or_dynamic_name,
            Some(track_descriptor::StaticOrDynamicName::Name(
                "child".to_string()
            ))
        );
    }
}
