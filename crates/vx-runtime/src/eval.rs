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

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use vx_ir::build::VirProgram;
use vx_ir::expr::{ArrayElement, BinOp, ExprKind, FieldKey, LitKind, StringPart};
use vx_ir::ids::{BindingId, ExprId};
use vx_ir::item::ItemKind;
use vx_ir::pattern::PatternKind;
use vx_ir::scope::DefTarget;
use vx_ir::stmt::StmtKind;

/// `assert`/`assert_eq` are themselves Vixen — prepended so the `.vx` tests
/// resolve them (same prelude the old harnesses use).
const PRELUDE: &str = r#"
fn assert(cond: Bool) -> Bool (
    if cond ( true ) else ( fail "assertion failed" )
)

fn assert_eq[T](got: T, want: T) -> Bool (
    if got == want ( true ) else ( fail "not equal" )
)
"#;

/// A poison — a detonation with a reason. Propagates like an error.
#[derive(Debug, Clone)]
pub struct Poison(pub String);

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

/// The outcome of evaluating a test body: clean, or poisoned.
#[derive(Debug)]
pub enum Outcome {
    Clean(Value),
    Poisoned(String),
}

type Env = HashMap<BindingId, Thunk>;

/// A lazily-evaluated, memoised value. Forcing runs its expression in its
/// captured environment once; re-forcing returns the cached result.
#[derive(Debug, Clone)]
pub struct Thunk(Rc<RefCell<ThunkState>>);

#[derive(Debug)]
enum ThunkState {
    Unforced { expr: ExprId, env: Env },
    Forcing,
    Done(Result<Value, Poison>),
}

struct Evaluator<'a> {
    program: &'a VirProgram,
}

