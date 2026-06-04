//! Capability **abilities** — the dependency-injection surface (`r[ability.*]`).
//!
//! Build code requests an ability with `Type where (predicate)`
//! (`r[ability.request]`); executors advertise concrete **providers**
//! (`r[ability.advertise]`); the control plane **matches** a request's structured
//! predicate against the advertised set and **injects** one (`r[ability.match]`).
//!
//! This module is the local, in-process stand-in for that control plane: the
//! [`ProviderRegistry`] is the fleet registry, and [`Constraint`] is the structured
//! predicate the orchestrator indexes providers by. The predicate is *extracted*
//! from the constraint expression (`extract`) — never run as code (`r[expr.where]`).

use std::collections::HashMap;

use vx_ir::ExprId;
use vx_ir::arena::VirArena;
use vx_ir::expr::{BinOp, ExprKind, LitKind};

use crate::eval::Value;

/// A local registry of advertised capability providers, keyed by spec name
/// (`r[ability.advertise]`). The Phase-3 stand-in for the fleet registry: an
/// executor's probes populate it; `Type where (predicate)` matches against it.
#[derive(Debug, Clone, Default)]
pub struct ProviderRegistry {
    by_spec: HashMap<String, Vec<Value>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advertise `provider` — a struct value carrying the spec's provider attributes
    /// (version, target, …) and tool fields (command, binary) — for capability `spec`.
    pub fn advertise(&mut self, spec: &str, provider: Value) {
        self.by_spec
            .entry(spec.to_string())
            .or_default()
            .push(provider);
    }

    /// The providers advertised for `spec`, in advertisement order.
    pub fn providers(&self, spec: &str) -> &[Value] {
        self.by_spec.get(spec).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// A comparison operator in a constraint clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// A literal an attribute is compared against.
#[derive(Debug, Clone, PartialEq)]
pub enum AttrLit {
    Int(u64),
    Str(String),
    Bool(bool),
}

/// A structured predicate extracted from a `where ( … )` constraint
/// (`r[expr.where]`): a tree of attribute comparisons, *not* runnable code. Field
/// names are the spec's provider attributes (`r[ability.request]`); the orchestrator
/// matches it against advertised providers (`r[ability.match]`).
#[derive(Debug, Clone)]
pub enum Constraint {
    /// Always satisfied — an absent/empty constraint.
    Always,
    /// `field <op> literal`.
    Cmp {
        field: String,
        op: CmpOp,
        value: AttrLit,
    },
    /// `a && b …` — every clause must hold.
    All(Vec<Constraint>),
    /// `a || b …` — some clause must hold.
    Any(Vec<Constraint>),
}

/// Extract the structured predicate from a `where ( … )` constraint expression by
/// walking its boolean/comparison tree. The constraint grammar is fixed
/// (`r[expr.where]`): an unsupported shape is an error rather than runnable code.
pub fn extract(arena: &VirArena, expr: ExprId) -> Result<Constraint, String> {
    match &arena[expr].kind {
        ExprKind::Binary(b) => match b.op {
            BinOp::And => Ok(Constraint::All(vec![
                extract(arena, b.lhs)?,
                extract(arena, b.rhs)?,
            ])),
            BinOp::Or => Ok(Constraint::Any(vec![
                extract(arena, b.lhs)?,
                extract(arena, b.rhs)?,
            ])),
            BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                Ok(Constraint::Cmp {
                    field: field_name(arena, b.lhs)?,
                    op: cmp_op(b.op),
                    value: attr_lit(arena, b.rhs)?,
                })
            }
            other => Err(format!("`{other:?}` is not a constraint operator")),
        },
        other => Err(format!(
            "a `where` constraint must be a boolean comparison, got {other:?}"
        )),
    }
}

/// The attribute (spec field) name on the left of a comparison.
fn field_name(arena: &VirArena, expr: ExprId) -> Result<String, String> {
    match &arena[expr].kind {
        ExprKind::NameRef(nr) => Ok(nr.name.clone()),
        other => Err(format!(
            "a constraint's left side must be a spec attribute, got {other:?}"
        )),
    }
}

/// The literal on the right of a comparison.
fn attr_lit(arena: &VirArena, expr: ExprId) -> Result<AttrLit, String> {
    match &arena[expr].kind {
        ExprKind::Literal(le) => match &le.value {
            LitKind::Int { raw } => raw
                .parse::<u64>()
                .map(AttrLit::Int)
                .map_err(|_| format!("bad integer literal in constraint: `{raw}`")),
            // `String { raw }` already has its quotes stripped.
            LitKind::String { raw } => Ok(AttrLit::Str(raw.clone())),
            LitKind::Bool(b) => Ok(AttrLit::Bool(*b)),
            LitKind::Float { raw } => Err(format!(
                "semver/float constraints are not supported yet: `{raw}`"
            )),
        },
        other => Err(format!(
            "a constraint's right side must be a literal, got {other:?}"
        )),
    }
}

fn cmp_op(op: BinOp) -> CmpOp {
    match op {
        BinOp::Eq => CmpOp::Eq,
        BinOp::Neq => CmpOp::Ne,
        BinOp::Lt => CmpOp::Lt,
        BinOp::LtEq => CmpOp::Le,
        BinOp::Gt => CmpOp::Gt,
        BinOp::GtEq => CmpOp::Ge,
        _ => unreachable!("cmp_op only called for comparison operators"),
    }
}

impl CmpOp {
    /// Apply this operator to an ordering between an attribute and the literal.
    pub fn holds(self, ordering: std::cmp::Ordering) -> bool {
        use std::cmp::Ordering::*;
        match self {
            CmpOp::Eq => ordering == Equal,
            CmpOp::Ne => ordering != Equal,
            CmpOp::Lt => ordering == Less,
            CmpOp::Le => ordering != Greater,
            CmpOp::Gt => ordering == Greater,
            CmpOp::Ge => ordering != Less,
        }
    }
}
