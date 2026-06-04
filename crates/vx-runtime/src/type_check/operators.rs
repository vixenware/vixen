//! Binary, prefix, and pipe operator inference.

use vx_types::{Scalar, Ty, TypeId};

use vx_ir::diagnostic::IrDiagnostic;
use vx_ir::expr::{BinOp, BinaryExpr, PrefixExpr, PrefixOp};

use super::TypeChecker;

pub(crate) fn infer_binary(tc: &mut TypeChecker, bin: &BinaryExpr) -> TypeId {
    if bin.op == BinOp::Pipe {
        return infer_pipe(tc, bin);
    }

    let lhs_ty = tc.infer_expr(bin.lhs);
    let rhs_ty = tc.infer_expr(bin.rhs);

    match bin.op {
        // Comparison/equality → Bool.
        BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
            tc.types.env.intern_scalar(Scalar::Bool)
        }
        // Logical → Bool.
        BinOp::And | BinOp::Or => tc.types.env.intern_scalar(Scalar::Bool),
        // Concat (++).
        BinOp::Concat => {
            let lhs_is_poison = tc.types.env.get_ty(lhs_ty).is_poison();
            let rhs_is_poison = tc.types.env.get_ty(rhs_ty).is_poison();
            if lhs_is_poison || rhs_is_poison {
                tc.poison()
            } else if let (Ty::Array(lhs_arr), Ty::Array(rhs_arr)) = (
                tc.types.env.get_ty(lhs_ty).clone(),
                tc.types.env.get_ty(rhs_ty).clone(),
            ) {
                if tc.types_compatible(lhs_arr.0, rhs_arr.0) {
                    tc.types.env.intern_array(lhs_arr.0)
                } else {
                    let lhs_span = tc.arena[bin.lhs].span;
                    let rhs_span = tc.arena[bin.rhs].span;
                    tc.push_diagnostic(IrDiagnostic::BinaryOpMismatch {
                        op: bin.op_span,
                        op_kind: bin.op,
                        lhs: lhs_span,
                        rhs: rhs_span,
                        lhs_ty,
                        rhs_ty,
                    });
                    tc.poison()
                }
            } else {
                let lhs_span = tc.arena[bin.lhs].span;
                let rhs_span = tc.arena[bin.rhs].span;
                tc.push_diagnostic(IrDiagnostic::BinaryOpMismatch {
                    op: bin.op_span,
                    op_kind: bin.op,
                    lhs: lhs_span,
                    rhs: rhs_span,
                    lhs_ty,
                    rhs_ty,
                });
                tc.poison()
            }
        }
        // Arithmetic: result type matches lhs (or rhs if lhs is Poison).
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
            if !tc.types.env.get_ty(lhs_ty).is_poison() {
                lhs_ty
            } else {
                rhs_ty
            }
        }
        BinOp::Pipe => unreachable!("handled above"),
    }
}

fn infer_pipe(tc: &mut TypeChecker, bin: &BinaryExpr) -> TypeId {
    let lhs_ty = tc.infer_expr(bin.lhs);

    // TODO: pipe placeholder (`_`) support requires implicit bindings.
    // For now, just infer the rhs and check if it's a function.

    let rhs_ty = tc.infer_expr(bin.rhs);

    let rhs_ty_clone = tc.types.env.get_ty(rhs_ty).clone();
    match rhs_ty_clone {
        Ty::Function(ref function) if !function.params.is_empty() => {
            let first_param = function.params[0];
            let ret = function.ret;
            let lhs_is_poison = tc.types.env.get_ty(lhs_ty).is_poison();
            let first_is_poison = tc.types.env.get_ty(first_param).is_poison();
            if !lhs_is_poison && !first_is_poison && !tc.types_compatible(lhs_ty, first_param) {
                // The pipe target's parameter is anonymous in source —
                // surface the operator token as the secondary "parameter"
                // span so the diagnostic can point at where the pipe
                // expects its input.
                let lhs_span = tc.arena[bin.lhs].span;
                tc.push_diagnostic(IrDiagnostic::ArgTypeMismatch {
                    arg: lhs_span,
                    param: bin.op_span,
                    expected: first_param,
                    found: lhs_ty,
                });
            }
            ret
        }
        _ => tc.poison(),
    }
}

pub(crate) fn infer_prefix(tc: &mut TypeChecker, pre: &PrefixExpr) -> TypeId {
    let operand_ty = tc.infer_expr(pre.operand);

    match pre.op {
        PrefixOp::Not => tc.types.env.intern_scalar(Scalar::Bool),
        PrefixOp::Neg => operand_ty,
    }
}
