//! Public compatibility entry points for the graph-driven runtime.
//!
//! Source and body evaluation enter [`crate::graph_exec`], which drives
//! `vx-runtime-driver` over a [`crate::graph::DemandGraph`]. The public
//! [`Value`] and [`Thunk`] shapes remain as an API-edge adapter for tests,
//! observer wire crossing, and host/provider values; they are not the
//! orchestrator's execution currency. Inside execution, values are graph node
//! outputs in the driver memo table.
//!
//! The demand tripwire is still the same: `💣` detonates only when graph demand
//! reaches its node. Placing it in an undemanded tuple/struct slot should leave it
//! dark.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use vx_exec_protocol::WireValue;
use vx_ir::build::VirProgram;
use vx_ir::ids::BodyId;
use vx_types::TypeEnv;

use crate::ability::ProviderRegistry;
use crate::cache::RuntimeCacheBackend;
use crate::graph::Span;
use crate::strand::{HandleId, ObserveClientId};

/// The uniform runtime envelope of a failure (the failure model's `context`) —
/// identical for every failure regardless of the domain error type `E`. Carried by
/// a [`Poison`] as it propagates and reified into a `Value::FailureContext` when a
/// `?` catches it, so the context survives the catch (a poison and the caught
/// `Err(Failure { .. })` are the *same* failure).
///
/// This cut is `span` + `demand_path`; the strand stack and timing fields are
/// deferred (see the failure model's `FailureContext`).
#[derive(Debug, Clone, Default)]
pub struct FailureContext {
    /// The innermost origin — where in the source the failure detonated (I6, what
    /// `Poison.span` used to be). The innermost expression wins.
    pub span: Option<Span>,
    /// The Vixen call-chain the poison crossed, innermost call first. Each `Call`
    /// frame the poison bubbles through pushes its span (not every expression), so
    /// this is the Vixen call stack, not the whole expression spine.
    pub demand_path: Vec<Span>,
}

/// A poison — a detonation carrying a typed **domain error** value and the failure
/// model's [`context`](Poison::context) (origin span + demand-path). This is the
/// runtime form of the failure model's `Failure`: `fail e` carries `e` as
/// [`err`](Poison::err) (a `Value::Str` for a bare message, a structured value for a
/// domain error); the human report is *derived* from `err` + `context`, never a
/// stored string. `?` catches it as a `Failure { err, context }` value. Propagates
/// like an error; `eval` attaches the span a poison surfaces at (innermost wins)
/// and accumulates the demand-path across `Call` frames.
#[derive(Debug, Clone)]
pub struct Poison {
    /// The domain error value (the failure model's `err`).
    pub err: Value,
    /// The uniform envelope (the failure model's `context`).
    pub context: FailureContext,
}

impl Poison {
    /// Render the domain error to a human string — `Value::Str` is itself, a
    /// variant shows its tag, anything else is debug-formatted. (The rich report
    /// derived from `err` + context is the failure model's job; this is the
    /// fallback used by the test harness's `Outcome`.)
    pub(crate) fn message(&self) -> String {
        match &self.err {
            Value::Str(s) => s.to_string(),
            Value::Variant { tag, .. } => tag.clone(),
            other => format!("{other:?}"),
        }
    }
}

/// Construct a context-less poison from a bare message (the internal-error path —
/// type mismatches, unhandled forms). The message becomes a `Value::Str` `err`.
fn poison(reason: String) -> Poison {
    Poison {
        err: Value::Str(Rc::from(reason.as_str())),
        context: FailureContext::default(),
    }
}

/// A thunk already resolved to `value` — reifies a computed value back into a
/// thunk-bearing position (e.g. a `Result` variant field built by `?`).
fn done(value: Value) -> Thunk {
    Thunk(Rc::new(RefCell::new(ThunkState::Done(Ok(value)))))
}

/// A **grammar coordinate** value `{ package, name }`: the import spec the grammar's
/// `GrammarSet` is loaded under (on the arena) plus the grammar's name. The runtime
/// resolves it to a `command_model` grammar by looking `package` up in
/// `arena.grammars` — the grammar's data is never copied into the value. This is the
/// command/grammar analogue of `Host { os, arch }`: small scalar fields the runtime
/// fills, validation backed by Rust reading the real `GrammarSet`.
pub fn grammar_coord_value(package: &str, name: &str) -> Value {
    Value::Struct(vec![
        (
            "package".to_string(),
            Thunk::done(Value::Str(Rc::from(package))),
        ),
        ("name".to_string(), Thunk::done(Value::Str(Rc::from(name)))),
    ])
}

/// Build a **tool** value `{ command, binary }` (`r[exec.tool]`) — the leading hole
/// of a backtick invocation. `command` is a coordinate `{ package, name }` locating
/// the command's grammar on the arena (`r[exec.command]`); `binary` is the program
/// to run. The executor identity, content fingerprint, and probed parameters fill in
/// when an ability provider is injected (`r[ability.match]`). Exposed for tests and
/// capability probes that synthesize tools directly.
pub fn tool_value(package: &str, command: &str, binary: &str) -> Value {
    Value::Struct(vec![
        (
            "command".to_string(),
            Thunk::done(grammar_coord_value(package, command)),
        ),
        (
            "binary".to_string(),
            Thunk::done(Value::Str(Rc::from(binary))),
        ),
    ])
}

