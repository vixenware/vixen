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
use vx_ir::expr::{ArrayElement, BinOp, ExprKind, FieldKey, LitKind, StringPart};
use vx_ir::ids::{BindingId, BodyId, ExprId};
use vx_ir::item::ItemKind;
use vx_ir::pattern::PatternKind;
use vx_ir::scope::DefTarget;
use vx_ir::stmt::StmtKind;
use vx_types::TypeEnv;

use crate::dispatch::{self, Dispatcher};
use crate::graph::Span;

/// A poison — a detonation with a reason and **where in the source** it
/// originated (I6). Propagates like an error; `eval` attaches the span of the
/// expression a poison surfaces at (the innermost one wins).
#[derive(Debug, Clone)]
pub struct Poison {
    pub reason: String,
    pub span: Option<Span>,
}

/// Construct a span-less poison (the span is attached as it bubbles through
/// [`Evaluator::eval`]).
fn poison(reason: String) -> Poison {
    Poison { reason, span: None }
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
    Unit,
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

#[derive(Debug)]
enum ThunkState {
    /// The captured `body` travels with the thunk so that forcing it re-enters the
    /// right body's demand graph for sub-expression dispatch.
    Unforced {
        expr: ExprId,
        env: Env,
        body: BodyId,
    },
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
                    let work = (*expr, env.clone(), *body);
                    *st = ThunkState::Forcing;
                    work
                }
            }
        };
        let prev = self.current_body.replace(work.2);
        let result = self.eval(work.0, &work.1);
        self.current_body.set(prev);
        *t.0.borrow_mut() = ThunkState::Done(result.clone());
        result
    }

    /// Evaluate an expression, attaching `e`'s source span to any poison that
    /// surfaces here without one — the innermost expression wins, so a poison
    /// reports where it originated (I6). Kernel poisons already carry their span
    /// and are left untouched.
    fn eval(&self, e: ExprId, env: &Env) -> Result<Value, Poison> {
        self.eval_inner(e, env).map_err(|mut p| {
            if p.span.is_none() {
                p.span = self.expr_span(e);
            }
            p
        })
    }

    /// The source span (byte range) of an expression.
    fn expr_span(&self, e: ExprId) -> Option<Span> {
        let range = self.program.arena[e].span.range;
        Some(Span {
            start: range.start().into(),
            end: range.end().into(),
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

            // Explicit failure — poisons whenever reached.
            ExprKind::Fail(fx) => {
                let msg = match fx.message {
                    Some(m) => match self.eval(m, env)? {
                        Value::Str(s) => s.to_string(),
                        other => format!("{other:?}"),
                    },
                    None => "fail".into(),
                };
                Err(poison(msg))
            }

            ExprKind::Literal(lit) => match &lit.value {
                LitKind::Int { raw } => raw
                    .parse::<u64>()
                    .map(Value::U64)
                    .map_err(|_| poison(format!("bad int literal `{raw}`"))),
                LitKind::Bool(b) => Ok(Value::Bool(*b)),
                LitKind::String { raw } => Ok(Value::Str(Rc::from(raw.as_str()))),
                LitKind::Float { raw } => Err(poison(format!("floats not yet supported: {raw}"))),
            },

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
            ExprKind::StructLiteral(s) => Ok(Value::Struct(
                s.fields
                    .iter()
                    .map(|f| (f.name.clone(), self.thunk(f.value, env)))
                    .collect(),
            )),
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

            // A name: force its bound thunk.
            ExprKind::NameRef(n) => match n.resolved {
                Some(DefTarget::Binding(bid)) => match env.get(&bid) {
                    Some(th) => self.force(th),
                    None => Err(poison(format!("unbound `{}`", n.name))),
                },
                _ => Err(poison(format!("unresolved name `{}`", n.name))),
            },

            // A call: resolve the callee to a function body and evaluate it with
            // the arguments bound (lazily) to its parameters.
            ExprKind::Call(c) => self.call(c.callee, c.args.iter().map(|a| a.expr), env),

            other => Err(poison(format!(
                "unsupported expression: {}",
                kind_name(other)
            ))),
        }
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
                // Propagate the kernel poison's source span (I6) into eval's poison.
                Err(p) => Err(Poison {
                    reason: p.reason,
                    span: p.span,
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
        // Resolve the callee name to a function item.
        let body = match &prog.arena[callee].kind {
            ExprKind::NameRef(n) => match n.resolved {
                Some(DefTarget::Item(item)) => match &prog.arena[item].kind {
                    ItemKind::Function(f) => f.body,
                    _ => return Err(poison(format!("`{}` is not callable", n.name))),
                },
                _ => return Err(poison(format!("unresolved call `{}`", n.name))),
            },
            _ => return Err(poison("indirect calls not yet supported".into())),
        };
        // Bind params to argument thunks (captured in the *caller's* env + body —
        // lazy). The callee body becomes current while its expressions evaluate;
        // forcing an argument later restores the caller's body (the thunk carries
        // it). The callee body's own scalar regions dispatch to kernels — so a
        // scalar function body still runs as compiled code, now via sub-expression
        // dispatch rather than a special call path.
        let arg_thunks: Vec<Thunk> = args.map(|a| self.thunk(a, env)).collect();
        let params = &prog.arena[body].params;
        let mut callee_env = Env::new();
        for (param, th) in params.iter().zip(arg_thunks) {
            if let PatternKind::Binding(pb) = &prog.arena[param.pattern].kind {
                callee_env.insert(pb.binding, th);
            }
        }
        let prev = self.current_body.replace(body);
        let result = self.eval(prog.arena[body].root_expr, &callee_env);
        self.current_body.set(prev);
        result
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
            other => Err(poison(format!("can't access `.{name}` on {other:?}"))),
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
            _ => false,
        })
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
    };
    let outcome = match ev.eval(root, &Env::new()) {
        Ok(v) => Outcome::Clean(v),
        Err(p) => Outcome::Poisoned {
            reason: p.reason,
            span: p.span,
        },
    };
    Ok((outcome, ev.dispatches.get()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(src: &str, f: &str) {
        match run(src, f) {
            Ok(Outcome::Clean(_)) => {}
            other => panic!("{f}: expected clean, got {other:?}"),
        }
    }
    fn poisons(src: &str, f: &str) {
        match run(src, f) {
            Ok(Outcome::Poisoned { .. }) => {}
            other => panic!("{f}: expected poison, got {other:?}"),
        }
    }

    // The real self-hosting unit tests, run on the demand-driven engine.
    const DEMAND_BASIS: &str = include_str!("../../vx-rt/tests/lang/demand_basis.vx");
    const WHICH_FIELD: &str = include_str!("../../vx-rt/tests/lang/which_field_which_graph.vx");
    const POISONS: &str = include_str!("../../vx-rt/tests/lang/poisons.vx");

    #[test]
    fn demand_basis_all_clean() {
        for f in [
            "unused_tuple_field_never_runs",
            "unused_struct_field_never_runs",
            "chained_projection_skips_dead_siblings",
            "deep_named_field_projection",
        ] {
            clean(DEMAND_BASIS, f);
        }
    }

    #[test]
    fn which_field_all_clean() {
        for f in [
            "demanding_fast_skips_slow",
            "each_field_resolves_independently",
            "demanding_index_selects_branch",
        ] {
            clean(WHICH_FIELD, f);
        }
    }

    #[test]
    fn poisons_all_poison() {
        for f in [
            "explicit_fail_detonates",
            "forcing_a_bomb_detonates",
            "false_assertion_detonates",
        ] {
            poisons(POISONS, f);
        }
    }

    // ---- kernel dispatch on the real eval path (rebuild-plan step: wire-in) ----

    #[test]
    fn asserts_dispatch_to_the_kernel() {
        // The prelude's `assert_eq` is a scalar `if`-kernel; the demand_basis tests
        // call it with U64 args, so it runs as compiled code on the real path — a
        // dispatch fires, and the bomb in the dead field still never detonates.
        let (outcome, dispatches) =
            run_with_dispatches(DEMAND_BASIS, "unused_tuple_field_never_runs").unwrap();
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
        // `each_field_resolves_independently` is `assert_eq(r.fast + r.slow, 18)`.
        // Two kernels fire: the `r.fast + r.slow` arithmetic SUB-EXPRESSION (the
        // struct projections fold into constants inside one kernel), and the
        // assert_eq comparison — neither is a whole function body of the caller.
        let (outcome, dispatches) =
            run_with_dispatches(WHICH_FIELD, "each_field_resolves_independently").unwrap();
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
}
