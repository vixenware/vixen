//! Literal and interpolated string inference.

use vx_types::{Scalar, TypeId};

use crate::expr::{InterpolatedStringExpr, LitKind, LiteralExpr, StringPart};

use super::TypeChecker;

pub(crate) fn infer_literal(tc: &mut TypeChecker, lit: &LiteralExpr) -> TypeId {
    let s = match &lit.value {
        LitKind::Int { .. } => Scalar::U64,
        LitKind::Float { .. } => Scalar::F64,
        LitKind::String { .. } => Scalar::String,
        LitKind::Bool(_) => Scalar::Bool,
    };
    tc.types.env.intern_scalar(s)
}

pub(crate) fn infer_interpolated_string(
    tc: &mut TypeChecker,
    is: &InterpolatedStringExpr,
) -> TypeId {
    for part in &is.parts {
        if let StringPart::Expr(id) = part {
            tc.infer_expr(*id);
        }
    }
    tc.types.env.intern_scalar(Scalar::String)
}
