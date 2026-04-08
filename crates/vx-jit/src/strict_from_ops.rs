//! KernelOp-based strict lowering: walks KernelOps + slot types
//! and produces a `VixenTypedFunction` ready for Kajit compilation.
//!
//! This replaces the legacy CST-based StrictLowerer. It only handles
//! a scalar subset of KernelOps; bodies with unsupported ops stay interpreted.

use std::collections::{HashMap, HashSet};

use kajit_hir::{FieldDef, Module, VixenExt};
use kajit_reprs::hir::{BinaryOp, Literal, LocalId, Type, TypeDef, TypeDefId, TypeDefKind};
use vx_jit_types::{
    VixenCallableRef, VixenTypedExpr, VixenTypedFunction, VixenTypedLocal, VixenTypedParam,
    VixenTypedStmt,
};
use vx_types::{Scalar, Ty};
use vx_vir::exec_types::{BodyId, ExecutableBody, KernelOp, Operand, ResultSource, SlotId};
use vx_vir::lower::SlotTypeMap;

/// Result of successfully strict-lowering a body.
pub struct StrictLowered {
    pub vtf: VixenTypedFunction,
    pub module: Module,
}

/// The subset of types that strict fragments support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrictTy {
    U64,
    Bool,
    String,
}

impl StrictTy {
    fn from_vx_ty(ty: &Ty) -> Option<Self> {
        match ty {
            Ty::Scalar(Scalar::U64) => Some(StrictTy::U64),
            Ty::Scalar(Scalar::Bool) => Some(StrictTy::Bool),
            Ty::Scalar(Scalar::String) => Some(StrictTy::String),
            _ => None,
        }
    }

    fn to_hir_type(self, str_def: TypeDefId) -> Type {
        match self {
            StrictTy::U64 => Type::u(64),
            StrictTy::Bool => Type::bool(),
            StrictTy::String => Type::named(str_def, Vec::new()),
        }
    }
}

/// Try to strict-lower a body from its KernelOps.
///
/// Returns `None` if the body is not strict-compatible (synthesized body,
/// unsupported ops, missing types).
pub fn try_strict_lower(
    body: &ExecutableBody,
    slot_types: &SlotTypeMap,
    original_body_count: u32,
) -> Option<StrictLowered> {
    // Reject synthesized bodies.
    if body.id.0 >= original_body_count {
        eprintln!(
            "[strict] rejecting body {:?} (>= original_body_count={})",
            body.id, original_body_count
        );
        return None;
    }
    eprintln!(
        "[strict] attempting body {:?} with {} ops, result_source={:?}",
        body.id,
        body.ops.len(),
        body.result_source
    );
    for (i, op) in body.ops.iter().enumerate() {
        eprintln!("[strict]   op[{i}]: {op:?}");
    }

    let mut module = Module::new();
    let _ = module.install_runtime_memory_callables();
    let str_def = module.add_type_def(TypeDef {
        name: "Str".to_owned(),
        generic_params: Vec::new(),
        kind: TypeDefKind::Struct {
            fields: vec![
                FieldDef {
                    name: "ptr".to_owned(),
                    ty: Type::u(64),
                    offset: None,
                },
                FieldDef {
                    name: "len".to_owned(),
                    ty: Type::u(64),
                    offset: None,
                },
            ],
        },
        size: None,
        transparent: false,
    });

    let mut ctx = LowerCtx {
        body_id: body.id,
        slot_types,
        str_def,
        slot_to_local: HashMap::new(),
        deferred_selects: HashMap::new(),
        inline_exprs: HashMap::new(),
        next_local_id: 0,
        params: Vec::new(),
        locals: Vec::new(),
        stmts: Vec::new(),
    };

    // Map param slots to locals.
    for (i, &slot) in body.params.iter().enumerate() {
        let ty = slot_types.get(&(body.id, slot)).or_else(|| {
            eprintln!(
                "[strict] param slot {:?} missing type in body {:?}",
                slot, body.id
            );
            None
        })?;
        let strict_ty = StrictTy::from_vx_ty(ty)?;
        let local = ctx.alloc_local();
        ctx.slot_to_local.insert(slot, (local, strict_ty));
        ctx.params.push(VixenTypedParam {
            local,
            name: format!("p{i}"),
            ty: strict_ty.to_hir_type(str_def),
        });
    }

    // Lower each op.
    for (i, op) in body.ops.iter().enumerate() {
        if ctx.lower_op(op).is_none() {
            eprintln!("[strict] failed at op[{i}]");
            return None;
        }
    }

    // Determine result.
    let result_slot = match body.result_source {
        ResultSource::Slot(s) => s,
        _ => return None,
    };

    // If the result is a deferred select chain, expand into structured If/Return.
    let return_type = if ctx.deferred_selects.contains_key(&result_slot) {
        let result_ty = ctx.slot_ty(result_slot)?;
        let ret_ty = result_ty.to_hir_type(str_def);
        let return_stmts = ctx.expand_select_return(result_slot)?;
        ctx.stmts.extend(return_stmts);
        ret_ty
    } else {
        let (result_local, result_ty) = *ctx.slot_to_local.get(&result_slot)?;
        let ret_ty = result_ty.to_hir_type(str_def);
        ctx.stmts
            .push(VixenTypedStmt::Return(Some(VixenTypedExpr::Local(
                result_local,
            ))));
        ret_ty
    };

    Some(StrictLowered {
        vtf: VixenTypedFunction {
            name: String::new(), // Filled in by caller.
            params: ctx.params,
            locals: ctx.locals,
            return_type,
            body: ctx.stmts,
            comment: None,
        },
        module,
    })
}

