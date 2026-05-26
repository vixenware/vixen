//! Field access and method resolution.

use vx_types::{FnSig, ParamDef, Ty};

use crate::ExprId;
use crate::def::{DefKind, Definition};
use crate::expr::{ExprKind, FieldExpr, FieldKey};
use crate::item::ItemKind;
use crate::scope::DefTarget;

use super::subst::{bind_type_params, infer_type_params, substitute_bound_type_params};
use super::{BoundMethodSig, TypeChecker};

pub(crate) fn infer_field(tc: &mut TypeChecker, _expr_id: ExprId, f: &FieldExpr) -> Ty {
    let FieldKey::Name(ref field_name) = f.field else {
        // Tuple index access.
        let receiver_ty = tc.infer_expr(f.receiver);
        if let Ty::Tuple(ref tuple_ty) = receiver_ty
            && let FieldKey::Index(idx) = f.field
            && let Some(elem) = tuple_ty.elements.get(idx as usize)
        {
            return elem.clone();
        }
        return Ty::Poison;
    };

    // Check if this is a static namespace member access.
    if let Some(static_ty) = infer_static_namespace_field(tc, f) {
        return static_ty;
    }

    let receiver_ty = tc.infer_expr(f.receiver);

    // Check namespace methods on the receiver type.
    if let Some(method_sig) = resolve_method_on_receiver(tc, &receiver_ty, field_name) {
        return Ty::Function(vx_types::FunctionTy {
            params: method_sig.params.into_iter().map(|p| p.ty).collect(),
            ret: Box::new(method_sig.ret),
            capabilities: method_sig.capabilities,
        });
    }

    // Check struct fields.
    if let Ty::Struct(ref struct_ty) = receiver_ty
        && let vx_types::TypeDef::Struct(ref s) = tc.types.env[struct_ty.id]
        && let Some(field_def) = s.fields.iter().find(|fd| fd.name == *field_name)
    {
        return field_def.ty.clone();
    }

    Ty::Poison
}

/// Static namespace field: `Namespace.member` where Namespace is a namespace item.
fn infer_static_namespace_field(tc: &mut TypeChecker, f: &FieldExpr) -> Option<Ty> {
    let FieldKey::Name(ref field_name) = f.field else {
        return None;
    };

    // Receiver must be a NameRef that resolved to a namespace Item.
    let receiver_kind = tc.arena[f.receiver].kind.clone();
    let ExprKind::NameRef(ref nr) = receiver_kind else {
        return None;
    };
    let Some(DefTarget::Item(ns_item_id)) = nr.resolved else {
        return None;
    };
    let ItemKind::Namespace(ref ns) = tc.arena[ns_item_id].kind else {
        return None;
    };

    // Find a child function matching field_name.
    for &child_id in &ns.items {
        if let ItemKind::Function(ref child_f) = tc.arena[child_id].kind
            && child_f.name == *field_name
            && let Some(sig) = tc.types.def_to_fn_sig.get(&child_f.name_span)
        {
            return Some(tc.function_ty_from_sig(sig));
        }
    }

    None
}

/// Resolve a method on a receiver type by looking up the type's namespace.
fn resolve_method_on_receiver(
    tc: &TypeChecker,
    receiver_ty: &Ty,
    method_name: &str,
) -> Option<BoundMethodSig> {
    let namespace_name = namespace_name_for_receiver(receiver_ty, &tc.types.env)?;

    // Look up the namespace item in the arena by scanning all items.
    let ns_item_id = tc.arena.items.iter().enumerate().find_map(|(i, item)| {
        if let ItemKind::Namespace(_) = item.kind
            && tc.arena.item_name(crate::ItemId::from_index(i)) == namespace_name
        {
            return Some(crate::ItemId::from_index(i));
        }
        None
    })?;

    let ItemKind::Namespace(ref ns) = tc.arena[ns_item_id].kind else {
        return None;
    };

    // Find the method in the namespace.
    for &child_id in &ns.items {
        if let ItemKind::Function(ref f) = tc.arena[child_id].kind
            && f.name == method_name
            && let Some(sig) = tc.types.def_to_fn_sig.get(&f.name_span)
        {
            return instantiate_method_sig(tc, receiver_ty, sig, &[]);
        }
    }

    None
}

pub(crate) fn namespace_name_for_receiver_pub(
    receiver_ty: &Ty,
    env: &vx_types::TypeEnv,
) -> Option<String> {
    namespace_name_for_receiver(receiver_ty, env)
}

fn namespace_name_for_receiver(receiver_ty: &Ty, env: &vx_types::TypeEnv) -> Option<String> {
    match receiver_ty {
        Ty::Scalar(vx_types::Scalar::String) => Some("String".to_string()),
        Ty::Optional(_) => Some("Option".to_string()),
        Ty::Array(_) => Some("Array".to_string()),
        Ty::Map(_, _) => Some("Map".to_string()),
        Ty::Stream(_) => Some("Stream".to_string()),
        Ty::Struct(s) => Some(env.get(s.id).name().to_string()),
        Ty::Enum(e) => Some(env.get(e.id).name().to_string()),
        Ty::Newtype(n) => Some(env.get(n.id).name().to_string()),
        Ty::Alias(a) => Some(env.get(a.id).name().to_string()),
        Ty::Opaque(o) => Some(env.get(o.id).name().to_string()),
        _ => None,
    }
}

