# The test runner is a firehose (draft)

Draft, non-normative. The design point everything else flows from.

## Thesis

A test run is not a verdict — it is a **complete, structured, timed account of
everything that happened**, from the first byte of source to the last value
produced, across every executor that participated. The runner is a
total-observability instrument: it emits a firehose of richly-typed events and
**discards nothing at the source**. Deciding what matters is the *reporter's*
job, never the runner's.

This inverts the usual test tool. "Assertion failed — good luck" is the
anti-goal. We have strong types, a rich data model, universal
`serialise`/`eq`/`hash`, a demand graph, a JIT, real executors talking over RPC,
and a content-addressed store. A failing test should hand you **we wanted *this*
typed value, we got *that* one, here is the structured diff, here is the entire
trace of how we got there, here is the source we ran, here is what was mounted
when and on which host** — as data, queryable, replayable, not as scrollback.

Because it is a firehose, verbosity costs the reporter nothing it doesn't opt
into: a CI summary folds it to a tally, a terminal reporter to a tree, the web
UI (vite + ts + react + vox, types generated from these same definitions) to a
live timeline, and an archival reporter to a content-addressed blob you can
re-open a year later and diff against today's run.

## The single source of truth

One stream, `Tx<TestEvent>`. Every reporter folds *the same stream*. No output
form is privileged; the human CLI is just another fold. The verdict is **one
terminal event among thousands**, computed once and carried in-band so no
reporter re-derives the rule.

A "test record" is not a separate type the runner builds — it is simply *the
events for one case, folded*. Persisting a run = persisting its event log. A
reporter that wants a record folds the log; a reporter that wants a live view
consumes the log as it arrives. Same data, different latency.

## Every event is envelope + body

```
struct Event {
    seq:      u64,            // monotonic, run-global — total order
    at:       Instant,       // monotonic since run start (for timings)
    wall:     Timestamp,     // absolute wall-clock (time of day, per his ask)
    executor: ExecutorId,    // WHICH machine this happened on (0 = primary)
    span:     SpanId,        // causal/nesting context — reporters rebuild the tree
    parent:   Option<SpanId>,
    body:     EventBody,     // the firehose taxonomy below
}
```

`seq` gives a total order even across executors; `span`/`parent` rebuild the
causal tree (a phase, an exec, an RPC, an assertion all nest); `executor` tags
provenance so a multi-daemon test reads as one timeline. Timings are derived by
the reporter from `at` deltas between paired events — the runner does not
pre-aggregate, because pre-aggregation is a decision and the runner makes none.

## The taxonomy (open, extensible — add bodies freely)

This is a firehose: the list grows. New `EventBody` variants are additive; old
reporters ignore unknown bodies. Phases below each emit a `…Started`/`…Finished`
pair (so duration is a delta and partial runs are still legible).

### Compilation pipeline — every phase, every timing

The thing he named directly: *how long to parse, how long to analyze into a
graph, how long the JIT took, what the JIT produced.*

- `Lexed { tokens, bytes }`
- `Parsed { cst_nodes, errors }`
- `NameResolved { bindings, unresolved }`
- `TypeChecked { inferences, errors }`
- `CapabilityChecked { required: Vec<Capability> }`
- `DemandGraphBuilt { nodes, edges, lazy_fields, reachable_bodies }` — the graph
- `Lowered { bodies, kernel_ops }`
- `Jitted { kernel: KernelId, strategy: CopyAndPatch | …, code_bytes, result: JitArtifact }`
  — per kernel, with **what the JIT produced** (artifact handle, not just a time)
- each carries its own `…Finished { duration }` so the reporter can show a flame
  bar per phase without the runner choosing a budget

### Demand-driven evaluation — the graph executing

- `SlotForced { slot, kernel }` / `SlotProduced { slot, value: TypedValue }`
- `ThunkAllocated` / `ThunkForced`
- `CacheLookup { key, hit: bool }` — the daemon's content-addressed cache *is*
  the product; every hit/miss is observable
- `Poisoned { site, diagnostics }` — a detonation, located

### Runtime boundaries & multi-executor — the real product

Tests must be able to ship a closure to a **second daemon** and communicate over
real RPC, not fake it in one executor. The topology is annotation-declared
(below) and every cross-executor act is on the firehose:

- `ExecutorProvisioned { id, name, host: HostInfo, role }` — host + its
  characteristics (os, arch, cpus, memory, kernel, …), captured at join
- `Mounted { executor, tree, path, bytes, at }` — *what was mounted, when, where*
- `Unmounted { executor, path, at }`
- `ExecStarted { executor, argv, env, cwd, mounts }` /
  `ExecFinished { executor, exit, stdout: BlobRef, stderr: BlobRef, duration }`