/// A deferred Select — we don't emit it as a statement, we expand it
/// into structured `If { Return, Return }` when building the return.
struct DeferredSelect {
    condition: VixenTypedExpr,
    then_value: SelectValue,
    else_value: SelectValue,
}

/// A Select branch value: either a concrete expression or another deferred Select.
enum SelectValue {
    Expr(VixenTypedExpr),
    Deferred(SlotId),
}

struct LowerCtx<'a> {
    body_id: BodyId,
    slot_types: &'a SlotTypeMap,
    str_def: TypeDefId,
    slot_to_local: HashMap<SlotId, (LocalId, StrictTy)>,
    deferred_selects: HashMap<SlotId, DeferredSelect>,
    /// Slot expressions that should be inlined rather than referenced via Local().
    /// Used for Bool conditions consumed by Select, to avoid Kajit nested-If bugs.
    inline_exprs: HashMap<SlotId, VixenTypedExpr>,
    /// Statements deferred for emission into If branches, keyed by the slot they produce.
    deferred_stmts: HashMap<SlotId, Vec<VixenTypedStmt>>,
    /// Tracks which slots are consumed by Select branches (to decide deferral).
    select_consumed_slots: HashSet<SlotId>,
    next_local_id: u32,
    params: Vec<VixenTypedParam>,
    locals: Vec<VixenTypedLocal>,
    stmts: Vec<VixenTypedStmt>,
}