/// A lazily materialized API-edge field. This is not a source-expression thunk:
/// graph-produced fields hold graph-boundary callbacks, and externally supplied
/// values usually hold already-resolved `Done` fields.
#[derive(Debug, Clone)]
pub struct Thunk(Rc<RefCell<ThunkState>>);

impl Thunk {
    pub fn native(f: impl Fn() -> Result<Value, Poison> + 'static) -> Thunk {
        Thunk(Rc::new(RefCell::new(ThunkState::Native(Intrinsic(
            Rc::new(f),
        )))))
    }

    pub fn done(value: Value) -> Thunk {
        done(value)
    }

    pub(crate) fn materialize_for_graph_boundary(&self) -> Result<Value, Poison> {
        let state = {
            let mut cell = self.0.borrow_mut();
            match &*cell {
                ThunkState::Done(result) => return result.clone(),
                ThunkState::Forcing => {
                    return Err(poison("circular graph boundary thunk".to_string()));
                }
                ThunkState::Native(_) => std::mem::replace(&mut *cell, ThunkState::Forcing),
            }
        };
        let ThunkState::Native(intrinsic) = state else {
            unreachable!("only native thunks are taken above");
        };
        let result = (intrinsic.0)();
        *self.0.borrow_mut() = ThunkState::Done(result.clone());
        result
    }
}

#[derive(Clone)]
struct Intrinsic(Rc<dyn Fn() -> Result<Value, Poison>>);

impl std::fmt::Debug for Intrinsic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<intrinsic>")
    }
}

#[derive(Debug)]
enum ThunkState {
    Native(Intrinsic),
    Forcing,
    Done(Result<Value, Poison>),
}

/// Public compatibility value. Compound fields are API-edge [`Thunk`]s; graph
/// execution itself carries node outputs in driver memo state.
#[derive(Debug, Clone)]
pub enum Value {
    U64(u64),
    Bool(bool),
    Str(Rc<str>),
    Tuple(Vec<Thunk>),
    Array(Vec<Thunk>),
    Struct(Vec<(String, Thunk)>),
    /// An enum variant value: the variant `tag` plus its payload fields. Struct-form
    /// variants (`Some { value: x }`) carry named fields; tuple-form variants
    /// (`Ok(v)`) carry positional fields named `"0"`, `"1"`, …; unit variants
    /// (`None`) carry none. The tag is what a `match` arm compares.
    Variant {
        tag: String,
        fields: Vec<(String, Thunk)>,
    },
    Unit,
    /// A **remote producing tree** — a value produced on another executor that
    /// resolves in parts (per path) over vox. eval holds only the scheduler-registered
    /// [`HandleId`](crate::strand::HandleId), never the vox client; forcing a path
    /// (`tree.file(@p)`) pulls that part via [`force_remote`], parking the strand until
    /// the producer yields it. The consumer half of `remote.rs`, reached from the
    /// language — the genuinely-remote leaf of an exec's outputs.
    Tree(HandleId),
    /// The **executor-side** output tree of the running exec — what `run.ok.out` /
    /// `run.stream.out` evaluate to *on the executor*, before crossing. It is local
    /// there (the executor produced it); `to_wire` marks it [`WireValue::Tree`] so it
    /// crosses back as a handle (the orchestrator never sees this variant — it gets a
    /// [`Value::Tree`]). Parameterless for now (one output tree per run).
    OutputTree,
    /// The opaque runtime envelope of a caught failure (the failure model's
    /// `FailureContext`). Reified by `?` into the `context` field of a `Failure`
    /// struct value, so a caught error carries the same span + demand-path the
    /// propagating poison did. Authors carry it through but do not inspect it
    /// (compared by identity, never structurally equal across distinct failures).
    FailureContext(Rc<FailureContext>),
}

/// The outcome of evaluating a test body: clean, or poisoned (with the source
/// span the poison originated at — I6).
#[derive(Debug)]
pub enum Outcome {
    Clean(Value),
    Poisoned { reason: String, span: Option<Span> },
}

/// Transitional execution-mode knob retained by public callers while the graph
/// runtime absorbs the old mode surface. All variants enter the graph driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelMode {
    /// Graph execution without selecting an additional native-kernel preference.
    Off,
    /// Graph execution with interpreter kernels preferred where the graph lowers.
    Interp,
    /// Graph execution with native JIT kernels preferred where available.
    Jit,
}

/// Evaluate function `entry` in `source` through the demand-driven engine, with
/// the assert prelude prepended. `Err` is a setup failure (parse / not found);
/// `Ok(Outcome)` is the evaluation result (clean or poisoned).
pub fn run(source: &str, entry: &str) -> Result<Outcome, String> {
    run_in_mode(source, entry, KernelMode::Jit).map(|(outcome, _)| outcome)
}

