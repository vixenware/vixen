//! Lower VIR to KernelOp bytecode.
//!
//! Entry point: [`lower_vir_program`].
//!
//! This is the VIR replacement for `vx_analysis::exec_slice`. It consumes
//! a [`VirProgram`] + [`VirDemandResult`] and produces an
//! [`ExecutableProgram`] that `vx-rt` can evaluate.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::exec_types::{
    BodyId as ABodyId, BridgeLocalBindingId, ExecSliceError, ExecutableBody, ExecutableProgram,
    ExecutableRuntimeNode, FieldKey, KernelKind, KernelOp, Operand, ResultSource,
    RuntimeBoundaryInputSpec, RuntimeBoundaryKind, RuntimeBoundaryPayload, RuntimeNodeId, SlotId,
    SourceExpr,
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

fn a_expr(id: ExprId) -> crate::exec_types::ExprId {
    crate::exec_types::ExprId(id.0)
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
// Runtime-boundary accumulator (collects lowering outputs)
// ---------------------------------------------------------------------------

/// Accumulates the per-boundary outputs as `lower_runtime_boundary_keywords`
/// walks the where-record. One value flows through instead of five mutable
/// `&mut Vec` parameters.
#[derive(Default)]
struct BoundaryAccumulator {
    positional_inputs: Vec<RuntimeBoundaryInputSpec>,
    mounts: Vec<(crate::exec_types::TreeFilePath, RuntimeBoundaryInputSpec)>,
    writable_roots: Vec<crate::exec_types::TreeFilePath>,
    create_dirs: Vec<crate::exec_types::TreeFilePath>,
    coarse_mounts: Vec<RuntimeBoundaryInputSpec>,
}

// ---------------------------------------------------------------------------
// Shared lowering context (across all bodies)
// ---------------------------------------------------------------------------

struct LoweringContext<'a> {
    arena: &'a crate::VirArena,
    /// Source IDs of user sources (non-stdlib). Functions from other sources
    /// with known names (exec, discover, stabilize) are runtime boundaries.
    user_sources: HashSet<vx_parser::SourceId>,
    /// Lazy fields from demand analysis, indexed for fast lookup.
    lazy_fields: HashSet<(BodyId, ExprId, FieldKey)>,
    /// Next body ID for synthesized thunk/closure bodies.
    next_synth_body_id: u32,
    /// Synthesized bodies (thunks, closure bodies lowered inline).
    synth_bodies: BTreeMap<ABodyId, ExecutableBody>,
    /// Runtime node allocator.
    next_runtime_node_id: u32,
    runtime_nodes: BTreeMap<RuntimeNodeId, ExecutableRuntimeNode>,
}

impl<'a> LoweringContext<'a> {
    fn new(
        arena: &'a crate::VirArena,
        demand: &VirDemandResult,
        user_sources: HashSet<vx_parser::SourceId>,
    ) -> Self {
        let mut lazy_fields = HashSet::new();
        for lf in &demand.lazy_fields {
            lazy_fields.insert((lf.body, lf.product_expr, to_analysis_field_key(&lf.field)));
        }
        // Start synth body IDs after all VIR-allocated bodies.
        let next_synth_body_id = arena.bodies.len() as u32;
        LoweringContext {
            arena,
            user_sources,
            lazy_fields,
            next_synth_body_id,
            synth_bodies: BTreeMap::new(),
            next_runtime_node_id: 0,
            runtime_nodes: BTreeMap::new(),
        }
    }

    fn alloc_runtime_node(&mut self) -> RuntimeNodeId {
        let id = RuntimeNodeId(self.next_runtime_node_id);
        self.next_runtime_node_id += 1;
        id
    }

    /// Check if a function item is a runtime boundary (exec/discover/stabilize from stdlib).
    fn classify_runtime_boundary(&self, item_id: crate::ItemId) -> Option<RuntimeBoundaryKind> {
        let item = &self.arena[item_id];
        if let ItemKind::Function(ref f) = item.kind {
            // If the function's source is one of the user sources, it's not a boundary.
            if self.user_sources.contains(&item.span.source) {
                return None;
            }
            match f.name.as_str() {
                "exec" => Some(RuntimeBoundaryKind::Exec),
                "discover" => Some(RuntimeBoundaryKind::Discover),
                "stabilize" => Some(RuntimeBoundaryKind::SharedKeyed),
                _ => None,
            }
        } else {
            None
        }
    }

    fn alloc_synth_body_id(&mut self) -> ABodyId {
        let id = ABodyId(self.next_synth_body_id);
        self.next_synth_body_id += 1;
        id
    }

    fn is_lazy(&self, body: BodyId, product_expr: ExprId, field: &FieldKey) -> bool {
        self.lazy_fields
            .contains(&(body, product_expr, field.clone()))
    }
}