/// Instantiate a method signature by binding the self param to receiver_ty.
pub(crate) fn instantiate_method_sig(
    _tc: &TypeChecker,
    receiver_ty: &Ty,
    sig: &FnSig,
    arg_tys: &[Ty],
) -> Option<BoundMethodSig> {
    let (self_param, params) = sig.positional_params.split_first()?;
    if self_param.name != "self" || sig.ret.is_poison() {
        return None;
    }

    // Special methods.
    if sig.name == "flatten"
        && params.is_empty()
        && let Ty::Stream(inner) = receiver_ty
    {
        let ret = match inner.as_ref() {
            Ty::Array(item) => Ty::Stream(item.clone()),
            Ty::Stream(item) => Ty::Stream(item.clone()),
            _ => return None,
        };
        return Some(BoundMethodSig {
            params: Vec::new(),
            ret,
            capabilities: sig.capabilities.clone(),
        });
    }
    if sig.name == "to_map"
        && params.is_empty()
        && let Ty::Stream(inner) = receiver_ty
    {
        let Ty::Tuple(ref tuple) = **inner else {
            return None;
        };
        if tuple.elements.len() != 2 {
            return None;
        }
        return Some(BoundMethodSig {
            params: Vec::new(),
            ret: Ty::Map(
                Box::new(tuple.elements[0].clone()),
                Box::new(tuple.elements[1].clone()),
            ),
            capabilities: sig.capabilities.clone(),
        });
    }

    let mut substitutions = vec![None; sig.type_params.len()];
    bind_type_params(&self_param.ty, receiver_ty, &mut substitutions);

    let mut bound_params: Vec<ParamDef> = params
        .iter()
        .map(|p| ParamDef {
            name: p.name.clone(),
            name_span: p.name_span,
            ty: substitute_bound_type_params(&p.ty, &substitutions),
            doc: p.doc.clone(),
        })
        .collect();
    for (p, arg_ty) in bound_params.iter().zip(arg_tys.iter()) {
        infer_type_params(&p.ty, arg_ty, &mut substitutions);
    }
    bound_params = params
        .iter()
        .map(|p| ParamDef {
            name: p.name.clone(),
            name_span: p.name_span,
            ty: substitute_bound_type_params(&p.ty, &substitutions),
            doc: p.doc.clone(),
        })
        .collect();

    let mut ret = substitute_bound_type_params(&sig.ret, &substitutions);

    // flat_map special case.
    if sig.name == "flat_map"
        && params.len() == 1
        && let Some(Ty::Function(function)) = arg_tys.first()
    {
        let flattened = match function.ret.as_ref() {
            Ty::Array(item) => item.clone(),
            Ty::Stream(item) => item.clone(),
            _ => return None,
        };
        ret = Ty::Stream(flattened);
    }

    if ret.is_poison() || bound_params.iter().any(|p| p.ty.is_poison()) {
        return None;
    }

    Some(BoundMethodSig {
        params: bound_params,
        ret,
        capabilities: sig.capabilities.clone(),
    })
}

/// Resolve method and get a bound sig suitable for a call context.
pub(crate) fn resolve_method_for_call(
    tc: &mut TypeChecker,
    receiver_ty: &Ty,
    method_name: &str,
    call_expr_id: ExprId,
) -> Option<BoundMethodSig> {
    let namespace_name = namespace_name_for_receiver(receiver_ty, &tc.types.env)?;

    let ns_item_id = tc.arena.items.iter().enumerate().find_map(|(i, item)| {
        if let ItemKind::Namespace(ref _ns) = item.kind
            && tc.arena.item_name(crate::ItemId::from_index(i)) == namespace_name
        {
            return Some(crate::ItemId::from_index(i));
        }
        None
    })?;

    let ns_kind = tc.arena[ns_item_id].kind.clone();
    let ItemKind::Namespace(ref ns) = ns_kind else {
        return None;
    };

    // Find matching method and clone the sig.
    let mut found_sig = None;
    let mut found_name = String::new();
    let mut found_item_id: Option<crate::ItemId> = None;
    for &child_id in &ns.items {
        let child_kind = tc.arena[child_id].kind.clone();
        if let ItemKind::Function(ref f) = child_kind
            && f.name == method_name
            && let Some(sig) = tc.types.def_to_fn_sig.get(&f.name_span)
        {
            found_sig = Some(sig.clone());
            found_name = f.name.clone();
            found_item_id = Some(child_id);
            break;
        }
    }

    let sig = found_sig?;

    // Record the resolved method definition under the field name span so the
    // LSP can jump from `host.which("cc")` straight at the stdlib `fn which`.
    if let Some(child_id) = found_item_id
        && let ItemKind::Function(ref f) = tc.arena[child_id].kind
    {
        let def = Definition {
            id: tc.arena[child_id].def_id,
            name: f.name.clone(),
            kind: DefKind::Function,
            name_span: f.name_span,
            full_span: tc.arena[child_id].span,
        };
        let call_kind = tc.arena[call_expr_id].kind.clone();
        if let crate::expr::ExprKind::Call(ref call) = call_kind {
            let callee_kind = tc.arena[call.callee].kind.clone();
            if let crate::expr::ExprKind::Field(ref field) = callee_kind {
                tc.member_defs.insert(field.field_span, def);
            }
        }
    }

    // First pass: get initial bound sig.
    let initial = instantiate_method_sig(tc, receiver_ty, &sig, &[])?;

    // Infer arg types.
    let call_kind = tc.arena[call_expr_id].kind.clone();
    let crate::expr::ExprKind::Call(ref call) = call_kind else {
        return Some(initial);
    };
    let arg_tys: Vec<Ty> = call
        .args
        .iter()
        .zip(initial.params.iter())
        .map(|(arg, _param)| tc.infer_expr(arg.expr))
        .collect();

    if found_name == "flat_map" && !arg_tys.is_empty() {
        return instantiate_method_sig(tc, receiver_ty, &sig, &arg_tys);
    }

    instantiate_method_sig(tc, receiver_ty, &sig, &arg_tys).or(Some(initial))
}
