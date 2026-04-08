//! Lower VIR to KernelOp bytecode.
//!
//! Entry point: [`lower_vir_program`].
//!
//! This is the VIR replacement for `vx_analysis::exec_slice`. It consumes
//! a [`VirProgram`] + [`VirDemandResult`] and produces an
//! [`ExecutableProgram`] that `vx-rt` can evaluate.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use vx_analysis::{
    BodyId as ABodyId, ExecSliceError, ExecutableBody, ExecutableProgram, KernelKind, KernelOp,
    Operand, ResultSource, SlotId, SourceExpr,
};

use crate::binding::BindingKind;
use crate::build::VirProgram;
use crate::demand::VirDemandResult;
use crate::expr::{BinOp, ExprKind, LitKind};
use crate::item::ItemKind;
use crate::scope::DefTarget;
use crate::stmt::StmtKind;
use crate::{BindingId, BodyId, ExprId};

// ---------------------------------------------------------------------------
// ID conversion helpers
// ---------------------------------------------------------------------------

fn a_body(id: BodyId) -> ABodyId {
    ABodyId(id.0)
}

fn a_expr(id: ExprId) -> vx_analysis::ExprId {
    vx_analysis::ExprId(id.0)
}

fn source_expr(body: BodyId, expr: ExprId) -> SourceExpr {
    SourceExpr {
        body: a_body(body),
        expr: a_expr(expr),
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum LowerError {
    UnsupportedShape(String),
    UnresolvedName { name: String, expr: ExprId },
    NoEntryPoint,
    MissingBody(BodyId),
    InvalidLiteral { raw: String },
}

impl From<LowerError> for ExecSliceError {
    fn from(e: LowerError) -> Self {
        match e {
            LowerError::UnsupportedShape(s) => ExecSliceError::UnsupportedShape(s),
            LowerError::UnresolvedName { name, expr } => {
                ExecSliceError::UnsupportedShape(format!("unresolved name `{name}` at {expr}"))
            }
            LowerError::NoEntryPoint => {
                ExecSliceError::InvalidProgram("no entry point found".into())
            }
            LowerError::MissingBody(id) => ExecSliceError::MissingBody(a_body(id)),
            LowerError::InvalidLiteral { raw } => {
                ExecSliceError::UnsupportedShape(format!("invalid literal: {raw}"))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Lower a VIR program to an executable program.
pub fn lower_vir_program(
    program: &VirProgram,
    demand: &VirDemandResult,
) -> Result<ExecutableProgram, LowerError> {
    let entry = *demand
        .entry_points
        .first()
        .ok_or(LowerError::NoEntryPoint)?;

    let mut bodies: BTreeMap<ABodyId, ExecutableBody> = BTreeMap::new();

    for &body_id in &demand.reachable {
        let lowering = VirBodyLowering::new(&program.arena, body_id);
        let result = lowering.lower()?;
        bodies.insert(a_body(body_id), result);
    }

    Ok(ExecutableProgram {
        entry: a_body(entry),
        bodies,
        runtime_nodes: BTreeMap::new(),
    })
}

// ---------------------------------------------------------------------------
// Per-body lowering context
// ---------------------------------------------------------------------------

struct VirBodyLowering<'a> {
    arena: &'a crate::VirArena,
    body_id: BodyId,

    // Slot management
    next_slot: u32,
    slot_by_binding: HashMap<BindingId, SlotId>,
    materialized: HashSet<BindingId>,

    // Output
    ops: Vec<KernelOp>,
    params: Vec<SlotId>,
    local_call_deps: BTreeSet<ABodyId>,
}

impl<'a> VirBodyLowering<'a> {
    fn new(arena: &'a crate::VirArena, body_id: BodyId) -> Self {
        let body = &arena[body_id];
        let mut next_slot = 0u32;
        let mut slot_by_binding = HashMap::new();
        let mut params = Vec::new();

        // Pre-allocate param slots.
        for param in &body.params {
            if let crate::pattern::PatternKind::Binding(ref b) = arena[param.pattern].kind {
                let slot = SlotId(next_slot);
                next_slot += 1;
                slot_by_binding.insert(b.binding, slot);
                params.push(slot);
            }
        }

        VirBodyLowering {
            arena,
            body_id,
            next_slot,
            slot_by_binding,
            materialized: HashSet::new(),
            ops: Vec::new(),
            params,
            local_call_deps: BTreeSet::new(),
        }
    }

    fn lower(mut self) -> Result<ExecutableBody, LowerError> {
        let root_expr = self.arena[self.body_id].root_expr;
        let result_operand = self.lower_expr(root_expr)?;
        let result_slot = self.ensure_slot(result_operand, self.body_id, root_expr);

        Ok(ExecutableBody {
            id: a_body(self.body_id),
            kernel: vx_analysis::KernelId(self.body_id.0),
            kernel_kind: KernelKind::Interpreted,
            params: self.params,
            ops: self.ops,
            result_source: ResultSource::Slot(result_slot),
            runtime_boundary_extractions: vec![],
            local_call_dependencies: self.local_call_deps.into_iter().collect(),
        })
    }

    fn alloc_temp(&mut self) -> SlotId {
        let s = SlotId(self.next_slot);
        self.next_slot += 1;
        s
    }

    fn ensure_slot(&mut self, operand: Operand, body: BodyId, expr: ExprId) -> SlotId {
        match operand {
            Operand::Slot(s) => s,
            Operand::Int(v) => {
                let dst = self.alloc_temp();
                self.ops.push(KernelOp::AssignConstInt {
                    dst,
                    source: source_expr(body, expr),
                    value: v,
                });
                dst
            }
            Operand::String(v) => {
                let dst = self.alloc_temp();
                self.ops.push(KernelOp::AssignConstString {
                    dst,
                    source: source_expr(body, expr),
                    value: v,
                });
                dst
            }
        }
    }

    fn source_for(&self, expr: ExprId) -> SourceExpr {
        source_expr(self.body_id, expr)
    }

    // -----------------------------------------------------------------------
    // Binding materialization
    // -----------------------------------------------------------------------

    fn alloc_binding_slot(&mut self, bid: BindingId) -> SlotId {
        if let Some(&s) = self.slot_by_binding.get(&bid) {
            return s;
        }
        let s = SlotId(self.next_slot);
        self.next_slot += 1;
        self.slot_by_binding.insert(bid, s);
        s
    }

    fn materialize_binding(&mut self, bid: BindingId) -> Result<Operand, LowerError> {
        let slot = self.alloc_binding_slot(bid);

        if self.materialized.contains(&bid) {
            return Ok(Operand::Slot(slot));
        }

        let binding = &self.arena[bid];

        // Parameters are already in their slot (filled by caller).
        if matches!(binding.kind, BindingKind::Parameter) {
            self.materialized.insert(bid);
            return Ok(Operand::Slot(slot));
        }

        // Let/PatternMatch/etc. with init_expr.
        if let Some(init) = binding.init_expr {
            self.materialized.insert(bid); // Mark early to prevent cycles.
            let value = self.lower_expr(init)?;
            match value {
                Operand::Slot(s) if s == slot => {
                    // Already in the right slot, skip alias.
                }
                _ => {
                    self.ops.push(KernelOp::AssignAlias {
                        dst: slot,
                        source: self.source_for(init),
                        value,
                    });
                }
            }
            return Ok(Operand::Slot(slot));
        }

        // No init_expr — the binding may be initialized via other means
        // (destructuring, etc.). Mark as materialized and hope for the best.
        self.materialized.insert(bid);
        Ok(Operand::Slot(slot))
    }

    // -----------------------------------------------------------------------
    // Expression lowering
    // -----------------------------------------------------------------------

    fn lower_expr(&mut self, expr_id: ExprId) -> Result<Operand, LowerError> {
        let kind = self.arena[expr_id].kind.clone();
        match kind {
            ExprKind::Literal(lit) => self.lower_literal(expr_id, &lit.value),
            ExprKind::NameRef(nr) => self.lower_name_ref(expr_id, &nr),
            ExprKind::Block(block) => self.lower_block(expr_id, &block),
            ExprKind::Binary(bin) => self.lower_binary(expr_id, &bin),
            ExprKind::Prefix(pre) => self.lower_prefix(expr_id, &pre),
            ExprKind::If(if_) => self.lower_if(expr_id, &if_),
            ExprKind::Fail(fail) => self.lower_fail(expr_id, &fail),
            ExprKind::InterpolatedString(is) => self.lower_interpolated_string(expr_id, &is),
            ExprKind::Call(call) => self.lower_call(expr_id, &call),

            // 010-1: closures, products, field projection
            ExprKind::Closure(_) => Err(LowerError::UnsupportedShape(
                "closure (deferred to 010-1)".into(),
            )),
            ExprKind::Tuple(_) => Err(LowerError::UnsupportedShape(
                "tuple (deferred to 010-1)".into(),
            )),
            ExprKind::Array(_) => Err(LowerError::UnsupportedShape(
                "array (deferred to 010-1)".into(),
            )),
            ExprKind::Record(_) => Err(LowerError::UnsupportedShape(
                "record (deferred to 010-1)".into(),
            )),
            ExprKind::StructLiteral(_) => Err(LowerError::UnsupportedShape(
                "struct literal (deferred to 010-1)".into(),
            )),
            ExprKind::Field(_) => Err(LowerError::UnsupportedShape(
                "field access (deferred to 010-1)".into(),
            )),
            ExprKind::Index(_) => Err(LowerError::UnsupportedShape(
                "index access (deferred to 010-1)".into(),
            )),
            ExprKind::Match(_) => Err(LowerError::UnsupportedShape(
                "match (deferred to 010-1)".into(),
            )),

            // 010-2: runtime, exotics
            ExprKind::Where(_) => Err(LowerError::UnsupportedShape(
                "where (deferred to 010-2)".into(),
            )),
            ExprKind::Invoke(_) => Err(LowerError::UnsupportedShape(
                "invoke (deferred to 010-2)".into(),
            )),
            ExprKind::PartialApp(_) => Err(LowerError::UnsupportedShape(
                "partial application (deferred to 010-2)".into(),
            )),
            ExprKind::Range(_) => Err(LowerError::UnsupportedShape(
                "range (deferred to 010-2)".into(),
            )),
            ExprKind::TypeOp(_) => Err(LowerError::UnsupportedShape(
                "type op (deferred to 010-2)".into(),
            )),
            ExprKind::With(_) => Err(LowerError::UnsupportedShape(
                "with (deferred to 010-2)".into(),
            )),
            ExprKind::Fallible(_) => Err(LowerError::UnsupportedShape(
                "fallible (deferred to 010-2)".into(),
            )),
            ExprKind::Import(_) => Err(LowerError::UnsupportedShape(
                "import (not in executable slice)".into(),
            )),
            ExprKind::PathLiteral(_) => Err(LowerError::UnsupportedShape(
                "path literal (deferred to 010-2)".into(),
            )),
            ExprKind::Map(_) => Err(LowerError::UnsupportedShape(
                "map (deferred to 010-2)".into(),
            )),
            ExprKind::Error => Err(LowerError::UnsupportedShape("error node".into())),
        }
    }

    // -----------------------------------------------------------------------
    // Literal
    // -----------------------------------------------------------------------

    fn lower_literal(&mut self, expr_id: ExprId, lit: &LitKind) -> Result<Operand, LowerError> {
        match lit {
            LitKind::Int { raw } => {
                let value: i64 = raw
                    .parse()
                    .map_err(|_| LowerError::InvalidLiteral { raw: raw.clone() })?;
                Ok(Operand::Int(value))
            }
            LitKind::String { raw } => Ok(Operand::String(raw.clone())),
            LitKind::Bool(b) => {
                let dst = self.alloc_temp();
                self.ops.push(KernelOp::AssignConstBool {
                    dst,
                    source: self.source_for(expr_id),
                    value: *b,
                });
                Ok(Operand::Slot(dst))
            }
            LitKind::Float { raw } => Err(LowerError::UnsupportedShape(format!(
                "float literal: {raw}"
            ))),
        }
    }

    // -----------------------------------------------------------------------
    // NameRef
    // -----------------------------------------------------------------------

    fn lower_name_ref(
        &mut self,
        expr_id: ExprId,
        nr: &crate::expr::NameRefExpr,
    ) -> Result<Operand, LowerError> {
        match nr.resolved {
            Some(DefTarget::Binding(bid)) => self.materialize_binding(bid),
            Some(DefTarget::Item(item_id)) => {
                // Check if it's a function used as value — deferred to 010-1.
                if matches!(self.arena[item_id].kind, ItemKind::Function(_)) {
                    return Err(LowerError::UnsupportedShape(
                        "function as value (deferred to 010-1)".into(),
                    ));
                }
                Err(LowerError::UnsupportedShape(format!(
                    "item ref `{}`",
                    nr.name
                )))
            }
            Some(DefTarget::Variant { .. }) => Err(LowerError::UnsupportedShape(
                "variant ref (deferred to 010-1)".into(),
            )),
            None => Err(LowerError::UnresolvedName {
                name: nr.name.clone(),
                expr: expr_id,
            }),
        }
    }

    // -----------------------------------------------------------------------
    // Block
    // -----------------------------------------------------------------------

    fn lower_block(
        &mut self,
        _expr_id: ExprId,
        block: &crate::expr::BlockExpr,
    ) -> Result<Operand, LowerError> {
        for &stmt_id in &block.stmts {
            self.lower_stmt(stmt_id)?;
        }
        if let Some(tail) = block.tail {
            self.lower_expr(tail)
        } else {
            Err(LowerError::UnsupportedShape(
                "block without tail expression".into(),
            ))
        }
    }

    // -----------------------------------------------------------------------
    // Binary
    // -----------------------------------------------------------------------

    fn lower_binary(
        &mut self,
        expr_id: ExprId,
        bin: &crate::expr::BinaryExpr,
    ) -> Result<Operand, LowerError> {
        let lhs = self.lower_expr(bin.lhs)?;
        let rhs = self.lower_expr(bin.rhs)?;
        let dst = self.alloc_temp();
        let source = self.source_for(expr_id);

        match bin.op {
            BinOp::Add => self.ops.push(KernelOp::Add {
                dst,
                source,
                lhs,
                rhs,
            }),
            BinOp::Sub => self.ops.push(KernelOp::Sub {
                dst,
                source,
                lhs,
                rhs,
            }),
            BinOp::Eq => self.ops.push(KernelOp::Equal {
                dst,
                source,
                lhs,
                rhs,
            }),
            BinOp::Neq => self.ops.push(KernelOp::NotEqual {
                dst,
                source,
                lhs,
                rhs,
            }),
            BinOp::Concat => self.ops.push(KernelOp::ConcatStrings {
                dst,
                source,
                parts: vec![lhs, rhs],
            }),
            other => {
                return Err(LowerError::UnsupportedShape(format!("binary op {other:?}")));
            }
        }

        Ok(Operand::Slot(dst))
    }

    // -----------------------------------------------------------------------
    // Prefix
    // -----------------------------------------------------------------------

    fn lower_prefix(
        &mut self,
        _expr_id: ExprId,
        pre: &crate::expr::PrefixExpr,
    ) -> Result<Operand, LowerError> {
        Err(LowerError::UnsupportedShape(format!(
            "prefix op {:?}",
            pre.op
        )))
    }

    // -----------------------------------------------------------------------
    // If/Select
    // -----------------------------------------------------------------------

    fn lower_if(
        &mut self,
        expr_id: ExprId,
        if_: &crate::expr::IfExpr,
    ) -> Result<Operand, LowerError> {
        let else_branch = if_
            .else_branch
            .ok_or_else(|| LowerError::UnsupportedShape("if without else branch".into()))?;

        let condition = self.lower_expr(if_.condition)?;
        let then_value = self.lower_expr(if_.then_branch)?;
        let else_value = self.lower_expr(else_branch)?;

        let dst = self.alloc_temp();
        self.ops.push(KernelOp::Select {
            dst,
            source: self.source_for(expr_id),
            condition,
            then_value,
            else_value,
        });
        Ok(Operand::Slot(dst))
    }

    // -----------------------------------------------------------------------
    // Fail
    // -----------------------------------------------------------------------

    fn lower_fail(
        &mut self,
        expr_id: ExprId,
        fail: &crate::expr::FailExpr,
    ) -> Result<Operand, LowerError> {
        let message = if let Some(msg) = fail.message {
            self.lower_expr(msg)?
        } else {
            Operand::String("explicit failure".into())
        };

        let dst = self.alloc_temp();
        self.ops.push(KernelOp::AssignFailure {
            dst,
            source: self.source_for(expr_id),
            message,
        });
        Ok(Operand::Slot(dst))
    }

    // -----------------------------------------------------------------------
    // InterpolatedString
    // -----------------------------------------------------------------------

    fn lower_interpolated_string(
        &mut self,
        expr_id: ExprId,
        is: &crate::expr::InterpolatedStringExpr,
    ) -> Result<Operand, LowerError> {
        let mut parts = Vec::new();
        for part in &is.parts {
            match part {
                crate::expr::StringPart::Text(s) => {
                    parts.push(Operand::String(s.clone()));
                }
                crate::expr::StringPart::Expr(id) => {
                    parts.push(self.lower_expr(*id)?);
                }
            }
        }
        let dst = self.alloc_temp();
        self.ops.push(KernelOp::ConcatStrings {
            dst,
            source: self.source_for(expr_id),
            parts,
        });
        Ok(Operand::Slot(dst))
    }

    // -----------------------------------------------------------------------
    // Call (direct named-body only in 010-0)
    // -----------------------------------------------------------------------

    fn lower_call(
        &mut self,
        expr_id: ExprId,
        call: &crate::expr::CallExpr,
    ) -> Result<Operand, LowerError> {
        // Check if callee is a direct named function.
        let callee_kind = self.arena[call.callee].kind.clone();
        if let ExprKind::NameRef(ref nr) = callee_kind {
            match nr.resolved {
                Some(DefTarget::Item(item_id)) => {
                    if let ItemKind::Function(ref func) = self.arena[item_id].kind {
                        // Direct named-body call → CallBody.
                        let callee_body = a_body(func.body);
                        let mut args = Vec::with_capacity(call.args.len());
                        for arg in &call.args {
                            args.push(self.lower_expr(arg.expr)?);
                        }
                        let dst = self.alloc_temp();
                        self.local_call_deps.insert(callee_body);
                        self.ops.push(KernelOp::CallBody {
                            dst,
                            source: self.source_for(expr_id),
                            callee: callee_body,
                            args,
                        });
                        return Ok(Operand::Slot(dst));
                    }
                }
                Some(DefTarget::Binding(_)) => {
                    // Closure application — deferred to 010-1.
                    return Err(LowerError::UnsupportedShape(
                        "closure application (deferred to 010-1)".into(),
                    ));
                }
                _ => {}
            }
        }

        Err(LowerError::UnsupportedShape(format!(
            "unsupported call callee kind"
        )))
    }

    // -----------------------------------------------------------------------
    // Statement lowering
    // -----------------------------------------------------------------------

    fn lower_stmt(&mut self, stmt_id: crate::StmtId) -> Result<(), LowerError> {
        let kind = self.arena[stmt_id].kind.clone();
        match kind {
            StmtKind::Let(let_) => {
                // Allocate slot for binding. Init is materialized lazily.
                if let crate::pattern::PatternKind::Binding(ref b) = self.arena[let_.pattern].kind {
                    self.alloc_binding_slot(b.binding);
                }
                // For non-simple patterns, we'd need destructuring — UnsupportedShape.
                Ok(())
            }
            StmtKind::Expr(expr_id) => {
                // Lower for side effects (result discarded).
                self.lower_expr(expr_id)?;
                Ok(())
            }
            StmtKind::Assign(assign) => {
                let Some(bid) = assign.resolved else {
                    return Err(LowerError::UnsupportedShape(
                        "unresolved assign target".into(),
                    ));
                };
                let slot = self.alloc_binding_slot(bid);
                let value = self.lower_expr(assign.value)?;
                self.ops.push(KernelOp::AssignAlias {
                    dst: slot,
                    source: source_expr(self.body_id, assign.value),
                    value,
                });
                self.materialized.insert(bid);
                Ok(())
            }
            StmtKind::Return(ret) => {
                if let Some(val) = ret.value {
                    self.lower_expr(val)?;
                }
                Ok(())
            }
            StmtKind::Item(_) => {
                // Item declarations are handled when referenced.
                Ok(())
            }
            StmtKind::CompoundUpdate(_) => Err(LowerError::UnsupportedShape(
                "compound update (deferred to 010-2)".into(),
            )),
            StmtKind::FieldAssign(_) => Err(LowerError::UnsupportedShape(
                "field assign (deferred to 010-2)".into(),
            )),
            StmtKind::Error => Err(LowerError::UnsupportedShape("error statement".into())),
        }
    }
}
