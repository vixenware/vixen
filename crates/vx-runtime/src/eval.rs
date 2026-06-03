//! A demand-driven evaluator — the honest engine (rebuild-plan Phase 2/3,
//! interpreter-first, correctness before speed).
//!
//! The point that every prior evaluator got wrong: **it is lazy.** Compound
//! construction (tuples, structs, arrays) yields **thunks** for its fields, not
//! forced values; only what is *demanded* (a projection, an operand of a
//! computation, the final result) is forced. So `💣` detonates **only when
//! forced** — drop it in an undemanded field and it stays inert; force it and it
//! poisons. This is what makes the `.vx` unit tests meaningful *on the real
//! engine* (not the eager `vx-rt`/`runtime_eval` being replaced).
//!
//! Scope: enough to run `tests/lang/*.vx` (the self-hosting unit tests) — calls
//! are resolved to their function bodies, so `assert_eq` actually executes.
//! Unhandled forms poison honestly rather than silently mis-evaluating.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use vx_ir::build::VirProgram;
use vx_ir::expr::{
    ArrayElement, BinOp, CliArgIr, CliLiteralExpr, ExprKind, FieldExpr, FieldKey, LitKind,
    StringPart, WhereExpr,
};
use vx_ir::ids::{BindingId, BodyId, ExprId, PatternId};
use vx_ir::item::ItemKind;
use vx_ir::pattern::PatternKind;
use vx_ir::scope::DefTarget;
use vx_ir::stmt::StmtKind;
use vx_types::TypeEnv;

use crate::dispatch::{self, Dispatcher};
use crate::graph::Span;
use crate::observe::{ExecCommand, ObserveRequest, WireValue};
use crate::remote::{Part, PartKey};
use crate::strand::{HandleId, ObserveClientId, ObserveOutcome, dispatch_observe, force_remote};

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
    fn message(&self) -> String {
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

/// A runtime value. Compound fields are [`Thunk`]s, so they stay unforced until
/// demanded (`r[eval.pending]`/`r[eval.forcing]`).
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
    /// A callable: a lambda capturing its definition environment, or a named
    /// function used as a value (with an empty capture). Calling it binds the
    /// arguments to `body`'s parameters over `env`.
    Closure {
        body: BodyId,
        env: Env,
    },
    /// A command value — what the leading hole of a CLI literal carries, and what
    /// `@[ ${cmd} … ]` evaluates to once it projects its grammar against the args.
    Command(Rc<crate::command::CommandValue>),
    /// An imported grammar package — `import "@vixen/grammar-cc"` as a *value*
    /// (per "Types are values"). Field access selects one of its grammars.
    Grammars(Rc<command_model::GrammarSet>),
    /// One grammar selected from a set (`grammars.cc`) — a value a `Command`
    /// carries and a CLI literal projects.
    Grammar {
        set: Rc<command_model::GrammarSet>,
        name: String,
    },
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

/// Which execution path dispatchable scalar regions take. The `.vx` harness runs
/// every test in each available mode and asserts the outcomes agree: `Off` (pure
/// tree-walk) is the ground-truth oracle for the kernel, and the interpreter is
/// the oracle for the JIT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelMode {
    /// Never dispatch — every expression tree-walks. The simplest evaluator, and
    /// the oracle the kernel path is checked against.
    Off,
    /// Dispatch scalar regions to the kernel interpreter; never the JIT.
    Interp,
    /// Dispatch, preferring the native JIT (interpreter fallback). The default.
    Jit,
}

type Env = HashMap<BindingId, Thunk>;

/// A lazily-evaluated, memoised value. Forcing runs its expression in its
/// captured environment once; re-forcing returns the cached result.
#[derive(Debug, Clone)]
pub struct Thunk(Rc<RefCell<ThunkState>>);

impl Thunk {
    /// A **native (intrinsic) thunk**: a value computed by Rust, lazily, memoised
    /// like any thunk. This is the mechanism behind native-backed struct fields —
    /// e.g. a `Run` value whose `ok` field forces a success gate, or whose `out`
    /// field is a producing handle. The closure runs at most once, on first force.
    pub fn native(f: impl Fn() -> Result<Value, Poison> + 'static) -> Thunk {
        Thunk(Rc::new(RefCell::new(ThunkState::Native(Intrinsic(
            Rc::new(f),
        )))))
    }

    /// An already-resolved thunk holding `value` — for the non-lazy fields of a
    /// natively-built struct value.
    pub fn done(value: Value) -> Thunk {
        done(value)
    }
}

/// A native (Rust) intrinsic backing a thunk — produces the value on force, rather
/// than evaluating a Vixen expression. Not `Debug` on its own, so we render a stub.
#[derive(Clone)]
struct Intrinsic(Rc<dyn Fn() -> Result<Value, Poison>>);

impl std::fmt::Debug for Intrinsic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<intrinsic>")
    }
}

#[derive(Debug)]
enum ThunkState {
    /// The captured `body` travels with the thunk so that forcing it re-enters the
    /// right body's demand graph for sub-expression dispatch.
    Unforced {
        expr: ExprId,
        env: Env,
        body: BodyId,
    },
    /// A native intrinsic — Rust code producing the value lazily, on force.
    Native(Intrinsic),
    Forcing,
    Done(Result<Value, Poison>),
}

struct Evaluator<'a> {
    program: &'a VirProgram,
    env: &'a TypeEnv,
    /// The body whose expressions are currently being evaluated — set when
    /// entering a body (a call) or forcing a thunk, so sub-expression dispatch
    /// uses the right graph. Saved/restored, not threaded through `eval`.
    current_body: Cell<BodyId>,
    /// The kernel-dispatch decider (per-body graphs + per-expression plans).
    dispatch: Dispatcher,
    /// How many (sub-)expressions ran as kernels — observability; tests assert it.
    dispatches: Cell<u64>,
    /// Which kernel path dispatch takes (`Off` ⇒ pure tree-walk, no dispatch).
    mode: KernelMode,
    /// The executor an `exec(…)` dispatches its observe RPC to (the orchestrator
    /// side). `None` when eval isn't driving exec — the mirror's observer eval, the
    /// `.vx` test harness, every non-strand path. Set only when running on a strand
    /// with an observe client registered; an `exec` without it poisons honestly
    /// (dispatch_observe must park, which only a strand can do).
    exec_client: Option<ObserveClientId>,
}

