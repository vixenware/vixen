//! Call, where, invoke, and partial application inference.

use vx_types::{FnSig, ParamDef, Ty};

use crate::ExprId;
use crate::expr::{CallExpr, ExprKind, InvokeExpr, PartialAppExpr, WhereExpr};
use crate::item::ItemKind;
use crate::scope::DefTarget;

use super::closures::bind_closure_params;
use super::fields::{instantiate_method_sig, resolve_method_for_call};
use super::names::{sig_for_name_ref_target, variant_sig};
use super::subst::{infer_type_params, substitute_bound_type_params};
use super::{TypeChecker, TypeError, TypeErrorKind};

pub(crate) fn infer_call(tc: &mut TypeChecker, expr_id: ExprId, call: &CallExpr) -> Ty {
    let callee_kind = tc.arena[call.callee].kind.clone();

    match callee_kind {
        ExprKind::NameRef(ref nr) => {
            if let Some(target) = nr.resolved {
                if let Some(sig) = call_sig_for_target(tc, target) {
                    // Type the callee itself as a function type.
                    let callee_ty = tc.function_ty_from_sig(&sig);
                    tc.arena[call.callee].ty = Some(callee_ty);

                    let sig = instantiate_call_sig(tc, call, sig);
                    populate_callback_param_types(tc, call, &sig);
                    check_call_args(tc, call, &sig);
                    infer_extra_call_args(tc, call, sig.positional_params.len());
                    return sig.ret.clone();
                }
            }
            // Fallback: infer the callee normally.
            tc.infer_expr(call.callee);
            infer_call_arg_exprs(tc, call);
            Ty::Poison
        }
        ExprKind::Field(ref field) => {
            // Static namespace call: Namespace.method(args)
            if let Some(sig) = infer_static_namespace_call_sig(tc, call, field) {
                // Type the callee field expression.
                let callee_ty = tc.function_ty_from_sig(&sig);
                tc.arena[call.callee].ty = Some(callee_ty.clone());
                // Also type the receiver (namespace) if it's a NameRef.
                tc.infer_expr(field.receiver);

                check_call_args(tc, call, &sig);
                infer_extra_call_args(tc, call, sig.positional_params.len());
                return sig.ret;
            }

            // Method call: receiver.method(args)
            let receiver_ty = tc.infer_expr(field.receiver);
            let method_name = match &field.field {
                crate::expr::FieldKey::Name(n) => n.clone(),
                _ => String::new(),
            };

            if let Some(method_sig) =
                resolve_method_for_call(tc, &receiver_ty, &method_name, expr_id)
            {
                // Type the callee field expression as the method's function type.
                let method_ty = Ty::Function(vx_types::FunctionTy {
                    params: method_sig.params.iter().map(|p| p.ty.clone()).collect(),
                    ret: Box::new(method_sig.ret.clone()),
                    capabilities: method_sig.capabilities.clone(),
                });
                tc.arena[call.callee].ty = Some(method_ty);

                check_method_args(tc, call, &method_sig.params, &method_name);
                infer_extra_call_args(tc, call, method_sig.params.len());
                return method_sig.ret.clone();
            }

            // Type the callee as Poison if method not found.
            tc.arena[call.callee].ty = Some(Ty::Poison);

            if receiver_ty != Ty::Poison {
                tc.push_error(TypeError {
                    span: tc.arena[call.callee].span,
                    kind: TypeErrorKind::NoSuchMethod {
                        receiver_ty: receiver_ty.clone(),
                        method_name,
                    },
                });
            }

            infer_call_arg_exprs(tc, call);
            Ty::Poison
        }
        _ => {
            let callee_ty = tc.infer_expr(call.callee);
            infer_call_arg_exprs(tc, call);
            match callee_ty {
                Ty::Function(fn_ty) => (*fn_ty.ret).clone(),
                _ => Ty::Poison,
            }
        }
    }
}

