//! Literal, path literal, and interpolated string inference.

use vx_types::{Scalar, Ty};

use crate::expr::{InterpolatedStringExpr, LitKind, LiteralExpr, StringPart};

use super::TypeChecker;

pub(crate) fn infer_literal(lit: &LiteralExpr) -> Ty {
    match &lit.value {
        LitKind::Int { .. } => Ty::Scalar(Scalar::U64),
        LitKind::Float { .. } => Ty::Scalar(Scalar::F64),
        LitKind::String { .. } => Ty::Scalar(Scalar::String),
        LitKind::Bool(_) => Ty::Scalar(Scalar::Bool),
    }
}

pub(crate) fn infer_path_literal(tc: &mut TypeChecker) -> Ty {
    tc.nominal_ty("Path")
}

pub(crate) fn infer_interpolated_string(tc: &mut TypeChecker, is: &InterpolatedStringExpr) -> Ty {
    for part in &is.parts {
        if let StringPart::Expr(id) = part {
            tc.infer_expr(*id);
        }
    }
    Ty::Scalar(Scalar::String)
}