fn to_analysis_field_key(fk: &crate::expr::FieldKey) -> FieldKey {
    match fk {
        crate::expr::FieldKey::Index(i) => FieldKey::Index(*i),
        crate::expr::FieldKey::Name(n) => FieldKey::Name(n.clone()),
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

    // The primary source unit is the user program. Later source units come from
    // the injected stdlib and should remain eligible for runtime-boundary
    // classification.
    let user_sources: HashSet<vx_parser::SourceId> = program
        .source_units
        .first()
        .map(|su_id| HashSet::from([program.arena[*su_id].source]))
        .unwrap_or_default();

    let mut ctx = LoweringContext::new(&program.arena, demand, user_sources);
    let mut bodies: BTreeMap<ABodyId, ExecutableBody> = BTreeMap::new();

    for &body_id in &demand.reachable {
        let lowering = VirBodyLowering::new(&mut ctx, body_id);
        let result = lowering.lower()?;
        bodies.insert(a_body(body_id), result);
    }

    // Add synthesized thunk/closure bodies.
    bodies.extend(ctx.synth_bodies);

    Ok(ExecutableProgram {
        entry: a_body(entry),
        bodies,
        runtime_nodes: ctx.runtime_nodes,
    })
}

// ---------------------------------------------------------------------------
// Per-body lowering context
// ---------------------------------------------------------------------------

struct VirBodyLowering<'a, 'ctx> {
    ctx: &'a mut LoweringContext<'ctx>,
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

impl<'a, 'ctx> VirBodyLowering<'a, 'ctx> {
    fn new(ctx: &'a mut LoweringContext<'ctx>, body_id: BodyId) -> Self {
        let body = &ctx.arena[body_id];
        let mut next_slot = 0u32;
        let mut slot_by_binding = HashMap::new();
        let mut params = Vec::new();

        // Pre-allocate param slots.
        for param in &body.params {
            if let crate::pattern::PatternKind::Binding(ref b) = ctx.arena[param.pattern].kind {
                let slot = SlotId(next_slot);
                next_slot += 1;
                slot_by_binding.insert(b.binding, slot);
                params.push(slot);
            }
        }

        VirBodyLowering {
            ctx,
            body_id,
            next_slot,
            slot_by_binding,
            materialized: HashSet::new(),
            ops: Vec::new(),
            params,
            local_call_deps: BTreeSet::new(),
        }
    }

    fn arena(&self) -> &crate::VirArena {
        self.ctx.arena
    }

    fn lower(mut self) -> Result<ExecutableBody, LowerError> {
        let root_expr = self.arena()[self.body_id].root_expr;
        let result_operand = self.lower_expr(root_expr)?;
        let result_slot = self.ensure_slot(result_operand, self.body_id, root_expr);

        Ok(ExecutableBody {
            id: a_body(self.body_id),
            kernel: crate::exec_types::KernelId(self.body_id.0),
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

    /// Allocate a temp slot. The `expr_id` parameter is retained for call-site
    /// readability but is no longer used (the previous strict-lowering JIT
    /// consumer is gone).
    fn alloc_typed_temp(&mut self, _expr_id: ExprId) -> SlotId {
        self.alloc_temp()
    }

    fn ensure_slot(&mut self, operand: Operand, body: BodyId, expr: ExprId) -> SlotId {
        match operand {
            Operand::Slot(s) => s,
            Operand::Int(v) => {
                let dst = self.alloc_typed_temp(expr);
                self.ops.push(KernelOp::AssignConstInt {
                    dst,
                    source: source_expr(body, expr),
                    value: v,
                });
                dst
            }
            Operand::String(v) => {
                let dst = self.alloc_typed_temp(expr);
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

    fn body_debug_label(&self) -> String {
        match &self.arena()[self.body_id].owner {
            crate::body::BodyOwner::Function { item } => match &self.arena()[*item].kind {
                ItemKind::Function(f) => format!("function {}", f.name),
                _ => format!("function item {:?}", item),
            },
            crate::body::BodyOwner::Closure => "closure".to_owned(),
            crate::body::BodyOwner::TopLevel { source } => format!("top-level source {:?}", source),
        }
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

        let binding = &self.arena()[bid];

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
        let kind = self.arena()[expr_id].kind.clone();
        match kind {
            ExprKind::Literal(lit) => self.lower_literal(expr_id, &lit.value),
            ExprKind::NameRef(nr) => self.lower_name_ref(expr_id, &nr),
            ExprKind::Block(block) => self.lower_block(expr_id, &block),
            ExprKind::Binary(bin) => self.lower_binary(expr_id, &bin),
            ExprKind::Prefix(pre) => self.lower_prefix(expr_id, &pre),
            ExprKind::If(if_) => self.lower_if(expr_id, &if_),
            ExprKind::Fail(fail) => self.lower_fail(expr_id, &fail),
            ExprKind::Bomb => self.lower_bomb(expr_id),
            ExprKind::InterpolatedString(is) => self.lower_interpolated_string(expr_id, &is),
            ExprKind::Call(call) => self.lower_call(expr_id, &call),

            // 010-1: closures, products, field projection
            ExprKind::Closure(cl) => self.lower_closure(expr_id, &cl),
            ExprKind::Tuple(t) => self.lower_tuple(expr_id, &t),
            ExprKind::Array(arr) => self.lower_array(expr_id, &arr),
            ExprKind::Record(r) => self.lower_record(expr_id, &r),
            ExprKind::StructLiteral(sl) => self.lower_struct_literal(expr_id, &sl),
            ExprKind::Field(f) => self.lower_field(expr_id, &f),
            ExprKind::Index(_) => Err(LowerError::UnsupportedShape(
                "index access (not in executable slice)".into(),
            )),
            ExprKind::Match(m) => self.lower_match(expr_id, &m),

            // 010-2: invoke, type ascription
            ExprKind::Invoke(inv) => self.lower_invoke(expr_id, &inv),
            ExprKind::TypeOp(top) => self.lower_type_op(expr_id, &top),

            // Unsupported shapes
            ExprKind::Where(w) => self.lower_where(expr_id, &w),
            ExprKind::PartialApp(_) => Err(LowerError::UnsupportedShape(
                "partial application (not in executable slice)".into(),
            )),
            ExprKind::Range(_) => Err(LowerError::UnsupportedShape(
                "range (not in executable slice)".into(),
            )),
            ExprKind::With(_) => Err(LowerError::UnsupportedShape(
                "with (not in executable slice)".into(),
            )),
            ExprKind::Fallible(_) => Err(LowerError::UnsupportedShape(
                "fallible (not in executable slice)".into(),
            )),
            ExprKind::Import(_) => Err(LowerError::UnsupportedShape(
                "import (not in executable slice)".into(),
            )),
            ExprKind::PathLiteral(pl) => self.lower_path_literal(&pl),
            ExprKind::Map(_) => Err(LowerError::UnsupportedShape(
                "map (not in executable slice)".into(),
            )),
            ExprKind::Error => Err(LowerError::UnsupportedShape(format!(
                "error node in {} ({:?}) expr {:?}",
                self.body_debug_label(),
                self.body_id,
                expr_id
            ))),
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
                let dst = self.alloc_typed_temp(expr_id);
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
                // Function used as value → AllocClosure with no captures.
                if let ItemKind::Function(ref func) = self.arena()[item_id].kind {
                    let fn_body = func.body;
                    let param_count = self.arena()[fn_body]
                        .params
                        .iter()
                        .filter(|p| !p.is_keyword)
                        .count();
                    let dst = self.alloc_typed_temp(expr_id);
                    self.ops.push(KernelOp::AllocClosure {
                        dst,
                        source: self.source_for(expr_id),
                        body: a_body(fn_body),
                        captures: vec![],
                        param_count,
                    });
                    return Ok(Operand::Slot(dst));
                }
                Err(LowerError::UnsupportedShape(format!(
                    "item ref `{}`",
                    nr.name
                )))
            }
            Some(DefTarget::Variant { .. }) => Err(LowerError::UnsupportedShape(
                "variant ref (deferred)".into(),
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
        let dst = self.alloc_typed_temp(expr_id);
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

        let dst = self.alloc_typed_temp(expr_id);
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

        let dst = self.alloc_typed_temp(expr_id);
        self.ops.push(KernelOp::AssignFailure {
            dst,
            source: self.source_for(expr_id),
            message,
        });
        Ok(Operand::Slot(dst))
    }

    // -----------------------------------------------------------------------
    // Bomb (💣) — the demand-driven tripwire
    // -----------------------------------------------------------------------

    fn lower_bomb(&mut self, expr_id: ExprId) -> Result<Operand, LowerError> {
        let dst = self.alloc_typed_temp(expr_id);
        self.ops.push(KernelOp::AssignLazyPoison {
            dst,
            source: self.source_for(expr_id),
            label: "💣".into(),
        });
        Ok(Operand::Slot(dst))
    }

    // -----------------------------------------------------------------------
    // Match
    // -----------------------------------------------------------------------

    fn lower_match(
        &mut self,
        expr_id: ExprId,
        m: &crate::expr::MatchExpr,
    ) -> Result<Operand, LowerError> {
        if m.arms.is_empty() {
            return Err(LowerError::UnsupportedShape("empty match".into()));
        }

        let scrutinee = self.lower_expr(m.scrutinee)?;

        // Lower arms right-to-left, building a chain of Select ops.
        // The innermost (last) arm must be a wildcard/binding (catch-all) or we emit a failure.
        let mut result: Option<Operand> = None;

        for arm in m.arms.iter().rev() {
            let pat = &self.arena()[arm.pattern].kind.clone();
            let body_expr = arm.body;

            match pat {
                crate::pattern::PatternKind::Wildcard => {
                    // Catch-all: just the body.
                    let body_val = self.lower_expr(body_expr)?;
                    result = Some(match result {
                        None => body_val,
                        Some(else_val) => {
                            // Wildcard not in last position — it shadows everything after.
                            // Just use wildcard's value (dead code after it).
                            let _ = else_val;
                            body_val
                        }
                    });
                }
                crate::pattern::PatternKind::Binding(b) => {
                    // Bind scrutinee to the pattern's binding, then evaluate body.
                    let slot = self.alloc_binding_slot(b.binding);
                    self.ops.push(KernelOp::AssignAlias {
                        dst: slot,
                        source: self.source_for(expr_id),
                        value: scrutinee.clone(),
                    });
                    self.materialized.insert(b.binding);
                    let body_val = self.lower_expr(body_expr)?;
                    result = Some(match result {
                        None => body_val,
                        Some(_else_val) => body_val, // binding is catch-all
                    });
                }
                crate::pattern::PatternKind::Literal(lit) => {
                    let body_val = self.lower_expr(body_expr)?;
                    let condition = self.lower_pattern_test(expr_id, &scrutinee, &lit.value)?;
                    let else_val = result.unwrap_or_else(|| {
                        // No catch-all — emit failure for non-exhaustive match.
                        let fail_dst = self.alloc_typed_temp(expr_id);
                        self.ops.push(KernelOp::AssignFailure {
                            dst: fail_dst,
                            source: self.source_for(expr_id),
                            message: Operand::String("non-exhaustive match".into()),
                        });
                        Operand::Slot(fail_dst)
                    });
                    let dst = self.alloc_typed_temp(expr_id);
                    self.ops.push(KernelOp::Select {
                        dst,
                        source: self.source_for(expr_id),
                        condition,
                        then_value: body_val,
                        else_value: else_val,
                    });
                    result = Some(Operand::Slot(dst));
                }
                other => {
                    return Err(LowerError::UnsupportedShape(format!(
                        "match pattern: {other:?}"
                    )));
                }
            }
        }

        result.ok_or_else(|| LowerError::UnsupportedShape("empty match arms".into()))
    }

    /// Emit an equality test between scrutinee and a literal pattern value.
    fn lower_pattern_test(
        &mut self,
        expr_id: ExprId,
        scrutinee: &Operand,
        lit: &LitKind,
    ) -> Result<Operand, LowerError> {
        let lit_operand = self.lower_literal(expr_id, lit)?;
        let dst = self.alloc_typed_temp(expr_id);
        self.ops.push(KernelOp::Equal {
            dst,
            source: self.source_for(expr_id),
            lhs: scrutinee.clone(),
            rhs: lit_operand,
        });
        Ok(Operand::Slot(dst))
    }

    // -----------------------------------------------------------------------
    // Invoke
    // -----------------------------------------------------------------------

    fn lower_where(
        &mut self,
        expr_id: ExprId,
        w: &crate::expr::WhereExpr,
    ) -> Result<Operand, LowerError> {
        let callee_kind = self.arena()[w.callee].kind.clone();
        match callee_kind {
            ExprKind::Call(call) => {
                let inner_callee_kind = self.arena()[call.callee].kind.clone();
                if let ExprKind::NameRef(nr) = inner_callee_kind
                    && let Some(DefTarget::Item(item_id)) = nr.resolved
                    && let Some(boundary_kind) = self.ctx.classify_runtime_boundary(item_id)
                {
                    return self.lower_runtime_boundary_call(
                        expr_id,
                        &call,
                        boundary_kind,
                        Some(w.keywords),
                    );
                }
                Err(LowerError::UnsupportedShape(
                    "where is only lowered for runtime boundary calls".into(),
                ))
            }
            _ => Err(LowerError::UnsupportedShape(
                "where (not in executable slice)".into(),
            )),
        }
    }

    fn lower_invoke(
        &mut self,
        expr_id: ExprId,
        inv: &crate::expr::InvokeExpr,
    ) -> Result<Operand, LowerError> {
        let callee = self.lower_expr(inv.callee)?;
        let dst = self.alloc_typed_temp(expr_id);
        self.ops.push(KernelOp::ApplyClosure {
            dst,
            source: self.source_for(expr_id),
            closure: callee,
            args: vec![],
        });
        Ok(Operand::Slot(dst))
    }

    // -----------------------------------------------------------------------
    // TypeOp
    // -----------------------------------------------------------------------

    fn lower_type_op(
        &mut self,
        _expr_id: ExprId,
        top: &crate::expr::TypeOpExpr,
    ) -> Result<Operand, LowerError> {
        match top.op {
            crate::expr::TypeOpKind::Ascription => {
                // Passthrough — no runtime effect.
                self.lower_expr(top.expr)
            }
            crate::expr::TypeOpKind::As => {
                // For representation-preserving conversions such as newtypes,
                // the current evaluator keeps the same runtime value.
                self.lower_expr(top.expr)
            }
            crate::expr::TypeOpKind::Is => Err(LowerError::UnsupportedShape(
                "type test (is) not in executable slice".into(),
            )),
        }
    }

    // -----------------------------------------------------------------------
    // PathLiteral
    // -----------------------------------------------------------------------

    fn lower_path_literal(
        &mut self,
        pl: &crate::expr::PathLiteralExpr,
    ) -> Result<Operand, LowerError> {
        Ok(Operand::String(self.assemble_path_literal(pl)))
    }

    fn assemble_path_literal(&self, pl: &crate::expr::PathLiteralExpr) -> String {
        let mut result = String::new();
        for part in &pl.parts {
            match part {
                crate::expr::StringPart::Text(t) => result.push_str(t),
                crate::expr::StringPart::Expr(_) => {
                    // Interpolated path segments — for now just placeholder.
                    result.push_str("<expr>");
                }
            }
        }
        result
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
        let dst = self.alloc_typed_temp(expr_id);
        self.ops.push(KernelOp::ConcatStrings {
            dst,
            source: self.source_for(expr_id),
            parts,
        });
        Ok(Operand::Slot(dst))
    }

    // -----------------------------------------------------------------------
    // Call
    // -----------------------------------------------------------------------

    fn lower_call(
        &mut self,
        expr_id: ExprId,
        call: &crate::expr::CallExpr,
    ) -> Result<Operand, LowerError> {
        // Check for builtin method calls: receiver.method(args).
        if let Some(result) = self.try_lower_method_call(expr_id, call)? {
            return Ok(result);
        }

        // Check if callee is a direct named function.
        let callee_kind = self.arena()[call.callee].kind.clone();
        if let ExprKind::NameRef(ref nr) = callee_kind {
            match nr.resolved {
                Some(DefTarget::Item(item_id)) => {
                    // Check for runtime boundary (exec/discover/stabilize from stdlib).
                    if let Some(boundary_kind) = self.ctx.classify_runtime_boundary(item_id) {
                        return self.lower_runtime_boundary_call(
                            expr_id,
                            call,
                            boundary_kind,
                            None,
                        );
                    }

                    if let ItemKind::Function(ref func) = self.arena()[item_id].kind {
                        // Direct named-body call → CallBody.
                        let callee_body = a_body(func.body);
                        let mut args = Vec::with_capacity(call.args.len());
                        for arg in &call.args {
                            args.push(self.lower_expr(arg.expr)?);
                        }
                        let dst = self.alloc_typed_temp(expr_id);
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
                Some(DefTarget::Binding(bid)) => {
                    // Closure application → ApplyClosure.
                    let closure = self.materialize_binding(bid)?;
                    let mut args = Vec::with_capacity(call.args.len());
                    for arg in &call.args {
                        args.push(self.lower_expr(arg.expr)?);
                    }
                    let dst = self.alloc_typed_temp(expr_id);
                    self.ops.push(KernelOp::ApplyClosure {
                        dst,
                        source: self.source_for(expr_id),
                        closure,
                        args,
                    });
                    return Ok(Operand::Slot(dst));
                }
                _ => {}
            }
        }

        // Complex callee (e.g. (make_fn())(arg)) → lower callee, ApplyClosure.
        let callee = self.lower_expr(call.callee)?;
        let mut args = Vec::with_capacity(call.args.len());
        for arg in &call.args {
            args.push(self.lower_expr(arg.expr)?);
        }
        let dst = self.alloc_typed_temp(expr_id);
        self.ops.push(KernelOp::ApplyClosure {
            dst,
            source: self.source_for(expr_id),
            closure: callee,
            args,
        });
        Ok(Operand::Slot(dst))
    }

    // -----------------------------------------------------------------------
    // Runtime boundary calls
    // -----------------------------------------------------------------------

    fn lower_runtime_boundary_call(
        &mut self,
        expr_id: ExprId,
        call: &crate::expr::CallExpr,
        kind: RuntimeBoundaryKind,
        keyword_expr: Option<ExprId>,
    ) -> Result<Operand, LowerError> {
        let node_id = self.ctx.alloc_runtime_node();

        // Lower positional args + keyword args into a single accumulator.
        let mut acc = BoundaryAccumulator::default();
        for arg in &call.args {
            let input = self.lower_boundary_input(arg.expr)?;
            acc.positional_inputs.push(input);
        }
        if let Some(keyword_expr) = keyword_expr {
            self.lower_runtime_boundary_keywords(kind, keyword_expr, &mut acc)?;
        }
        let BoundaryAccumulator {
            positional_inputs,
            mounts,
            writable_roots,
            create_dirs,
            coarse_mounts,
        } = acc;

        let has_closure_arg = positional_inputs
            .iter()
            .any(|input| matches!(input, RuntimeBoundaryInputSpec::Closure { .. }));
        let has_where_clause = keyword_expr.is_some();

        let dst = self.alloc_typed_temp(expr_id);
        self.ops.push(KernelOp::AwaitRuntime {
            dst,
            source: self.source_for(expr_id),
            node: node_id,
            positional_inputs: positional_inputs.clone(),
            mounts: mounts.clone(),
            writable_roots: writable_roots.clone(),
            create_dirs: create_dirs.clone(),
            coarse_mounts: coarse_mounts.clone(),
        });

        let payload = match kind {
            RuntimeBoundaryKind::Exec => {
                RuntimeBoundaryPayload::Exec(crate::exec_types::ExecBoundaryPayload {
                    positional_arg_count: positional_inputs.len(),
                    has_where_clause,
                    has_mounts: !mounts.is_empty(),
                    has_writable: !writable_roots.is_empty(),
                    has_create_dirs: !create_dirs.is_empty(),
                    has_coarse_mounts: !coarse_mounts.is_empty(),
                })
            }
            RuntimeBoundaryKind::Discover => {
                RuntimeBoundaryPayload::Discover(crate::exec_types::DiscoverBoundaryPayload {
                    positional_arg_count: positional_inputs.len(),
                    has_closure_arg,
                })
            }
            RuntimeBoundaryKind::SharedKeyed => {
                RuntimeBoundaryPayload::SharedKeyed(crate::exec_types::SharedKeyedBoundaryPayload {
                    positional_arg_count: positional_inputs.len(),
                })
            }
        };

        self.ctx.runtime_nodes.insert(
            node_id,
            ExecutableRuntimeNode {
                id: node_id,
                kind,
                source: self.source_for(expr_id),
                payload,
            },
        );

        Ok(Operand::Slot(dst))
    }

    fn lower_runtime_boundary_keywords(
        &mut self,
        kind: RuntimeBoundaryKind,
        keyword_expr: ExprId,
        acc: &mut BoundaryAccumulator,
    ) -> Result<(), LowerError> {
        let BoundaryAccumulator {
            positional_inputs,
            mounts,
            writable_roots,
            create_dirs,
            coarse_mounts,
        } = acc;
        let ExprKind::Record(record) = self.arena()[keyword_expr].kind.clone() else {
            return Err(LowerError::UnsupportedShape(
                "runtime boundary where expects a record".into(),
            ));
        };

        for field in record.fields {
            match kind {
                RuntimeBoundaryKind::Exec => match field.name.as_str() {
                    "observer" => positional_inputs.push(self.lower_boundary_input(field.value)?),
                    "mounts" => mounts.extend(self.lower_boundary_mounts(field.value)?),
                    "writable" => {
                        writable_roots.extend(self.lower_boundary_writable_roots(field.value)?)
                    }
                    "create_dirs" => {
                        create_dirs.extend(self.lower_boundary_writable_roots(field.value)?)
                    }
                    "coarse_mounts" => {
                        coarse_mounts.extend(self.lower_boundary_array_items(field.value)?)
                    }
                    other => {
                        return Err(LowerError::UnsupportedShape(format!(
                            "exec where keyword `{other}` not in executable slice"
                        )));
                    }
                },
                _ => {
                    return Err(LowerError::UnsupportedShape(format!(
                        "runtime boundary {kind:?} does not accept where in executable slice"
                    )));
                }
            }
        }

        Ok(())
    }

    fn lower_boundary_mounts(
        &mut self,
        expr_id: ExprId,
    ) -> Result<Vec<(crate::exec_types::TreeFilePath, RuntimeBoundaryInputSpec)>, LowerError> {
        let ExprKind::Map(map) = self.arena()[expr_id].kind.clone() else {
            return Err(LowerError::UnsupportedShape(
                "exec mounts keyword expects a map literal".into(),
            ));
        };

        let mut result = Vec::new();
        for entry in map.entries {
            let crate::expr::MapElement::Entry(entry) = entry else {
                return Err(LowerError::UnsupportedShape(
                    "spread in exec mounts is not supported yet".into(),
                ));
            };
            let path = match entry.key {
                crate::expr::MapKey::Expr(key_expr) => self.lower_boundary_path(key_expr)?,
                crate::expr::MapKey::Name { name, .. } => crate::exec_types::TreeFilePath(name),
            };
            let value = self.lower_boundary_input(entry.value)?;
            result.push((path, value));
        }
        Ok(result)
    }

    fn lower_boundary_writable_roots(
        &mut self,
        expr_id: ExprId,
    ) -> Result<Vec<crate::exec_types::TreeFilePath>, LowerError> {
        let mut result = Vec::new();
        for item in self.lower_boundary_array_items(expr_id)? {
            let path = match item {
                RuntimeBoundaryInputSpec::PathLiteral(path) => path,
                other => {
                    return Err(LowerError::UnsupportedShape(format!(
                        "exec writable entries must be path literals, got {other:?}"
                    )));
                }
            };
            result.push(path);
        }
        Ok(result)
    }

    fn lower_boundary_array_items(
        &mut self,
        expr_id: ExprId,
    ) -> Result<Vec<RuntimeBoundaryInputSpec>, LowerError> {
        let ExprKind::Array(array) = self.arena()[expr_id].kind.clone() else {
            return Err(LowerError::UnsupportedShape(
                "runtime boundary keyword expects an array literal".into(),
            ));
        };

        let mut result = Vec::new();
        for element in array.elements {
            match element {
                crate::expr::ArrayElement::Expr(expr_id) => {
                    result.push(self.lower_boundary_input(expr_id)?);
                }
                crate::expr::ArrayElement::Spread(_) => {
                    return Err(LowerError::UnsupportedShape(
                        "spread in runtime boundary array keyword is not supported yet".into(),
                    ));
                }
            }
        }
        Ok(result)
    }

    fn lower_boundary_path(
        &mut self,
        expr_id: ExprId,
    ) -> Result<crate::exec_types::TreeFilePath, LowerError> {
        match self.arena()[expr_id].kind.clone() {
            ExprKind::PathLiteral(pl) => Ok(crate::exec_types::TreeFilePath(
                self.assemble_path_literal(&pl),
            )),
            other => Err(LowerError::UnsupportedShape(format!(
                "runtime boundary path expects a path literal, got {other:?}"
            ))),
        }
    }

    fn lower_boundary_input(
        &mut self,
        expr_id: ExprId,
    ) -> Result<RuntimeBoundaryInputSpec, LowerError> {
        let kind = self.arena()[expr_id].kind.clone();
        match kind {
            ExprKind::Closure(cl) => self.lower_boundary_closure(expr_id, &cl),
            ExprKind::Literal(lit) => match &lit.value {
                LitKind::Int { raw } => {
                    let value: i64 = raw
                        .parse()
                        .map_err(|_| LowerError::InvalidLiteral { raw: raw.clone() })?;
                    Ok(RuntimeBoundaryInputSpec::IntLiteral(value))
                }
                LitKind::String { raw } => Ok(RuntimeBoundaryInputSpec::StringLiteral(raw.clone())),
                _ => {
                    // Fall back to lowering as an expression and using a slot binding.
                    let operand = self.lower_expr(expr_id)?;
                    let slot = self.ensure_slot(operand, self.body_id, expr_id);
                    Ok(RuntimeBoundaryInputSpec::Binding {
                        binding: BridgeLocalBindingId(slot.0),
                        slot,
                    })
                }
            },
            ExprKind::PathLiteral(ref pl) => {
                let path = self.assemble_path_literal(pl);
                Ok(RuntimeBoundaryInputSpec::PathLiteral(
                    crate::exec_types::TreeFilePath(path),
                ))
            }
            ExprKind::Array(ref arr) => {
                let mut items = Vec::new();
                for elem in &arr.elements {
                    match elem {
                        crate::expr::ArrayElement::Expr(id) => {
                            items.push(self.lower_boundary_input(*id)?);
                        }
                        crate::expr::ArrayElement::Spread(_) => {
                            return Err(LowerError::UnsupportedShape(
                                "spread in runtime boundary array arg".into(),
                            ));
                        }
                    }
                }
                Ok(RuntimeBoundaryInputSpec::Array(items))
            }
            _ => {
                // General expression → lower to slot.
                let operand = self.lower_expr(expr_id)?;
                let slot = self.ensure_slot(operand, self.body_id, expr_id);
                Ok(RuntimeBoundaryInputSpec::Binding {
                    binding: BridgeLocalBindingId(slot.0),
                    slot,
                })
            }
        }
    }

    fn lower_boundary_closure(
        &mut self,
        expr_id: ExprId,
        cl: &crate::expr::ClosureExpr,
    ) -> Result<RuntimeBoundaryInputSpec, LowerError> {
        let captures = self.arena()[cl.body].captures.clone();
        if !captures.is_empty() {
            return Err(LowerError::UnsupportedShape(
                "runtime boundary closure captures are not supported yet".into(),
            ));
        }

        self.lower_closure_body(cl.body, &captures)?;
        let exec_body = self
            .ctx
            .synth_bodies
            .remove(&a_body(cl.body))
            .ok_or_else(|| {
                LowerError::UnsupportedShape(
                    "failed to materialize runtime boundary closure body".into(),
                )
            })?;

        Ok(RuntimeBoundaryInputSpec::Closure {
            source: self.source_for(expr_id),
            closure: crate::exec_types::ExecutableClosure {
                params: exec_body.params,
                ops: exec_body.ops,
                result_source: exec_body.result_source,
            },
        })
    }

    // -----------------------------------------------------------------------
    // Builtin method calls
    // -----------------------------------------------------------------------

    /// Try to lower `receiver.method(args)` as a builtin method call.
    /// Returns `Ok(None)` if this is not a method call pattern.
    fn try_lower_method_call(
        &mut self,
        expr_id: ExprId,
        call: &crate::expr::CallExpr,
    ) -> Result<Option<Operand>, LowerError> {
        let callee_kind = self.arena()[call.callee].kind.clone();
        let field = match callee_kind {
            ExprKind::Field(f) => f,
            _ => return Ok(None),
        };

        let method_name = match &field.field {
            crate::expr::FieldKey::Name(n) => n.clone(),
            _ => return Ok(None),
        };

        let receiver = self.lower_expr(field.receiver)?;
        let source = self.source_for(expr_id);

        match method_name.as_str() {
            "len" => {
                let dst = self.alloc_typed_temp(expr_id);
                self.ops.push(KernelOp::StringLen {
                    dst,
                    source,
                    value: receiver,
                });
                Ok(Some(Operand::Slot(dst)))
            }
            "slice" => {
                let (start, end) = if call.args.len() == 2 {
                    (
                        self.lower_expr(call.args[0].expr)?,
                        self.lower_expr(call.args[1].expr)?,
                    )
                } else if call.args.len() == 1 {
                    // .slice(start..end) — single range arg
                    let arg_expr = call.args[0].expr;
                    if let ExprKind::Range(ref range) = self.arena()[arg_expr].kind {
                        let s = range.start.ok_or_else(|| {
                            LowerError::UnsupportedShape(".slice() range missing start".into())
                        })?;
                        let e = range.end.ok_or_else(|| {
                            LowerError::UnsupportedShape(".slice() range missing end".into())
                        })?;
                        (self.lower_expr(s)?, self.lower_expr(e)?)
                    } else {
                        return Err(LowerError::UnsupportedShape(
                            ".slice() expects 2 args or a range, got 1 non-range arg".into(),
                        ));
                    }
                } else {
                    return Err(LowerError::UnsupportedShape(format!(
                        ".slice() expects 2 args or a range, got {}",
                        call.args.len()
                    )));
                };
                let dst = self.alloc_typed_temp(expr_id);
                self.ops.push(KernelOp::StringSlice {
                    dst,
                    source,
                    value: receiver,
                    start,
                    end,
                });
                Ok(Some(Operand::Slot(dst)))
            }
            "collect" | "blob" | "which" => {
                let method = match method_name.as_str() {
                    "collect" => crate::exec_types::RuntimeMemberOp::Collect,
                    "blob" => crate::exec_types::RuntimeMemberOp::Blob,
                    "which" => crate::exec_types::RuntimeMemberOp::Which,
                    _ => unreachable!(),
                };
                let mut args = Vec::new();
                for arg in &call.args {
                    args.push(self.lower_expr(arg.expr)?);
                }
                let dst = self.alloc_typed_temp(expr_id);
                self.ops.push(KernelOp::CallMember {
                    dst,
                    source,
                    receiver,
                    method,
                    args,
                });
                Ok(Some(Operand::Slot(dst)))
            }
            _ => Ok(None), // Not a builtin method — fall through to normal call.
        }
    }

    // -----------------------------------------------------------------------
    // Tuple (2-element only)
    // -----------------------------------------------------------------------

    fn lower_tuple(
        &mut self,
        expr_id: ExprId,
        t: &crate::expr::TupleExpr,
    ) -> Result<Operand, LowerError> {
        if t.elements.len() != 2 {
            return Err(LowerError::UnsupportedShape(format!(
                "only 2-item tuples supported, got {}",
                t.elements.len()
            )));
        }

        let first = self.lower_element_or_thunk(expr_id, t.elements[0], FieldKey::Index(0))?;
        let second = self.lower_element_or_thunk(expr_id, t.elements[1], FieldKey::Index(1))?;

        let dst = self.alloc_typed_temp(expr_id);
        self.ops.push(KernelOp::MakePair {
            dst,
            source: self.source_for(expr_id),
            first,
            second,
        });
        Ok(Operand::Slot(dst))
    }

    // -----------------------------------------------------------------------
    // Record
    // -----------------------------------------------------------------------

    fn lower_record(
        &mut self,
        expr_id: ExprId,
        r: &crate::expr::RecordExpr,
    ) -> Result<Operand, LowerError> {
        let mut fields = Vec::with_capacity(r.fields.len());
        for f in &r.fields {
            let fk = FieldKey::Name(f.name.clone());
            let operand = self.lower_element_or_thunk(expr_id, f.value, fk)?;
            fields.push((f.name.clone(), operand));
        }
        let dst = self.alloc_typed_temp(expr_id);
        self.ops.push(KernelOp::MakeStruct {
            dst,
            source: self.source_for(expr_id),
            fields,
        });
        Ok(Operand::Slot(dst))
    }

    // -----------------------------------------------------------------------
    // StructLiteral
    // -----------------------------------------------------------------------

    fn lower_struct_literal(
        &mut self,
        expr_id: ExprId,
        sl: &crate::expr::StructLiteralExpr,
    ) -> Result<Operand, LowerError> {
        let mut fields = Vec::with_capacity(sl.fields.len());
        for f in &sl.fields {
            let fk = FieldKey::Name(f.name.clone());
            let operand = self.lower_element_or_thunk(expr_id, f.value, fk)?;
            fields.push((f.name.clone(), operand));
        }
        let dst = self.alloc_typed_temp(expr_id);
        self.ops.push(KernelOp::MakeStruct {
            dst,
            source: self.source_for(expr_id),
            fields,
        });
        Ok(Operand::Slot(dst))
    }

    // -----------------------------------------------------------------------
    // Array
    // -----------------------------------------------------------------------

    fn lower_array(
        &mut self,
        expr_id: ExprId,
        arr: &crate::expr::ArrayExpr,
    ) -> Result<Operand, LowerError> {
        let mut items = Vec::with_capacity(arr.elements.len());
        for elem in &arr.elements {
            match elem {
                crate::expr::ArrayElement::Expr(id) => {
                    items.push(self.lower_expr(*id)?);
                }
                crate::expr::ArrayElement::Spread(_) => {
                    return Err(LowerError::UnsupportedShape(
                        "array spread not supported".into(),
                    ));
                }
            }
        }
        let dst = self.alloc_typed_temp(expr_id);
        self.ops.push(KernelOp::MakeArray {
            dst,
            source: self.source_for(expr_id),
            items,
        });
        Ok(Operand::Slot(dst))
    }

    // -----------------------------------------------------------------------
    // Field projection with static resolution
    // -----------------------------------------------------------------------

    fn lower_field(
        &mut self,
        expr_id: ExprId,
        f: &crate::expr::FieldExpr,
    ) -> Result<Operand, LowerError> {
        // Try static resolution: trace through bindings to a product construction.
        if let Some(sub_expr) = self.resolve_field_statically(f.receiver, &f.field) {
            return self.lower_expr(sub_expr);
        }

        // Fall back to runtime projection.
        let receiver = self.lower_expr(f.receiver)?;
        let dst = self.alloc_typed_temp(expr_id);
        let source = self.source_for(expr_id);

        match &f.field {
            crate::expr::FieldKey::Index(i) => {
                self.ops.push(KernelOp::ProjectIndex {
                    dst,
                    source,
                    pair: receiver,
                    index: (*i) as usize,
                });
            }
            crate::expr::FieldKey::Name(name) => {
                self.ops.push(KernelOp::ProjectNamedField {
                    dst,
                    source,
                    value: receiver,
                    field: name.clone(),
                });
            }
        }
        Ok(Operand::Slot(dst))
    }

    /// Try to resolve a field access statically to the sub-expression at that field.
    fn resolve_field_statically(
        &self,
        receiver: ExprId,
        field: &crate::expr::FieldKey,
    ) -> Option<ExprId> {
        let mut visited = HashSet::new();
        self.resolve_to_product_field(receiver, field, &mut visited)
    }

    fn resolve_to_product_field(
        &self,
        expr: ExprId,
        field: &crate::expr::FieldKey,
        visited: &mut HashSet<ExprId>,
    ) -> Option<ExprId> {
        if !visited.insert(expr) {
            return None;
        }

        match &self.arena()[expr].kind {
            // Direct product → index into it.
            ExprKind::Tuple(t) => {
                if let crate::expr::FieldKey::Index(i) = field {
                    t.elements.get(*i as usize).copied()
                } else {
                    None
                }
            }
            ExprKind::Record(r) => {
                if let crate::expr::FieldKey::Name(name) = field {
                    r.fields.iter().find(|f| f.name == *name).map(|f| f.value)
                } else {
                    None
                }
            }
            ExprKind::StructLiteral(sl) => {
                if let crate::expr::FieldKey::Name(name) = field {
                    sl.fields.iter().find(|f| f.name == *name).map(|f| f.value)
                } else {
                    None
                }
            }
            // NameRef → Binding → follow init_expr.
            ExprKind::NameRef(nr) => {
                if let Some(DefTarget::Binding(bid)) = nr.resolved
                    && let Some(init) = self.arena()[bid].init_expr
                {
                    return self.resolve_to_product_field(init, field, visited);
                }
                None
            }
            // Nested field → resolve outer, then inner.
            ExprKind::Field(outer) => {
                let sub = self.resolve_to_product_field(outer.receiver, &outer.field, visited)?;
                self.resolve_to_product_field(sub, field, visited)
            }
            _ => None,
        }
    }

    // -----------------------------------------------------------------------
    // Closure
    // -----------------------------------------------------------------------

    fn lower_closure(
        &mut self,
        expr_id: ExprId,
        cl: &crate::expr::ClosureExpr,
    ) -> Result<Operand, LowerError> {
        let closure_body = cl.body;
        let captures_list = self.arena()[closure_body].captures.clone();
        let param_count = self.arena()[closure_body]
            .params
            .iter()
            .filter(|p| !p.is_keyword)
            .count();

        // Build capture operands: materialize each captured binding in parent.
        let mut capture_operands = Vec::with_capacity(captures_list.len());
        for &cap_bid in &captures_list {
            capture_operands.push(self.materialize_binding(cap_bid)?);
        }

        // Lower the closure body as a new ExecutableBody.
        let closure_a_body = a_body(closure_body);
        self.lower_closure_body(closure_body, &captures_list)?;

        let dst = self.alloc_typed_temp(expr_id);
        self.ops.push(KernelOp::AllocClosure {
            dst,
            source: self.source_for(expr_id),
            body: closure_a_body,
            captures: capture_operands,
            param_count,
        });
        Ok(Operand::Slot(dst))
    }

    /// Lower a closure body into an ExecutableBody and register it.
    fn lower_closure_body(
        &mut self,
        closure_body_id: BodyId,
        captures: &[BindingId],
    ) -> Result<(), LowerError> {
        // Extract everything we need from the arena before creating the child
        // (which borrows self.ctx mutably).
        let root_expr = self.ctx.arena[closure_body_id].root_expr;
        let param_bindings: Vec<BindingId> = self.ctx.arena[closure_body_id]
            .params
            .iter()
            .filter_map(|param| {
                if let crate::pattern::PatternKind::Binding(ref b) =
                    self.ctx.arena[param.pattern].kind
                {
                    Some(b.binding)
                } else {
                    None
                }
            })
            .collect();

        let mut next_slot = 0u32;
        let mut slot_by_binding = HashMap::new();
        let mut params = Vec::new();

        // Captures get slots first.
        for &cap_bid in captures {
            let slot = SlotId(next_slot);
            next_slot += 1;
            slot_by_binding.insert(cap_bid, slot);
            params.push(slot);
        }

        // Then parameters.
        for &bid in &param_bindings {
            let slot = SlotId(next_slot);
            next_slot += 1;
            slot_by_binding.insert(bid, slot);
            params.push(slot);
        }

        // Pre-materialized set: all captures + params.
        let mut materialized = HashSet::new();
        for &cap_bid in captures {
            materialized.insert(cap_bid);
        }
        for &bid in &param_bindings {
            materialized.insert(bid);
        }

        let mut child = VirBodyLowering {
            ctx: self.ctx,
            body_id: closure_body_id,
            next_slot,
            slot_by_binding,
            materialized,
            ops: Vec::new(),
            params,
            local_call_deps: BTreeSet::new(),
        };
        let result_operand = child.lower_expr(root_expr)?;
        let result_slot = child.ensure_slot(result_operand, closure_body_id, root_expr);

        let exec_body = ExecutableBody {
            id: a_body(closure_body_id),
            kernel: crate::exec_types::KernelId(closure_body_id.0),
            kernel_kind: KernelKind::Interpreted,
            params: child.params,
            ops: child.ops,
            result_source: ResultSource::Slot(result_slot),
            runtime_boundary_extractions: vec![],
            local_call_dependencies: child.local_call_deps.into_iter().collect(),
        };

        self.ctx
            .synth_bodies
            .insert(a_body(closure_body_id), exec_body);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Thunk synthesis
    // -----------------------------------------------------------------------

    /// Lower an element, or synthesize a thunk body if this field is lazy.
    fn lower_element_or_thunk(
        &mut self,
        product_expr: ExprId,
        element_expr: ExprId,
        field: FieldKey,
    ) -> Result<Operand, LowerError> {
        if self.ctx.is_lazy(self.body_id, product_expr, &field) {
            self.synthesize_thunk(product_expr, element_expr)
        } else {
            self.lower_expr(element_expr)
        }
    }

    /// Synthesize a thunk body for a lazy field element.
    /// Strategy: provision all parent bindings as captures, then prune unused ones.
    fn synthesize_thunk(
        &mut self,
        product_expr: ExprId,
        element_expr: ExprId,
    ) -> Result<Operand, LowerError> {
        let thunk_body_id = self.ctx.alloc_synth_body_id();

        // Collect all parent bindings → potential captures.
        let parent_bindings: Vec<(BindingId, SlotId)> = self
            .slot_by_binding
            .iter()
            .map(|(&bid, &slot)| (bid, slot))
            .collect();

        // Build child context with captures as params.
        let mut child_slot_by_binding = HashMap::new();
        let mut child_params = Vec::new();
        let mut capture_operands = Vec::new();
        let mut child_next_slot = 0u32;

        for &(bid, parent_slot) in &parent_bindings {
            let thunk_slot = SlotId(child_next_slot);
            child_next_slot += 1;
            child_slot_by_binding.insert(bid, thunk_slot);
            child_params.push(thunk_slot);
            capture_operands.push(Operand::Slot(parent_slot));
        }

        let mut child = VirBodyLowering {
            ctx: self.ctx,
            body_id: self.body_id, // Thunk shares parent's body for source exprs.
            next_slot: child_next_slot,
            slot_by_binding: child_slot_by_binding,
            materialized: HashSet::new(),
            ops: Vec::new(),
            params: child_params.clone(),
            local_call_deps: BTreeSet::new(),
        };

        // Mark all capture bindings as materialized in child.
        for &(bid, _) in &parent_bindings {
            child.materialized.insert(bid);
        }

        let result_operand = child.lower_expr(element_expr)?;
        let result_slot = child.ensure_slot(result_operand, self.body_id, element_expr);

        // Prune unused captures.
        let capture_count = parent_bindings.len();
        let mut used_slots: HashSet<SlotId> = HashSet::new();
        used_slots.insert(result_slot);
        for op in &child.ops {
            collect_referenced_slots(op, &mut used_slots);
        }

        // Figure out which captures are used (indices into parent_bindings).
        let used_capture_indices: Vec<usize> = (0..capture_count)
            .filter(|&i| used_slots.contains(&SlotId(i as u32)))
            .collect();

        // Extract child results (releases borrow on self.ctx via partial move).
        let child_next_slot = child.next_slot;
        let child_ops = child.ops;
        let child_call_deps = child.local_call_deps;

        // Now we can call self.materialize_binding freely.
        let mut slot_remap: HashMap<SlotId, SlotId> = HashMap::new();
        let mut final_params = Vec::new();
        let mut final_captures = Vec::new();
        let mut new_slot = 0u32;

        for &i in &used_capture_indices {
            let old_slot = SlotId(i as u32);
            let (bid, _) = parent_bindings[i];
            self.materialize_binding(bid)?;

            let ns = SlotId(new_slot);
            new_slot += 1;
            slot_remap.insert(old_slot, ns);
            final_params.push(ns);
            final_captures.push(capture_operands[i].clone());
        }

        // Remap generated (non-param) slots.
        for s in (capture_count as u32)..child_next_slot {
            let old = SlotId(s);
            if let std::collections::hash_map::Entry::Vacant(e) = slot_remap.entry(old) {
                e.insert(SlotId(new_slot));
                new_slot += 1;
            }
        }

        let remapped_result = slot_remap.get(&result_slot).copied().unwrap_or(result_slot);

        // Remap all ops.
        let mut final_ops = child_ops;
        for op in &mut final_ops {
            remap_op_slots(op, &slot_remap);
        }

        let exec_body = ExecutableBody {
            id: thunk_body_id,
            kernel: crate::exec_types::KernelId(thunk_body_id.0),
            kernel_kind: KernelKind::Interpreted,
            params: final_params,
            ops: final_ops,
            result_source: ResultSource::Slot(remapped_result),
            runtime_boundary_extractions: vec![],
            local_call_dependencies: child_call_deps.into_iter().collect(),
        };

        self.ctx.synth_bodies.insert(thunk_body_id, exec_body);

        // Emit AllocThunk in parent.
        let dst = self.alloc_typed_temp(product_expr);
        self.ops.push(KernelOp::AllocThunk {
            dst,
            source: self.source_for(product_expr),
            thunk_body: thunk_body_id,
            captures: final_captures,
        });
        Ok(Operand::Slot(dst))
    }

    // -----------------------------------------------------------------------
    // Statement lowering
    // -----------------------------------------------------------------------

    fn lower_stmt(&mut self, stmt_id: crate::StmtId) -> Result<(), LowerError> {
        let kind = self.arena()[stmt_id].kind.clone();
        match kind {
            StmtKind::Let(let_) => {
                // Allocate slot for binding. Init is materialized lazily.
                if let crate::pattern::PatternKind::Binding(ref b) = self.arena()[let_.pattern].kind
                {
                    self.alloc_binding_slot(b.binding);
                }
                Ok(())
            }
            StmtKind::Expr(expr_id) => {
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
            StmtKind::Item(_) => Ok(()),
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

// ---------------------------------------------------------------------------
// Slot remapping utilities (for thunk/closure capture pruning)
// ---------------------------------------------------------------------------

fn collect_referenced_slots(op: &KernelOp, slots: &mut HashSet<SlotId>) {
    fn collect_operand(op: &Operand, slots: &mut HashSet<SlotId>) {
        if let Operand::Slot(s) = op {
            slots.insert(*s);
        }
    }

    match op {
        KernelOp::AssignConstInt { .. }
        | KernelOp::AssignConstString { .. }
        | KernelOp::AssignConstBool { .. }
        | KernelOp::AssignLazyPoison { .. }
        | KernelOp::AssignHostLocal { .. } => {}

        KernelOp::AssignAlias { value, .. } => collect_operand(value, slots),
        KernelOp::AssignFailure { message, .. } => collect_operand(message, slots),
        KernelOp::MakeBlob { value, .. } => collect_operand(value, slots),
        KernelOp::AssignCallableAddConst { addend, .. } => collect_operand(addend, slots),
        KernelOp::AssignSandboxTreeRoot { .. } => {}

        KernelOp::AllocClosure { captures, .. } => {
            for c in captures {
                collect_operand(c, slots);
            }
        }
        KernelOp::AllocThunk { captures, .. } => {
            for c in captures {
                collect_operand(c, slots);
            }
        }
        KernelOp::ApplyCallable { callable, arg, .. } => {
            collect_operand(callable, slots);
            collect_operand(arg, slots);
        }
        KernelOp::ApplyClosure { closure, args, .. } => {
            collect_operand(closure, slots);
            for a in args {
                collect_operand(a, slots);
            }
        }

        KernelOp::MakePair { first, second, .. } => {
            collect_operand(first, slots);
            collect_operand(second, slots);
        }
        KernelOp::MakeStruct { fields, .. } => {
            for (_, v) in fields {
                collect_operand(v, slots);
            }
        }
        KernelOp::MakeArray { items, .. } => {
            for i in items {
                collect_operand(i, slots);
            }
        }

        KernelOp::ProjectIndex { pair, .. } => collect_operand(pair, slots),
        KernelOp::ProjectNamedField { value, .. } => collect_operand(value, slots),
        KernelOp::ProjectField { value, .. } => collect_operand(value, slots),

        KernelOp::Add { lhs, rhs, .. }
        | KernelOp::Sub { lhs, rhs, .. }
        | KernelOp::Equal { lhs, rhs, .. }
        | KernelOp::NotEqual { lhs, rhs, .. } => {
            collect_operand(lhs, slots);
            collect_operand(rhs, slots);
        }

        KernelOp::Select {
            condition,
            then_value,
            else_value,
            ..
        } => {
            collect_operand(condition, slots);
            collect_operand(then_value, slots);
            collect_operand(else_value, slots);
        }

        KernelOp::ConcatStrings { parts, .. } => {
            for p in parts {
                collect_operand(p, slots);
            }
        }
        KernelOp::StringLen { value, .. } => collect_operand(value, slots),
        KernelOp::StringSlice {
            value, start, end, ..
        } => {
            collect_operand(value, slots);
            collect_operand(start, slots);
            collect_operand(end, slots);
        }

        KernelOp::BuildTree { entries, .. } => {
            for (_, v) in entries {
                collect_operand(v, slots);
            }
        }
        KernelOp::CallMember { receiver, args, .. } => {
            collect_operand(receiver, slots);
            for a in args {
                collect_operand(a, slots);
            }
        }
        KernelOp::CallBody { args, .. } => {
            for a in args {
                collect_operand(a, slots);
            }
        }
        KernelOp::AwaitRuntime { .. } => {
            // Runtime boundary inputs use RuntimeBoundaryInputSpec, not plain Operand.
            // Full handling deferred to 010-2.
        }
    }
}

fn remap_op_slots(op: &mut KernelOp, map: &HashMap<SlotId, SlotId>) {
    fn remap_slot(s: &mut SlotId, map: &HashMap<SlotId, SlotId>) {
        if let Some(&new) = map.get(s) {
            *s = new;
        }
    }
    fn remap_operand(op: &mut Operand, map: &HashMap<SlotId, SlotId>) {
        if let Operand::Slot(s) = op {
            remap_slot(s, map);
        }
    }

    match op {
        KernelOp::AssignConstInt { dst, .. }
        | KernelOp::AssignConstString { dst, .. }
        | KernelOp::AssignConstBool { dst, .. }
        | KernelOp::AssignLazyPoison { dst, .. }
        | KernelOp::AssignHostLocal { dst, .. }
        | KernelOp::AssignSandboxTreeRoot { dst, .. } => {
            remap_slot(dst, map);
        }

        KernelOp::AssignAlias { dst, value, .. } => {
            remap_slot(dst, map);
            remap_operand(value, map);
        }
        KernelOp::AssignFailure { dst, message, .. } => {
            remap_slot(dst, map);
            remap_operand(message, map);
        }
        KernelOp::MakeBlob { dst, value, .. } => {
            remap_slot(dst, map);
            remap_operand(value, map);
        }
        KernelOp::AssignCallableAddConst { dst, addend, .. } => {
            remap_slot(dst, map);
            remap_operand(addend, map);
        }

        KernelOp::AllocClosure { dst, captures, .. } => {
            remap_slot(dst, map);
            for c in captures {
                remap_operand(c, map);
            }
        }
        KernelOp::AllocThunk { dst, captures, .. } => {
            remap_slot(dst, map);
            for c in captures {
                remap_operand(c, map);
            }
        }
        KernelOp::ApplyCallable {
            dst, callable, arg, ..
        } => {
            remap_slot(dst, map);
            remap_operand(callable, map);
            remap_operand(arg, map);
        }
        KernelOp::ApplyClosure {
            dst, closure, args, ..
        } => {
            remap_slot(dst, map);
            remap_operand(closure, map);
            for a in args {
                remap_operand(a, map);
            }
        }

        KernelOp::MakePair {
            dst, first, second, ..
        } => {
            remap_slot(dst, map);
            remap_operand(first, map);
            remap_operand(second, map);
        }
        KernelOp::MakeStruct { dst, fields, .. } => {
            remap_slot(dst, map);
            for (_, v) in fields {
                remap_operand(v, map);
            }
        }
        KernelOp::MakeArray { dst, items, .. } => {
            remap_slot(dst, map);
            for i in items {
                remap_operand(i, map);
            }
        }

        KernelOp::ProjectIndex { dst, pair, .. } => {
            remap_slot(dst, map);
            remap_operand(pair, map);
        }
        KernelOp::ProjectNamedField { dst, value, .. } => {
            remap_slot(dst, map);
            remap_operand(value, map);
        }
        KernelOp::ProjectField { dst, value, .. } => {
            remap_slot(dst, map);
            remap_operand(value, map);
        }

        KernelOp::Add { dst, lhs, rhs, .. }
        | KernelOp::Sub { dst, lhs, rhs, .. }
        | KernelOp::Equal { dst, lhs, rhs, .. }
        | KernelOp::NotEqual { dst, lhs, rhs, .. } => {
            remap_slot(dst, map);
            remap_operand(lhs, map);
            remap_operand(rhs, map);
        }

        KernelOp::Select {
            dst,
            condition,
            then_value,
            else_value,
            ..
        } => {
            remap_slot(dst, map);
            remap_operand(condition, map);
            remap_operand(then_value, map);
            remap_operand(else_value, map);
        }

        KernelOp::ConcatStrings { dst, parts, .. } => {
            remap_slot(dst, map);
            for p in parts {
                remap_operand(p, map);
            }
        }
        KernelOp::StringLen { dst, value, .. } => {
            remap_slot(dst, map);
            remap_operand(value, map);
        }
        KernelOp::StringSlice {
            dst,
            value,
            start,
            end,
            ..
        } => {
            remap_slot(dst, map);
            remap_operand(value, map);
            remap_operand(start, map);
            remap_operand(end, map);
        }

        KernelOp::BuildTree { dst, entries, .. } => {
            remap_slot(dst, map);
            for (_, v) in entries {
                remap_operand(v, map);
            }
        }
        KernelOp::CallMember {
            dst,
            receiver,
            args,
            ..
        } => {
            remap_slot(dst, map);
            remap_operand(receiver, map);
            for a in args {
                remap_operand(a, map);
            }
        }
        KernelOp::CallBody { dst, args, .. } => {
            remap_slot(dst, map);
            for a in args {
                remap_operand(a, map);
            }
        }
        KernelOp::AwaitRuntime { dst, .. } => {
            remap_slot(dst, map);
        }
    }
}