/// As [`run`], but also reports how many graph kernels executed on the public
/// evaluation run. Uses the default [`KernelMode::Jit`].
pub fn run_with_dispatches(source: &str, entry: &str) -> Result<(Outcome, u64), String> {
    run_in_mode(source, entry, KernelMode::Jit)
}

/// Evaluate `entry` in `source` under an explicit [`KernelMode`], reporting the
/// outcome and the kernel-dispatch count. This public source entry is graph-driven;
/// `mode` is retained while legacy body-evaluation entry points are cut over.
pub fn run_in_mode(source: &str, entry: &str, mode: KernelMode) -> Result<(Outcome, u64), String> {
    let _ = mode;
    crate::graph_exec::run_source(source, entry)
}

/// Evaluate `body` with `args` bound to its parameters, in an already-built
/// `program` + `env` — the exec **mirror** entry. Unlike [`run_in_mode`] it skips
/// the front-end entirely: the mirror installs a bundle (`vx_ir::Bundle::install`)
/// and runs its observer body here, never compiling and never allocating an id. The
/// observer's sole argument — the `Run` — arrives in `args`. No exec client: the
/// observer is a leaf (it consumes a `Run`; a nested `exec` is later).
pub fn eval_body(
    program: &VirProgram,
    env: &TypeEnv,
    body: BodyId,
    args: Vec<Value>,
    mode: KernelMode,
) -> Outcome {
    let _ = mode;
    graph_body_outcome(program, env, body, args, None)
}

/// As [`eval_body`], but with an explicit temporary cache-index directory for
/// graph results that are safe to persist.
pub fn eval_body_with_cache_dir(
    program: &VirProgram,
    env: &TypeEnv,
    body: BodyId,
    args: Vec<Value>,
    mode: KernelMode,
    cache_dir: impl AsRef<Path>,
) -> Outcome {
    let _ = mode;
    match crate::graph_exec::run_body_with_cache_dir(
        program,
        env,
        body,
        args,
        cache_dir.as_ref().to_path_buf(),
    ) {
        Ok((outcome, _)) => outcome,
        Err(reason) => Outcome::Poisoned { reason, span: None },
    }
}

/// As [`eval_body`], but with a caller-owned persistent cache backend. The
/// backend receives only `(namespace, pre-run input digest)` plus opaque record
/// text; it must not demand graph values or run service RPC directly on the
/// evaluator stack.
pub fn eval_body_with_cache_backend(
    program: &VirProgram,
    env: &TypeEnv,
    body: BodyId,
    args: Vec<Value>,
    mode: KernelMode,
    backend: impl RuntimeCacheBackend + 'static,
) -> Outcome {
    let _ = mode;
    match crate::graph_exec::run_body_with_cache_backend(
        program,
        env,
        body,
        args,
        Box::new(backend),
    ) {
        Ok((outcome, _)) => outcome,
        Err(reason) => Outcome::Poisoned { reason, span: None },
    }
}

/// As [`eval_body`], but with the executor an `exec(…)` in `body` dispatches to —
/// the **orchestrator** entry. eval must run on a strand (so the `observe` RPC can
/// park via the strand boss); register the client with
/// [`Scheduler::register_observe_client`](crate::strand::Scheduler::register_observe_client)
/// and pass its id here. `exec` reached without this poisons honestly.
pub fn eval_body_with_exec(
    program: &VirProgram,
    env: &TypeEnv,
    body: BodyId,
    args: Vec<Value>,
    mode: KernelMode,
    exec_client: ObserveClientId,
) -> Outcome {
    let _ = mode;
    graph_body_outcome(program, env, body, args, Some(exec_client))
}

/// As [`eval_body_with_exec`], but with a caller-owned persistent cache backend.
/// Async cache services must be hidden behind an orchestrator/strand boundary,
/// not driven directly from runtime evaluation.
pub fn eval_body_with_exec_and_cache_backend(
    program: &VirProgram,
    env: &TypeEnv,
    body: BodyId,
    args: Vec<Value>,
    mode: KernelMode,
    exec_client: ObserveClientId,
    backend: impl RuntimeCacheBackend + 'static,
) -> Outcome {
    let _ = mode;
    match crate::graph_exec::run_body_with_exec_and_cache_backend(
        program,
        env,
        body,
        args,
        exec_client,
        Box::new(backend),
    ) {
        Ok((outcome, _)) => outcome,
        Err(reason) => Outcome::Poisoned { reason, span: None },
    }
}

