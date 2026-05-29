//! Test-runner service protocol.
//!
//! The runner produces **one stream of events** (`Tx<TestEvent>`); reporters
//! (terminal, JSON, the web UI) all fold that single stream. There is no
//! privileged output form — the stream is the source of truth, and the verdict
//! for each case is computed once, by the runner, and carried in the event so
//! no reporter re-derives the pass/fail rule.
//!
//! Tests run through the daemon's real evaluation path (the same one `vx eval`
//! uses), so a `.vx` test that drives `exec` exercises the actual sandbox. The
//! daemon owns scheduling — it dispatches cases concurrently and caches
//! results — so wall-clock comes from parallelism, not from bypassing it.

use facet::Facet;
use vox::Tx;

use crate::orchestrator::RuntimeValue;

/// Test-runner protocol version.
pub const TEST_RUNNER_PROTOCOL_VERSION: u32 = 1;

/// A source file submitted for testing. The CLI reads files and ships their
/// text; the daemon discovers the annotated test functions within.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TestSource {
    /// Logical path, for reporting (e.g. `lang/demand_basis.vx`).
    pub path: String,
    /// Full source text of the file.
    pub text: String,
}

/// A request to run the test functions discovered across `sources`.
#[derive(Debug, Clone, Facet)]
pub struct RunTestsRequest {
    pub sources: Vec<TestSource>,
}

/// Fully-qualified identity of one test case.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TestId {
    /// The `TestSource::path` the case came from.
    pub path: String,
    /// The test function's name.
    pub function: String,
}

/// What a test asserts about its own evaluation — read off its attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum Expectation {
    /// `#[test]` — must evaluate without poisoning.
    Clean,
    /// `#[test(poisons)]` — must poison (error-path test).
    Poisons,
}

/// What actually happened when a case was evaluated — the raw fact, before any
/// expectation is applied.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum EvalOutcome {
    /// Evaluated to a value without poisoning.
    Clean { value: RuntimeValue },
    /// Poisoned during evaluation (a `fail`, a forced `💣`, a runtime poison).
    Poisoned { diagnostics: margin::Diagnostics },
    /// Could not be compiled or run at all (parse / resolve / type error).
    Errored { diagnostics: margin::Diagnostics },
}

/// The pass/fail verdict, computed once by the runner from
/// `(Expectation, EvalOutcome)` so every reporter agrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum Verdict {
    Pass,
    Fail,
}

/// The one event stream. Reporters fold these; nothing else is authoritative.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum TestEvent {
    /// Emitted once, before any case, with the discovered case count.
    RunStarted { total: u64 },
    /// A case began evaluating.
    CaseStarted { id: TestId },
    /// A case finished. Carries the raw outcome *and* the computed verdict.
    CaseFinished {
        id: TestId,
        expectation: Expectation,
        outcome: EvalOutcome,
        verdict: Verdict,
        duration_ms: u64,
    },
    /// Emitted once, after every case.
    RunFinished {
        passed: u64,
        failed: u64,
        duration_ms: u64,
    },
}

/// Returned by `run_tests` for callers that only want the tally. The full story
/// is in the event stream; this is a convenience, derivable from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
pub struct RunSummary {
    pub passed: u64,
    pub failed: u64,
    pub duration_ms: u64,
}

#[vox::service]
pub trait TestRunner {
    /// Discover and run every annotated test across `request.sources`, pushing
    /// a `TestEvent` for each lifecycle point onto `events`, and returning the
    /// final tally.
    async fn run_tests(&self, request: RunTestsRequest, events: Tx<TestEvent>) -> RunSummary;
}