- `ClosureShipped { from: ExecutorId, to: ExecutorId, closure: ClosureRef, captures, at }`
  — the load-bearing multi-daemon event
- `RpcCall { from, to, method, request: ValueRef, response: ValueRef, duration }`

### Test semantics — assertions as structured claims

An assertion is not a `bool` that poisons with a string. It is a recorded
**claim**, with both operands captured as typed values and a structured diff:

```
struct Assertion {
    kind:     AssertionKind,   // Eq | Ne | Lt | Le | Gt | Ge | Contains | StartsWith
                               // | Matches | IsVariant | Approx | Custom(Symbol) | …
    source:   String,          // the assertion expression, verbatim
    location: SourceSpan,
    operands: Vec<TypedValue>, // e.g. [got, want] — full typed values
    passed:   bool,
    diff:     Option<ValueDiff>, // rediff-style structured diff when it fails
}
```

`TypedValue { ty: TypeRef, value: Value, rendered: String }` — `ty` is the real
inferred type, `value` is the structured value (not a string), `rendered` is a
courtesy. The whole point: a reporter shows `wanted Pair { a: 1, b: 2 }, got
Pair { a: 1, b: 3 }` with the `b` field highlighted, because it has the *typed
values and their diff*, not a message. Assertions are open: `assert_eq`,
`assert`, `assert_contains`, `assert_matches`, `assert_approx`, and
user-defined assertions all emit `Assertion` events — the runner doesn't enumerate
them, it records whatever the program claims.

### The trace — tracing, in-band

- `Span { id, parent, name, fields }` open/close — the `tracing` tree, captured
- `Log { level, target, message, fields }` — every event the code emits

### Lifecycle bookends

- `RunStarted { discovered: Vec<TestId>, topology, runner_host: HostInfo }`
- `CaseStarted { id, expectation, closure: SourceClosure, topology }`
- `CaseFinished { id, verdict, terminal: EvalOutcome, wall_duration }`
- `RunFinished { passed, failed, errored, wall_duration }`

## The source closure — a test is self-describing

`CaseStarted` carries a `SourceClosure`: **the entire program needed to run this
test** — its own source, the prelude, every definition in scope, the resolved
imports, content-addressed. "A closure over everything that was happening." A
recorded run is therefore replayable in isolation and diffable against another
run's closure: you can ask *did the test change, or did the world change?*

## Topology — multi-daemon tests, annotation-declared

```
#[test]                              // one executor (primary)
#[test(poisons)]                     // expected to detonate
#[test(executors(primary, remote))]  // provision a second daemon; ship/RPC for real
#[test(executors(primary, remote(host = "aarch64-linux")))]  // pin its characteristics
```

The topology is declared on the test, the runner **provisions the daemons**
(spawns/leases them), assigns `ExecutorId`s, and the firehose records
provisioning, the host of each, every closure shipped, every RPC. A test that
exercises cross-architecture closure shipping reads as one ordered timeline with
events tagged by executor.

## Reporters fold the firehose

Reporters are pure functions over the stream; they differ only in what they keep:

- **tally** (CI gate): fold to `passed/failed/errored`, exit code.
- **terminal**: a live tree — phases as a flame bar, assertions with typed
  diffs, executors as lanes; collapses passing cases, expands failures.
- **json**: the event log, via `facet-json` (never hand-emitted; tagged enum).
- **web UI**: subscribes live over vox/websocket, TS types generated from
  `EventBody`; scrub the timeline, click a span, see the typed values, diff two
  runs.
- **archival**: write the log to the content-addressed store keyed by the source
  closure; re-open and diff later.

None is privileged; adding one never touches the runner.

## Why firehose-first (the design consequence)

Because the runner emits everything, we never have to go back and "add
instrumentation for X" — X was always on the wire; a reporter just starts
reading it. The cost of verbosity is paid once, in bandwidth the reporter can
drop. The cost of *under*-instrumentation is paid forever, in the tests you
couldn't diagnose. So: emit it all, structured, timed, located, typed. Filtering
is cheap and late; observability is expensive and early. We choose early.

## Open / deliberately unbounded

- The `EventBody` taxonomy is **meant** to grow; it is not a closed set.
- Wire format & volume control: the firehose can be large. Bodies carry
  content-addressed *refs* (`BlobRef`, `ValueRef`, `ClosureRef`) for big
  payloads, fetched on demand by the reporter that cares — so the stream stays
  events, not bulk.
- `#[bench]`: timing is already first-class on every phase; a bench is a case
  whose reporter cares about `…Finished` deltas and which runs cold (cache
  bypassed). Likely no new verdict, just a reporter lens.
- Sampling/level: a reporter may ask the runner to *not* send some bodies (a
  subscription filter) when bandwidth matters — but the default is everything.
