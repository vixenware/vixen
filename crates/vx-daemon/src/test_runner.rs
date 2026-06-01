//! Daemon-side test runner.
//!
//! Discovers the annotated test functions in the shipped sources and runs each
//! through **vx-runtime** — the demand-driven evaluator, the daemon's sole
//! evaluation entry point now that the eager `runtime_eval` is gone. Pushes the
//! firehose of `Event`s onto the caller's `Tx`.
//!
//! Emits `RunStarted`, `CaseStarted`, `CaseFinished` (raw outcome + the computed
//! verdict), and `RunFinished`. (Per-phase compile timings rode the old eager
//! compiler's `CompileObserver`; vx-runtime's phases differ, so they return when
//! it exposes them. `assert`/`assert_eq` are stdlib now, so no prelude is
//! prepended.)

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use tokio::sync::mpsc::UnboundedSender;
use vox::Tx;
use vx_parser::{Item, SourceId, parse_source_file};
use vx_runtime::eval::{Outcome, Value};
use vx_services::{
    EvalOutcome, Event, EventBody, ExecutorId, Expectation, HostInfo, RunSummary, RunTestsRequest,
    RuntimeValue, SpanId, TestId, TestRunner, TestSource, TypedValue, Verdict,
};

/// Bounded concurrency for case evaluation. (vx-runtime's evaluator is
/// synchronous and `!Send`, so cases currently run to completion in turn; the
/// bound is retained for when strand-scheduled concurrency lands.)
const MAX_CONCURRENCY: usize = 16;

#[derive(Clone, Default)]
pub struct TestRunnerService;

impl TestRunnerService {
    pub fn new() -> Self {
        Self
    }
}

/// One discovered test, with everything needed to run it.
struct Case {
    id: TestId,
    expectation: Expectation,
    /// The file's full source — the closure the test runs over.
    source: String,
    function: String,
}

/// Parse each source and collect its annotated test functions.
fn discover(sources: &[TestSource]) -> Vec<Case> {
    let mut cases = Vec::new();
    for source in sources {
        let parsed = parse_source_file(&source.text, SourceId(0));
        if !parsed.errors().is_empty() {
            // A file that doesn't even parse can't host tests; skip discovery.
            continue;
        }
        for item in parsed.tree().items() {
            let Item::FnDecl(func) = item else { continue };
            let Some(expectation) = test_kind(&func) else {
                continue;
            };
            let Some(function) = func.name().map(|t| t.text().to_string()) else {
                continue;
            };
            cases.push(Case {
                id: TestId {
                    path: source.path.clone(),
                    function: function.clone(),
                },
                expectation,
                source: source.text.clone(),
                function,
            });
        }
    }
    cases
}

/// `#[test]` → Clean, `#[test(poisons)]` → Poisons, anything else → not a test.
fn test_kind(func: &vx_parser::FnDecl) -> Option<Expectation> {
    let list = func.attributes()?;
    let mut is_test = false;
    let mut poisons = false;
    for attr in list.attributes() {
        for entry in attr.entries() {
            if entry.key().map(|t| t.text().to_string()).as_deref() == Some("test") {
                is_test = true;
                for arg in entry.args() {
                    if arg.key().map(|t| t.text().to_string()).as_deref() == Some("poisons") {
                        poisons = true;
                    }
                }
            }
        }
    }
    if !is_test {
        return None;
    }
    Some(if poisons {
        Expectation::Poisons
    } else {
        Expectation::Clean
    })
}

/// Map a vx-runtime evaluation to the structured outcome. Clean → the value; a
/// poison (a `fail`, a forced `💣`, a failed assert) → Poisoned; a setup failure
/// (parse / resolve / type error, missing entry) → Errored.
fn outcome_of(result: Result<Outcome, String>, source: &str) -> EvalOutcome {
    match result {
        Ok(Outcome::Clean(value)) => EvalOutcome::Clean {
            value: typed_value(map_value(value)),
        },
        Ok(Outcome::Poisoned { reason, .. }) => EvalOutcome::Poisoned {
            diagnostics: one_note("test poisoned", &reason, source),
        },
        Err(setup) => EvalOutcome::Errored {
            diagnostics: one_note("compile error", &setup, source),
        },
    }
}

