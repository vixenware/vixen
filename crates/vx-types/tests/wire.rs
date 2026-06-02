//! The type universe ships.
//!
//! A `TypeEnv` round-trips through `facet-postcard`, so the exec bundle can carry
//! the observer's types to a fresh **mirror** runtime with the orchestrator's
//! `TypeId`s intact — the mirror installs them verbatim and never re-allocates.
//! If types didn't serialize faithfully, the "mirror never allocates" invariant
//! would have nothing to install.

use vx_types::{Scalar, StructDef, TypeDef, TypeEnv};

#[test]
fn type_env_round_trips_through_postcard() {
    let mut env = TypeEnv::new();

    // A nominal struct def + a spread of interned types (scalar, nominal, generic).
    let def = env.define(
        "Stat",
        TypeDef::Struct(StructDef {
            name: "Stat".into(),
            parent_enum: None,
            type_params: vec![],
            attrs: Default::default(),
            fields: vec![],
        }),
    );
    let u64_ty = env.intern_scalar(Scalar::U64);
    let stat_ty = env.intern_struct(def, vec![]);
    let stream_ty = env.intern_stream(u64_ty); // Stream[U64] — references u64_ty by id

    // Ship it over the wire (postcard = vox's format).
    let bytes = facet_postcard::to_vec(&env).expect("TypeEnv serializes");
    let back: TypeEnv = facet_postcard::from_slice(&bytes).expect("TypeEnv deserializes");

    // Same counts, and the same `TypeId` resolves to the same structural `Ty` —
    // the ids are the orchestrator's, reused verbatim on the other side.
    assert_eq!(back.types_len(), env.types_len(), "interned types survive");
    assert_eq!(back.defs_len(), env.defs_len(), "type defs survive");
    assert_eq!(back.get_ty(u64_ty), env.get_ty(u64_ty), "U64");
    assert_eq!(back.get_ty(stat_ty), env.get_ty(stat_ty), "Stat");
    assert_eq!(back.get_ty(stream_ty), env.get_ty(stream_ty), "Stream[U64]");
    // Nominal lookup survives: `Stat` resolves to the same def id.
    assert_eq!(back.lookup_def("Stat"), env.lookup_def("Stat"));
}
