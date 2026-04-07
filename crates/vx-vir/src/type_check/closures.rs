//! Closure inference and callback parameter binding.

use vx_types::Ty;

use crate::expr::ClosureExpr;
use crate::pattern::PatternKind;

use super::TypeChecker;

pub(crate) fn infer_closure(tc: &mut TypeChecker, closure: &ClosureExpr) -> Ty {
    let body = tc.arena[closure.body].clone();

    // Collect param types: use Binding.ty if already populated (by bind_closure_params),
    // otherwise Poison.
    let params: Vec<Ty> = body
        .params
        .iter()
        .map(|param| {
            let pat = &tc.arena[param.pattern];
            match &pat.kind {
                PatternKind::Binding(bp) => tc.arena[bp.binding].ty.clone().unwrap_or(Ty::Poison),
                _ => Ty::Poison,
            }
        })
        .collect();

    let ret = tc.infer_expr(body.root_expr);

    Ty::Function(vx_types::FunctionTy {
        params,
        ret: Box::new(ret),
        capabilities: vec![],
    })
}

/// Bind closure parameter types from a callback slot's function type.
/// Writes directly to `Binding.ty` on the arena.
pub(crate) fn bind_closure_params(
    tc: &mut TypeChecker,
    closure_body_id: crate::BodyId,
    param_types: &[Ty],
    slot_doc: Option<&str>,
    slot_name: &str,
) {
    let body = tc.arena[closure_body_id].clone();
    let inherited_doc = slot_doc.map(|doc| format!("`{slot_name}` callback slot.\n\n{doc}"));

    for (param, ty) in body.params.iter().zip(param_types.iter()) {
        let pat_kind = tc.arena[param.pattern].kind.clone();
        if let PatternKind::Binding(bp) = &pat_kind {
            tc.arena[bp.binding].ty = Some(ty.clone());
            if let Some(ref doc) = inherited_doc {
                tc.closure_param_docs.insert(bp.binding, doc.clone());
            }
        }
    }
}