/// The pass/fail rule, computed once so reporters never re-derive it.
fn verdict_of(expectation: Expectation, outcome: &EvalOutcome) -> Verdict {
    match (expectation, outcome) {
        (Expectation::Clean, EvalOutcome::Clean { .. }) => Verdict::Pass,
        (Expectation::Poisons, EvalOutcome::Poisoned { .. }) => Verdict::Pass,
        // Clean-but-poisoned, poisons-but-clean, and any error all fail.
        _ => Verdict::Fail,
    }
}

/// A vx-runtime value mapped onto the RPC value model (for display). Scalars map
/// directly; a compound renders to a placeholder — a well-formed test ends in
/// `assert_eq`, so its result is a `Bool`, and the verdict is clean-vs-poison
/// regardless of the value.
fn map_value(value: Value) -> RuntimeValue {
    match value {
        Value::U64(n) => RuntimeValue::U64(n),
        Value::Bool(b) => RuntimeValue::Bool(b),
        Value::Str(s) => RuntimeValue::String(s.to_string()),
        Value::Unit => RuntimeValue::String("()".to_string()),
        Value::Tuple(_) => RuntimeValue::String("(…)".to_string()),
        Value::Array(_) => RuntimeValue::String("[…]".to_string()),
        Value::Struct(_) => RuntimeValue::String("{…}".to_string()),
        Value::Closure { .. } => RuntimeValue::String("<closure>".to_string()),
        Value::Command(_) => RuntimeValue::String("<command>".to_string()),
        Value::Grammars(_) => RuntimeValue::String("<grammars>".to_string()),
        Value::Grammar { .. } => RuntimeValue::String("<grammar>".to_string()),
    }
}

/// A one-note error diagnostic over `source` — the simple form for a poison
/// reason or a setup-error string. (Rich compile-error rendering returns when
/// vx-runtime's front-end exposes structured errors rather than a `String`.)
fn one_note(title: &str, detail: &str, source: &str) -> margin::Diagnostics {
    use margin::{Diagnostics, Note, NoteKind, Report, Severity, Source, SourceId};
    Diagnostics {
        sources: vec![Source {
            id: SourceId("0".to_string()),
            name: "input.vx".to_string(),
            hyperlink: None,
            text: source.to_string(),
        }],
        reports: vec![Report {
            severity: Severity::Error,
            title: title.to_string(),
            annotations: Vec::new(),
            notes: vec![Note {
                kind: NoteKind::Note,
                text: detail.to_string(),
            }],
            sections: Vec::new(),
        }],
    }
}

fn typed_value(value: RuntimeValue) -> TypedValue {
    TypedValue {
        ty: runtime_kind(&value).to_string(),
        rendered: render_value(&value),
        value,
    }
}

fn runtime_kind(value: &RuntimeValue) -> &'static str {
    match value {
        RuntimeValue::U64(_) => "U64",
        RuntimeValue::Bool(_) => "Bool",
        RuntimeValue::String(_) => "String",
        RuntimeValue::Path(_) => "Path",
        RuntimeValue::StringArray(_) => "Array[String]",
        RuntimeValue::StringMap(_) => "Map[String, String]",
        RuntimeValue::Tree(_) => "Tree",
    }
}

fn render_value(value: &RuntimeValue) -> String {
    match value {
        RuntimeValue::U64(v) => v.to_string(),
        RuntimeValue::Bool(v) => v.to_string(),
        RuntimeValue::String(v) => v.clone(),
        RuntimeValue::Path(v) => v.clone(),
        RuntimeValue::StringArray(v) => format!("{v:?}"),
        RuntimeValue::StringMap(v) => format!("{v:?}"),
        RuntimeValue::Tree(h) => format!("tree:{}", h.to_hex()),
    }
}