impl LowerCtx<'_> {
    fn alloc_local(&mut self) -> LocalId {
        let id = LocalId::new(self.next_local_id);
        self.next_local_id += 1;
        id
    }

    fn slot_ty(&self, slot: SlotId) -> Option<StrictTy> {
        let ty = self.slot_types.get(&(self.body_id, slot))?;
        StrictTy::from_vx_ty(ty)
    }

    fn slot_local(&self, slot: SlotId) -> Option<(LocalId, StrictTy)> {
        self.slot_to_local.get(&slot).copied()
    }

    fn operand_expr(&self, op: &Operand) -> Option<(VixenTypedExpr, StrictTy)> {
        match op {
            Operand::Slot(s) => {
                // Prefer inline expression if available (avoids Kajit nested-If bug).
                if let Some(expr) = self.inline_exprs.get(s) {
                    let ty = self.slot_ty(*s)?;
                    return Some((expr.clone(), ty));
                }
                let (local, ty) = self.slot_local(*s)?;
                Some((VixenTypedExpr::Local(local), ty))
            }
            Operand::Int(v) => Some((
                VixenTypedExpr::Literal(Literal::Integer(*v as u64)),
                StrictTy::U64,
            )),
            Operand::String(s) => Some((
                VixenTypedExpr::TypedLiteral {
                    literal: Literal::String(s.clone()),
                    ty: Type::named(self.str_def, Vec::new()),
                },
                StrictTy::String,
            )),
        }
    }

    /// Ensure a slot has a local binding. If not yet mapped, create one.
    fn ensure_dst(&mut self, slot: SlotId) -> Option<(LocalId, StrictTy)> {
        if let Some(&pair) = self.slot_to_local.get(&slot) {
            return Some(pair);
        }
        let strict_ty = self.slot_ty(slot).or_else(|| {
            eprintln!(
                "[strict] ensure_dst: no type for slot {:?} in body {:?}",
                slot, self.body_id
            );
            None
        })?;
        let local = self.alloc_local();
        self.locals.push(VixenTypedLocal {
            local,
            name: format!("s{}", slot.0),
            ty: strict_ty.to_hir_type(self.str_def),
        });
        self.slot_to_local.insert(slot, (local, strict_ty));
        Some((local, strict_ty))
    }

    fn lower_op(&mut self, op: &KernelOp) -> Option<()> {
        match op {
            KernelOp::AssignConstInt { dst, value, .. } => {
                let (local, _) = self.ensure_dst(*dst)?;
                self.stmts.push(VixenTypedStmt::Let {
                    local,
                    value: VixenTypedExpr::Literal(Literal::Integer(*value as u64)),
                });
            }
            KernelOp::AssignConstBool { dst, value, .. } => {
                let (local, _) = self.ensure_dst(*dst)?;
                self.stmts.push(VixenTypedStmt::Let {
                    local,
                    value: VixenTypedExpr::Literal(Literal::Bool(*value)),
                });
            }
            KernelOp::AssignConstString { dst, value, .. } => {
                let (local, _) = self.ensure_dst(*dst)?;
                self.stmts.push(VixenTypedStmt::Let {
                    local,
                    value: VixenTypedExpr::TypedLiteral {
                        literal: Literal::String(value.clone()),
                        ty: Type::named(self.str_def, Vec::new()),
                    },
                });
            }
            KernelOp::AssignAlias { dst, value, .. } => {
                let (src_expr, src_ty) = self.operand_expr(value)?;
                let (local, _) = self.ensure_dst(*dst)?;
                // Also propagate type if dst wasn't in slot_types.
                self.slot_to_local.insert(*dst, (local, src_ty));
                self.stmts.push(VixenTypedStmt::Let {
                    local,
                    value: src_expr,
                });
            }
            KernelOp::Add { dst, lhs, rhs, .. } => {
                self.lower_binary_op(*dst, lhs, rhs, BinaryOp::Add)?;
            }
            KernelOp::Sub { dst, lhs, rhs, .. } => {
                self.lower_binary_op(*dst, lhs, rhs, BinaryOp::Sub)?;
            }
            KernelOp::Equal { dst, lhs, rhs, .. } => {
                self.lower_inline_binary(*dst, lhs, rhs, BinaryOp::Eq)?;
            }
            KernelOp::NotEqual { dst, lhs, rhs, .. } => {
                self.lower_inline_binary(*dst, lhs, rhs, BinaryOp::Ne)?;
            }
            KernelOp::Select {
                dst,
                condition,
                then_value,
                else_value,
                ..
            } => {
                let (cond_expr, _) = self.operand_expr(condition)?;
                let then_sv = self.select_value(then_value)?;
                let else_sv = self.select_value(else_value)?;
                self.deferred_selects.insert(
                    *dst,
                    DeferredSelect {
                        condition: cond_expr,
                        then_value: then_sv,
                        else_value: else_sv,
                    },
                );
            }
            KernelOp::StringLen { dst, value, .. } => {
                let (val_expr, _) = self.operand_expr(value)?;
                let (local, _) = self.ensure_dst(*dst)?;
                self.stmts.push(VixenTypedStmt::Let {
                    local,
                    value: VixenTypedExpr::Field {
                        base: Box::new(val_expr),
                        field: "len".to_owned(),
                    },
                });
            }
            KernelOp::StringSlice {
                dst,
                value,
                start,
                end,
                ..
            } => {
                let (val_expr, _) = self.operand_expr(value)?;
                let (start_expr, _) = self.operand_expr(start)?;
                let (end_expr, _) = self.operand_expr(end)?;
                let (local, _) = self.ensure_dst(*dst)?;
                self.stmts.push(VixenTypedStmt::Let {
                    local,
                    value: VixenTypedExpr::Struct {
                        def: self.str_def,
                        fields: vec![
                            (
                                "ptr".to_owned(),
                                VixenTypedExpr::Binary {
                                    op: BinaryOp::Add,
                                    lhs: Box::new(VixenTypedExpr::Field {
                                        base: Box::new(val_expr),
                                        field: "ptr".to_owned(),
                                    }),
                                    rhs: Box::new(start_expr.clone()),
                                },
                            ),
                            (
                                "len".to_owned(),
                                VixenTypedExpr::Binary {
                                    op: BinaryOp::Sub,
                                    lhs: Box::new(end_expr),
                                    rhs: Box::new(start_expr),
                                },
                            ),
                        ],
                    },
                });
            }
            KernelOp::ConcatStrings { dst, parts, .. } => {
                if self.lower_concat(*dst, parts).is_none() {
                    eprintln!("[strict] lower_concat failed for dst={dst:?}");
                    return None;
                }
            }
            // Everything else: not strict-compatible.
            _ => {
                eprintln!("[strict] unsupported op: {op:?}");
                return None;
            }
        }
        Some(())
    }

    fn lower_binary_op(
        &mut self,
        dst: SlotId,
        lhs: &Operand,
        rhs: &Operand,
        op: BinaryOp,
    ) -> Option<()> {
        let (lhs_expr, _) = self.operand_expr(lhs)?;
        let (rhs_expr, _) = self.operand_expr(rhs)?;
        let (local, _) = self.ensure_dst(dst)?;
        self.stmts.push(VixenTypedStmt::Let {
            local,
            value: VixenTypedExpr::Binary {
                op,
                lhs: Box::new(lhs_expr),
                rhs: Box::new(rhs_expr),
            },
        });
        Some(())
    }

    /// Store a comparison as an inline expression rather than a Let binding.
    /// This avoids a Kajit bug where pre-bound Bool locals used as nested
    /// If conditions produce incorrect branch selection.
    fn lower_inline_binary(
        &mut self,
        dst: SlotId,
        lhs: &Operand,
        rhs: &Operand,
        op: BinaryOp,
    ) -> Option<()> {
        let (lhs_expr, _) = self.operand_expr(lhs)?;
        let (rhs_expr, _) = self.operand_expr(rhs)?;
        self.inline_exprs.insert(
            dst,
            VixenTypedExpr::Binary {
                op,
                lhs: Box::new(lhs_expr),
                rhs: Box::new(rhs_expr),
            },
        );
        Some(())
    }

    /// Convert an operand into a SelectValue, checking if it references a deferred select.
    fn select_value(&self, op: &Operand) -> Option<SelectValue> {
        match op {
            Operand::Slot(s) if self.deferred_selects.contains_key(s) => {
                Some(SelectValue::Deferred(*s))
            }
            _ => {
                let (expr, _) = self.operand_expr(op)?;
                Some(SelectValue::Expr(expr))
            }
        }
    }

    /// Expand a deferred select chain into structured If/Return statements.
    fn expand_select_return(&self, slot: SlotId) -> Option<Vec<VixenTypedStmt>> {
        let sel = self.deferred_selects.get(&slot)?;
        let then_stmts = self.expand_select_branch(&sel.then_value)?;
        let else_stmts = self.expand_select_branch(&sel.else_value)?;
        Some(vec![VixenTypedStmt::If {
            condition: sel.condition.clone(),
            then_body: then_stmts,
            else_body: else_stmts,
        }])
    }

    fn expand_select_branch(&self, sv: &SelectValue) -> Option<Vec<VixenTypedStmt>> {
        match sv {
            SelectValue::Expr(expr) => Some(vec![VixenTypedStmt::Return(Some(expr.clone()))]),
            SelectValue::Deferred(slot) => self.expand_select_return(*slot),
        }
    }

    fn lower_concat(&mut self, dst: SlotId, parts: &[Operand]) -> Option<()> {
        if parts.is_empty() {
            let (local, _) = self.ensure_dst(dst)?;
            self.stmts.push(VixenTypedStmt::Let {
                local,
                value: VixenTypedExpr::TypedLiteral {
                    literal: Literal::String(String::new()),
                    ty: Type::named(self.str_def, Vec::new()),
                },
            });
            return Some(());
        }

        // Collect part expressions.
        let part_exprs: Vec<VixenTypedExpr> = parts
            .iter()
            .map(|p| self.operand_expr(p).map(|(e, _)| e))
            .collect::<Option<Vec<_>>>()?;

        // Compute total length: sum of each part's len.
        let len_exprs: Vec<VixenTypedExpr> = part_exprs.iter().map(string_len_expr).collect();
        let total_len_expr = sum_expr(len_exprs)?;

        let total_len_local = self.alloc_local();
        self.locals.push(VixenTypedLocal {
            local: total_len_local,
            name: "__concat_total_len".to_owned(),
            ty: Type::u(64),
        });
        self.stmts.push(VixenTypedStmt::Let {
            local: total_len_local,
            value: total_len_expr,
        });

        // Allocate transient buffer.
        let buf_local = self.alloc_local();
        self.locals.push(VixenTypedLocal {
            local: buf_local,
            name: "__concat_buf".to_owned(),
            ty: Type::u(64),
        });
        self.stmts.push(VixenTypedStmt::Let {
            local: buf_local,
            value: VixenTypedExpr::Call {
                callee: VixenCallableRef::Named("runtime.alloc_transient".to_owned()),
                args: vec![
                    VixenTypedExpr::Local(total_len_local),
                    VixenTypedExpr::Literal(Literal::Integer(1)),
                ],
            },
        });

        // Memcpy each part.
        let mut cursor = VixenTypedExpr::Local(buf_local);
        for (i, part) in part_exprs.iter().enumerate() {
            let next = self.alloc_local();
            self.locals.push(VixenTypedLocal {
                local: next,
                name: format!("__concat_cursor_{i}"),
                ty: Type::u(64),
            });
            self.stmts.push(VixenTypedStmt::Let {
                local: next,
                value: VixenTypedExpr::Call {
                    callee: VixenCallableRef::Named("runtime.memcpy".to_owned()),
                    args: vec![cursor, string_ptr_expr(part), string_len_expr(part)],
                },
            });
            cursor = VixenTypedExpr::Local(next);
        }

        // Build result struct.
        let (local, _) = self.ensure_dst(dst)?;
        self.stmts.push(VixenTypedStmt::Let {
            local,
            value: VixenTypedExpr::Struct {
                def: self.str_def,
                fields: vec![
                    ("ptr".to_owned(), VixenTypedExpr::Local(buf_local)),
                    ("len".to_owned(), VixenTypedExpr::Local(total_len_local)),
                ],
            },
        });
        Some(())
    }
}

