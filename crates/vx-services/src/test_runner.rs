//! Test-runner service protocol — a total-observability firehose.
//!
//! See `docs/design/test-runner.md`. The runner emits **everything** that
//! happens during a test, fully structured and timed, across every executor —
//! and discards nothing. Reporters (terminal, JSON, web UI, archival) fold the
//! same `Tx<Event>` stream and keep only what they care about. The verdict is
//! one terminal event among thousands, computed once and carried in-band.

use std::collections::BTreeMap;

use facet::Facet;
use vox::Tx;

use crate::orchestrator::RuntimeValue;
use crate::store::{BlobRef, NodeHash};

/// Test-runner protocol version.
pub const TEST_RUNNER_PROTOCOL_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Request & topology
// ---------------------------------------------------------------------------

/// A source file submitted for testing. The CLI reads files and ships their
/// text; the daemon discovers the annotated test functions within.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TestSource {
    /// Logical path, for reporting (e.g. `lang/demand_basis.vx`).
    pub path: String,
    /// Full source text.
    pub text: String,
}

#[derive(Debug, Clone, Facet)]
pub struct RunTestsRequest {
    pub sources: Vec<TestSource>,
}

/// Fully-qualified identity of one test case.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TestId {
    pub path: String,
    pub function: String,
}

/// What a test asserts about its own evaluation, read off its attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum Expectation {
    /// `#[test]` — must evaluate without poisoning.
    Clean,
    /// `#[test(poisons)]` — must poison (error-path test).
    Poisons,
}

/// The executors a test runs across — declared by `#[test(executors(...))]`.
/// A single-executor test has one entry (`primary`).
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct Topology {
    pub executors: Vec<ExecutorSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ExecutorSpec {
    /// Logical name used in the test (`primary`, `remote`, …).
    pub name: String,
    /// Optional pinned host characteristic, e.g. `"aarch64-linux"`.
    pub host_constraint: Option<String>,
}

// ---------------------------------------------------------------------------
// Addressing — big payloads travel as content-addressed refs, fetched on demand
// ---------------------------------------------------------------------------

/// An executor instance within a run. `0` is always the primary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Facet)]
pub struct ExecutorId(pub u32);

/// Causal/nesting context, so reporters rebuild the trace tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Facet)]
pub struct SpanId(pub u64);

/// A handle to a serialised structured value too large to inline.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ValueRef {
    pub hash: NodeHash,
}

/// A handle to a shipped closure (multi-executor).
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ClosureRef {
    pub hash: NodeHash,
}

// ---------------------------------------------------------------------------
// Host, values, diffs
// ---------------------------------------------------------------------------

/// Characteristics of an executor's host, captured when it joins the run.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct HostInfo {
    pub os: String,
    pub arch: String,
    pub hostname: String,
    pub kernel: String,
    pub cpus: u32,
    pub memory_bytes: u64,
}

/// A value plus its real inferred type and a rendered courtesy form. The
/// structured `value` — not a string — is what makes rich diffs possible.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TypedValue {
    pub ty: String,
    pub value: RuntimeValue,
    pub rendered: String,
}

/// A structured (rediff-style) diff between two values. Pragmatic for now —
/// `rendered` is always present; `differing_paths` localises the change (e.g.
/// `["b"]` for a struct whose `b` field differs) so a reporter can highlight.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ValueDiff {
    pub rendered: String,
    pub differing_paths: Vec<String>,
}

// ---------------------------------------------------------------------------
// Assertions — recorded claims, not booleans
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum AssertionKind {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Contains,
    StartsWith,
    Matches,
    IsVariant,
    /// A user-defined assertion, named.
    Custom(String),
}

/// A claim the program made, with both operands captured as typed values and a
/// structured diff when it fails. "We wanted *this*, we got *that*."
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct Assertion {
    pub kind: AssertionKind,
    /// The assertion expression, verbatim.
    pub source: String,
    pub operands: Vec<TypedValue>,
    pub passed: bool,
    pub diff: Option<ValueDiff>,
}

// ---------------------------------------------------------------------------
// Pipeline phases — every one timed
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum Phase {
    Lex,
    Parse,
    NameResolve,
    TypeCheck,
    CapabilityCheck,
    DemandGraph,
    Lower,
    Jit,
    Evaluate,
}