impl Evaluator<'_> {
    fn thunk(&self, expr: ExprId, env: &Env) -> Thunk {
        Thunk(Rc::new(RefCell::new(ThunkState::Unforced {
            expr,
            env: env.clone(),
        })))
    }

    /// Force a thunk to a value (memoised; detects cycles).
    fn force(&self, t: &Thunk) -> Result<Value, Poison> {
        // Extract the work without holding the borrow across the recursive eval.
        let work = {
            let mut st = t.0.borrow_mut();
            match &*st {
                ThunkState::Done(r) => return r.clone(),
                ThunkState::Forcing => {
                    return Err(Poison(
                        "circular dependency (a value demands itself)".into(),
                    ));
                }
                ThunkState::Unforced { expr, env } => {
                    let (expr, env) = (*expr, env.clone());
                    *st = ThunkState::Forcing;
                    (expr, env)
                }
            }
        };
        let result = self.eval(work.0, &work.1);
        *t.0.borrow_mut() = ThunkState::Done(result.clone());
        result
    }

    /// Evaluate an expression to a value. Compound construction is lazy (fields
    /// become thunks); only demanded positions are forced.
    fn eval(&self, e: ExprId, env: &Env) -> Result<Value, Poison> {
        let prog = self.program;
        match &prog.arena[e].kind {
            // 💣 — poisons the instant it is evaluated (i.e. forced).
            ExprKind::Bomb => Err(Poison("💣 forced".into())),

            // Explicit failure — poisons whenever reached.
            ExprKind::Fail(fx) => {
                let msg = match fx.message {
                    Some(m) => match self.eval(m, env)? {
                        Value::Str(s) => s.to_string(),
                        other => format!("{other:?}"),
                    },
                    None => "fail".into(),
                };
                Err(Poison(msg))
            }

            ExprKind::Literal(lit) => match &lit.value {
                LitKind::Int { raw } => raw
                    .parse::<u64>()
                    .map(Value::U64)
                    .map_err(|_| Poison(format!("bad int literal `{raw}`"))),
                LitKind::Bool(b) => Ok(Value::Bool(*b)),
                LitKind::String { raw } => Ok(Value::Str(Rc::from(raw.as_str()))),
                LitKind::Float { raw } => Err(Poison(format!("floats not yet supported: {raw}"))),
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
                    other => return Err(Poison(format!("non-integer index: {other:?}"))),
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
                    other => Err(Poison(format!("unsupported prefix on {other:?}"))),
                }
            }

            // Control flow: only the taken branch is evaluated (short-circuit).
            ExprKind::If(ifx) => match self.eval(ifx.condition, env)? {
                Value::Bool(true) => self.eval(ifx.then_branch, env),
                Value::Bool(false) => match ifx.else_branch {
                    Some(eb) => self.eval(eb, env),
                    None => Ok(Value::Unit),
                },
                other => Err(Poison(format!("non-bool condition: {other:?}"))),
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
                            other => return Err(Poison(format!("can't interpolate {other:?}"))),
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
                    None => Err(Poison(format!("unbound `{}`", n.name))),
                },
                _ => Err(Poison(format!("unresolved name `{}`", n.name))),
            },

            // A call: resolve the callee to a function body and evaluate it with
            // the arguments bound (lazily) to its parameters.
            ExprKind::Call(c) => self.call(c.callee, c.args.iter().map(|a| a.expr), env),

            other => Err(Poison(format!(
                "unsupported expression: {}",
                kind_name(other)
            ))),
        }
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
                    _ => return Err(Poison(format!("`{}` is not callable", n.name))),
                },
                _ => return Err(Poison(format!("unresolved call `{}`", n.name))),
            },
            _ => return Err(Poison("indirect calls not yet supported".into())),
        };
        // Bind params to argument thunks (captured in the *caller's* env — lazy).
        let arg_thunks: Vec<Thunk> = args.map(|a| self.thunk(a, env)).collect();
        let params = &prog.arena[body].params;
        let mut callee_env = Env::new();
        for (param, th) in params.iter().zip(arg_thunks) {
            if let PatternKind::Binding(pb) = &prog.arena[param.pattern].kind {
                callee_env.insert(pb.binding, th);
            }
        }
        self.eval(prog.arena[body].root_expr, &callee_env)
    }

    fn project_index(&self, container: &Value, idx: usize) -> Result<Value, Poison> {
        let slot = match container {
            Value::Tuple(ts) | Value::Array(ts) => ts.get(idx),
            other => return Err(Poison(format!("can't index {other:?}"))),
        };
        match slot {
            Some(th) => self.force(th),
            None => Err(Poison(format!("index {idx} out of bounds"))),
        }
    }

    fn project_name(&self, container: &Value, name: &str) -> Result<Value, Poison> {
        match container {
            Value::Struct(fields) => match fields.iter().find(|(n, _)| n == name) {
                Some((_, th)) => self.force(th),
                None => Err(Poison(format!("no field `{name}`"))),
            },
            other => Err(Poison(format!("can't access `.{name}` on {other:?}"))),
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
                _ => Err(Poison("`&&` wants bools".into())),
            },
            Or => match (l, r) {
                (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(a || b)),
                _ => Err(Poison("`||` wants bools".into())),
            },
            Concat => match (&l, &r) {
                (Value::Str(a), Value::Str(b)) => {
                    Ok(Value::Str(Rc::from(format!("{a}{b}").as_str())))
                }
                _ => Err(Poison("`++` wants strings".into())),
            },
            Add | Sub | Mul | Div | Rem | Lt | LtEq | Gt | GtEq => {
                let (a, b) = match (int(&l), int(&r)) {
                    (Some(a), Some(b)) => (a, b),
                    _ => {
                        return Err(Poison(format!(
                            "`{op:?}` wants integers, got {l:?} / {r:?}"
                        )));
                    }
                };
                Ok(match op {
                    Add => Value::U64(a.wrapping_add(b)),
                    Sub => Value::U64(a.wrapping_sub(b)),
                    Mul => Value::U64(a.wrapping_mul(b)),
                    Div if b == 0 => return Err(Poison("divide by zero".into())),
                    Div => Value::U64(a / b),
                    Rem if b == 0 => return Err(Poison("remainder by zero".into())),
                    Rem => Value::U64(a % b),
                    Lt => Value::Bool(a < b),
                    LtEq => Value::Bool(a <= b),
                    Gt => Value::Bool(a > b),
                    GtEq => Value::Bool(a >= b),
                    _ => unreachable!(),
                })
            }
            Pipe => Err(Poison("`|>` not yet supported".into())),
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

/// Evaluate function `entry` in `source` through the demand-driven engine, with
/// the assert prelude prepended. `Err` is a setup failure (parse / not found);
/// `Ok(Outcome)` is the evaluation result (clean or poisoned).
pub fn run(source: &str, entry: &str) -> Result<Outcome, String> {
    let full = format!("{PRELUDE}\n{source}");
    let (program, _types) = crate::analyze::front_end(&full)?;
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
    let ev = Evaluator { program: &program };
    Ok(match ev.eval(root, &Env::new()) {
        Ok(v) => Outcome::Clean(v),
        Err(p) => Outcome::Poisoned(p.0),
    })
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
            Ok(Outcome::Poisoned(_)) => {}
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
}
