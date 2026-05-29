//! Daemon-side test runner.
//!
//! Discovers the annotated test functions in the shipped sources, runs each
//! through the **real** evaluation path (the same `evaluate_source_text_function`
//! that `vx eval` uses, so a test that drives `exec` exercises the actual
//! sandbox), schedules them concurrently, and pushes the firehose of `Event`s
//! onto the caller's `Tx`. See `docs/design/test-runner.md`.
//!
//! This is the lifecycle slice of the firehose: it emits `RunStarted`,
//! `CaseStarted`, `CaseFinished` (with the raw outcome + the computed verdict),
//! and `RunFinished`. The richer bodies (per-phase timings, assertions as typed
//! claims, multi-executor) hang off the same stream as the emission seam lands.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use tokio::sync::mpsc::UnboundedSender;
use vox::Tx;
use vx_parser::{Item, SourceId, parse_source_file};
use vx_services::{
    EvalOutcome, Event, EventBody, ExecutorId, Expectation, HostInfo, Phase, PhaseDetail,
    RunSummary, RunTestsRequest, RuntimeValue, SpanId, TestId, TestRunner, TestSource, TypedValue,
    Verdict,
};

use crate::orchestrator::OrchestratorService;
use crate::runtime_eval::{RuntimeEvalError, runtime_error_to_diagnostics};

/// Prepended to every source so assertions are themselves Vixen.
const PRELUDE: &str = r#"
fn assert(cond: Bool) -> Bool (
    if cond ( true ) else ( fail "assertion failed" )
)

fn assert_eq[T](got: T, want: T) -> Bool (
    if got == want ( true ) else ( fail "not equal" )
)
"#;

/// Bounded concurrency for case evaluation. The daemon owns scheduling.
const MAX_CONCURRENCY: usize = 16;

#[derive(Clone)]
pub struct TestRunnerService {
    orchestrator: OrchestratorService,
}

impl TestRunnerService {
    pub fn new(orchestrator: OrchestratorService) -> Self {
        Self { orchestrator }
    }
}

/// One discovered test, with everything needed to run it.
struct Case {
    id: TestId,
    expectation: Expectation,
    /// Prelude + the file's full source — the closure the test runs over.
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
            // (It will surface as zero cases — a louder signal lands with the
            // file-level error event in a later pass.)
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
                source: format!("{PRELUDE}\n{}", source.text),
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

/// Classify a raw eval result into the structured outcome.
fn outcome_of(result: Result<RuntimeValue, RuntimeEvalError>, source: &str) -> EvalOutcome {
    match result {
        Ok(value) => EvalOutcome::Clean {
            value: typed_value(value),
        },
        // A `fail` or a forced `💣` (which surfaces as a pending value) is a
        // poison: the program ran and detonated.
        Err(
            err @ (RuntimeEvalError::FailureResult { .. } | RuntimeEvalError::PendingResult { .. }),
        ) => EvalOutcome::Poisoned {
            diagnostics: runtime_error_to_diagnostics(&err, source, None),
        },
        // Anything else (compile/parse/type/unsupported) means the test never
        // ran cleanly — it errored.
        Err(err) => EvalOutcome::Errored {
            diagnostics: runtime_error_to_diagnostics(&err, source, None),
        },
    }
}

/// The pass/fail rule, computed once so reporters never re-derive it.
fn verdict_of(expectation: Expectation, outcome: &EvalOutcome) -> Verdict {
    match (expectation, outcome) {
        (Expectation::Clean, EvalOutcome::Clean { .. }) => Verdict::Pass,
        (Expectation::Poisons, EvalOutcome::Poisoned { .. }) => Verdict::Pass,
        // Clean-but-poisoned, poisons-but-clean, and any compile error all fail.
        _ => Verdict::Fail,
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

/// A synchronous, cloneable event sink. Events born deep in synchronous code
/// (compile phases) can't `.await` a vox `Tx`, so everything funnels through an
/// unbounded channel here; a single async forwarder drains it to the wire. This
/// is the daemon side of the emission seam: one push point, sync, shareable.
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

/// Bridges the compiler's `CompileObserver` callbacks onto the firehose, tagging
/// every phase event with the case's span so reporters can attribute them.
struct PhaseEmitter {
    sink: EventSink,
    span: SpanId,
}

impl PhaseEmitter {
    fn finish(&self, phase: Phase, elapsed: Duration, detail: PhaseDetail) {
        self.sink.emit(self.span, EventBody::PhaseStarted { phase });
        self.sink.emit(
            self.span,
            EventBody::PhaseFinished {
                phase,
                duration_ns: elapsed.as_nanos() as u64,
                detail,
            },
        );
    }
}

impl vx_ir::compile::CompileObserver for PhaseEmitter {
    fn parsed(&mut self, elapsed: Duration, cst_nodes: u64, _bytes: u64) {
        self.finish(
            Phase::Parse,
            elapsed,
            PhaseDetail::Parse {
                cst_nodes,
                errors: 0,
            },
        );
    }
    fn name_resolved(&mut self, elapsed: Duration) {
        self.finish(
            Phase::NameResolve,
            elapsed,
            PhaseDetail::NameResolve {
                bindings: 0,
                unresolved: 0,
            },
        );
    }
    fn type_checked(&mut self, elapsed: Duration) {
        self.finish(
            Phase::TypeCheck,
            elapsed,
            PhaseDetail::TypeCheck {
                inferences: 0,
                errors: 0,
            },
        );
    }
    fn capability_checked(&mut self, elapsed: Duration) {
        self.finish(
            Phase::CapabilityCheck,
            elapsed,
            PhaseDetail::CapabilityCheck { required: vec![] },
        );
    }
    fn demand_analyzed(&mut self, elapsed: Duration) {
        self.finish(
            Phase::DemandGraph,
            elapsed,
            PhaseDetail::DemandGraph {
                nodes: 0,
                edges: 0,
                lazy_fields: 0,
                reachable_bodies: 0,
            },
        );
    }
    fn lowered(&mut self, elapsed: Duration, bodies: u64, kernel_ops: u64) {
        self.finish(
            Phase::Lower,
            elapsed,
            PhaseDetail::Lower { bodies, kernel_ops },
        );
    }
}

impl TestRunner for TestRunnerService {
    async fn run_tests(&self, request: RunTestsRequest, events: Tx<Event>) -> RunSummary {
        let start = Instant::now();

        // The emission seam: synchronous code pushes onto this unbounded
        // channel; one async forwarder drains it to the vox `Tx`.
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
                // Per-case span (1-based; 0 is the run itself) ties every phase
                // event for this case back to it, even under interleaving.
                let span = SpanId(index as u64 + 1);
                let sink = sink.clone();
                let this = self.clone();
                async move {
                    sink.emit(
                        span,
                        EventBody::CaseStarted {
                            id: case.id.clone(),
                            expectation: case.expectation,
                            // Content-addressed source closure lands with the
                            // store-backed seam; omitted here.
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
                    let mut emitter = PhaseEmitter {
                        sink: sink.clone(),
                        span,
                    };
                    let result = this
                        .orchestrator
                        .eval_raw_observed(&case.source, &case.function, &[], &mut emitter)
                        .await
                        .map(|out| out.value);
                    let outcome = outcome_of(result, &case.source);
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
