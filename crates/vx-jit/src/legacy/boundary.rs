use vx_resolve::{DefKind, SourceId};

use super::{BridgeResolvedRef, RuntimeBoundaryKind};

pub(crate) fn classify_runtime_boundary(
    source_id: SourceId,
    resolved: &BridgeResolvedRef,
) -> Option<RuntimeBoundaryKind> {
    let target = resolved.target.as_ref().ok()?;
    if target.kind != DefKind::Function {
        return None;
    }
    if target.source() == source_id {
        return None;
    }

    match target.name.as_str() {
        "exec" => Some(RuntimeBoundaryKind::Exec),
        "discover" => Some(RuntimeBoundaryKind::Discover),
        "stabilize" => Some(RuntimeBoundaryKind::SharedKeyed),
        _ => None,
    }
}
