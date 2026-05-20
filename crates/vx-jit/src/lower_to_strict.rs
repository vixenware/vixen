use std::collections::HashMap;

use vx_jit_types::{
    LocalId, LocalInfo, StrictBinOp, StrictExpr, StrictFunction, StrictLiteral, StrictParam,
    StrictStmt, StrictType, StringOwnership,
};
use vx_types::{Scalar, Ty};
use vx_vir::exec_types::{BodyId, ExecutableBody, KernelOp, Operand, ResultSource, SlotId};
use vx_vir::lower::SlotTypeMap;

fn strict_type_of(ty: &Ty) -> Option<StrictType> {
    match ty {
        Ty::Scalar(Scalar::U64) => Some(StrictType::U64),
        Ty::Scalar(Scalar::Bool) => Some(StrictType::Bool),
        Ty::Scalar(Scalar::String) => Some(StrictType::String),
        _ => None,
    }
}

/// Try to lower a body to a `StrictFunction`.
///
/// Returns `None` if the body contains unsupported ops, missing types, or is synthesized.
pub fn try_strict_lower(
    body: &ExecutableBody,
    slot_types: &SlotTypeMap,
    original_body_count: u32,
) -> Option<StrictFunction> {
    if body.id.0 >= original_body_count {
        return None;
    }

    let mut ctx = LowerCtx {
        body_id: body.id,
        slot_types,
        slot_to_local: HashMap::new(),
        deferred_selects: HashMap::new(),
        next_local_id: 0,
        params: Vec::new(),
        locals: Vec::new(),
        stmts: Vec::new(),
    };

    for (i, &slot) in body.params.iter().enumerate() {
        let ty = ctx.slot_types.get(&(ctx.body_id, slot))?;
        let strict_ty = strict_type_of(ty)?;
        let local = ctx.alloc_local();
        ctx.slot_to_local.insert(slot, (local, strict_ty));
        ctx.params.push(StrictParam {
            local,
            name: format!("p{i}"),
            ty: strict_ty,
        });
    }

    for op in &body.ops {
        ctx.lower_op(op)?;
    }

    let result_slot = match body.result_source {
        ResultSource::Slot(s) => s,
        _ => return None,
    };

    let return_type = ctx.slot_ty(result_slot)?;
    let return_ownership = if ctx.deferred_selects.contains_key(&result_slot) {
        let stmts = ctx.expand_select_return(result_slot)?;
        let ownership = ctx.infer_select_ownership(result_slot)?;
        ctx.stmts.extend(stmts);
        ownership
    } else {
        let (local, ty) = *ctx.slot_to_local.get(&result_slot)?;
        let ownership = ctx.local_ownership(local, ty);
        ctx.stmts
            .push(StrictStmt::Return(Some(StrictExpr::Local(local))));
        ownership
    };

    Some(StrictFunction {
        name: String::new(),
        params: ctx.params,
        locals: ctx.locals,
        return_type,
        return_ownership,
        body: ctx.stmts,
    })
}

struct DeferredSelect {
    condition: StrictExpr,
    then_value: SelectValue,
    else_value: SelectValue,
}

enum SelectValue {
    Expr(StrictExpr, StringOwnership),
    Deferred(SlotId),
}

struct LowerCtx<'a> {
    body_id: BodyId,
    slot_types: &'a SlotTypeMap,
    slot_to_local: HashMap<SlotId, (LocalId, StrictType)>,
    deferred_selects: HashMap<SlotId, DeferredSelect>,
    next_local_id: u32,
    params: Vec<StrictParam>,
    locals: Vec<LocalInfo>,
    stmts: Vec<StrictStmt>,
}

