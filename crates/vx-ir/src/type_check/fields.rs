//! Field access and method resolution.

use vx_types::{FnSig, ParamDef, Ty, TypeEnv, TypeId};

use crate::ExprId;
use crate::def::{DefKind, Definition};
use crate::expr::{ExprKind, FieldExpr, FieldKey};
use crate::item::ItemKind;
use crate::scope::DefTarget;

use super::subst::{bind_type_params, infer_type_params, substitute_bound_type_params};
use super::{BoundMethodSig, TypeChecker};

pub(crate) fn infer_field(tc: &mut TypeChecker, _expr_id: ExprId, f: &FieldExpr) -> TypeId {
    let FieldKey::Name(ref field_name) = f.field else {
        // Tuple index access.
        let receiver_ty = tc.infer_expr(f.receiver);
        let receiver_ty_clone = tc.types.env.get_ty(receiver_ty).clone();
        if let Ty::Tuple(ref tuple_ty) = receiver_ty_clone
            && let FieldKey::Index(idx) = f.field
            && let Some(&elem) = tuple_ty.0.get(idx as usize)
        {
            return elem;
        }
        return tc.poison();
    };

    // Check if this is a static namespace member access.
    if let Some(static_ty) = infer_static_namespace_field(tc, f) {
        return static_ty;
    }

    // Check if this is a module-record field access: `let m = import "…"; m.x`.
    if let Some(module_ty) = infer_module_record_field(tc, f) {
        return module_ty;
    }

    let receiver_ty = tc.infer_expr(f.receiver);

    // Check namespace methods on the receiver type.
    if let Some(method_sig) = resolve_method_on_receiver(tc, receiver_ty, field_name) {
        let params: Vec<TypeId> = method_sig.params.into_iter().map(|p| p.ty).collect();
        return tc
            .types
            .env
            .intern_function(params, method_sig.ret, method_sig.capabilities);
    }

    // Check struct fields.
    let receiver_ty_clone = tc.types.env.get_ty(receiver_ty).clone();
    if let Ty::Struct(ref struct_ty) = receiver_ty_clone
        && let vx_types::TypeDef::Struct(ref s) = tc.types.env[struct_ty.def]
        && let Some(field_def) = s.fields.iter().find(|fd| fd.name == *field_name)
    {
        return field_def.ty;
    }

    tc.poison()
}

/// Static namespace field: `Namespace.member` where Namespace is a namespace item.
fn infer_static_namespace_field(tc: &mut TypeChecker, f: &FieldExpr) -> Option<TypeId> {
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
    let ns_items: Vec<crate::ItemId> = ns.items.clone();
    for child_id in ns_items {
        if let ItemKind::Function(ref child_f) = tc.arena[child_id].kind
            && child_f.name == *field_name
        {
            let name_span = child_f.name_span;
            if let Some(sig) = tc.types.def_to_fn_sig.get(&name_span).cloned() {
                return Some(tc.function_ty_from_sig(&sig));
            }
        }
    }

    None
}

/// Module-record field: `m.field` where `m` is a whole-module import binding
/// (`let m = import "…"`). A module is a value — a record — so its fields are
/// the imported unit's exports. Resolves `field` to an export and returns its
/// type. Mirrors the destructure path (which redirects names through the same
/// `unit_export`), but here the projection is spelled with a `.`.
fn infer_module_record_field(tc: &mut TypeChecker, f: &FieldExpr) -> Option<TypeId> {
    let FieldKey::Name(ref field_name) = f.field else {
        return None;
    };
    // Receiver must be a NameRef that resolved to a whole-module import binding.
    let ExprKind::NameRef(ref nr) = tc.arena[f.receiver].kind else {
        return None;
    };
    let DefTarget::Binding(bid) = nr.resolved? else {
        return None;
    };
    let unit = match &tc.arena[bid].import_origin {
        Some(origin) if origin.export.is_none() => origin.unit,
        _ => return None,
    };

    // The field names an export of the imported unit.
    match crate::resolve::unit_export(tc.arena, unit, field_name)? {
        DefTarget::Item(item_id) => Some(super::names::type_of_item(tc, item_id)),
        DefTarget::Binding(export_bid) => Some(super::names::type_of_binding(tc, export_bid)),
        DefTarget::Variant {
            enum_item,
            variant_index,
        } => Some(super::names::type_of_variant(tc, enum_item, variant_index)),
    }
}