/// As [`eval_body_with_exec`], but also with the advertised capability `providers`
/// the local control plane injects `Type where (predicate)` requests against
/// (`r[ability.match]`) — the full orchestrator entry: exec **and** injection. This
/// is the path a real build runs on (a tool requested by `where`, then invoked).
pub fn eval_body_with_exec_and_providers(
    program: &VirProgram,
    env: &TypeEnv,
    body: BodyId,
    args: Vec<Value>,
    mode: KernelMode,
    exec_client: ObserveClientId,
    providers: ProviderRegistry,
) -> Outcome {
    let _ = mode;
    match crate::graph_exec::run_body_with_exec_and_providers(
        program,
        env,
        body,
        args,
        exec_client,
        providers,
    ) {
        Ok((outcome, _)) => outcome,
        Err(reason) => Outcome::Poisoned { reason, span: None },
    }
}

fn graph_body_outcome(
    program: &VirProgram,
    env: &TypeEnv,
    body: BodyId,
    args: Vec<Value>,
    exec_client: Option<ObserveClientId>,
) -> Outcome {
    let result = match exec_client {
        Some(client) => crate::graph_exec::run_body_with_exec(program, env, body, args, client),
        None => crate::graph_exec::run_body(program, env, body, args),
    };
    match result {
        Ok((outcome, _)) => outcome,
        Err(reason) => Outcome::Poisoned { reason, span: None },
    }
}

/// Evaluate the observer `body` against `args` and **deep-force** its result into a
/// [`WireValue`] — the executor entry of the value crossing. The whole value is
/// forced to data here, on the executor, so the observer's result (not a lossy
/// top-level peek) crosses back over vox. `Err` is the poison's message — a failed
/// gate, a forced bomb, an un-crossable value. No exec client: a nested `exec` in an
/// observer is a later piece.
pub fn eval_body_wire(
    program: &VirProgram,
    env: &TypeEnv,
    body: BodyId,
    args: Vec<Value>,
    mode: KernelMode,
) -> Result<WireValue, String> {
    let _ = mode;
    match crate::graph_exec::run_body(program, env, body, args) {
        Ok((Outcome::Clean(value), _)) => {
            wire_graph_value(&value).map_err(|poison| poison.message())
        }
        Ok((Outcome::Poisoned { reason, .. }, _)) | Err(reason) => Err(reason),
    }
}

fn wire_graph_value(value: &Value) -> Result<WireValue, Poison> {
    Ok(match value {
        Value::U64(n) => WireValue::U64(*n),
        Value::Bool(b) => WireValue::Bool(*b),
        Value::Str(s) => WireValue::Str(s.to_string()),
        Value::Unit => WireValue::Unit,
        Value::Tuple(fields) => WireValue::Tuple(wire_graph_thunks(fields)?),
        Value::Array(fields) => WireValue::Array(wire_graph_thunks(fields)?),
        Value::Struct(fields) => WireValue::Struct(wire_graph_fields(fields)?),
        Value::Variant { tag, fields } => WireValue::Variant {
            tag: tag.clone(),
            fields: wire_graph_fields(fields)?,
        },
        Value::OutputTree => WireValue::Tree,
        other => {
            return Err(poison(format!(
                "`exec` observer returned a value that cannot cross back: {other:?}"
            )));
        }
    })
}

fn wire_graph_thunks(fields: &[Thunk]) -> Result<Vec<WireValue>, Poison> {
    fields
        .iter()
        .map(|field| wire_graph_value(&field.materialize_for_graph_boundary()?))
        .collect()
}

fn wire_graph_fields(fields: &[(String, Thunk)]) -> Result<Vec<(String, WireValue)>, Poison> {
    fields
        .iter()
        .map(|(name, field)| {
            Ok((
                name.clone(),
                wire_graph_value(&field.materialize_for_graph_boundary()?)?,
            ))
        })
        .collect()
}

/// Reconstruct a [`Value`] from a [`WireValue`] — the orchestrator side of the value
/// crossing. The wire form is already fully forced, so every field is a resolved
/// (`done`) thunk.
pub fn from_wire(wire: &WireValue) -> Result<Value, Poison> {
    Ok(match wire {
        WireValue::U64(n) => Value::U64(*n),
        WireValue::Bool(b) => Value::Bool(*b),
        WireValue::Str(s) => Value::Str(Rc::from(s.as_str())),
        WireValue::Unit => Value::Unit,
        WireValue::Tuple(ws) => Value::Tuple(from_wire_thunks(ws)?),
        WireValue::Array(ws) => Value::Array(from_wire_thunks(ws)?),
        WireValue::Struct(fields) => Value::Struct(from_wire_fields(fields)?),
        WireValue::Variant { tag, fields } => Value::Variant {
            tag: tag.clone(),
            fields: from_wire_fields(fields)?,
        },
        // A producing tree crosses as a handle, but resolving it into a
        // `Value::Tree(HandleId)` means opening a `Producing` virtual connection to the
        // executor and `register_handle` — async, boss-side, during `dispatch_observe`.
        // That join is the next piece; until then a returned tree poisons honestly
        // rather than pretending to resolve.
        WireValue::Tree => {
            return Err(poison(
                "`exec` returned a producing tree — the handle crossing (open a \
                 Producing vconn + register) is not wired yet"
                    .into(),
            ));
        }
    })
}