// --- String helpers ---

fn string_len_expr(expr: &VixenTypedExpr) -> VixenTypedExpr {
    match expr {
        VixenTypedExpr::TypedLiteral {
            literal: Literal::String(s),
            ..
        } => VixenTypedExpr::Literal(Literal::Integer(s.len() as u64)),
        _ => VixenTypedExpr::Field {
            base: Box::new(expr.clone()),
            field: "len".to_owned(),
        },
    }
}

fn string_ptr_expr(expr: &VixenTypedExpr) -> VixenTypedExpr {
    match expr {
        VixenTypedExpr::TypedLiteral {
            literal: Literal::String(value),
            ..
        } => {
            let leaked: &'static [u8] = Box::leak(value.as_bytes().to_vec().into_boxed_slice());
            VixenTypedExpr::Literal(Literal::Integer(leaked.as_ptr() as u64))
        }
        _ => VixenTypedExpr::Field {
            base: Box::new(expr.clone()),
            field: "ptr".to_owned(),
        },
    }
}

fn sum_expr(mut exprs: Vec<VixenTypedExpr>) -> Option<VixenTypedExpr> {
    if exprs.is_empty() {
        return None;
    }
    let mut expr = exprs.remove(0);
    for rhs in exprs {
        expr = VixenTypedExpr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(expr),
            rhs: Box::new(rhs),
        };
    }
    Some(expr)
}