/// Resolve a method on a receiver type by looking up the type's namespace.
fn resolve_method_on_receiver(
    tc: &mut TypeChecker,
    receiver_ty: TypeId,
    method_name: &str,
) -> Option<BoundMethodSig> {
    let namespace_name =
        namespace_name_for_receiver(tc.types.env.get_ty(receiver_ty), &tc.types.env)?;

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
    let ns_items: Vec<crate::ItemId> = ns.items.clone();
    for child_id in ns_items {
        if let ItemKind::Function(ref f) = tc.arena[child_id].kind
            && f.name == method_name
        {
            let name_span = f.name_span;
            if let Some(sig) = tc.types.def_to_fn_sig.get(&name_span).cloned() {
                return instantiate_method_sig(tc, receiver_ty, &sig, &[]);
            }
        }
    }

    None
}

pub(crate) fn namespace_name_for_receiver_pub(receiver_ty: &Ty, env: &TypeEnv) -> Option<String> {
    namespace_name_for_receiver(receiver_ty, env)
}

/// Enumerate sibling method candidates for a failed lookup, ranked by edit
/// distance against `method_name`. Returns up to 3 candidates with edit
/// distance <= 3 (a method name that's mistyped by more than 3 chars is
/// unlikely to have been "meant" by the user).
///
/// Used by the NoSuchMethod diagnostic to surface "did you mean..." hints
/// without copying identifier text into the diagnostic — each candidate
/// carries the ItemId + the method-declaration's name_span so the renderer
/// reads the actual identifier from source.
pub(crate) fn candidates_for_method(
    tc: &TypeChecker,
    receiver_ty: TypeId,
    method_name: &str,
) -> Vec<crate::diagnostic::MethodCandidate> {
    let Some(namespace_name) =
        namespace_name_for_receiver(tc.types.env.get_ty(receiver_ty), &tc.types.env)
    else {
        return Vec::new();
    };

    let Some(ns_item_id) = tc.arena.items.iter().enumerate().find_map(|(i, item)| {
        if matches!(item.kind, ItemKind::Namespace(_))
            && tc.arena.item_name(crate::ItemId::from_index(i)) == namespace_name
        {
            Some(crate::ItemId::from_index(i))
        } else {
            None
        }
    }) else {
        return Vec::new();
    };

    let ItemKind::Namespace(ref ns) = tc.arena[ns_item_id].kind else {
        return Vec::new();
    };

    let mut ranked: Vec<(u32, crate::diagnostic::MethodCandidate)> = ns
        .items
        .iter()
        .filter_map(|&child_id| {
            if let ItemKind::Function(ref f) = tc.arena[child_id].kind {
                let d = edit_distance(method_name, &f.name);
                if d <= 3 {
                    return Some((
                        d,
                        crate::diagnostic::MethodCandidate {
                            item: child_id,
                            name_span: f.name_span,
                        },
                    ));
                }
            }
            None
        })
        .collect();
    ranked.sort_by_key(|(d, _)| *d);
    ranked.truncate(3);
    ranked.into_iter().map(|(_, c)| c).collect()
}