/// Per-phase structured detail, attached to `PhaseFinished`. The runner records
/// counts and artifacts; it never aggregates them into a "budget".
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum PhaseDetail {
    Lex {
        tokens: u64,
        bytes: u64,
    },
    Parse {
        cst_nodes: u64,
        errors: u64,
    },
    NameResolve {
        bindings: u64,
        unresolved: u64,
    },
    TypeCheck {
        inferences: u64,
        errors: u64,
    },
    CapabilityCheck {
        required: Vec<String>,
    },
    DemandGraph {
        nodes: u64,
        edges: u64,
        lazy_fields: u64,
        reachable_bodies: u64,
    },
    Lower {
        bodies: u64,
        kernel_ops: u64,
    },
    /// What the JIT produced for one kernel, not just how long it took.
    Jit {
        kernel: u32,
        strategy: String,
        code_bytes: u64,
    },
    Evaluate {},
    None,
}

// ---------------------------------------------------------------------------
// Terminal outcome & verdict
// ---------------------------------------------------------------------------

/// What actually happened — the raw fact, before expectation is applied.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum EvalOutcome {
    Clean { value: TypedValue },
    Poisoned { diagnostics: margin::Diagnostics },
    Errored { diagnostics: margin::Diagnostics },
}

/// Pass/fail, computed once by the runner from `(Expectation, EvalOutcome)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum Verdict {
    Pass,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(C)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

// ---------------------------------------------------------------------------
// The firehose
// ---------------------------------------------------------------------------

/// Every event is envelope + body. The envelope gives total order (`seq`),
/// timing (`at_ns` monotonic since run start), wall-clock (`wall_unix_nanos`,
/// for time-of-day), provenance (`executor`), and causal nesting
/// (`span`/`parent`).
#[derive(Debug, Clone, Facet)]
pub struct Event {
    pub seq: u64,
    pub at_ns: u64,
    pub wall_unix_nanos: u64,
    pub executor: ExecutorId,
    pub span: SpanId,
    pub parent: Option<SpanId>,
    pub body: EventBody,
}

/// The open, extensible taxonomy. New variants are additive; reporters ignore
/// unknown bodies. This list is *meant* to grow.
#[derive(Debug, Clone, Facet)]
#[repr(C)]
pub enum EventBody {
    // --- lifecycle bookends ---
    RunStarted {
        discovered: Vec<TestId>,
        runner_host: HostInfo,
    },
    CaseStarted {
        id: TestId,
        expectation: Expectation,
        closure: ClosureRef,
        topology: Topology,
    },
    CaseFinished {
        id: TestId,
        verdict: Verdict,
        terminal: EvalOutcome,
        wall_ms: u64,
    },
    RunFinished {
        passed: u64,
        failed: u64,
        errored: u64,
        wall_ms: u64,
    },

    // --- compilation pipeline (Started/Finished pairs; duration is a delta) ---
    PhaseStarted {
        phase: Phase,
    },
    PhaseFinished {
        phase: Phase,
        duration_ns: u64,
        detail: PhaseDetail,
    },

    // --- demand-driven evaluation ---
    SlotForced {
        slot: u32,
        kernel: u32,
    },
    SlotProduced {
        slot: u32,
        value: TypedValue,
    },
    CacheLookup {
        key: NodeHash,
        hit: bool,
    },
    Poisoned {
        diagnostics: margin::Diagnostics,
    },

    // --- runtime boundaries & multi-executor ---
    ExecutorProvisioned {
        executor: ExecutorId,
        name: String,
        host: HostInfo,
    },
    Mounted {
        tree: NodeHash,
        path: String,
        bytes: u64,
    },
    Unmounted {
        path: String,
    },
    ExecStarted {
        argv: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: String,
    },
    ExecFinished {
        exit: u64,
        stdout: BlobRef,
        stderr: BlobRef,
        duration_ns: u64,
    },
    ClosureShipped {
        to: ExecutorId,
        closure: ClosureRef,
    },
    RpcCall {
        to: ExecutorId,
        method: String,
        duration_ns: u64,
    },

    // --- test semantics ---
    Asserted {
        assertion: Assertion,
    },

    // --- trace (tracing, in-band) ---
    SpanOpened {
        name: String,
        fields: BTreeMap<String, String>,
    },
    SpanClosed {},
    Logged {
        level: LogLevel,
        target: String,
        message: String,
        fields: BTreeMap<String, String>,
    },
}

/// Returned by `run_tests` for callers that only want the tally — derivable by
/// folding `RunFinished`, kept as a convenience.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
pub struct RunSummary {
    pub passed: u64,
    pub failed: u64,
    pub errored: u64,
    pub wall_ms: u64,
}

#[vox::service]
pub trait TestRunner {
    /// Discover and run every annotated test across `request.sources`, pushing
    /// the full firehose of `Event`s onto `events`, and returning the tally.
    async fn run_tests(&self, request: RunTestsRequest, events: Tx<Event>) -> RunSummary;
}