fn call_sig_for_target(tc: &mut TypeChecker, target: DefTarget) -> Option<FnSig> {
    match target {
        DefTarget::Item(item_id) => {
            let kind = &tc.arena[item_id].kind;
            match kind {
                ItemKind::Function(f) => tc.types.def_to_fn_sig.get(&f.name_span).cloned(),
                ItemKind::Namespace(ns) => {
                    // Namespace with constructor functions — collect all sigs.
                    let sigs: Vec<FnSig> = ns
                        .items
                        .iter()
                        .filter_map(|&child_id| {
                            if let ItemKind::Function(f) = &tc.arena[child_id].kind {
                                tc.types.def_to_fn_sig.get(&f.name_span).cloned()
                            } else {
                                None
                            }
                        })
                        .collect();
                    (sigs.len() == 1).then(|| sigs.into_iter().next().unwrap())
                }
                _ => None,
            }
        }
        DefTarget::Variant {
            enum_item,
            variant_index,
        } => variant_sig(tc, enum_item, variant_index),
        DefTarget::Binding(_) => None,
    }
}

fn instantiate_call_sig(tc: &mut TypeChecker, call: &CallExpr, mut sig: FnSig) -> FnSig {
    if sig.type_params.is_empty() {
        return sig;
    }

    let mut substitutions = vec![None; sig.type_params.len()];

    // Explicit type args.
    for (slot, &tr_id) in substitutions.iter_mut().zip(call.type_args.iter()) {
        let resolved = tc.resolve_type_ref_id(tr_id);
        *slot = Some(resolved);
    }

    // Infer from positional args.
    for (arg, param) in call.args.iter().zip(sig.positional_params.iter()) {
        // Skip closures — they need callback binding first.
        let arg_kind = &tc.arena[arg.expr].kind;
        if matches!(param.ty, Ty::Function(_)) && matches!(arg_kind, ExprKind::Closure(_)) {
            continue;
        }
        let arg_ty = tc.infer_expr(arg.expr);
        infer_type_params(&param.ty, &arg_ty, &mut substitutions);
    }

    for param in &mut sig.positional_params {
        param.ty = substitute_bound_type_params(&param.ty, &substitutions);
    }
    for param in &mut sig.keyword_params {
        param.ty = substitute_bound_type_params(&param.ty, &substitutions);
    }
    sig.ret = substitute_bound_type_params(&sig.ret, &substitutions);
    sig
}

fn populate_callback_param_types(tc: &mut TypeChecker, call: &CallExpr, sig: &FnSig) {
    // Positional args.
    for (arg, param) in call.args.iter().zip(sig.positional_params.iter()) {
        if let Ty::Function(ref fn_ty) = param.ty {
            let arg_kind = tc.arena[arg.expr].kind.clone();
            if let ExprKind::Closure(ref closure) = arg_kind {
                bind_closure_params(
                    tc,
                    closure.body,
                    &fn_ty.params,
                    param.doc.as_deref(),
                    &param.name,
                );
            }
        }
    }
}

fn check_call_args(tc: &mut TypeChecker, call: &CallExpr, sig: &FnSig) {
    for (arg, param) in call.args.iter().zip(sig.positional_params.iter()) {
        let arg_ty = tc.infer_expr(arg.expr);
        if !arg_ty.is_poison() && !param.ty.is_poison() && !tc.types_compatible(&arg_ty, &param.ty)
        {
            tc.push_error(TypeError {
                span: tc.arena[arg.expr].span,
                kind: TypeErrorKind::ArgTypeMismatch {
                    param_name: param.name.clone(),
                    expected: param.ty.clone(),
                    found: arg_ty,
                },
            });
        }
    }
}

fn check_method_args(
    tc: &mut TypeChecker,
    call: &CallExpr,
    params: &[ParamDef],
    _method_name: &str,
) {
    for (arg, param) in call.args.iter().zip(params.iter()) {
        let arg_ty = tc.infer_expr(arg.expr);
        if !arg_ty.is_poison() && !param.ty.is_poison() && !tc.types_compatible(&arg_ty, &param.ty)
        {
            tc.push_error(TypeError {
                span: tc.arena[arg.expr].span,
                kind: TypeErrorKind::ArgTypeMismatch {
                    param_name: param.name.clone(),
                    expected: param.ty.clone(),
                    found: arg_ty,
                },
            });
        }
    }
}

