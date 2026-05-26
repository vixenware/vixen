//! Call, where, invoke, and partial application inference.

use vx_types::{FnSig, KeywordParamDef, ParamDef, Ty, TypeId};

use crate::ExprId;
use crate::expr::{CallExpr, ExprKind, InvokeExpr, PartialAppExpr, RecordField, WhereExpr};
use crate::item::ItemKind;
use crate::scope::DefTarget;

use super::TypeChecker;
use super::closures::bind_closure_params;
use super::fields::{instantiate_method_sig, resolve_method_for_call};
use super::names::{sig_for_name_ref_target, variant_sig};
use super::subst::{infer_type_params, substitute_bound_type_params};
use crate::diagnostic::IrDiagnostic;

pub(crate) fn infer_call(tc: &mut TypeChecker, expr_id: ExprId, call: &CallExpr) -> TypeId {
    let callee_kind = tc.arena[call.callee].kind.clone();

    match callee_kind {
        ExprKind::NameRef(ref nr) => {
            if let Some(target) = nr.resolved
                && let Some(sig) = call_sig_for_target(tc, target)
            {
                // Type the callee itself as a function type.
                let callee_ty = tc.function_ty_from_sig(&sig);
                tc.arena[call.callee].ty = Some(callee_ty);

                let sig = instantiate_call_sig(tc, call, sig);
                populate_callback_param_types(tc, call, &sig);
                check_call_args(tc, call, &sig);
                infer_extra_call_args(tc, call, sig.positional_params.len());
                return sig.ret;
            }
            // Fallback: infer the callee normally.
            tc.infer_expr(call.callee);
            infer_call_arg_exprs(tc, call);
            tc.poison()
        }
        ExprKind::Field(ref field) => {
            // Static namespace call: Namespace.method(args)
            if let Some(sig) = infer_static_namespace_call_sig(tc, call, field) {
                // Type the callee field expression.
                let callee_ty = tc.function_ty_from_sig(&sig);
                tc.arena[call.callee].ty = Some(callee_ty);
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
                resolve_method_for_call(tc, receiver_ty, &method_name, expr_id)
            {
                // Type the callee field expression as the method's function type.
                let params: Vec<TypeId> = method_sig.params.iter().map(|p| p.ty).collect();
                let method_ty = tc.types.env.intern_function(
                    params,
                    method_sig.ret,
                    method_sig.capabilities.clone(),
                );
                tc.arena[call.callee].ty = Some(method_ty);

                check_method_args(tc, call, &method_sig.params, &method_name);
                infer_extra_call_args(tc, call, method_sig.params.len());
                return method_sig.ret;
            }

            // Type the callee as Poison if method not found.
            let poison = tc.poison();
            tc.arena[call.callee].ty = Some(poison);

            if !tc.types.env.get_ty(receiver_ty).is_poison() {
                let receiver_span = tc.arena[field.receiver].span;
                tc.push_diagnostic(IrDiagnostic::NoSuchMethod {
                    method_call: field.field_span,
                    receiver: receiver_span,
                    receiver_ty,
                    // Candidate enumeration deferred — the diagnostic still
                    // carries the structural slot for it.
                    candidates: Vec::new(),
                });
            }
            // method_name kept above for downstream poison propagation reasoning;
            // unused-by-diagnostic.
            let _ = method_name;

            infer_call_arg_exprs(tc, call);
            tc.poison()
        }
        _ => {
            let callee_ty = tc.infer_expr(call.callee);
            infer_call_arg_exprs(tc, call);
            let callee_ty_clone = tc.types.env.get_ty(callee_ty).clone();
            match callee_ty_clone {
                Ty::Function(fn_ty) => fn_ty.ret,
                _ => tc.poison(),
            }
        }
    }
}

fn call_sig_for_target(tc: &mut TypeChecker, target: DefTarget) -> Option<FnSig> {
    match target {
        DefTarget::Item(item_id) => {
            let kind = tc.arena[item_id].kind.clone();
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

    let mut substitutions: Vec<Option<TypeId>> = vec![None; sig.type_params.len()];

    // Explicit type args.
    let type_arg_refs: Vec<crate::TypeRefId> = call.type_args.clone();
    for (slot, tr_id) in substitutions.iter_mut().zip(type_arg_refs) {
        let resolved = tc.resolve_type_ref_id(tr_id);
        *slot = Some(resolved);
    }

    // Infer from positional args.
    for (arg, param) in call.args.iter().zip(sig.positional_params.iter()) {
        // Skip closures — they need callback binding first.
        let arg_kind = tc.arena[arg.expr].kind.clone();
        let param_ty = tc.types.env.get_ty(param.ty).clone();
        if matches!(param_ty, Ty::Function(_)) && matches!(arg_kind, ExprKind::Closure(_)) {
            continue;
        }
        let arg_ty = tc.infer_expr(arg.expr);
        infer_type_params(param.ty, arg_ty, &mut substitutions, &tc.types.env);
    }

    for param in &mut sig.positional_params {
        param.ty = substitute_bound_type_params(param.ty, &substitutions, &mut tc.types.env);
    }
    for param in &mut sig.keyword_params {
        param.ty = substitute_bound_type_params(param.ty, &substitutions, &mut tc.types.env);
    }
    sig.ret = substitute_bound_type_params(sig.ret, &substitutions, &mut tc.types.env);
    sig
}

fn populate_callback_param_types(tc: &mut TypeChecker, call: &CallExpr, sig: &FnSig) {
    // Positional args.
    for (arg, param) in call.args.iter().zip(sig.positional_params.iter()) {
        let param_ty = tc.types.env.get_ty(param.ty).clone();
        if let Ty::Function(fn_ty) = param_ty {
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
    let args = call.args.clone();
    let params = sig.positional_params.clone();
    for (arg, param) in args.iter().zip(params.iter()) {
        let arg_ty = tc.infer_expr(arg.expr);
        let arg_is_poison = tc.types.env.get_ty(arg_ty).is_poison();
        let param_is_poison = tc.types.env.get_ty(param.ty).is_poison();
        if !arg_is_poison && !param_is_poison && !tc.types_compatible(arg_ty, param.ty) {
            let arg_span = tc.arena[arg.expr].span;
            tc.push_diagnostic(IrDiagnostic::ArgTypeMismatch {
                arg: arg_span,
                param: param.name_span,
                expected: param.ty,
                found: arg_ty,
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
    let args = call.args.clone();
    let params: Vec<ParamDef> = params.to_vec();
    for (arg, param) in args.iter().zip(params.iter()) {
        let arg_ty = tc.infer_expr(arg.expr);
        let arg_is_poison = tc.types.env.get_ty(arg_ty).is_poison();
        let param_is_poison = tc.types.env.get_ty(param.ty).is_poison();
        if !arg_is_poison && !param_is_poison && !tc.types_compatible(arg_ty, param.ty) {
            let arg_span = tc.arena[arg.expr].span;
            tc.push_diagnostic(IrDiagnostic::ArgTypeMismatch {
                arg: arg_span,
                param: param.name_span,
                expected: param.ty,
                found: arg_ty,
            });
        }
    }
}

fn infer_extra_call_args(tc: &mut TypeChecker, call: &CallExpr, checked_positional: usize) {
    let arg_ids: Vec<ExprId> = call
        .args
        .iter()
        .skip(checked_positional)
        .map(|a| a.expr)
        .collect();
    for id in arg_ids {
        tc.infer_expr(id);
    }
}

fn infer_call_arg_exprs(tc: &mut TypeChecker, call: &CallExpr) {
    let arg_ids: Vec<ExprId> = call.args.iter().map(|a| a.expr).collect();
    for id in arg_ids {
        tc.infer_expr(id);
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

    let child_ids: Vec<crate::ItemId> = ns.items.clone();
    for child_id in child_ids {
        if let ItemKind::Function(ref f) = tc.arena[child_id].kind
            && f.name == *field_name
        {
            let name_span = f.name_span;
            if let Some(sig) = tc.types.def_to_fn_sig.get(&name_span).cloned() {
                return Some(instantiate_call_sig(tc, call, sig));
            }
        }
    }

    None
}

pub(crate) fn infer_where(tc: &mut TypeChecker, w: &WhereExpr) -> TypeId {
    // A `where` either decorates a Call (`exec(...) where {...}`) or a
    // PartialApp (`partial exec where {...}` / `partial exec where common`).
    // In both cases we need to inspect the underlying function's signature so
    // we can bind closure parameter types from callback slots and feed the
    // closure body's return type back into type-parameter substitution.

    let callee_kind = tc.arena[w.callee].kind.clone();
    let (sig, kind) = match &callee_kind {
        ExprKind::Call(call) => match underlying_call_sig(tc, call) {
            Some(sig) => (sig, WhereCalleeKind::Call(call.clone())),
            None => {
                tc.infer_expr(w.callee);
                tc.infer_expr(w.keywords);
                return tc.arena[w.callee].ty.unwrap_or_else(|| tc.poison());
            }
        },
        ExprKind::PartialApp(pa) => match underlying_partial_sig(tc, pa) {
            Some(sig) => (sig, WhereCalleeKind::Partial(pa.clone())),
            None => {
                tc.infer_expr(w.callee);
                tc.infer_expr(w.keywords);
                return tc.arena[w.callee].ty.unwrap_or_else(|| tc.poison());
            }
        },
        // Bare NameRef: `f where {...}` returns a partially-applied function.
        // This shape shows up as the inner expression of `partial f where {...}`
        // since the parser parses that as `PartialApp(Where(NameRef, ...))`.
        ExprKind::NameRef(nr) => match nr.resolved.and_then(|t| sig_for_name_ref_target(tc, t)) {
            Some(sig) => (sig, WhereCalleeKind::NameRef),
            None => {
                let callee_ty = tc.infer_expr(w.callee);
                tc.infer_expr(w.keywords);
                return callee_ty;
            }
        },
        _ => {
            let callee_ty = tc.infer_expr(w.callee);
            tc.infer_expr(w.keywords);
            return callee_ty;
        }
    };

    let mut substitutions: Vec<Option<TypeId>> = vec![None; sig.type_params.len()];

    // Seed substitutions from positional args, where present.
    if let WhereCalleeKind::Call(ref call) = kind {
        // Explicit type args.
        let type_arg_refs: Vec<crate::TypeRefId> = call.type_args.clone();
        for (slot, tr_id) in substitutions.iter_mut().zip(type_arg_refs) {
            let resolved = tc.resolve_type_ref_id(tr_id);
            *slot = Some(resolved);
        }
        for (arg, param) in call.args.iter().zip(sig.positional_params.iter()) {
            let arg_kind = tc.arena[arg.expr].kind.clone();
            let param_ty = tc.types.env.get_ty(param.ty).clone();
            // Skip closures here — they need callback binding from the param type first.
            if matches!(param_ty, Ty::Function(_)) && matches!(arg_kind, ExprKind::Closure(_)) {
                continue;
            }
            let arg_ty = tc.infer_expr(arg.expr);
            infer_type_params(param.ty, arg_ty, &mut substitutions, &tc.types.env);
        }
    }

    // Process the keyword record.
    let keywords_kind = tc.arena[w.keywords].kind.clone();
    match keywords_kind {
        ExprKind::Record(rec) => {
            process_record_keywords(tc, &rec.fields, &sig.keyword_params, &mut substitutions);
            // Make sure the keywords expression itself gets a type so the
            // arena invariant holds (some closure fields are skipped by
            // infer_record). Re-walk it after we've bound closure params.
            tc.infer_expr(w.keywords);
        }
        _ => {
            // Non-literal record (e.g. NameRef to a bound record). First make
            // sure all the field exprs that produced the record are typed —
            // that requires the closures inside the record to have been
            // typed too. Walk back through the binding to its initializer
            // record literal, if any.
            tc.infer_expr(w.keywords);
            if let Some(rec_fields) = find_record_fields_for_keywords(tc, w.keywords) {
                process_record_keywords(tc, &rec_fields, &sig.keyword_params, &mut substitutions);
            }
        }
    }

    // Type the callee expression with the (substituted-where-possible) sig so
    // that the inner Call/PartialApp's recorded type still looks right.
    let mut callee_sig = sig.clone();
    for param in &mut callee_sig.positional_params {
        param.ty = substitute_bound_type_params(param.ty, &substitutions, &mut tc.types.env);
    }
    for param in &mut callee_sig.keyword_params {
        param.ty = substitute_bound_type_params(param.ty, &substitutions, &mut tc.types.env);
    }
    callee_sig.ret = substitute_bound_type_params(sig.ret, &substitutions, &mut tc.types.env);

    // Walk through the underlying callee expression to populate its types.
    // The closures inside the keyword record have already been bound, so any
    // re-entry into the call/partial-app infer paths sees the right closure
    // param types.
    tc.infer_expr(w.callee);

    match kind {
        WhereCalleeKind::Call(_) => callee_sig.ret,
        WhereCalleeKind::Partial(pa) => {
            // Remaining positional parameters (the unapplied ones — for
            // `partial exec` with no explicit args, that's all of them).
            let remaining_positional: Vec<ParamDef> = callee_sig
                .positional_params
                .iter()
                .skip(pa.args.len())
                .cloned()
                .collect();
            let fn_sig = FnSig {
                name: callee_sig.name,
                name_span: callee_sig.name_span,
                type_params: callee_sig.type_params,
                positional_params: remaining_positional,
                keyword_params: callee_sig.keyword_params,
                ret: callee_sig.ret,
                capabilities: callee_sig.capabilities,
            };
            tc.function_ty_from_sig(&fn_sig)
        }
        WhereCalleeKind::NameRef => tc.function_ty_from_sig(&callee_sig),
    }
}

enum WhereCalleeKind {
    Call(CallExpr),
    Partial(PartialAppExpr),
    /// `f where {...}` where `f` is a bare function name — yields the
    /// partially-applied function type with no positional args dropped.
    NameRef,
}

/// Return the FnSig of the function being called by a Call expression, if its
/// callee is a NameRef that resolves to a function-like item.
fn underlying_call_sig(tc: &mut TypeChecker, call: &CallExpr) -> Option<FnSig> {
    let callee_kind = tc.arena[call.callee].kind.clone();
    let ExprKind::NameRef(ref nr) = callee_kind else {
        return None;
    };
    let target = nr.resolved?;
    call_sig_for_target(tc, target)
}

/// Return the FnSig of the function being partially applied, if any.
fn underlying_partial_sig(tc: &mut TypeChecker, pa: &PartialAppExpr) -> Option<FnSig> {
    let callee_kind = tc.arena[pa.callee].kind.clone();
    let ExprKind::NameRef(ref nr) = callee_kind else {
        return None;
    };
    let target = nr.resolved?;
    sig_for_name_ref_target(tc, target)
}

/// For each keyword field, bind closure params from the slot's function type
/// (so the closure's params get inferred properly), then infer the field
/// expression. Use the field's resulting type to bind type-param substitutions
/// against the slot's declared type.
fn process_record_keywords(
    tc: &mut TypeChecker,
    fields: &[RecordField],
    keyword_params: &[KeywordParamDef],
    substitutions: &mut [Option<TypeId>],
) {
    let fields = fields.to_vec();
    let keyword_params = keyword_params.to_vec();
    for field in fields {
        let Some(param) = keyword_params.iter().find(|p| p.name == field.name) else {
            // Unknown keyword — still infer to keep the arena consistent.
            tc.infer_expr(field.value);
            continue;
        };
        let param_ty = substitute_bound_type_params(param.ty, substitutions, &mut tc.types.env);

        let value_kind = tc.arena[field.value].kind.clone();
        let param_ty_clone = tc.types.env.get_ty(param_ty).clone();
        if let Ty::Function(ref fn_ty) = param_ty_clone {
            if let ExprKind::Closure(ref closure) = value_kind {
                bind_closure_params(
                    tc,
                    closure.body,
                    &fn_ty.params,
                    param.doc.as_deref(),
                    &param.name,
                );
                let value_ty = tc.infer_expr(field.value);
                // The closure's inferred return type feeds substitution against
                // the slot's declared return type.
                let value_ty_clone = tc.types.env.get_ty(value_ty).clone();
                if let Ty::Function(ref value_fn) = value_ty_clone {
                    infer_type_params(fn_ty.ret, value_fn.ret, substitutions, &tc.types.env);
                }
            } else {
                let value_ty = tc.infer_expr(field.value);
                infer_type_params(param.ty, value_ty, substitutions, &tc.types.env);
            }
        } else {
            let value_ty = tc.infer_expr(field.value);
            infer_type_params(param.ty, value_ty, substitutions, &tc.types.env);
        }
    }
}

/// If `keywords_expr_id` is a NameRef to a binding whose initializer is a
/// Record literal, return that record's fields. This lets `partial exec where
/// common` see through `let common = { observer: ... }`.
fn find_record_fields_for_keywords(
    tc: &TypeChecker,
    keywords_expr_id: ExprId,
) -> Option<Vec<RecordField>> {
    let kind = tc.arena[keywords_expr_id].kind.clone();
    let ExprKind::NameRef(nr) = kind else {
        return None;
    };
    let DefTarget::Binding(binding_id) = nr.resolved? else {
        return None;
    };

    // Walk all let-statements to find the one binding this id, then return
    // its initializer record fields if it's a record literal.
    for stmt in &tc.arena.stmts {
        let crate::stmt::StmtKind::Let(ref let_) = stmt.kind else {
            continue;
        };
        if !let_binds(tc, let_.pattern, binding_id) {
            continue;
        }
        let init_kind = &tc.arena[let_.init].kind;
        if let ExprKind::Record(rec) = init_kind {
            return Some(rec.fields.clone());
        }
        return None;
    }
    None
}

fn let_binds(tc: &TypeChecker, pattern_id: crate::PatternId, binding_id: crate::BindingId) -> bool {
    use crate::pattern::PatternKind;
    let kind = &tc.arena[pattern_id].kind;
    match kind {
        PatternKind::Binding(bp) => bp.binding == binding_id,
        _ => false,
    }
}

pub(crate) fn infer_invoke(tc: &mut TypeChecker, inv: &InvokeExpr) -> TypeId {
    let callee_kind = tc.arena[inv.callee].kind.clone();

    match callee_kind {
        ExprKind::Field(ref field) => {
            let receiver_ty = tc.infer_expr(field.receiver);
            let method_name = match &field.field {
                crate::expr::FieldKey::Name(n) => n.clone(),
                _ => return tc.poison(),
            };

            // Try method resolution with no args.
            let namespace_name = super::fields::namespace_name_for_receiver_pub(
                tc.types.env.get_ty(receiver_ty),
                &tc.types.env,
            );
            if let Some(ns_name) = namespace_name
                && let Some(sig) = find_namespace_method_sig(tc, &ns_name, &method_name)
                && let Some(method_sig) = instantiate_method_sig(tc, receiver_ty, &sig, &[])
                && method_sig.params.is_empty()
            {
                return method_sig.ret;
            }

            tc.poison()
        }
        _ => {
            let callee_ty = tc.infer_expr(inv.callee);
            let callee_ty_clone = tc.types.env.get_ty(callee_ty).clone();
            match callee_ty_clone {
                Ty::Function(fn_ty) if fn_ty.params.is_empty() => fn_ty.ret,
                _ => tc.poison(),
            }
        }
    }
}

pub(crate) fn infer_partial_app(tc: &mut TypeChecker, pa: &PartialAppExpr) -> TypeId {
    let callee_kind = tc.arena[pa.callee].kind.clone();

    if let ExprKind::NameRef(ref nr) = callee_kind
        && let Some(target) = nr.resolved
        && let Some(sig) = sig_for_name_ref_target(tc, target)
    {
        return tc.function_ty_from_sig(&sig);
    }

    tc.infer_expr(pa.callee)
}

fn find_namespace_method_sig(tc: &TypeChecker, ns_name: &str, method_name: &str) -> Option<FnSig> {
    for (i, item) in tc.arena.items.iter().enumerate() {
        if let ItemKind::Namespace(ref ns) = item.kind
            && tc.arena.item_name(crate::ItemId::from_index(i)) == ns_name
        {
            for &child_id in &ns.items {
                if let ItemKind::Function(ref f) = tc.arena[child_id].kind
                    && f.name == method_name
                {
                    return tc.types.def_to_fn_sig.get(&f.name_span).cloned();
                }
            }
        }
    }
    None
}