impl LowerCtx<'_> {
    fn alloc_local(&mut self) -> LocalId {
        let id = LocalId(self.next_local_id);
        self.next_local_id += 1;
        id
    }

    fn slot_ty(&self, slot: SlotId) -> Option<StrictType> {
        strict_type_of(self.slot_types.get(&(self.body_id, slot))?)
    }

    fn ensure_dst(&mut self, slot: SlotId) -> Option<(LocalId, StrictType)> {
        if let Some(&pair) = self.slot_to_local.get(&slot) {
            return Some(pair);
        }
        let strict_ty = self.slot_ty(slot)?;
        let local = self.alloc_local();
        self.locals.push(LocalInfo {
            id: local,
            ty: strict_ty,
            ownership: StringOwnership::Borrowed,
        });
        self.slot_to_local.insert(slot, (local, strict_ty));
        Some((local, strict_ty))
    }

    fn operand_expr(&self, op: &Operand) -> Option<(StrictExpr, StrictType, StringOwnership)> {
        match op {
            Operand::Slot(s) => {
                let (local, ty) = *self.slot_to_local.get(s)?;
                let ownership = self.local_ownership(local, ty);
                Some((StrictExpr::Local(local), ty, ownership))
            }
            Operand::Int(v) => Some((
                StrictExpr::Literal(StrictLiteral::U64(*v as u64)),
                StrictType::U64,
                StringOwnership::Borrowed,
            )),
            Operand::String(s) => Some((
                StrictExpr::Literal(StrictLiteral::String(s.clone())),
                StrictType::String,
                StringOwnership::Static,
            )),
        }
    }

    fn local_ownership(&self, local: LocalId, ty: StrictType) -> StringOwnership {
        if ty != StrictType::String {
            return StringOwnership::Borrowed;
        }
        self.locals
            .iter()
            .find(|l| l.id == local)
            .map(|l| l.ownership)
            .unwrap_or(StringOwnership::Borrowed)
    }

    fn set_local_ownership(&mut self, local: LocalId, ownership: StringOwnership) {
        if let Some(info) = self.locals.iter_mut().find(|l| l.id == local) {
            info.ownership = ownership;
        }
    }

    fn lower_op(&mut self, op: &KernelOp) -> Option<()> {
        match op {
            KernelOp::AssignConstInt { dst, value, .. } => {
                let (local, _) = self.ensure_dst(*dst)?;
                self.stmts.push(StrictStmt::Let {
                    local,
                    value: StrictExpr::Literal(StrictLiteral::U64(*value as u64)),
                });
            }
            KernelOp::AssignConstBool { dst, value, .. } => {
                let (local, _) = self.ensure_dst(*dst)?;
                self.stmts.push(StrictStmt::Let {
                    local,
                    value: StrictExpr::Literal(StrictLiteral::Bool(*value)),
                });
            }
            KernelOp::AssignConstString { dst, value, .. } => {
                let (local, _) = self.ensure_dst(*dst)?;
                self.set_local_ownership(local, StringOwnership::Static);
                self.stmts.push(StrictStmt::Let {
                    local,
                    value: StrictExpr::Literal(StrictLiteral::String(value.clone())),
                });
            }
            KernelOp::AssignAlias { dst, value, .. } => {
                let (src_expr, src_ty, src_ownership) = self.operand_expr(value)?;
                let (local, _) = self.ensure_dst(*dst)?;
                self.slot_to_local.insert(*dst, (local, src_ty));
                if src_ty == StrictType::String {
                    self.set_local_ownership(local, src_ownership);
                }
                self.stmts.push(StrictStmt::Let {
                    local,
                    value: src_expr,
                });
            }
            KernelOp::Add { dst, lhs, rhs, .. } => {
                let (lhs_expr, _, _) = self.operand_expr(lhs)?;
                let (rhs_expr, _, _) = self.operand_expr(rhs)?;
                let (local, _) = self.ensure_dst(*dst)?;
                self.stmts.push(StrictStmt::Let {
                    local,
                    value: StrictExpr::Binary {
                        op: StrictBinOp::Add,
                        lhs: Box::new(lhs_expr),
                        rhs: Box::new(rhs_expr),
                    },
                });
            }
            KernelOp::Sub { dst, lhs, rhs, .. } => {
                let (lhs_expr, _, _) = self.operand_expr(lhs)?;
                let (rhs_expr, _, _) = self.operand_expr(rhs)?;
                let (local, _) = self.ensure_dst(*dst)?;
                self.stmts.push(StrictStmt::Let {
                    local,
                    value: StrictExpr::Binary {
                        op: StrictBinOp::Sub,
                        lhs: Box::new(lhs_expr),
                        rhs: Box::new(rhs_expr),
                    },
                });
            }
            KernelOp::Equal { dst, lhs, rhs, .. } => {
                let (lhs_expr, _, _) = self.operand_expr(lhs)?;
                let (rhs_expr, _, _) = self.operand_expr(rhs)?;
                let (local, _) = self.ensure_dst(*dst)?;
                self.stmts.push(StrictStmt::Let {
                    local,
                    value: StrictExpr::Binary {
                        op: StrictBinOp::Eq,
                        lhs: Box::new(lhs_expr),
                        rhs: Box::new(rhs_expr),
                    },
                });
            }
            KernelOp::NotEqual { dst, lhs, rhs, .. } => {
                let (lhs_expr, _, _) = self.operand_expr(lhs)?;
                let (rhs_expr, _, _) = self.operand_expr(rhs)?;
                let (local, _) = self.ensure_dst(*dst)?;
                self.stmts.push(StrictStmt::Let {
                    local,
                    value: StrictExpr::Binary {
                        op: StrictBinOp::Ne,
                        lhs: Box::new(lhs_expr),
                        rhs: Box::new(rhs_expr),
                    },
                });
            }
            KernelOp::Select {
                dst,
                condition,
                then_value,
                else_value,
                ..
            } => {
                let (cond_expr, _, _) = self.operand_expr(condition)?;
                let then_sv = self.make_select_value(then_value)?;
                let else_sv = self.make_select_value(else_value)?;
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
                let (val_expr, _, _) = self.operand_expr(value)?;
                let (local, _) = self.ensure_dst(*dst)?;
                self.stmts.push(StrictStmt::Let {
                    local,
                    value: StrictExpr::StringLen {
                        value: Box::new(val_expr),
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
                let (val_expr, _, _) = self.operand_expr(value)?;
                let (start_expr, _, _) = self.operand_expr(start)?;
                let (end_expr, _, _) = self.operand_expr(end)?;
                let (local, _) = self.ensure_dst(*dst)?;
                self.set_local_ownership(local, StringOwnership::Borrowed);
                self.stmts.push(StrictStmt::Let {
                    local,
                    value: StrictExpr::StringSlice {
                        value: Box::new(val_expr),
                        start: Box::new(start_expr),
                        end: Box::new(end_expr),
                    },
                });
            }
            KernelOp::ConcatStrings { dst, parts, .. } => {
                let part_exprs: Vec<StrictExpr> = parts
                    .iter()
                    .map(|p| self.operand_expr(p).map(|(e, _, _)| e))
                    .collect::<Option<Vec<_>>>()?;
                let (local, _) = self.ensure_dst(*dst)?;
                self.set_local_ownership(local, StringOwnership::Owned);
                self.stmts.push(StrictStmt::Let {
                    local,
                    value: StrictExpr::ConcatStrings { parts: part_exprs },
                });
            }
            _ => return None,
        }
        Some(())
    }

    fn make_select_value(&self, op: &Operand) -> Option<SelectValue> {
        match op {
            Operand::Slot(s) if self.deferred_selects.contains_key(s) => {
                Some(SelectValue::Deferred(*s))
            }
            _ => {
                let (expr, ty, ownership) = self.operand_expr(op)?;
                let _ = ty;
                Some(SelectValue::Expr(expr, ownership))
            }
        }
    }

    fn expand_select_return(&self, slot: SlotId) -> Option<Vec<StrictStmt>> {
        let sel = self.deferred_selects.get(&slot)?;
        let then_stmts = self.expand_select_branch(&sel.then_value)?;
        let else_stmts = self.expand_select_branch(&sel.else_value)?;
        Some(vec![StrictStmt::If {
            condition: sel.condition.clone(),
            then_body: then_stmts,
            else_body: else_stmts,
        }])
    }

    fn expand_select_branch(&self, sv: &SelectValue) -> Option<Vec<StrictStmt>> {
        match sv {
            SelectValue::Expr(expr, _) => Some(vec![StrictStmt::Return(Some(expr.clone()))]),
            SelectValue::Deferred(slot) => self.expand_select_return(*slot),
        }
    }

    fn infer_select_ownership(&self, slot: SlotId) -> Option<StringOwnership> {
        let sel = self.deferred_selects.get(&slot)?;
        let then_own = self.branch_ownership(&sel.then_value)?;
        let else_own = self.branch_ownership(&sel.else_value)?;
        // Owned wins: if either branch produces an owned string, the return is Owned.
        Some(match (then_own, else_own) {
            (StringOwnership::Owned, _) | (_, StringOwnership::Owned) => StringOwnership::Owned,
            (StringOwnership::Borrowed, _) | (_, StringOwnership::Borrowed) => {
                StringOwnership::Borrowed
            }
            _ => StringOwnership::Static,
        })
    }

    fn branch_ownership(&self, sv: &SelectValue) -> Option<StringOwnership> {
        match sv {
            SelectValue::Expr(_, ownership) => Some(*ownership),
            SelectValue::Deferred(slot) => self.infer_select_ownership(*slot),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use vx_jit_types::{StrictBinOp, StrictExpr, StrictStmt, StrictType, StringOwnership};
    use vx_types::{Scalar, Ty};
    use vx_vir::exec_types::{
        BodyId, ExecutableBody, ExprId, KernelId, KernelKind, KernelOp, Operand, ResultSource,
        SlotId, SourceExpr,
    };

    use super::try_strict_lower;

    fn body_id(n: u32) -> BodyId {
        BodyId(n)
    }
    fn slot(n: u32) -> SlotId {
        SlotId(n)
    }
    fn src(body: u32, expr: u32) -> SourceExpr {
        SourceExpr {
            body: BodyId(body),
            expr: ExprId(expr),
        }
    }

    fn make_body(id: u32, params: Vec<u32>, ops: Vec<KernelOp>, result: u32) -> ExecutableBody {
        ExecutableBody {
            id: body_id(id),
            kernel: KernelId(0),
            kernel_kind: KernelKind::Interpreted,
            params: params.into_iter().map(slot).collect(),
            ops,
            result_source: ResultSource::Slot(slot(result)),
            runtime_boundary_extractions: vec![],
            local_call_dependencies: vec![],
        }
    }

    fn u64_type() -> Ty {
        Ty::Scalar(Scalar::U64)
    }
    fn bool_type() -> Ty {
        Ty::Scalar(Scalar::Bool)
    }
    fn str_type() -> Ty {
        Ty::Scalar(Scalar::String)
    }

    // --- IR constructor tests ---

    #[test]
    fn ir_add_u64() {
        let mut slot_types = HashMap::new();
        slot_types.insert((body_id(0), slot(0)), u64_type());
        slot_types.insert((body_id(0), slot(1)), u64_type());
        slot_types.insert((body_id(0), slot(2)), u64_type());

        let body = make_body(
            0,
            vec![0, 1],
            vec![KernelOp::Add {
                dst: slot(2),
                source: src(0, 0),
                lhs: Operand::Slot(slot(0)),
                rhs: Operand::Slot(slot(1)),
            }],
            2,
        );

        let func = try_strict_lower(&body, &slot_types, 1).expect("add should lower");
        assert_eq!(func.params.len(), 2);
        assert_eq!(func.return_type, StrictType::U64);
        assert!(matches!(
            &func.body[0],
            StrictStmt::Let {
                value: StrictExpr::Binary {
                    op: StrictBinOp::Add,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn ir_const_string_has_static_ownership() {
        let mut slot_types = HashMap::new();
        slot_types.insert((body_id(0), slot(0)), str_type());

        let body = make_body(
            0,
            vec![],
            vec![KernelOp::AssignConstString {
                dst: slot(0),
                source: src(0, 0),
                value: "hello".to_owned(),
            }],
            0,
        );

        let func = try_strict_lower(&body, &slot_types, 1).expect("const string should lower");
        assert_eq!(func.return_type, StrictType::String);
        assert_eq!(func.return_ownership, StringOwnership::Static);
    }

    #[test]
    fn ir_string_slice_has_borrowed_ownership() {
        let mut slot_types = HashMap::new();
        slot_types.insert((body_id(0), slot(0)), str_type());
        slot_types.insert((body_id(0), slot(1)), u64_type());
        slot_types.insert((body_id(0), slot(2)), u64_type());
        slot_types.insert((body_id(0), slot(3)), str_type());

        let body = make_body(
            0,
            vec![0, 1, 2],
            vec![KernelOp::StringSlice {
                dst: slot(3),
                source: src(0, 0),
                value: Operand::Slot(slot(0)),
                start: Operand::Slot(slot(1)),
                end: Operand::Slot(slot(2)),
            }],
            3,
        );

        let func = try_strict_lower(&body, &slot_types, 1).expect("slice should lower");
        assert_eq!(func.return_type, StrictType::String);
        assert_eq!(func.return_ownership, StringOwnership::Borrowed);
    }

    #[test]
    fn ir_concat_has_owned_ownership() {
        let mut slot_types = HashMap::new();
        slot_types.insert((body_id(0), slot(0)), str_type());
        slot_types.insert((body_id(0), slot(1)), str_type());
        slot_types.insert((body_id(0), slot(2)), str_type());

        let body = make_body(
            0,
            vec![0, 1],
            vec![KernelOp::ConcatStrings {
                dst: slot(2),
                source: src(0, 0),
                parts: vec![Operand::Slot(slot(0)), Operand::Slot(slot(1))],
            }],
            2,
        );

        let func = try_strict_lower(&body, &slot_types, 1).expect("concat should lower");
        assert_eq!(func.return_type, StrictType::String);
        assert_eq!(func.return_ownership, StringOwnership::Owned);
        assert!(matches!(
            &func.body[0],
            StrictStmt::Let {
                value: StrictExpr::ConcatStrings { .. },
                ..
            }
        ));
    }

    #[test]
    fn ir_select_expands_to_if_return() {
        let mut slot_types = HashMap::new();
        slot_types.insert((body_id(0), slot(0)), bool_type());
        slot_types.insert((body_id(0), slot(1)), u64_type());
        slot_types.insert((body_id(0), slot(2)), u64_type());
        slot_types.insert((body_id(0), slot(3)), u64_type());

        let body = make_body(
            0,
            vec![0],
            vec![KernelOp::Select {
                dst: slot(3),
                source: src(0, 0),
                condition: Operand::Slot(slot(0)),
                then_value: Operand::Int(1),
                else_value: Operand::Int(0),
            }],
            3,
        );

        let func = try_strict_lower(&body, &slot_types, 1).expect("select should lower");
        assert_eq!(func.return_type, StrictType::U64);
        // Select must be expanded into If { Return, Return } — no Select node in the IR.
        assert!(
            matches!(&func.body[0], StrictStmt::If { .. }),
            "select must expand to If, got {:?}",
            func.body
        );
    }

    #[test]
    fn synthesized_body_rejected() {
        let slot_types = HashMap::new();
        let body = make_body(5, vec![], vec![], 0);
        // original_body_count = 5, body id = 5, so it should be rejected
        let result = try_strict_lower(&body, &slot_types, 5);
        assert!(result.is_none(), "synthesized body must be rejected");
    }

    #[test]
    fn unsupported_op_rejects_body() {
        let mut slot_types = HashMap::new();
        slot_types.insert((body_id(0), slot(0)), u64_type());

        let body = make_body(
            0,
            vec![],
            vec![KernelOp::MakeArray {
                dst: slot(0),
                source: src(0, 0),
                items: vec![],
            }],
            0,
        );

        let result = try_strict_lower(&body, &slot_types, 1);
        assert!(result.is_none(), "MakeArray must reject strict lowering");
    }

    // --- VIR compilation → strict lowering integration ---

    #[test]
    fn lower_add_from_vir() {
        let source = "fn add(lhs: U64, rhs: U64) -> U64 ( lhs + rhs )";
        let output = vx_vir::compile::compile_source_with_types(source, "add")
            .expect("VIR compilation should succeed");

        let entry = output.program.entry;
        let body = output
            .program
            .bodies
            .get(&entry)
            .expect("entry body must exist");
        let func = try_strict_lower(body, &output.slot_types, output.original_body_count)
            .expect("add should strict-lower");

        assert_eq!(func.params.len(), 2);
        assert_eq!(func.return_type, StrictType::U64);
    }

    #[test]
    fn lower_str_len_from_vir() {
        let source = "fn str_len(s: String) -> U64 ( s.len() )";
        let output = vx_vir::compile::compile_source_with_types(source, "str_len")
            .expect("VIR compilation should succeed");

        let entry = output.program.entry;
        let body = output
            .program
            .bodies
            .get(&entry)
            .expect("entry body must exist");
        let func = try_strict_lower(body, &output.slot_types, output.original_body_count)
            .expect("str_len should strict-lower");

        assert_eq!(func.params.len(), 1);
        assert_eq!(func.return_type, StrictType::U64);
    }

    #[test]
    fn lower_str_concat_from_vir() {
        let source = "fn join(a: String, b: String) -> String ( `{a}{b}` )";
        let output = vx_vir::compile::compile_source_with_types(source, "join")
            .expect("VIR compilation should succeed");

        let entry = output.program.entry;
        let body = output
            .program
            .bodies
            .get(&entry)
            .expect("entry body must exist");
        let func = try_strict_lower(body, &output.slot_types, output.original_body_count)
            .expect("join should strict-lower");

        assert_eq!(func.return_type, StrictType::String);
        assert_eq!(func.return_ownership, StringOwnership::Owned);
    }

    #[test]
    fn lower_if_from_vir() {
        let source = "fn pick(flag: U64) -> String ( if flag == 1 ( \"yes\" ) else ( \"no\" ) )";
        let output = vx_vir::compile::compile_source_with_types(source, "pick")
            .expect("VIR compilation should succeed");

        let entry = output.program.entry;
        let body = output
            .program
            .bodies
            .get(&entry)
            .expect("entry body must exist");
        let func = try_strict_lower(body, &output.slot_types, output.original_body_count)
            .expect("pick should strict-lower");

        assert_eq!(func.return_type, StrictType::String);
        // Both branches return Static strings, so return_ownership should be Static.
        assert_eq!(func.return_ownership, StringOwnership::Static);
    }
}