fn infer_extra_call_args(tc: &mut TypeChecker, call: &CallExpr, checked_positional: usize) {
    for arg in call.args.iter().skip(checked_positional) {
        tc.infer_expr(arg.expr);
    }
}

fn infer_call_arg_exprs(tc: &mut TypeChecker, call: &CallExpr) {
    for arg in &call.args {
        tc.infer_expr(arg.expr);
    }
}

fn infer_static_namespace_call_sig(
    tc: &mut TypeChecker,
    call: &CallExpr,
    field: &crate::expr::FieldExpr,
) -> Option<FnSig> {
    let crate::expr::FieldKey::Name(ref field_name) = field.field else {
        return None;
    };

    let receiver_kind = tc.arena[field.receiver].kind.clone();
    let ExprKind::NameRef(ref nr) = receiver_kind else {
        return None;
    };
    let Some(DefTarget::Item(ns_item_id)) = nr.resolved else {
        return None;
    };
    let ItemKind::Namespace(ref ns) = tc.arena[ns_item_id].kind else {
        return None;
    };

    for &child_id in &ns.items {
        if let ItemKind::Function(ref f) = tc.arena[child_id].kind {
            if f.name == *field_name {
                if let Some(sig) = tc.types.def_to_fn_sig.get(&f.name_span) {
                    return Some(instantiate_call_sig(tc, call, sig.clone()));
                }
            }
        }
    }

    None
}

pub(crate) fn infer_where(tc: &mut TypeChecker, w: &WhereExpr) -> Ty {
    let callee_ty = tc.infer_expr(w.callee);
    tc.infer_expr(w.keywords);
    callee_ty
}

pub(crate) fn infer_invoke(tc: &mut TypeChecker, inv: &InvokeExpr) -> Ty {
    let callee_kind = tc.arena[inv.callee].kind.clone();

    match callee_kind {
        ExprKind::Field(ref field) => {
            let receiver_ty = tc.infer_expr(field.receiver);
            let method_name = match &field.field {
                crate::expr::FieldKey::Name(n) => n.clone(),
                _ => return Ty::Poison,
            };

            // Try method resolution with no args.
            let namespace_name =
                super::fields::namespace_name_for_receiver_pub(&receiver_ty, &tc.types.env);
            if let Some(ref ns_name) = namespace_name {
                // Find method and check it's zero-arg.
                if let Some(sig) = find_namespace_method_sig(tc, ns_name, &method_name) {
                    if let Some(method_sig) = instantiate_method_sig(tc, &receiver_ty, &sig, &[]) {
                        if method_sig.params.is_empty() {
                            return method_sig.ret;
                        }
                    }
                }
            }

            Ty::Poison
        }
        _ => {
            let callee_ty = tc.infer_expr(inv.callee);
            match callee_ty {
                Ty::Function(fn_ty) if fn_ty.params.is_empty() => (*fn_ty.ret).clone(),
                _ => Ty::Poison,
            }
        }
    }
}

pub(crate) fn infer_partial_app(tc: &mut TypeChecker, pa: &PartialAppExpr) -> Ty {
    let callee_kind = tc.arena[pa.callee].kind.clone();

    if let ExprKind::NameRef(ref nr) = callee_kind {
        if let Some(target) = nr.resolved {
            if let Some(sig) = sig_for_name_ref_target(tc, target) {
                return tc.function_ty_from_sig(&sig);
            }
        }
    }

    tc.infer_expr(pa.callee)
}

fn find_namespace_method_sig(tc: &TypeChecker, ns_name: &str, method_name: &str) -> Option<FnSig> {
    for (i, item) in tc.arena.items.iter().enumerate() {
        if let ItemKind::Namespace(ref ns) = item.kind {
            if tc.arena.item_name(crate::ItemId::from_index(i)) == ns_name {
                for &child_id in &ns.items {
                    if let ItemKind::Function(ref f) = tc.arena[child_id].kind {
                        if f.name == method_name {
                            return tc.types.def_to_fn_sig.get(&f.name_span).cloned();
                        }
                    }
                }
            }
        }
    }
    None
}