impl Evaluator<'_> {
    fn thunk(&self, expr: ExprId, env: &Env) -> Thunk {
        Thunk(Rc::new(RefCell::new(ThunkState::Unforced {
            expr,
            env: env.clone(),
            body: self.current_body.get(),
        })))
    }

    /// Force a thunk to a value (memoised; detects cycles). The thunk's captured
    /// body is made current for the duration, then restored — so forcing a value
    /// built in another body dispatches against *that* body's graph.
    fn force(&self, t: &Thunk) -> Result<Value, Poison> {
        // Extract the work without holding the borrow across the recursive eval.
        enum Work {
            Expr(ExprId, Env, BodyId),
            Native(Intrinsic),
        }
        let work = {
            let mut st = t.0.borrow_mut();
            match &*st {
                ThunkState::Done(r) => return r.clone(),
                ThunkState::Forcing => {
                    return Err(poison(
                        "circular dependency (a value demands itself)".into(),
                    ));
                }
                ThunkState::Unforced { expr, env, body } => {
                    let work = Work::Expr(*expr, env.clone(), *body);
                    *st = ThunkState::Forcing;
                    work
                }
                ThunkState::Native(intrinsic) => {
                    let work = Work::Native(intrinsic.clone());
                    *st = ThunkState::Forcing;
                    work
                }
            }
        };
        let result = match work {
            Work::Expr(expr, env, body) => {
                let prev = self.current_body.replace(body);
                let r = self.eval(expr, &env);
                self.current_body.set(prev);
                r
            }
            // A native intrinsic runs its Rust closure — no body/graph to enter.
            Work::Native(intrinsic) => (intrinsic.0)(),
        };
        *t.0.borrow_mut() = ThunkState::Done(result.clone());
        result
    }

    /// Evaluate an expression, maintaining the poison's failure context as it
    /// bubbles: attach `e`'s source span as the origin if none is set yet (the
    /// innermost expression wins — I6), and push `e`'s span onto the demand-path
    /// when `e` is a `Call` (so the path is the Vixen call stack, innermost call
    /// first). Kernel poisons already carry their origin span and keep it.
    fn eval(&self, e: ExprId, env: &Env) -> Result<Value, Poison> {
        self.eval_inner(e, env).map_err(|mut p| {
            if p.context.span.is_none() {
                p.context.span = self.expr_span(e);
            }
            // Only `Call` frames extend the demand-path, so it tracks the Vixen
            // call chain a poison crossed rather than every expression it passed.
            if matches!(self.program.arena[e].kind, ExprKind::Call(_))
                && let Some(span) = self.expr_span(e)
            {
                p.context.demand_path.push(span);
            }
            p
        })
    }

    /// The source span (byte range) of an expression.
    fn expr_span(&self, e: ExprId) -> Option<Span> {
        let span = self.program.arena[e].span;
        Some(Span {
            start: span.start(),
            end: span.end(),
        })
    }

    /// Evaluate an expression to a value. Compound construction is lazy (fields
    /// become thunks); only demanded positions are forced. Scalar *compute*
    /// sub-expressions (`+`, `<`, `if`, …) run as kernels when they lower; the
    /// match below is the fallback for everything else.
    fn eval_inner(&self, e: ExprId, env: &Env) -> Result<Value, Poison> {
        if let Some(result) = self.dispatch_scalar(e, env) {
            return result;
        }
        let prog = self.program;
        match &prog.arena[e].kind {
            // 💣 — poisons the instant it is evaluated (i.e. forced).
            ExprKind::Bomb => Err(poison("💣 forced".into())),

            // Explicit failure — poisons whenever reached, carrying the failed
            // value as the domain `err` (a String, or a structured error value).
            // `fail f` re-raises a *caught* `Failure` (a struct carrying both an
            // `err` and a `Value::FailureContext`): unwrap it so we re-raise the
            // inner `err` with the carried context intact, never a Failure-in-a-
            // Failure. A fresh domain error gets a default context (its span fills
            // in as it bubbles).
            ExprKind::Fail(fx) => {
                let raised = match fx.message {
                    Some(m) => self.eval(m, env)?,
                    None => Value::Str(Rc::from("fail")),
                };
                match self.as_caught_failure(&raised)? {
                    // Re-raise: preserve the caught failure's err + context.
                    Some((err, context)) => Err(Poison { err, context }),
                    // A fresh domain error: span fills in as the poison bubbles.
                    None => Err(Poison {
                        err: raised,
                        context: FailureContext::default(),
                    }),
                }
            }

            // `expr?` — a catching force: evaluate `expr` and reify the outcome as a
            // `Result` value instead of propagating a poison. Success → `Ok(v)`; a
            // poison → `Err(Failure { err, context })` — the *same* failure as the
            // poison, so its context (origin span + demand-path) survives the catch
            // (the failure model's "one value, two modes"). The inner may dispatch to
            // the kernel — a `fail`'s `err` survives via the kernel's value-pool side
            // table, so both modes catch the same value.
            ExprKind::Fallible(fb) => match self.eval(fb.expr, env) {
                Ok(v) => Ok(Value::Variant {
                    tag: "Ok".to_string(),
                    fields: vec![("0".to_string(), done(v))],
                }),
                Err(p) => {
                    let failure = Value::Struct(vec![
                        ("err".to_string(), done(p.err)),
                        (
                            "context".to_string(),
                            done(Value::FailureContext(Rc::new(p.context))),
                        ),
                    ]);
                    Ok(Value::Variant {
                        tag: "Err".to_string(),
                        fields: vec![("0".to_string(), done(failure))],
                    })
                }
            },

            ExprKind::Literal(lit) => self.lit_value(&lit.value),

            // A path literal `@some/path` (or interpolated `@{x}/path`): render to a
            // string — paths are strings to eval, the same shape `render_cli_arg`
            // produces. (A real `Path`/`ExecPath` value lands with input handling.)
            ExprKind::PathLiteral(p) => {
                let mut out = String::new();
                for part in &p.parts {
                    match part {
                        StringPart::Text(t) => out.push_str(t),
                        StringPart::Expr(x) => match self.eval(*x, env)? {
                            Value::Str(s) => out.push_str(&s),
                            Value::U64(n) => out.push_str(&n.to_string()),
                            other => {
                                return Err(poison(format!(
                                    "can't interpolate {other:?} into a path"
                                )));
                            }
                        },
                    }
                }
                Ok(Value::Str(Rc::from(out.as_str())))
            }

            // Lazy data: fields are thunks, NOT forced here.
            ExprKind::Tuple(t) => Ok(Value::Tuple(
                t.elements.iter().map(|&x| self.thunk(x, env)).collect(),
            )),
            ExprKind::Array(a) => Ok(Value::Array(
                a.elements
                    .iter()
                    .map(|el| match el {
                        ArrayElement::Expr(x) | ArrayElement::Spread(x) => self.thunk(*x, env),
                    })
                    .collect(),
            )),
            ExprKind::StructLiteral(s) => {
                let fields: Vec<(String, Thunk)> = s
                    .fields
                    .iter()
                    .map(|f| (f.name.clone(), self.thunk(f.value, env)))
                    .collect();
                // A resolved variant index marks `Enum.Variant { … }` (or bare
                // `Variant { … }`) — a tagged value, not a plain struct.
                Ok(if s.resolved_variant.is_some() {
                    let tag = s.variant.clone().unwrap_or_else(|| s.name.clone());
                    Value::Variant { tag, fields }
                } else {
                    Value::Struct(fields)
                })
            }
            ExprKind::Record(r) => Ok(Value::Struct(
                r.fields
                    .iter()
                    .map(|f| (f.name.clone(), self.thunk(f.value, env)))
                    .collect(),
            )),

            // Projection: force the container, then force only the selected slot.
            ExprKind::Index(ix) => {
                let container = self.eval(ix.receiver, env)?;
                let idx = match self.eval(ix.index, env)? {
                    Value::U64(n) => n as usize,
                    other => return Err(poison(format!("non-integer index: {other:?}"))),
                };
                self.project_index(&container, idx)
            }
            ExprKind::Field(fx) => {
                let container = self.eval(fx.receiver, env)?;
                match &fx.field {
                    FieldKey::Index(i) => self.project_index(&container, *i as usize),
                    FieldKey::Name(name) => self.project_name(&container, name),
                }
            }

            // Eager compute (operands forced).
            ExprKind::Binary(bx) => {
                let l = self.eval(bx.lhs, env)?;
                let r = self.eval(bx.rhs, env)?;
                self.binary(bx.op, l, r)
            }
            ExprKind::Prefix(px) => {
                let v = self.eval(px.operand, env)?;
                match v {
                    Value::Bool(b) => Ok(Value::Bool(!b)),
                    other => Err(poison(format!("unsupported prefix on {other:?}"))),
                }
            }

            // Control flow: only the taken branch is evaluated (short-circuit).
            ExprKind::If(ifx) => match self.eval(ifx.condition, env)? {
                Value::Bool(true) => self.eval(ifx.then_branch, env),
                Value::Bool(false) => match ifx.else_branch {
                    Some(eb) => self.eval(eb, env),
                    None => Ok(Value::Unit),
                },
                other => Err(poison(format!("non-bool condition: {other:?}"))),
            },

            // Match: the first arm whose pattern matches wins. A literal pattern
            // forces the scrutinee to compare it; a binding/wildcard binds it
            // lazily (an unread binding never forces it). Mirrors `lower_match`.
            ExprKind::Match(m) => {
                let scrutinee = self.thunk(m.scrutinee, env);
                for arm in &m.arms {
                    if let Some(arm_env) = self.match_pattern(arm.pattern, &scrutinee, env)? {
                        return self.eval(arm.body, &arm_env);
                    }
                }
                Err(poison("non-exhaustive match".into()))
            }

            // Interpolated string: force each part, concatenate.
            ExprKind::InterpolatedString(s) => {
                let mut out = String::new();
                for part in &s.parts {
                    match part {
                        StringPart::Text(t) => out.push_str(t),
                        StringPart::Expr(x) => match self.eval(*x, env)? {
                            Value::Str(s) => out.push_str(&s),
                            Value::U64(n) => out.push_str(&n.to_string()),
                            Value::Bool(b) => out.push_str(&b.to_string()),
                            other => return Err(poison(format!("can't interpolate {other:?}"))),
                        },
                    }
                }
                Ok(Value::Str(Rc::from(out.as_str())))
            }

            // Block: bind `let`s (lazily — the value is a thunk), run expression
            // statements, value is the tail / last expr / unit.
            ExprKind::Block(bl) => {
                let mut local = env.clone();
                let mut last: Option<Value> = None;
                for &s in &bl.stmts {
                    match &prog.arena[s].kind {
                        StmtKind::Let(l) => {
                            let th = self.thunk(l.init, &local);
                            if let PatternKind::Binding(pb) = &prog.arena[l.pattern].kind {
                                local.insert(pb.binding, th);
                            }
                        }
                        StmtKind::Expr(x) => last = Some(self.eval(*x, &local)?),
                        _ => {}
                    }
                }
                match bl.tail {
                    Some(t) => self.eval(t, &local),
                    None => Ok(last.unwrap_or(Value::Unit)),
                }
            }

            // A name: a local binding forces its thunk; a function item used as a
            // value becomes a closure with an empty capture (a top-level function
            // closes over nothing local).
            ExprKind::NameRef(n) => match n.resolved {
                Some(DefTarget::Binding(bid)) => match env.get(&bid) {
                    Some(th) => self.force(th),
                    None => Err(poison(format!("unbound `{}`", n.name))),
                },
                Some(DefTarget::Item(item)) => match &self.program.arena[item].kind {
                    ItemKind::Function(f) => Ok(Value::Closure {
                        body: f.body,
                        env: Env::new(),
                    }),
                    _ => Err(poison(format!("`{}` is not a value", n.name))),
                },
                // A bare variant name in value position — a unit variant (`None`).
                // (A fielded variant referenced bare is a constructor value, handled
                // through `Call` for `Ok(v)`; this is the no-payload case.)
                Some(DefTarget::Variant {
                    enum_item,
                    variant_index,
                }) => match &self.program.arena[enum_item].kind {
                    ItemKind::Enum(e) => match e.variants.get(variant_index as usize) {
                        Some(v) => Ok(Value::Variant {
                            tag: v.name.clone(),
                            fields: vec![],
                        }),
                        None => Err(poison(format!(
                            "variant index out of range for `{}`",
                            n.name
                        ))),
                    },
                    _ => Err(poison(format!("`{}` is not an enum variant", n.name))),
                },
                _ => Err(poison(format!("unresolved name `{}`", n.name))),
            },

            // A lambda: capture the current environment (the body reads only what
            // it closed over; capturing all in-scope thunks is correct if not
            // minimal). `call` binds the parameters when it is invoked.
            ExprKind::Closure(c) => Ok(Value::Closure {
                body: c.body,
                env: env.clone(),
            }),

            // A call: resolve the callee to a function body and evaluate it with
            // the arguments bound (lazily) to its parameters.
            ExprKind::Call(c) => self.call(c.callee, c.args.iter().map(|a| a.expr), env),

            // `exec(command) with { observer: (run) => … }` — the language surface
            // for the exec spine. The only `with`-decorated callable wired so far.
            ExprKind::Where(w) => self.eval_where(w, env),

            // A CLI literal `@[ ${cmd} args… ]`: the leading hole carries the
            // grammar (a value); project it against the rendered arguments.
            ExprKind::CliLiteral(cli) => self.eval_cli_literal(cli, env),

            // `import "@vixen/grammar-cc"` as a value: the package's grammar set,
            // loaded by the front-end. Field access (`.cc`) selects one grammar.
            // (Non-grammar package imports aren't modeled as values yet.)
            ExprKind::Import(imp) => match self.program.arena.grammars.get(&imp.path) {
                Some(set) => Ok(Value::Grammars(Rc::new(set.clone()))),
                None => Err(poison(format!(
                    "import `{}` is not a grammar package (only grammar imports are values yet)",
                    imp.path
                ))),
            },

            other => Err(poison(format!(
                "unsupported expression: {}",
                kind_name(other)
            ))),
        }
    }

    /// Recognise a *caught* `Failure` value — a `Value::Struct` carrying both an
    /// `err` field and a `context` field that is a `Value::FailureContext`. Returns
    /// `(err, context)` so `fail f` can re-raise it with the context intact (the
    /// failure model's "fail f preserves err + context"); `None` for any other value
    /// (a fresh domain error). Forcing the two fields to inspect them may itself
    /// poison, which propagates.
    fn as_caught_failure(&self, value: &Value) -> Result<Option<(Value, FailureContext)>, Poison> {
        let Value::Struct(fields) = value else {
            return Ok(None);
        };
        let (Some(err_th), Some(ctx_th)) = (
            fields.iter().find(|(n, _)| n == "err").map(|(_, t)| t),
            fields.iter().find(|(n, _)| n == "context").map(|(_, t)| t),
        ) else {
            return Ok(None);
        };
        // A `context` field that forces to a FailureContext marks this as a caught
        // Failure (a plain struct that merely has an `err` field is not one).
        let Value::FailureContext(ctx) = self.force(ctx_th)? else {
            return Ok(None);
        };
        Ok(Some((self.force(err_th)?, (*ctx).clone())))
    }

    /// Try to match `scrutinee` against `pattern`, returning the environment
    /// extended with the pattern's bindings on success, or `None` on a tag/shape/
    /// literal mismatch (the caller tries the next arm). Forcing the scrutinee to
    /// inspect it may poison, which propagates. Recursive: a variant pattern checks
    /// its tag, then matches each sub-pattern against the variant's fields.
    fn match_pattern(
        &self,
        pattern: PatternId,
        scrutinee: &Thunk,
        env: &Env,
    ) -> Result<Option<Env>, Poison> {
        match &self.program.arena[pattern].kind {
            // Irrefutable: match anything / bind the whole value (lazily — an unread
            // binding never forces the scrutinee).
            PatternKind::Wildcard => Ok(Some(env.clone())),
            PatternKind::Binding(b) => {
                let mut e = env.clone();
                e.insert(b.binding, scrutinee.clone());
                Ok(Some(e))
            }
            // Refutable scalar: force and compare.
            PatternKind::Literal(lit) => {
                let got = self.force(scrutinee)?;
                let want = self.lit_value(&lit.value)?;
                Ok(self.value_eq(&got, &want)?.then(|| env.clone()))
            }
            // A variant pattern, in either of the two shapes the parser produces for
            // an `UpperIdent { … }` / `UpperIdent(…)` pattern:
            //   - over a `Value::Variant`: compare the tag (last `.`-segment), then
            //     match each field sub-pattern against the variant's like-named field.
            //     A unit pattern (`None`, no fields) matches on the tag alone.
            //   - over a `Value::Struct`: the name is a *struct* (e.g. `Failure`),
            //     written in variant syntax — there is no tag, so match its field
            //     sub-patterns against the struct's like-named fields.
            PatternKind::Variant(vp) => {
                let got = self.force(scrutinee)?;
                let fields = match &got {
                    Value::Variant { tag, fields } => {
                        let want = vp.name.rsplit('.').next().unwrap_or(vp.name.as_str());
                        if tag.as_str() != want {
                            return Ok(None);
                        }
                        fields
                    }
                    // A struct destructure (`Failure { err: e }`): no tag to check.
                    Value::Struct(fields) => fields,
                    _ => return Ok(None),
                };
                let mut e = env.clone();
                if let Some(pat_fields) = &vp.fields {
                    for pf in pat_fields {
                        let Some((_, field_thunk)) = fields.iter().find(|(n, _)| n == &pf.name)
                        else {
                            return Ok(None);
                        };
                        match self.match_pattern(pf.pattern, field_thunk, &e)? {
                            Some(ext) => e = ext,
                            None => return Ok(None),
                        }
                    }
                }
                Ok(Some(e))
            }
            other => Err(poison(format!("unsupported match pattern: {other:?}"))),
        }
    }

    /// Evaluate a grammar-less CLI literal `@[ ${cmd} args… ]`: force the leading
    /// hole to a [`Value::Command`] (carrying its grammar), render the remaining
    /// arguments to argv strings, and project — the grammar classifies inputs vs.
    /// outputs. The result is the command value carrying its projected form.
    fn eval_cli_literal(&self, cli: &CliLiteralExpr, env: &Env) -> Result<Value, Poison> {
        let Some((lead, rest)) = cli.args.split_first() else {
            return Err(poison("empty CLI literal".into()));
        };
        // The leading argument must be a single `${command}` hole.
        let cmd_expr = match lead.pieces.as_slice() {
            [StringPart::Expr(id)] => *id,
            _ => {
                return Err(poison(
                    "`@[ … ]` must lead with a single `${command}` hole".into(),
                ));
            }
        };
        // Extract the bundle (grammar value + program) from the leading hole.
        let bundle = self.command_bundle(&self.eval(cmd_expr, env)?)?;
        // Render the remaining arguments to argv strings (static text verbatim,
        // holes forced to a scalar).
        let mut args = Vec::with_capacity(rest.len());
        for arg in rest {
            args.push(self.render_cli_arg(arg, env)?);
        }
        match crate::command::apply_args(&bundle, &args) {
            Some(applied) => Ok(Value::Command(Rc::new(applied))),
            None => Err(poison(format!(
                "grammar `{}` is not in the command's grammar set",
                bundle.grammar
            ))),
        }
    }

    /// Extract a command bundle (grammar set + grammar name + program) from the
    /// leading hole of a CLI literal — either a command value, or a record
    /// `{ grammar: <Grammar>, binary: <String>, … }` (the shape a capability builds
    /// from `import`ed grammars + a probed binary).
    fn command_bundle(&self, val: &Value) -> Result<crate::command::CommandValue, Poison> {
        use crate::command::CommandValue;
        match val {
            Value::Command(c) => Ok((**c).clone()),
            Value::Struct(fields) => {
                let field = |want: &str| -> Result<Value, Poison> {
                    fields
                        .iter()
                        .find(|(n, _)| n == want)
                        .ok_or_else(|| poison(format!("command record has no `{want}` field")))
                        .and_then(|(_, th)| self.force(th))
                };
                let (set, name) = match field("grammar")? {
                    Value::Grammar { set, name } => (set, name),
                    other => {
                        return Err(poison(format!(
                            "`grammar` must be a Grammar, got {other:?}"
                        )));
                    }
                };
                let binary = match field("binary")? {
                    Value::Str(s) => s.to_string(),
                    other => {
                        return Err(poison(format!("`binary` must be a String, got {other:?}")));
                    }
                };
                Ok(CommandValue {
                    grammars: (*set).clone(),
                    grammar: name,
                    binary,
                    projected: None,
                })
            }
            other => Err(poison(format!(
                "the leading hole of `@[ … ]` must be a command, got {other:?}"
            ))),
        }
    }

    /// Render one CLI-literal argument to a string: static text verbatim, each
    /// `$x` / `${ … }` hole forced to a scalar.
    fn render_cli_arg(&self, arg: &CliArgIr, env: &Env) -> Result<String, Poison> {
        let mut s = String::new();
        for piece in &arg.pieces {
            match piece {
                StringPart::Text(t) => s.push_str(t),
                StringPart::Expr(id) => match self.eval(*id, env)? {
                    Value::Str(v) => s.push_str(&v),
                    Value::U64(n) => s.push_str(&n.to_string()),
                    other => {
                        return Err(poison(format!("can't render {other:?} as a CLI argument")));
                    }
                },
            }
        }
        Ok(s)
    }

    /// Try to run expression `e` (in `env`) as a kernel — the JIT/interpreter path
    /// on the real eval run. `None` ⇒ not dispatchable; `eval` tree-walks. `Some` ⇒
    /// the value (or poison), provably equal to tree-walking. Only scalar *compute*
    /// expressions are candidates; the kernel folds projections into constants, so
    /// an arithmetic expression over struct/tuple fields runs as one kernel. The
    /// plan (and its soundness gates) lives in [`crate::dispatch`]; this is the hook
    /// that forces the parameter bindings from `env` and runs it.
    fn dispatch_scalar(&self, e: ExprId, env: &Env) -> Option<Result<Value, Poison>> {
        if self.mode == KernelMode::Off {
            return None; // pure tree-walk — never dispatch (the ground-truth oracle)
        }
        if !matches!(
            self.program.arena[e].kind,
            ExprKind::Binary(_) | ExprKind::Prefix(_) | ExprKind::If(_)
        ) {
            return None;
        }
        let plan = self
            .dispatch
            .plan(self.program, self.env, e, self.current_body.get())?;

        // Force each parameter's binding to a scalar word. A non-scalar value (a
        // string, a compound) ⇒ fall back to the tree-walker, which handles it
        // correctly; a poison while forcing a used parameter ⇒ propagate.
        let mut words = vec![0u64; plan.param_bindings.len()];
        for (slot, binding) in plan.param_bindings.iter().enumerate() {
            match self.force(env.get(binding)?) {
                Ok(v) => words[slot] = scalar_word(&v)?,
                Err(p) => return Some(Err(p)),
            }
        }

        self.dispatches.set(self.dispatches.get() + 1);
        Some(
            match dispatch::run_kernel(&plan.kernel, &words, self.mode == KernelMode::Jit) {
                Ok(w) => Ok(if plan.returns_bool {
                    Value::Bool(w != 0)
                } else {
                    Value::U64(w)
                }),
                // Lift the kernel poison into an eval poison — it already carries the
                // typed `err` (a `fail`'s pooled value, or a `Value::Str` for an
                // internal fault) and the origin span (I6), which becomes the failure
                // context's `span`. The kernel has no Vixen call stack of its own, so
                // the demand-path starts empty and accumulates as this bubbles.
                Err(p) => Err(Poison {
                    err: p.err,
                    context: FailureContext {
                        span: p.span,
                        demand_path: Vec::new(),
                    },
                }),
            },
        )
    }

    fn call(
        &self,
        callee: ExprId,
        args: impl Iterator<Item = ExprId>,
        env: &Env,
    ) -> Result<Value, Poison> {
        let prog = self.program;
        // Fast path: a direct call to a named function — bind args over an empty
        // base env (a top-level function closes over nothing local).
        if let ExprKind::NameRef(n) = &prog.arena[callee].kind {
            match n.resolved {
                Some(DefTarget::Item(item)) => {
                    return match &prog.arena[item].kind {
                        ItemKind::Function(f) => self.invoke_body(f.body, args, env, Env::new()),
                        _ => Err(poison(format!("`{}` is not callable", n.name))),
                    };
                }
                // Tuple-variant construction `Ok(v)` — bind the args to the variant's
                // positional "0","1",… fields (the names a tuple variant's payload
                // carries), so a tuple variant is a struct variant with index names.
                Some(DefTarget::Variant {
                    enum_item,
                    variant_index,
                }) => {
                    let tag = match &prog.arena[enum_item].kind {
                        ItemKind::Enum(e) => match e.variants.get(variant_index as usize) {
                            Some(v) => v.name.clone(),
                            None => {
                                return Err(poison(format!(
                                    "variant index out of range for `{}`",
                                    n.name
                                )));
                            }
                        },
                        _ => return Err(poison(format!("`{}` is not an enum variant", n.name))),
                    };
                    let fields = args
                        .enumerate()
                        .map(|(i, a)| (i.to_string(), self.thunk(a, env)))
                        .collect();
                    return Ok(Value::Variant { tag, fields });
                }
                _ => {}
            }
        }
        // `receiver.method(args)` — a native builtin method.
        if let ExprKind::Field(fx) = &prog.arena[callee].kind {
            return self.call_method(fx, args, env);
        }
        // Otherwise the callee must evaluate to a closure value — a closure-bound
        // name, a lambda literal, a call result. Invoke it over its captured env.
        match self.eval(callee, env)? {
            Value::Closure {
                body,
                env: captured,
            } => self.invoke_body(body, args, env, captured),
            other => Err(poison(format!("not callable: {other:?}"))),
        }
    }

    /// Invoke `body` binding its parameters to already-evaluated `vals` over `base`
    /// — for native callers that hold values rather than argument expressions.
    fn invoke_body_values(
        &self,
        body: BodyId,
        vals: Vec<Value>,
        base: Env,
    ) -> Result<Value, Poison> {
        let prog = self.program;
        let mut callee_env = base;
        for (param, val) in prog.arena[body].params.iter().zip(vals) {
            if let PatternKind::Binding(pb) = &prog.arena[param.pattern].kind {
                callee_env.insert(
                    pb.binding,
                    Thunk(Rc::new(RefCell::new(ThunkState::Done(Ok(val))))),
                );
            }
        }
        let prev = self.current_body.replace(body);
        let result = self.eval(prog.arena[body].root_expr, &callee_env);
        self.current_body.set(prev);
        result
    }

    /// `exec(command) with { observer: (run) => … }` — the eval surface of the exec
    /// spine. Recognise the exec shape, project the command to its execution argv,
    /// bundle the observer's *static reachable closure* (its code, not a forced
    /// value), and dispatch the `observe` RPC to an executor **over vox** — parking
    /// the strand until the boss drives the call and it returns. The orchestrator
    /// ships code; the executor runs the command, builds a `Run`, evaluates the
    /// observer there, and returns the result (`crate::observe`).
    ///
    /// eval must be running on a strand (so `dispatch_observe` can park, freeing its
    /// worker): an `exec` reached off a strand — or with no executor registered —
    /// poisons honestly rather than blocking.
    fn eval_where(&self, w: &WhereExpr, env: &Env) -> Result<Value, Poison> {
        // The only `where`-decorated callable wired so far is `exec(command)`: the
        // callee is a `Call` whose own callee is the `exec` name (mirroring the
        // `discover` recognition in `call`).
        let ExprKind::Call(call) = &self.program.arena[w.callee].kind else {
            return Err(poison(
                "`where` keyword arguments are only supported on `exec` so far".into(),
            ));
        };
        let is_exec = matches!(
            &self.program.arena[call.callee].kind,
            ExprKind::NameRef(n) if n.name == "exec"
        );
        if !is_exec {
            return Err(poison(
                "`where` keyword arguments are only supported on `exec` so far".into(),
            ));
        }

        // The command — evaluate the single positional arg to a `Command` value and
        // project it to the minimal `ExecCommand` (program + args) the RPC carries.
        let command_expr = call
            .args
            .first()
            .ok_or_else(|| poison("`exec` expects a command argument".into()))?
            .expr;
        let command = self.exec_command(&self.eval(command_expr, env)?)?;

        // The observer — ship its body's reachable closure, not a forced value.
        let observer_body = self.observer_body(w.keywords)?;
        let bundle = vx_ir::reachable(&self.program.arena, self.env, observer_body);

        // Dispatch over vox on the strand. The boss drives `observe()`; the strand
        // parks (`AwaitingObserve`) until it lands. eval knows only the client id.
        let client = self.exec_client.ok_or_else(|| {
            poison(
                "`exec` must run on a strand with an observe executor registered \
                 (no executor available)"
                    .into(),
            )
        })?;
        let observed = dispatch_observe(client, ObserveRequest { bundle, command });

        // The result, with handles already crossed by the boss: a data value rebuilds
        // (`from_wire`); a returned producing tree is a registered `HandleId` that
        // `force_remote` pulls (`run.stream.out.file(@p)` over vox); a poison detonates
        // here (I6 across the boundary).
        match &*observed {
            ObserveOutcome::Value(wire) => from_wire(wire),
            ObserveOutcome::Tree(handle) => Ok(Value::Tree(*handle)),
            ObserveOutcome::Poisoned(reason) => Err(poison(reason.clone())),
        }
    }

    /// Project a `Command` value to the minimal [`ExecCommand`] the `observe` RPC
    /// carries: the literal execution argv (holes substituted), split into the
    /// program and its arguments. Poisons if the value is not a projected command.
    fn exec_command(&self, v: &Value) -> Result<ExecCommand, Poison> {
        let Value::Command(cmd) = v else {
            return Err(poison(format!("`exec` expects a Command, got {v:?}")));
        };
        let projected = cmd.projected.as_ref().ok_or_else(|| {
            poison("`exec`'s command has not been projected against its grammar".into())
        })?;
        let (program, args) = projected
            .execution_argv
            .split_first()
            .ok_or_else(|| poison("`exec`'s command has an empty argv".into()))?;
        Ok(ExecCommand {
            program: program.clone(),
            args: args.to_vec(),
        })
    }

    /// The observer body from an `exec` `where` record: find the `observer` keyword
    /// (which must be a closure) and return its `BodyId`. Slice-1 requires an
    /// explicit observer; the signature's default (`(run) => run.ok.out`) is a later
    /// refinement (it lives in the sig, not the call's keyword record).
    fn observer_body(&self, keywords: ExprId) -> Result<BodyId, Poison> {
        let ExprKind::Record(rec) = &self.program.arena[keywords].kind else {
            return Err(poison(
                "`exec`'s `where` clause must be a keyword record".into(),
            ));
        };
        let observer = rec
            .fields
            .iter()
            .find(|f| f.name == "observer")
            .ok_or_else(|| poison("`exec`'s `where` clause needs an `observer`".into()))?;
        match &self.program.arena[observer.value].kind {
            ExprKind::Closure(c) => Ok(c.body),
            _ => Err(poison("`exec`'s `observer` must be a closure".into())),
        }
    }

    /// Deep-force a value into a [`WireValue`] — the executor side of the value
    /// crossing. Every nested thunk is forced (a value crosses as data, never as a
    /// thunk), so the observer's whole result travels, not a lossy top-level peek.
    /// Forcing may poison (a forced bomb, a failed gate), which propagates. Values
    /// that are not data — closures (code), commands/grammars/host (capabilities),
    /// the failure envelope — cannot cross this way and poison honestly; the
    /// genuinely-remote leaves (`Stream`/`Tree`) will cross as handles, separately.
    fn to_wire(&self, v: &Value) -> Result<WireValue, Poison> {
        Ok(match v {
            Value::U64(n) => WireValue::U64(*n),
            Value::Bool(b) => WireValue::Bool(*b),
            Value::Str(s) => WireValue::Str(s.to_string()),
            Value::Unit => WireValue::Unit,
            Value::Tuple(ts) => WireValue::Tuple(self.wire_thunks(ts)?),
            Value::Array(ts) => WireValue::Array(self.wire_thunks(ts)?),
            Value::Struct(fields) => WireValue::Struct(self.wire_fields(fields)?),
            Value::Variant { tag, fields } => WireValue::Variant {
                tag: tag.clone(),
                fields: self.wire_fields(fields)?,
            },
            // The run's output tree is a genuinely-remote leaf — it crosses as a
            // handle, not inline. Mark it; the executor serves its parts over a
            // `Producing` virtual connection the orchestrator opens (the producer +
            // boss-side `register_handle` are the next pieces).
            Value::OutputTree => WireValue::Tree,
            other => {
                return Err(poison(format!(
                    "`exec` observer returned a value that cannot cross back: {other:?}"
                )));
            }
        })
    }

    /// Force each thunk and wire it — for tuple/array elements.
    fn wire_thunks(&self, ts: &[Thunk]) -> Result<Vec<WireValue>, Poison> {
        ts.iter().map(|t| self.to_wire(&self.force(t)?)).collect()
    }

    /// Force each field's thunk and wire it, keeping the name — for struct fields and
    /// variant payloads.
    fn wire_fields(&self, fields: &[(String, Thunk)]) -> Result<Vec<(String, WireValue)>, Poison> {
        fields
            .iter()
            .map(|(name, t)| Ok((name.clone(), self.to_wire(&self.force(t)?)?)))
            .collect()
    }

    /// Bind `args` (lazy thunks in the *caller's* `caller_env`) to `body`'s
    /// parameters over `base` — a closure's captured env, or empty for a top-level
    /// function — then evaluate `body`. `body` becomes current while it runs;
    /// forcing an argument later restores the caller's body (the thunk carries it),
    /// and the body's own scalar regions still dispatch to kernels.
    fn invoke_body(
        &self,
        body: BodyId,
        args: impl Iterator<Item = ExprId>,
        caller_env: &Env,
        base: Env,
    ) -> Result<Value, Poison> {
        let prog = self.program;
        let arg_thunks: Vec<Thunk> = args.map(|a| self.thunk(a, caller_env)).collect();
        let mut callee_env = base;
        for (param, th) in prog.arena[body].params.iter().zip(arg_thunks) {
            if let PatternKind::Binding(pb) = &prog.arena[param.pattern].kind {
                callee_env.insert(pb.binding, th);
            }
        }
        let prev = self.current_body.replace(body);
        let result = self.eval(prog.arena[body].root_expr, &callee_env);
        self.current_body.set(prev);
        result
    }

    /// Dispatch a native builtin method `receiver.method(args)`. The receiver and
    /// arguments are forced — these are eager scalar/string ops. The implemented
    /// set mirrors vx-ir's `try_lower_method_call`; only the string methods the
    /// language needs so far are wired (the exec-path ones — `collect`/`blob`/
    /// `which` — land with exec). An unknown method poisons.
    fn call_method(
        &self,
        fx: &FieldExpr,
        args: impl Iterator<Item = ExprId>,
        env: &Env,
    ) -> Result<Value, Poison> {
        let method = match &fx.field {
            FieldKey::Name(n) => n.clone(),
            FieldKey::Index(i) => return Err(poison(format!("cannot call tuple index `{i}`"))),
        };
        let recv = self.eval(fx.receiver, env)?;
        let argv = args
            .map(|a| self.eval(a, env))
            .collect::<Result<Vec<_>, _>>()?;
        match (method.as_str(), &recv) {
            // Byte length of the string (per core.vx's `String::len`).
            ("len", Value::Str(s)) => Ok(Value::U64(s.len() as u64)),
            // Substring `s[start..end]` — two integer args, bounds- and UTF-8
            // boundary-checked (poisons otherwise, matching the old evaluator).
            ("slice", Value::Str(s)) => {
                let (start, end) = match argv.as_slice() {
                    [Value::U64(a), Value::U64(b)] => (*a as usize, *b as usize),
                    other => {
                        return Err(poison(format!(
                            ".slice() expects two integer args, got {other:?}"
                        )));
                    }
                };
                s.get(start..end)
                    .map(|sub| Value::Str(Rc::from(sub)))
                    .ok_or_else(|| {
                        poison(format!(
                            "string slice out of bounds: [{start}..{end}] on string of length {}",
                            s.len()
                        ))
                    })
            }
            // `tree.file(@path)` on a remote producing tree — the demand-pull across
            // executors, reached from the language. Force the path over vox via the
            // strand (`force_remote` parks until the producer yields it, or poisons if
            // the producer finished without it). The bytes render lossily as a `Str`
            // until a real `Bytes`/`Blob` value lands.
            ("file", Value::Tree(handle)) => {
                let path = match argv.as_slice() {
                    [Value::Str(p)] => p.to_string(),
                    other => {
                        return Err(poison(format!("tree.file expects one path, got {other:?}")));
                    }
                };
                match &*force_remote(*handle, PartKey::TreePath(path.clone())) {
                    Part::Bytes(b) => Ok(Value::Str(Rc::from(String::from_utf8_lossy(b).as_ref()))),
                    Part::Absent(why) => Err(poison(format!("tree path `{path}` absent: {why}"))),
                }
            }
            (m, r) => Err(poison(format!("unknown method `{m}` on {r:?}"))),
        }
    }

    fn project_index(&self, container: &Value, idx: usize) -> Result<Value, Poison> {
        let slot = match container {
            Value::Tuple(ts) | Value::Array(ts) => ts.get(idx),
            other => return Err(poison(format!("can't index {other:?}"))),
        };
        match slot {
            Some(th) => self.force(th),
            None => Err(poison(format!("index {idx} out of bounds"))),
        }
    }

    fn project_name(&self, container: &Value, name: &str) -> Result<Value, Poison> {
        match container {
            Value::Struct(fields) => match fields.iter().find(|(n, _)| n == name) {
                Some((_, th)) => self.force(th),
                None => Err(poison(format!("no field `{name}`"))),
            },
            // Selecting a grammar from an imported grammar package (`grammars.cc`).
            Value::Grammars(set) if set.get(name).is_some() => Ok(Value::Grammar {
                set: set.clone(),
                name: name.to_string(),
            }),
            Value::Grammars(_) => Err(poison(format!(
                "grammar `{name}` is not in the imported package"
            ))),
            other => Err(poison(format!("can't access `.{name}` on {other:?}"))),
        }
    }

    /// A literal's runtime value — shared by literal expressions and literal match
    /// patterns, so both parse identically.
    fn lit_value(&self, lit: &LitKind) -> Result<Value, Poison> {
        match lit {
            LitKind::Int { raw } => raw
                .parse::<u64>()
                .map(Value::U64)
                .map_err(|_| poison(format!("bad int literal `{raw}`"))),
            LitKind::Bool(b) => Ok(Value::Bool(*b)),
            LitKind::String { raw } => Ok(Value::Str(Rc::from(raw.as_str()))),
            LitKind::Float { raw } => Err(poison(format!("floats not yet supported: {raw}"))),
        }
    }

    fn binary(&self, op: BinOp, l: Value, r: Value) -> Result<Value, Poison> {
        use BinOp::*;
        let int = |v: &Value| match v {
            Value::U64(n) => Some(*n),
            _ => None,
        };
        match op {
            Eq => Ok(Value::Bool(self.value_eq(&l, &r)?)),
            Neq => Ok(Value::Bool(!self.value_eq(&l, &r)?)),
            And => match (l, r) {
                (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(a && b)),
                _ => Err(poison("`&&` wants bools".into())),
            },
            Or => match (l, r) {
                (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(a || b)),
                _ => Err(poison("`||` wants bools".into())),
            },
            Concat => match (&l, &r) {
                (Value::Str(a), Value::Str(b)) => {
                    Ok(Value::Str(Rc::from(format!("{a}{b}").as_str())))
                }
                _ => Err(poison("`++` wants strings".into())),
            },
            Add | Sub | Mul | Div | Rem | Lt | LtEq | Gt | GtEq => {
                let (a, b) = match (int(&l), int(&r)) {
                    (Some(a), Some(b)) => (a, b),
                    _ => {
                        return Err(poison(format!(
                            "`{op:?}` wants integers, got {l:?} / {r:?}"
                        )));
                    }
                };
                Ok(match op {
                    Add => Value::U64(a.wrapping_add(b)),
                    Sub => Value::U64(a.wrapping_sub(b)),
                    Mul => Value::U64(a.wrapping_mul(b)),
                    Div if b == 0 => return Err(poison("divide by zero".into())),
                    Div => Value::U64(a / b),
                    Rem if b == 0 => return Err(poison("remainder by zero".into())),
                    Rem => Value::U64(a % b),
                    Lt => Value::Bool(a < b),
                    LtEq => Value::Bool(a <= b),
                    Gt => Value::Bool(a > b),
                    GtEq => Value::Bool(a >= b),
                    _ => unreachable!(),
                })
            }
            Pipe => Err(poison("`|>` not yet supported".into())),
        }
    }

    /// Structural equality — forces both sides (and, for compounds, their fields).
    fn value_eq(&self, l: &Value, r: &Value) -> Result<bool, Poison> {
        Ok(match (l, r) {
            (Value::U64(a), Value::U64(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Unit, Value::Unit) => true,
            (Value::Tuple(a), Value::Tuple(b)) | (Value::Array(a), Value::Array(b)) => {
                a.len() == b.len()
                    && a.iter().zip(b).try_fold(true, |acc, (x, y)| {
                        Ok(acc && self.value_eq(&self.force(x)?, &self.force(y)?)?)
                    })?
            }
            // Same tag and field-wise equal (matched by name — construction order is
            // stable, but a by-name compare is robust to it). A plain struct is the
            // tagless case of the same shape.
            (
                Value::Variant {
                    tag: ta,
                    fields: fa,
                },
                Value::Variant {
                    tag: tb,
                    fields: fb,
                },
            ) => ta == tb && self.fields_eq(fa, fb)?,
            (Value::Struct(fa), Value::Struct(fb)) => self.fields_eq(fa, fb)?,
            // The opaque failure envelope compares by identity (the same `Rc`):
            // distinct failures are never structurally equal — authors carry it
            // through, they do not compare it.
            (Value::FailureContext(a), Value::FailureContext(b)) => Rc::ptr_eq(a, b),
            _ => false,
        })
    }

    /// Field-wise equality of two named-field sequences (struct fields or variant
    /// payloads): same length, and every name in `a` has an equal-valued match in `b`.
    fn fields_eq(&self, a: &[(String, Thunk)], b: &[(String, Thunk)]) -> Result<bool, Poison> {
        if a.len() != b.len() {
            return Ok(false);
        }
        for (name, x) in a {
            let Some((_, y)) = b.iter().find(|(m, _)| m == name) else {
                return Ok(false);
            };
            if !self.value_eq(&self.force(x)?, &self.force(y)?)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn kind_name(_k: &ExprKind) -> &'static str {
    "<unhandled>"
}

/// A scalar value as an ABI word: `U64` is itself, `Bool` is `0`/`1`. Anything
/// else (string, compound) is not a kernel value.
fn scalar_word(v: &Value) -> Option<u64> {
    match v {
        Value::U64(n) => Some(*n),
        Value::Bool(b) => Some(*b as u64),
        _ => None,
    }
}

/// Evaluate function `entry` in `source` through the demand-driven engine, with
/// the assert prelude prepended. `Err` is a setup failure (parse / not found);
/// `Ok(Outcome)` is the evaluation result (clean or poisoned).
pub fn run(source: &str, entry: &str) -> Result<Outcome, String> {
    run_in_mode(source, entry, KernelMode::Jit).map(|(outcome, _)| outcome)
}

/// As [`run`], but also reports how many calls ran as kernels (the JIT/interpreter
/// path) rather than tree-walking — the observable proof that the kernel path is
/// on the real evaluation run. Uses the default [`KernelMode::Jit`].
pub fn run_with_dispatches(source: &str, entry: &str) -> Result<(Outcome, u64), String> {
    run_in_mode(source, entry, KernelMode::Jit)
}

/// Evaluate `entry` in `source` under an explicit [`KernelMode`], reporting the
/// outcome and the kernel-dispatch count. The self-hosting `.vx` harness calls
/// this once per available mode and asserts the outcomes agree (cross-mode
/// parity = the kernel and JIT oracles). `assert`/`assert_eq` resolve from the
/// stdlib prelude — no string prepending.
pub fn run_in_mode(source: &str, entry: &str, mode: KernelMode) -> Result<(Outcome, u64), String> {
    let (program, env) = crate::analyze::front_end(source)?;
    let primary = *program.source_units.first().ok_or("no source unit")?;
    let body = program.arena[primary]
        .items
        .iter()
        .copied()
        .find_map(|id| match &program.arena[id].kind {
            ItemKind::Function(f) if f.name == entry => Some(f.body),
            _ => None,
        })
        .ok_or_else(|| format!("function `{entry}` not found"))?;
    let root = program.arena[body].root_expr;
    let ev = Evaluator {
        program: &program,
        env: &env,
        current_body: Cell::new(body),
        dispatch: Dispatcher::default(),
        dispatches: Cell::new(0),
        mode,
        exec_client: None,
    };
    let outcome = match ev.eval(root, &Env::new()) {
        Ok(v) => Outcome::Clean(v),
        Err(p) => Outcome::Poisoned {
            reason: p.message(),
            span: p.context.span,
        },
    };
    Ok((outcome, ev.dispatches.get()))
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
    eval_body_inner(program, env, body, args, mode, None)
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
    eval_body_inner(program, env, body, args, mode, Some(exec_client))
}

fn eval_body_inner(
    program: &VirProgram,
    env: &TypeEnv,
    body: BodyId,
    args: Vec<Value>,
    mode: KernelMode,
    exec_client: Option<ObserveClientId>,
) -> Outcome {
    let ev = Evaluator {
        program,
        env,
        current_body: Cell::new(body),
        dispatch: Dispatcher::default(),
        dispatches: Cell::new(0),
        mode,
        exec_client,
    };
    match ev.invoke_body_values(body, args, Env::new()) {
        Ok(v) => Outcome::Clean(v),
        Err(p) => Outcome::Poisoned {
            reason: p.message(),
            span: p.context.span,
        },
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
    let ev = Evaluator {
        program,
        env,
        current_body: Cell::new(body),
        dispatch: Dispatcher::default(),
        dispatches: Cell::new(0),
        mode,
        exec_client: None,
    };
    ev.invoke_body_values(body, args, Env::new())
        .and_then(|v| ev.to_wire(&v))
        .map_err(|p| p.message())
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
    // assert engine internals a program can't observe (dispatch counts, the
    // tree-walk fallback, the I6 span), so they stay in Rust.

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
    fn conditionally_used_param_declines_dispatch() {
        // `a`/`b` are read only inside their branches, not the entry block: the
        // laziness gate must decline the kernel so the lazy walker takes the live
        // branch and never forces the bomb in the dead one.
        let src = "fn pick(c: Bool, a: U64, b: U64) -> U64 ( if c ( a ) else ( b ) ) \
                   fn main() -> U64 ( pick(true, 7, 💣) )";
        let (outcome, dispatches) = run_with_dispatches(src, "main").unwrap();
        assert!(
            matches!(outcome, Outcome::Clean(Value::U64(7))),
            "got {outcome:?}"
        );
        assert_eq!(
            dispatches, 0,
            "the gate should decline; the walker handles it"
        );
    }

    #[test]
    fn non_scalar_args_fall_back_to_tree_walk() {
        // `a == b` on strings would mis-lower to a word compare — but the non-scalar
        // argument trips the call-site gate, so it falls back to the tree-walker's
        // real string equality. No dispatch.
        let src = r#"fn eq(a: String, b: String) -> Bool ( a == b ) fn main() -> Bool ( eq("hi", "hi") )"#;
        let (outcome, dispatches) = run_with_dispatches(src, "main").unwrap();
        assert!(
            matches!(outcome, Outcome::Clean(Value::Bool(true))),
            "got {outcome:?}"
        );
        assert_eq!(
            dispatches, 0,
            "string args must fall back, not run the word kernel"
        );
    }

    /// I6 at the eval boundary: a poison reaching `Outcome` carries a source span —
    /// from a dispatched kernel (divide-by-zero) *and* from the tree-walker (a
    /// forced bomb). The spanned source text is the token that detonated.
    #[test]
    fn i6_eval_outcome_carries_the_source_span() {
        for (src, marker) in [
            ("fn f() -> U64 ( 10 / 0 )", "/"),       // kernel-dispatched poison
            ("fn f() -> U64 ( (42, 💣)[1] )", "💣"), // tree-walked poison
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

    /// Evaluate `entry` in `source` on the pure tree-walk path and return the raw
    /// [`Poison`] it detonates with — so a test can inspect the failure context
    /// (`demand_path`, `span`) that `Outcome` flattens away. Panics if it evaluates
    /// clean (the caller expects a failure).
    fn poison_of(source: &str, entry: &str) -> Poison {
        let (program, env) = crate::analyze::front_end(source).expect("front-end");
        let primary = *program.source_units.first().expect("source unit");
        let body = program.arena[primary]
            .items
            .iter()
            .copied()
            .find_map(|id| match &program.arena[id].kind {
                ItemKind::Function(f) if f.name == entry => Some(f.body),
                _ => None,
            })
            .unwrap_or_else(|| panic!("function `{entry}` not found"));
        let root = program.arena[body].root_expr;
        let ev = Evaluator {
            program: &program,
            env: &env,
            current_body: Cell::new(body),
            dispatch: Dispatcher::default(),
            dispatches: Cell::new(0),
            mode: KernelMode::Off,
            exec_client: None,
        };
        match ev.eval(root, &Env::new()) {
            Ok(v) => panic!("expected a poison, evaluated clean: {v:?}"),
            Err(p) => p,
        }
    }

    /// The failure model's demand-path: a poison crossing the Vixen call chain
    /// accumulates each `Call` frame's span as it bubbles, innermost call first.
    /// Entering `c` evaluates its body `b()`, which calls `a()`, and `a` `fail`s —
    /// so as the poison climbs it crosses the `a()` call (pushed first = innermost)
    /// then the `b()` call, giving the path `[a(), b()]`, each span indexing one
    /// call's source text. (Only `Call` frames are pushed, so the path is the Vixen
    /// call stack, not the whole expression spine — the `fail` and the `if`/block
    /// scaffolding contribute nothing.) Inspected directly off the `Poison`, which
    /// is where the demand-path lives — `Outcome` keeps only the origin span.
    #[test]
    fn demand_path_accumulates_the_call_chain() {
        let src = "fn a() -> U64 ( fail \"boom\" )\n\
                   fn b() -> U64 ( a() )\n\
                   fn c() -> U64 ( b() )";
        let p = poison_of(src, "c");

        // The origin span is the `fail`'s message, not a call.
        let origin = p.context.span.expect("origin span");
        let origin_text = &src[origin.start as usize..origin.end as usize];
        assert!(
            origin_text.contains("boom"),
            "origin should be the failed value, got {origin_text:?}"
        );

        // The demand-path is the two call sites the poison crossed, innermost first:
        // a() (the call that detonated) then b() (its caller). The span can include
        // leading trivia, so compare on the trimmed text.
        let path_texts: Vec<String> = p
            .context
            .demand_path
            .iter()
            .map(|s| src[s.start as usize..s.end as usize].trim().to_string())
            .collect();
        assert_eq!(
            path_texts,
            vec!["a()".to_string(), "b()".to_string()],
            "demand-path should be the call chain innermost-first, got {path_texts:?}"
        );
    }

    /// The CLI-literal resolver, on the real eval path: `@[ ${cmd} -c x.c -o x.o ]`
    /// forces the leading hole to a command value (carrying the cc grammar as a
    /// value), renders the rest to argv, and projects — the grammar classifies
    /// `x.c` as an input and the `-o` target as an output.
    #[test]
    fn cli_literal_projects_the_command_grammar() {
        use crate::command::CommandValue;
        use crate::dispatch::Dispatcher;
        use command_model::examples;
        use std::cell::{Cell, RefCell};
        use std::rc::Rc;
        use vx_ir::item::ItemKind;
        use vx_ir::pattern::PatternKind;

        let src = "fn t(cmd: Command) -> Command ( @[ ${cmd} -c x.c -o x.o ] )";
        let (program, tenv) = crate::analyze::front_end(src).expect("front-end");

        // Locate `t`, its body, `cmd`'s binding, and the CLI-literal root expr.
        let primary = *program.source_units.first().unwrap();
        let body = program.arena[primary]
            .items
            .iter()
            .find_map(|&id| match &program.arena[id].kind {
                ItemKind::Function(f) if f.name == "t" => Some(f.body),
                _ => None,
            })
            .expect("fn t");
        let cmd_bid = match &program.arena[program.arena[body].params[0].pattern].kind {
            PatternKind::Binding(pb) => pb.binding,
            other => panic!("cmd must be a binding pattern, got {other:?}"),
        };
        let root = program.arena[body].root_expr;

        // Bind `cmd` to a cc command value — the grammar carried as a value.
        let cmd = Value::Command(Rc::new(CommandValue {
            grammars: examples(),
            grammar: "cc".to_string(),
            binary: "cc".to_string(),
            projected: None,
        }));
        let mut start = Env::new();
        start.insert(
            cmd_bid,
            Thunk(Rc::new(RefCell::new(ThunkState::Done(Ok(cmd))))),
        );

        let ev = Evaluator {
            program: &program,
            env: &tenv,
            current_body: Cell::new(body),
            dispatch: Dispatcher::default(),
            dispatches: Cell::new(0),
            mode: KernelMode::Off,
            exec_client: None,
        };
        let Value::Command(applied) = ev.eval(root, &start).expect("eval cli literal") else {
            panic!("expected a Command value");
        };
        let p = applied
            .projected
            .as_ref()
            .expect("projected after the literal");
        assert!(
            p.effective.inputs.iter().any(|i| i.path == "x.c"),
            "x.c is an input: {:?}",
            p.effective.inputs
        );
        assert!(
            p.outputs.iter().any(|o| o.path == "x.o"),
            "the -o target is an output: {:?}",
            p.outputs
        );
        assert_eq!(p.execution_argv, vec!["cc", "-c", "x.c", "-o", "x.o"]);
    }

    /// `import "@vixen/grammar-cc"` evaluates to a grammar-set value, and `.cc`
    /// selects one grammar — the grammar-as-value path on the real eval, against
    /// the on-disk package (resolved via a CompositeResolver over packages/).
    #[test]
    fn import_grammar_package_selects_a_grammar_value() {
        use crate::dispatch::Dispatcher;
        use std::cell::Cell;
        use vx_ir::compile::{NoopObserver, semantic_front_end};
        use vx_ir::item::ItemKind;
        use vx_package::CompositeResolver;

        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages");
        let resolver = CompositeResolver::new(root);
        let src = "fn t() ( let g = import \"@vixen/grammar-cc\"; g.cc )";
        let fe = semantic_front_end(src, &resolver, &mut NoopObserver).expect("front-end");

        let primary = *fe.program.source_units.first().unwrap();
        let body = fe.program.arena[primary]
            .items
            .iter()
            .find_map(|&id| match &fe.program.arena[id].kind {
                ItemKind::Function(f) if f.name == "t" => Some(f.body),
                _ => None,
            })
            .expect("fn t");
        let root_expr = fe.program.arena[body].root_expr;

        let ev = Evaluator {
            program: &fe.program,
            env: &fe.env,
            current_body: Cell::new(body),
            dispatch: Dispatcher::default(),
            dispatches: Cell::new(0),
            mode: KernelMode::Off,
            exec_client: None,
        };
        match ev
            .eval(root_expr, &Env::new())
            .expect("eval grammar import")
        {
            Value::Grammar { name, .. } => assert_eq!(name, "cc"),
            other => panic!("expected a Grammar value, got {other:?}"),
        }
    }

    /// The whole grammar-as-value path from real `.vx`: import a grammar, build a
    /// command record `{ grammar: g.cc, binary: "cc" }`, and have `@[ ${cmd} … ]`
    /// project it — no test-injected value, no grammar name in the syntax.
    #[test]
    fn cli_literal_from_imported_grammar_projects() {
        use crate::dispatch::Dispatcher;
        use std::cell::Cell;
        use vx_ir::compile::{NoopObserver, semantic_front_end};
        use vx_ir::item::ItemKind;
        use vx_package::CompositeResolver;

        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages");
        let resolver = CompositeResolver::new(root);
        let src = "fn t() ( \
                   let g = import \"@vixen/grammar-cc\"; \
                   let cmd = { grammar: g.cc, binary: \"cc\" }; \
                   @[ ${cmd} -c x.c -o x.o ] )";
        let fe = semantic_front_end(src, &resolver, &mut NoopObserver).expect("front-end");

        let primary = *fe.program.source_units.first().unwrap();
        let body = fe.program.arena[primary]
            .items
            .iter()
            .find_map(|&id| match &fe.program.arena[id].kind {
                ItemKind::Function(f) if f.name == "t" => Some(f.body),
                _ => None,
            })
            .expect("fn t");
        let root_expr = fe.program.arena[body].root_expr;

        let ev = Evaluator {
            program: &fe.program,
            env: &fe.env,
            current_body: Cell::new(body),
            dispatch: Dispatcher::default(),
            dispatches: Cell::new(0),
            mode: KernelMode::Off,
            exec_client: None,
        };
        let Value::Command(applied) = ev.eval(root_expr, &Env::new()).expect("eval") else {
            panic!("expected a Command value");
        };
        let p = applied.projected.as_ref().expect("projected");
        assert!(
            p.effective.inputs.iter().any(|i| i.path == "x.c"),
            "x.c is an input: {:?}",
            p.effective.inputs
        );
        assert!(
            p.outputs.iter().any(|o| o.path == "x.o"),
            "x.o is an output: {:?}",
            p.outputs
        );
        assert_eq!(p.execution_argv, vec!["cc", "-c", "x.c", "-o", "x.o"]);
    }
}
