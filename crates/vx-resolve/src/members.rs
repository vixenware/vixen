use rowan::TextSize;
use vx_types::{Builtin, Scalar, Ty, TypeEnv};

use crate::{DefKind, Definition, ProgramScopes, SourceId};

pub fn resolve_namespace_member(
    scopes: &ProgramScopes,
    namespace: &Definition,
    member_name: &str,
) -> Option<Definition> {
    resolve_namespace_members(scopes, namespace, member_name)
        .into_iter()
        .next()
}

pub fn resolve_namespace_members(
    scopes: &ProgramScopes,
    namespace: &Definition,
    member_name: &str,
) -> Vec<Definition> {
    if namespace.kind != DefKind::Namespace {
        return Vec::new();
    }
    scopes
        .lookup_members(namespace, member_name)
        .into_iter()
        .cloned()
        .collect()
}

pub fn resolve_visible_namespace_member(
    scopes: &ProgramScopes,
    source: SourceId,
    offset: TextSize,
    namespace_name: &str,
    member_name: &str,
) -> Option<Definition> {
    let scope = scopes.scope_at(source, offset)?;
    let namespace = scopes.lookup_at(source, namespace_name, scope, offset)?;
    resolve_namespace_member(scopes, namespace, member_name)
}

pub fn resolve_visible_namespace_members(
    scopes: &ProgramScopes,
    source: SourceId,
    offset: TextSize,
    namespace_name: &str,
    member_name: &str,
) -> Vec<Definition> {
    let Some(scope) = scopes.scope_at(source, offset) else {
        return Vec::new();
    };
    let Some(namespace) = scopes.lookup_at(source, namespace_name, scope, offset) else {
        return Vec::new();
    };
    resolve_namespace_members(scopes, namespace, member_name)
}

pub fn resolve_member_for_receiver(
    scopes: &ProgramScopes,
    env: &TypeEnv,
    source: SourceId,
    offset: TextSize,
    receiver_ty: &Ty,
    member_name: &str,
) -> Option<Definition> {
    let namespace_name = namespace_name_for_receiver(receiver_ty, env)?;
    resolve_visible_namespace_member(scopes, source, offset, &namespace_name, member_name)
}

pub fn visible_members_for_receiver(
    scopes: &ProgramScopes,
    env: &TypeEnv,
    source: SourceId,
    offset: TextSize,
    receiver_ty: &Ty,
) -> Vec<Definition> {
    let Some(namespace_name) = namespace_name_for_receiver(receiver_ty, env) else {
        return Vec::new();
    };
    let Some(scope) = scopes.scope_at(source, offset) else {
        return Vec::new();
    };
    let Some(namespace) = scopes.lookup_at(source, &namespace_name, scope, offset) else {
        return Vec::new();
    };
    let Some(namespace_scope) = scopes.owned_scope(namespace) else {
        return Vec::new();
    };
    scopes
        .definitions(namespace.source(), namespace_scope)
        .into_iter()
        .flatten()
        .cloned()
        .collect()
}

fn namespace_name_for_receiver(receiver_ty: &Ty, env: &TypeEnv) -> Option<String> {
    match receiver_ty {
        Ty::Builtin(Builtin::Blob) => Some("Blob".to_string()),
        Ty::Builtin(Builtin::Tree) => Some("Tree".to_string()),
        Ty::Builtin(Builtin::Host) => Some("Host".to_string()),
        Ty::Builtin(Builtin::Path) => Some("Path".to_string()),
        Ty::Builtin(Builtin::Process) => Some("Process".to_string()),
        Ty::Builtin(Builtin::ExecutorClass) => Some("ExecutorClass".to_string()),
        Ty::Scalar(Scalar::String) => Some("String".to_string()),
        Ty::Array(_) => Some("Array".to_string()),
        Ty::Map(_, _) => Some("Map".to_string()),
        Ty::Stream(_) => Some("Stream".to_string()),
        Ty::Struct(struct_ty) => Some(env.get(struct_ty.id).name().to_string()),
        Ty::Enum(enum_ty) => Some(env.get(enum_ty.id).name().to_string()),
        Ty::Newtype(newtype_ty) => Some(env.get(newtype_ty.id).name().to_string()),
        Ty::Alias(alias_ty) => Some(env.get(alias_ty.id).name().to_string()),
        Ty::Opaque(opaque_ty) => Some(env.get(opaque_ty.id).name().to_string()),
        _ => None,
    }
}
