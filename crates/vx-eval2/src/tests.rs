use std::sync::Arc;

use crate::{
    BoundaryConversion, Observation, PendingRef, Portability, ProjectionStep, Provenance,
    Resolution, RuntimeInputKind, Value, ValueRep,
};

#[test]
fn scalar_values_can_carry_obligations_without_wrapper_enums() {
    let value = Value::u64(42)
        .with_provenance(Provenance::host_probe("windows-1", "discover toolchain"))
        .with_obligation(Observation::Arch {
            value: Arc::from("x86_64"),
        });

    assert_eq!(value.as_u64(), Some(42));
    assert_eq!(value.meta.obligations.len(), 1);
    assert!(matches!(value.rep, ValueRep::U64(42)));
}

#[test]
fn projection_updates_provenance_without_changing_payload_kind() {
    let value = Value::string("clang")
        .with_provenance(Provenance::runtime_input(
            RuntimeInputKind::DiscoverResult,
            "toolchain",
        ))
        .project(ProjectionStep::Field(Arc::from("cc")));

    assert!(matches!(value.rep, ValueRep::String(_)));
    assert_eq!(
        value.meta.provenance.path.as_ref(),
        [ProjectionStep::Field(Arc::from("cc"))]
    );
}

#[test]
fn readiness_is_not_encoded_inside_value() {
    let ready = Resolution::Ready(Value::bool(true));
    let pending = Resolution::Pending(PendingRef::new(7, "waiting on discover"));

    assert!(matches!(ready, Resolution::Ready(_)));
    assert!(matches!(pending, Resolution::Pending(_)));
}

#[test]
fn default_portability_distinguishes_boundary_cases() {
    let local_tree = Value::local_tree("/workspace");
    let store_tree = Value::store_tree("sha256:tree");
    let host = Value::host_handle("ssh:orb");

    assert_eq!(
        local_tree.meta.portability,
        Portability::Convertible {
            via: BoundaryConversion::SnapshotTree,
        }
    );
    assert_eq!(store_tree.meta.portability, Portability::Portable);
    assert!(matches!(host.meta.portability, Portability::Pinned { .. }));
}