/// Force-free reconstruction of wire elements into resolved (`done`) thunks.
fn from_wire_thunks(ws: &[WireValue]) -> Result<Vec<Thunk>, Poison> {
    ws.iter().map(|w| Ok(done(from_wire(w)?))).collect()
}

fn from_wire_fields(fields: &[(String, WireValue)]) -> Result<Vec<(String, Thunk)>, Poison> {
    fields
        .iter()
        .map(|(n, w)| Ok((n.clone(), done(from_wire(w)?))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The `.vx` language + demand tests live in `tests/lang/*.vx`, run by the
    // self-hosting `lang_suite` harness across every `KernelMode`. The tests below
    // assert engine internals a program can't observe directly (kernel execution
    // counts, graph/native value-shape crossing, the I6 span), so they stay in Rust.

    #[test]
    fn asserts_dispatch_to_the_kernel() {
        // `assert_eq` is a scalar `if`-kernel; called with U64 args it runs as
        // compiled code on the real eval path — a dispatch fires.
        let (outcome, dispatches) =
            run_with_dispatches("fn t() -> Bool ( assert_eq(20 + 22, 42) )", "t").unwrap();
        assert!(
            matches!(outcome, Outcome::Clean(_)),
            "expected clean: {outcome:?}"
        );
        assert!(dispatches >= 1, "assert_eq should dispatch to the kernel");
    }

    #[test]
    fn scalar_function_dispatches_and_computes() {
        let src = "fn add(a: U64, b: U64) -> U64 ( a + b ) fn main() -> U64 ( add(40, 2) )";
        let (outcome, dispatches) = run_with_dispatches(src, "main").unwrap();
        assert!(
            matches!(outcome, Outcome::Clean(Value::U64(42))),
            "got {outcome:?}"
        );
        assert!(dispatches >= 1, "add should dispatch to the kernel");
    }

    #[test]
    fn bool_returning_kernel_wraps_as_bool() {
        // `returns_bool` must read the returned word as Bool, not U64.
        let src = "fn lt(a: U64, b: U64) -> Bool ( a < b ) fn main() -> Bool ( lt(3, 5) )";
        assert!(matches!(
            run(src, "main"),
            Ok(Outcome::Clean(Value::Bool(true)))
        ));
    }

    #[test]
    fn unused_param_stays_lazy_through_dispatch() {
        // `a + a` ignores `b`, so `b` is not a kernel param: the dispatch forces
        // only `a`, leaving the bomb in `b`'s argument unforced. Clean, dispatched.
        let src = "fn f(a: U64, b: U64) -> U64 ( a + a ) fn main() -> U64 ( f(7, 💣) )";
        let (outcome, dispatches) = run_with_dispatches(src, "main").unwrap();
        assert!(
            matches!(outcome, Outcome::Clean(Value::U64(14))),
            "got {outcome:?}"
        );
        assert!(
            dispatches >= 1,
            "the `a + a` kernel should dispatch (bomb unforced)"
        );
    }

    #[test]
    fn sub_expression_dispatches_not_just_whole_bodies() {
        // Two kernels fire: the `r.fast + r.slow` arithmetic SUB-EXPRESSION (the
        // struct projections fold into constants inside one kernel), and the
        // `assert_eq` comparison — neither is a whole function body of the caller.
        let src = "struct Two { fast: U64, slow: U64 } \
                   fn t() -> Bool ( let r = Two { fast: 7, slow: 11 }; assert_eq(r.fast + r.slow, 18) )";
        let (outcome, dispatches) = run_with_dispatches(src, "t").unwrap();
        assert!(
            matches!(outcome, Outcome::Clean(_)),
            "expected clean: {outcome:?}"
        );
        assert!(
            dispatches >= 2,
            "the sub-expression and the assert should both dispatch, got {dispatches}"
        );
    }

    #[test]
    fn block_tail_sub_expression_dispatches_with_a_param() {
        // The tail `x * x + 1` is a scalar sub-expression that lowers to a kernel
        // with `x` as a parameter, fed from the binding.
        let src = "fn sq1(x: U64) -> U64 ( x * x + 1 ) fn main() -> U64 ( sq1(6) )";
        let (outcome, dispatches) = run_with_dispatches(src, "main").unwrap();
        assert!(
            matches!(outcome, Outcome::Clean(Value::U64(37))),
            "got {outcome:?}"
        );
        assert!(dispatches >= 1, "x*x+1 should dispatch");
    }

    #[test]
    fn function_output_l2_reuses_scalar_result_across_fresh_graphs() {
        let src = "fn answer() -> U64 ( 40 + 2 ) fn main() -> U64 ( answer() )";
        let fe = crate::compile::semantic_front_end_bundled(src).expect("front-end");
        let body = body_in(&fe.program, "main");
        let cache_dir = std::env::temp_dir().join(format!(
            "vx-function-output-l2-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&cache_dir);

        let (first, first_dispatches) = crate::graph_exec::run_body_with_cache_dir(
            &fe.program,
            &fe.env,
            body,
            vec![],
            cache_dir.clone(),
        )
        .expect("first graph run");
        assert!(
            matches!(first, Outcome::Clean(Value::U64(42))),
            "first run got {first:?}"
        );
        assert!(
            first_dispatches >= 1,
            "first run should execute the answer body kernel"
        );

        let (second, second_dispatches) = crate::graph_exec::run_body_with_cache_dir(
            &fe.program,
            &fe.env,
            body,
            vec![],
            cache_dir.clone(),
        )
        .expect("second graph run");
        assert!(
            matches!(second, Outcome::Clean(Value::U64(42))),
            "second run got {second:?}"
        );
        assert_eq!(
            second_dispatches, 0,
            "fresh graph should load the scalar call output from L2"
        );

        let _ = std::fs::remove_dir_all(cache_dir);
    }

    #[derive(Clone, Default)]
    struct RecordingCacheBackend {
        records:
            std::sync::Arc<std::sync::Mutex<std::collections::HashMap<(String, String), String>>>,
        puts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl RecordingCacheBackend {
        fn put_count(&self) -> usize {
            self.puts.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl RuntimeCacheBackend for RecordingCacheBackend {
        fn get_record(&self, key: &crate::cache::RuntimeCacheRecordKey) -> Option<String> {
            self.records
                .lock()
                .unwrap()
                .get(&(key.namespace().to_string(), key.input_digest().to_string()))
                .cloned()
        }

        fn put_record(&self, key: &crate::cache::RuntimeCacheRecordKey, record: &str) {
            self.puts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.records.lock().unwrap().insert(
                (key.namespace().to_string(), key.input_digest().to_string()),
                record.to_string(),
            );
        }
    }

    #[test]
    fn function_output_cache_backend_reuses_scalar_result_across_fresh_graphs() {
        let src = "fn answer() -> U64 ( 40 + 2 ) fn main() -> U64 ( answer() )";
        let fe = crate::compile::semantic_front_end_bundled(src).expect("front-end");
        let body = body_in(&fe.program, "main");
        let backend = RecordingCacheBackend::default();

        let first = eval_body_with_cache_backend(
            &fe.program,
            &fe.env,
            body,
            vec![],
            KernelMode::Off,
            backend.clone(),
        );
        assert!(
            matches!(first, Outcome::Clean(Value::U64(42))),
            "first run got {first:?}"
        );
        let puts_after_first = backend.put_count();
        assert!(
            puts_after_first > 0,
            "first run should write the scalar function output"
        );

        let second = eval_body_with_cache_backend(
            &fe.program,
            &fe.env,
            body,
            vec![],
            KernelMode::Off,
            backend.clone(),
        );
        assert!(
            matches!(second, Outcome::Clean(Value::U64(42))),
            "second run got {second:?}"
        );
        assert_eq!(
            backend.put_count(),
            puts_after_first,
            "fresh graph should reuse the injected cache backend"
        );
    }

    #[test]
    fn if_kernel_does_not_force_dead_branch() {
        // `a`/`b` are read only inside their branches. The graph `if` kernel
        // demands the condition, then only the selected branch, so the bomb in the
        // dead branch is never demanded.
        let src = "fn pick(c: Bool, a: U64, b: U64) -> U64 ( if c ( a ) else ( b ) ) \
                   fn main() -> U64 ( pick(true, 7, 💣) )";
        let (outcome, dispatches) = run_with_dispatches(src, "main").unwrap();
        assert!(
            matches!(outcome, Outcome::Clean(Value::U64(7))),
            "got {outcome:?}"
        );
        assert!(dispatches >= 1, "the graph if kernel should execute");
    }

    #[test]
    fn string_equality_runs_as_graph_kernel() {
        // String equality is a graph-visible pure kernel over resolved graph
        // outputs, not a word-kernel fallback through source evaluation.
        let src = r#"fn eq(a: String, b: String) -> Bool ( a == b ) fn main() -> Bool ( eq("hi", "hi") )"#;
        let (outcome, dispatches) = run_with_dispatches(src, "main").unwrap();
        assert!(
            matches!(outcome, Outcome::Clean(Value::Bool(true))),
            "got {outcome:?}"
        );
        assert!(
            dispatches >= 1,
            "string equality should execute as a graph kernel"
        );
    }

    /// I6 at the eval boundary: a poison reaching `Outcome` carries a source span
    /// from graph execution. The spanned source text is the token that detonated.
    #[test]
    fn i6_eval_outcome_carries_the_source_span() {
        for (src, marker) in [
            ("fn f() -> U64 ( 10 / 0 )", "/"),
            ("fn f() -> U64 ( (42, 💣)[1] )", "💣"),
        ] {
            match run(src, "f").unwrap() {
                Outcome::Poisoned {
                    span: Some(s),
                    reason,
                } => {
                    // The span indexes the user source directly — the stdlib prelude
                    // is its own source unit now, not a prepended string.
                    let text = src
                        .get(s.start as usize..s.end as usize)
                        .unwrap_or_else(|| panic!("{src}: span {s:?} out of bounds"));
                    assert!(
                        text.contains(marker),
                        "{src}: span text {text:?} (reason {reason:?}) should contain {marker:?}"
                    );
                }
                other => panic!("{src}: expected poisoned-with-span, got {other:?}"),
            }
        }
    }

    fn body_in(program: &VirProgram, entry: &str) -> BodyId {
        use vx_ir::item::ItemKind;

        let primary = *program.source_units.first().expect("source unit");
        program.arena[primary]
            .items
            .iter()
            .find_map(|&id| match &program.arena[id].kind {
                ItemKind::Function(f) if f.name == entry => Some(f.body),
                _ => None,
            })
            .unwrap_or_else(|| panic!("fn {entry}"))
    }

    fn clean(outcome: Outcome) -> Value {
        match outcome {
            Outcome::Clean(value) => value,
            other => panic!("expected clean value, got {other:?}"),
        }
    }

    fn field_value(value: &Value, name: &str) -> Value {
        let Value::Struct(fields) = value else {
            panic!("expected a struct with `{name}`, got {value:?}");
        };
        fields
            .iter()
            .find(|(field, _)| field == name)
            .unwrap_or_else(|| panic!("field `{name}` missing in {value:?}"))
            .1
            .materialize_for_graph_boundary()
            .expect("graph boundary field")
    }

    fn string_field(value: &Value, name: &str) -> String {
        match field_value(value, name) {
            Value::Str(text) => text.to_string(),
            other => panic!("`{name}` must be a string, got {other:?}"),
        }
    }

    fn read_grammar_coord(value: &Value) -> (String, String) {
        (string_field(value, "name"), string_field(value, "package"))
    }

    fn read_tool_value(value: &Value) -> (Value, String) {
        let command = field_value(value, "command");
        let binary = string_field(value, "binary");
        (command, binary)
    }

    /// A **tool** value (`r[exec.tool]`) carries its command as a coordinate
    /// `{ package, name }` (`r[exec.command]`) — the grammar data is *not* inlined.
    /// The coordinate resolves on the arena, and the cc grammar classifies `x.c` as
    /// an input and the `-o` target an output. (Running an invocation is exercised on
    /// a strand in `exec_on_strands`; here we check the static Tool → Command linkage
    /// the checker const-evaluates in `r[exec.validate.static]`.)
    #[test]
    fn tool_carries_its_command_coordinate() {
        use command_model::examples;
        use std::collections::BTreeMap;

        // A tool: its command is a coordinate locating the cc grammar on the arena.
        let tool = tool_value("@vixen/grammar-cc", "cc", "cc");
        let (command, binary) = read_tool_value(&tool);
        assert_eq!(binary, "cc");
        let (name, pkg) = read_grammar_coord(&command);
        assert_eq!(name, "cc", "the tool's command names the cc grammar");
        assert_eq!(pkg, "@vixen/grammar-cc", "...located by its import spec");

        // The coordinate resolves on the arena and the grammar classifies an argv.
        let argv = ["cc", "-c", "x.c", "-o", "x.o"].map(String::from).to_vec();
        let projected = crate::command::project(&examples(), "cc", &argv, &BTreeMap::new())
            .expect("cc grammar projects");
        assert!(
            projected.effective.inputs.iter().any(|i| i.path == "x.c"),
            "x.c is an input: {:?}",
            projected.effective.inputs
        );
        assert!(
            projected.outputs.iter().any(|o| o.path == "x.o"),
            "the -o target is an output: {:?}",
            projected.outputs
        );
    }

    /// `import "@vixen/grammar-cc"` evaluates to a package struct (one field per
    /// grammar), and `.cc` selects a grammar **coordinate** `{ package, name }` — the
    /// grammar-as-value path on the real eval, against the on-disk package (resolved
    /// via a CompositeResolver over packages/).
    #[test]
    fn import_grammar_package_selects_a_grammar_value() {
        use crate::compile::{NoopObserver, semantic_front_end};
        use vx_package::CompositeResolver;

        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages");
        let resolver = CompositeResolver::new(root);
        let src = "fn t() -> String ( let g = import \"@vixen/grammar-cc\"; g.cc.name )";
        let fe = semantic_front_end(src, &resolver, &mut NoopObserver).expect("front-end");

        let body = body_in(&fe.program, "t");
        let name = clean(eval_body(
            &fe.program,
            &fe.env,
            body,
            vec![],
            KernelMode::Off,
        ));
        assert!(matches!(name, Value::Str(s) if s.as_ref() == "cc"));
    }

    /// The whole grammar-as-value path from real `.vx`: import a grammar and build a
    /// **tool** `{ command: g.cc, binary: "cc" }` — no test-injected value, no grammar
    /// name in the syntax. The tool's `command` is the imported coordinate; the set
    /// the import loaded onto the arena then classifies an argv.
    #[test]
    fn tool_from_imported_grammar_projects() {
        use crate::compile::{NoopObserver, semantic_front_end};
        use vx_package::CompositeResolver;

        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages");
        let resolver = CompositeResolver::new(root);
        let src = "fn t() -> String ( \
	                   let g = import \"@vixen/grammar-cc\"; \
	                   let tool = { command: g.cc, binary: \"cc\" }; \
	                   tool.command.name )";
        let fe = semantic_front_end(src, &resolver, &mut NoopObserver).expect("front-end");

        let body = body_in(&fe.program, "t");
        let command_name = clean(eval_body(
            &fe.program,
            &fe.env,
            body,
            vec![],
            KernelMode::Off,
        ));
        assert!(matches!(command_name, Value::Str(s) if s.as_ref() == "cc"));

        // The grammar set the import loaded onto the arena classifies a hand-built argv.
        let set = fe
            .program
            .arena
            .grammars
            .get("@vixen/grammar-cc")
            .expect("grammar set on arena");
        let argv = ["cc", "-c", "x.c", "-o", "x.o"].map(String::from).to_vec();
        let projected =
            crate::command::project(set, "cc", &argv, &std::collections::BTreeMap::new())
                .expect("cc grammar projects");
        assert!(
            projected.effective.inputs.iter().any(|i| i.path == "x.c"),
            "x.c is an input: {:?}",
            projected.effective.inputs
        );
        assert!(
            projected.outputs.iter().any(|o| o.path == "x.o"),
            "x.o is an output: {:?}",
            projected.outputs
        );
    }

    /// A `Gcc` provider value: its `version`/`target` provider attributes plus a
    /// `binary` (`r[ability.advertise]`).
    fn gcc_provider(version: u64, target: &str, binary: &str) -> Value {
        Value::Struct(vec![
            ("version".to_string(), Thunk::done(Value::U64(version))),
            (
                "target".to_string(),
                Thunk::done(Value::Str(Rc::from(target))),
            ),
            (
                "binary".to_string(),
                Thunk::done(Value::Str(Rc::from(binary))),
            ),
        ])
    }

    /// Request `Gcc where (constraint)` against an advertised `providers` set, and
    /// return the injected provider's `binary` (or the poison message). Exercises the
    /// real `where` path: front-end (resolver skips the predicate, checker types it
    /// `Gcc`) → `eval_constraint` extracts the structured predicate and matches.
    fn inject_binary(constraint: &str, providers: ProviderRegistry) -> Result<String, String> {
        let src = format!(
            "struct Gcc {{ version: U64, target: String }} \
             fn t() ( Gcc where ({constraint}) )"
        );
        let (program, tenv) = crate::analyze::front_end(&src).expect("front-end");
        let body = body_in(&program, "t");
        match eval_body_with_exec_and_providers(
            &program,
            &tenv,
            body,
            vec![],
            KernelMode::Off,
            0,
            providers,
        ) {
            Outcome::Clean(value) => match field_value(&value, "binary") {
                Value::Str(s) => Ok(s.to_string()),
                other => Err(format!("`binary` is not a string: {other:?}")),
            },
            Outcome::Poisoned { reason, .. } => Err(reason),
        }
    }

    /// `Type where (predicate)` injects the first advertised provider whose attributes
    /// satisfy the structured constraint (`r[ability.request]`/`r[ability.match]`): of
    /// two advertised gccs, `version >= 11` selects the v12 one, skipping v10.
    #[test]
    fn where_injects_the_first_matching_provider() {
        let mut providers = ProviderRegistry::new();
        providers.advertise("Gcc", gcc_provider(10, "x86_64-linux", "gcc-10"));
        providers.advertise("Gcc", gcc_provider(12, "x86_64-linux", "gcc-12"));
        assert_eq!(
            inject_binary("version >= 11", providers),
            Ok("gcc-12".to_string()),
            "v10 fails the constraint; v12 is injected"
        );
    }

    /// A multi-clause constraint requires *every* clause (`r[expr.where]`): the v12
    /// linux provider is injected; the v12 darwin one fails the `target` clause.
    #[test]
    fn where_matches_every_clause() {
        let mut providers = ProviderRegistry::new();
        providers.advertise("Gcc", gcc_provider(12, "aarch64-darwin", "gcc-darwin"));
        providers.advertise("Gcc", gcc_provider(12, "x86_64-linux", "gcc-linux"));
        assert_eq!(
            inject_binary("version >= 11 && target == \"x86_64-linux\"", providers),
            Ok("gcc-linux".to_string()),
        );
    }

    /// When no advertised provider satisfies the constraint, the request poisons —
    /// the local registry is the whole fleet, so an unmet demand is honest failure.
    #[test]
    fn where_poisons_when_no_provider_matches() {
        let mut providers = ProviderRegistry::new();
        providers.advertise("Gcc", gcc_provider(10, "x86_64-linux", "gcc-10"));
        let result = inject_binary("version >= 20", providers);
        assert!(
            result.is_err(),
            "no provider satisfies version >= 20, got {result:?}"
        );
    }
}