/// Best-effort host characteristics for the primary executor.
fn local_host() -> HostInfo {
    HostInfo {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        hostname: std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string()),
        kernel: "unknown".to_string(),
        cpus: std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(0),
        memory_bytes: 0,
    }
}

/// A synchronous, cloneable event sink. Everything funnels through an unbounded
/// channel; a single async forwarder drains it to the wire. One push point,
/// sync, shareable.
#[derive(Clone)]
struct EventSink {
    tx: UnboundedSender<Event>,
    seq: Arc<AtomicU64>,
    start: Instant,
}

impl EventSink {
    fn emit(&self, span: SpanId, body: EventBody) {
        let event = Event {
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
            at_ns: self.start.elapsed().as_nanos() as u64,
            wall_unix_nanos: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
            executor: ExecutorId(0),
            span,
            parent: None,
            body,
        };
        let _ = self.tx.send(event);
    }
}

impl TestRunner for TestRunnerService {
    async fn run_tests(&self, request: RunTestsRequest, events: Tx<Event>) -> RunSummary {
        let start = Instant::now();

        // The emission seam: synchronous code pushes onto this unbounded channel;
        // one async forwarder drains it to the vox `Tx`.
        let (raw_tx, mut raw_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
        let forwarder = tokio::spawn(async move {
            while let Some(event) = raw_rx.recv().await {
                let _ = events.send(event).await;
            }
        });
        let sink = EventSink {
            tx: raw_tx,
            seq: Arc::new(AtomicU64::new(0)),
            start,
        };

        let cases = discover(&request.sources);
        let discovered: Vec<TestId> = cases.iter().map(|c| c.id.clone()).collect();

        sink.emit(
            SpanId(0),
            EventBody::RunStarted {
                discovered,
                runner_host: local_host(),
            },
        );

        let verdicts: Vec<Verdict> = futures_util::stream::iter(cases.into_iter().enumerate())
            .map(|(index, case)| {
                // Per-case span (1-based; 0 is the run itself) ties events back.
                let span = SpanId(index as u64 + 1);
                let sink = sink.clone();
                async move {
                    sink.emit(
                        span,
                        EventBody::CaseStarted {
                            id: case.id.clone(),
                            expectation: case.expectation,
                            closure: None,
                            topology: vx_services::Topology {
                                executors: vec![vx_services::ExecutorSpec {
                                    name: "primary".to_string(),
                                    host_constraint: None,
                                }],
                            },
                        },
                    );

                    let case_start = Instant::now();
                    // Evaluate on vx-runtime. The result is consumed into the
                    // (Send) `EvalOutcome` here, before any emission.
                    let outcome = outcome_of(
                        vx_runtime::eval::run(&case.source, &case.function),
                        &case.source,
                    );
                    let verdict = verdict_of(case.expectation, &outcome);

                    sink.emit(
                        span,
                        EventBody::CaseFinished {
                            id: case.id.clone(),
                            verdict,
                            terminal: outcome,
                            wall_ms: case_start.elapsed().as_millis() as u64,
                        },
                    );

                    verdict
                }
            })
            .buffer_unordered(MAX_CONCURRENCY)
            .collect()
            .await;

        let passed = verdicts
            .iter()
            .filter(|v| matches!(v, Verdict::Pass))
            .count() as u64;
        let failed = verdicts.len() as u64 - passed;
        let wall_ms = start.elapsed().as_millis() as u64;

        sink.emit(
            SpanId(0),
            EventBody::RunFinished {
                passed,
                failed,
                errored: 0,
                wall_ms,
            },
        );

        // Close the sink so the forwarder drains and exits before we return.
        drop(sink);
        let _ = forwarder.await;

        RunSummary {
            passed,
            failed,
            errored: 0,
            wall_ms,
        }
    }
}
