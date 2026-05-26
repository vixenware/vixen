//! Binary, prefix, and pipe operator inference.

use vx_types::{Scalar, Ty};

use crate::diagnostic::IrDiagnostic;
use crate::expr::{BinOp, BinaryExpr, PrefixExpr, PrefixOp};

use super::TypeChecker;

pub(crate) fn infer_binary(tc: &mut TypeChecker, bin: &BinaryExpr) -> Ty {
    if bin.op == BinOp::Pipe {
        return infer_pipe(tc, bin);
    }

    let lhs_ty = tc.infer_expr(bin.lhs);
    let rhs_ty = tc.infer_expr(bin.rhs);

    match bin.op {
        // Comparison/equality → Bool.
        BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
            Ty::Scalar(Scalar::Bool)
        }
        // Logical → Bool.
        BinOp::And | BinOp::Or => Ty::Scalar(Scalar::Bool),
        // Concat (++).
        BinOp::Concat => {
            if lhs_ty.is_poison() || rhs_ty.is_poison() {
                Ty::Poison
            } else if let (Ty::Array(lhs_inner), Ty::Array(rhs_inner)) = (&lhs_ty, &rhs_ty) {
                if tc.types_compatible(lhs_inner, rhs_inner) {
                    Ty::Array(lhs_inner.clone())
                } else {
                    tc.push_diagnostic(IrDiagnostic::BinaryOpMismatch {
                        op: bin.op_span,
                        op_kind: bin.op,
                        lhs: tc.arena[bin.lhs].span,
                        rhs: tc.arena[bin.rhs].span,
                        lhs_ty,
                        rhs_ty,
                    });
                    Ty::Poison
                }
            } else {
                tc.push_diagnostic(IrDiagnostic::BinaryOpMismatch {
                    op: bin.op_span,
                    op_kind: bin.op,
                    lhs: tc.arena[bin.lhs].span,
                    rhs: tc.arena[bin.rhs].span,
                    lhs_ty,
                    rhs_ty,
                });
                Ty::Poison
            }
        }
        // Arithmetic: result type matches lhs (or rhs if lhs is Poison).
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
            if !lhs_ty.is_poison() {
                lhs_ty
            } else {
                rhs_ty
            }
        }
        BinOp::Pipe => unreachable!("handled above"),
    }
}

fn infer_pipe(tc: &mut TypeChecker, bin: &BinaryExpr) -> Ty {
    let lhs_ty = tc.infer_expr(bin.lhs);

    // TODO: pipe placeholder (`_`) support requires implicit bindings.
    // For now, just infer the rhs and check if it's a function.

    let rhs_ty = tc.infer_expr(bin.rhs);

    match rhs_ty {
        Ty::Function(ref function) if !function.params.is_empty() => {
            let first_param = &function.params[0];
            if !lhs_ty.is_poison()
                && !first_param.is_poison()
                && !tc.types_compatible(&lhs_ty, first_param)
            {
                // The pipe target's parameter is anonymous in source —
                // surface the operator token as the secondary "parameter"
                // span so the diagnostic can point at where the pipe
                // expects its input.
                tc.push_diagnostic(IrDiagnostic::ArgTypeMismatch {
                    arg: tc.arena[bin.lhs].span,
                    param: bin.op_span,
                    expected: first_param.clone(),
                    found: lhs_ty,
                });
            }
            (*function.ret).clone()
        }
        _ => Ty::Poison,
    }
}

pub(crate) fn infer_prefix(tc: &mut TypeChecker, pre: &PrefixExpr) -> Ty {
    let operand_ty = tc.infer_expr(pre.operand);

    match pre.op {
        PrefixOp::Not => Ty::Scalar(Scalar::Bool),
        PrefixOp::Neg => operand_ty,
    }
}
