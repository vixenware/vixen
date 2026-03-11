use rowan::TextSize;
use vx_types::{Builtin, Scalar, Ty};

use crate::{DefKind, Definition, ProgramScopes, SourceId};

pub fn resolve_namespace_member(
    scopes: &ProgramScopes,
    namespace: &Definition,
    member_name: &str,
) -> Option<Definition> {
    if namespace.kind != DefKind::Namespace {
        return None;
    }
    scopes.lookup_member(namespace, member_name).cloned()
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

pub fn resolve_member_for_receiver(
    scopes: &ProgramScopes,
    source: SourceId,
    offset: TextSize,
    receiver_ty: &Ty,
    member_name: &str,
) -> Option<Definition> {
    let namespace_name = namespace_name_for_receiver(receiver_ty)?;
    resolve_visible_namespace_member(scopes, source, offset, namespace_name, member_name)
}

pub fn visible_members_for_receiver(
    scopes: &ProgramScopes,
    source: SourceId,
    offset: TextSize,
    receiver_ty: &Ty,
) -> Vec<Definition> {
    let Some(namespace_name) = namespace_name_for_receiver(receiver_ty) else {
        return Vec::new();
    };
    let Some(scope) = scopes.scope_at(source, offset) else {
        return Vec::new();
    };
    let Some(namespace) = scopes.lookup_at(source, namespace_name, scope, offset) else {
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

fn namespace_name_for_receiver(receiver_ty: &Ty) -> Option<&'static str> {
    match receiver_ty {
        Ty::Builtin(Builtin::Blob) => Some("Blob"),
        Ty::Builtin(Builtin::Tree) => Some("Tree"),
        Ty::Builtin(Builtin::Host) => Some("Host"),
        Ty::Builtin(Builtin::Path) => Some("Path"),
        Ty::Builtin(Builtin::Process) => Some("Process"),
        Ty::Builtin(Builtin::ExecutorClass) => Some("ExecutorClass"),
        Ty::Scalar(Scalar::String) => Some("String"),
        Ty::Array(_) => Some("Array"),
        Ty::Map(_, _) => Some("Map"),
        Ty::Stream(_) => Some("Stream"),
        _ => None,
    }
}