/// Levenshtein distance between two strings, capped at u32. Iterative
/// two-row DP — O(n * m) time, O(min(n, m)) space.
fn edit_distance(a: &str, b: &str) -> u32 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len() as u32;
    }
    if b.is_empty() {
        return a.len() as u32;
    }
    let mut prev: Vec<u32> = (0..=b.len() as u32).collect();
    let mut curr: Vec<u32> = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = (i + 1) as u32;
        for (j, cb) in b.iter().enumerate() {
            let cost = u32::from(ca != cb);
            curr[j + 1] = (curr[j] + 1).min(prev[j + 1] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

fn namespace_name_for_receiver(receiver_ty: &Ty, env: &TypeEnv) -> Option<String> {
    match receiver_ty {
        Ty::Scalar(vx_types::Scalar::String) => Some("String".to_string()),
        Ty::Array(_) => Some("Array".to_string()),
        Ty::Map(_) => Some("Map".to_string()),
        Ty::Stream(_) => Some("Stream".to_string()),
        Ty::Struct(s) => Some(env.get_def(s.def).name().to_string()),
        Ty::Enum(e) => Some(env.get_def(e.def).name().to_string()),
        Ty::Newtype(n) => Some(env.get_def(n.def).name().to_string()),
        Ty::Alias(a) => Some(env.get_def(a.def).name().to_string()),
        Ty::Opaque(o) => Some(env.get_def(o.def).name().to_string()),
        _ => None,
    }
}

/// Instantiate a method signature by binding the self param to receiver_ty.
pub(crate) fn instantiate_method_sig(
    tc: &mut TypeChecker,
    receiver_ty: TypeId,
    sig: &FnSig,
    arg_tys: &[TypeId],
) -> Option<BoundMethodSig> {
    let (self_param, params) = sig.positional_params.split_first()?;
    if self_param.name != "self" || tc.types.env.get_ty(sig.ret).is_poison() {
        return None;
    }

    let receiver_ty_clone = tc.types.env.get_ty(receiver_ty).clone();
    // Special methods.
    if sig.name == "flatten"
        && params.is_empty()
        && let Ty::Stream(stream) = receiver_ty_clone.clone()
    {
        let inner_ty = tc.types.env.get_ty(stream.0).clone();
        let item = match inner_ty {
            Ty::Array(arr) => arr.0,
            Ty::Stream(s) => s.0,
            _ => return None,
        };
        let ret = tc.types.env.intern_stream(item);
        return Some(BoundMethodSig {
            params: Vec::new(),
            ret,
            capabilities: sig.capabilities.clone(),
        });
    }
    if sig.name == "to_map"
        && params.is_empty()
        && let Ty::Stream(stream) = receiver_ty_clone
    {
        let inner_ty = tc.types.env.get_ty(stream.0).clone();
        let Ty::Tuple(ref tuple) = inner_ty else {
            return None;
        };
        if tuple.0.len() != 2 {
            return None;
        }
        let k = tuple.0[0];
        let v = tuple.0[1];
        let ret = tc.types.env.intern_map(k, v);
        return Some(BoundMethodSig {
            params: Vec::new(),
            ret,
            capabilities: sig.capabilities.clone(),
        });
    }

    let mut substitutions: Vec<Option<TypeId>> = vec![None; sig.type_params.len()];
    bind_type_params(
        self_param.ty,
        receiver_ty,
        &mut substitutions,
        &tc.types.env,
    );

    let params_snapshot: Vec<ParamDef> = params.to_vec();

    let mut bound_params: Vec<ParamDef> = params_snapshot
        .iter()
        .map(|p| ParamDef {
            name: p.name.clone(),
            name_span: p.name_span,
            ty: substitute_bound_type_params(p.ty, &substitutions, &mut tc.types.env),
            doc: p.doc.clone(),
        })
        .collect();
    for (p, &arg_ty) in bound_params.iter().zip(arg_tys.iter()) {
        infer_type_params(p.ty, arg_ty, &mut substitutions, &tc.types.env);
    }
    bound_params = params_snapshot
        .iter()
        .map(|p| ParamDef {
            name: p.name.clone(),
            name_span: p.name_span,
            ty: substitute_bound_type_params(p.ty, &substitutions, &mut tc.types.env),
            doc: p.doc.clone(),
        })
        .collect();

    let mut ret = substitute_bound_type_params(sig.ret, &substitutions, &mut tc.types.env);

    // flat_map special case.
    if sig.name == "flat_map"
        && params.len() == 1
        && let Some(&arg_ty) = arg_tys.first()
    {
        let arg_ty_clone = tc.types.env.get_ty(arg_ty).clone();
        if let Ty::Function(function) = arg_ty_clone {
            let ret_ty_clone = tc.types.env.get_ty(function.ret).clone();
            let flattened = match ret_ty_clone {
                Ty::Array(a) => a.0,
                Ty::Stream(s) => s.0,
                _ => return None,
            };
            ret = tc.types.env.intern_stream(flattened);
        }
    }

    if tc.types.env.get_ty(ret).is_poison()
        || bound_params
            .iter()
            .any(|p| tc.types.env.get_ty(p.ty).is_poison())
    {
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
    receiver_ty: TypeId,
    method_name: &str,
    call_expr_id: ExprId,
) -> Option<BoundMethodSig> {
    let namespace_name =
        namespace_name_for_receiver(tc.types.env.get_ty(receiver_ty), &tc.types.env)?;

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
    let initial_param_count = initial.params.len();
    let arg_tys: Vec<TypeId> = call
        .args
        .iter()
        .take(initial_param_count)
        .map(|arg| tc.infer_expr(arg.expr))
        .collect();

    if found_name == "flat_map" && !arg_tys.is_empty() {
        return instantiate_method_sig(tc, receiver_ty, &sig, &arg_tys);
    }

    instantiate_method_sig(tc, receiver_ty, &sig, &arg_tys).or(Some(initial))
}
