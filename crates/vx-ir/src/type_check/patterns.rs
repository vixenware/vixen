//! Pattern typing — assign types to bindings introduced by patterns.

use vx_types::{Ty, TypeId};

use crate::PatternId;
use crate::arena::VirArena;
use crate::pattern::PatternKind;
use crate::type_collection::TypeCollection;

use super::TypeChecker;
use super::subst::substitute_type_params;

/// Assign types to all bindings introduced by a pattern, given the type of the
/// matched value. Type-check-pass entry point (holds a [`TypeChecker`]).
pub(crate) fn type_pattern(tc: &mut TypeChecker, pat_id: PatternId, ty: TypeId) {
    type_pattern_raw(tc.arena, tc.types, pat_id, ty);
}

/// Re-assign pattern binding types after the fact — used by the
/// [`error_union`](crate::error_union) pass to push a `?`'s inferred
/// `Result[_, E]` into a `match`'s `Err(e)` binding. Same logic as
/// [`type_pattern`], operating directly on the arena + type collection.
pub fn retype_pattern(
    arena: &mut VirArena,
    types: &mut TypeCollection,
    pat_id: PatternId,
    ty: TypeId,
) {
    type_pattern_raw(arena, types, pat_id, ty);
}

/// The shared pattern-typing core, over `(arena, types)`.
fn type_pattern_raw(
    arena: &mut VirArena,
    types: &mut TypeCollection,
    pat_id: PatternId,
    ty: TypeId,
) {
    let kind = arena[pat_id].kind.clone();
    arena[pat_id].ty = Some(ty);
    let is_poison = types.env.get_ty(ty).is_poison();

    match kind {
        PatternKind::Binding(bp) => {
            if !is_poison {
                arena[bp.binding].ty = Some(ty);
            }
        }
        PatternKind::Wildcard
        | PatternKind::Literal(_)
        | PatternKind::Unit
        | PatternKind::Error => {
            // No bindings to type.
        }
        PatternKind::Tuple(tp) => {
            let ty_clone = types.env.get_ty(ty).clone();
            if let Ty::Tuple(tuple_ty) = ty_clone {
                for (elem_pat, elem_ty) in tp.elements.iter().zip(tuple_ty.0.iter()) {
                    type_pattern_raw(arena, types, *elem_pat, *elem_ty);
                }
            }
        }
        PatternKind::Struct(sp) => {
            let ty_clone = types.env.get_ty(ty).clone();
            if let Ty::Struct(struct_ty) = ty_clone
                && let vx_types::TypeDef::Struct(ref sdef) = types.env[struct_ty.def]
            {
                // Snapshot to release the immutable borrow.
                let fields_snapshot: Vec<(String, TypeId)> =
                    sdef.fields.iter().map(|f| (f.name.clone(), f.ty)).collect();
                // Substitute the struct's type args so a generic struct's field
                // types resolve concretely (`Failure[U64]` → `err`'s field is `U64`,
                // not the bare param `E`), mirroring the `Ty::Enum` substitution in
                // `resolve_variant_fields`.
                let args = struct_ty.type_args.clone();
                for field in &sp.fields {
                    let field_ty = fields_snapshot
                        .iter()
                        .find(|(n, _)| *n == field.name)
                        .map(|(_, t)| *t)
                        .map(|t| {
                            if args.is_empty() {
                                t
                            } else {
                                substitute_type_params(t, &args, &mut types.env)
                            }
                        })
                        .unwrap_or_else(|| types.env.intern_poison());
                    type_pattern_raw(arena, types, field.pattern, field_ty);
                }
            }
        }
        PatternKind::Variant(vp) => {
            let variant_fields = resolve_variant_fields(types, &vp.name, ty);
            if let Some(ref vp_fields) = vp.fields {
                for field in vp_fields {
                    let field_ty = variant_fields
                        .as_ref()
                        .and_then(|vf| vf.iter().find(|(n, _)| *n == field.name))
                        .map(|(_, t)| *t)
                        .unwrap_or_else(|| types.env.intern_poison());
                    type_pattern_raw(arena, types, field.pattern, field_ty);
                }
            }
        }
        PatternKind::Array(arr) => {
            let ty_clone = types.env.get_ty(ty).clone();
            if let Ty::Array(arr_ty) = ty_clone {
                for &elem_pat in &arr.elements {
                    type_pattern_raw(arena, types, elem_pat, arr_ty.0);
                }
            }
        }
        PatternKind::ArrayRest(rest) => {
            if let Some(binding_id) = rest.binding {
                // Rest binding gets the full Array type.
                if matches!(types.env.get_ty(ty), Ty::Array(_)) {
                    arena[binding_id].ty = Some(ty);
                }
            }
        }
        PatternKind::At(at) => {
            if !is_poison {
                arena[at.binding].ty = Some(ty);
            }
            type_pattern_raw(arena, types, at.inner, ty);
        }
        PatternKind::Or(or) => {
            for &alt in &or.alternatives {
                type_pattern_raw(arena, types, alt, ty);
            }
        }
    }
}

/// Resolve a variant pattern's field types from the scrutinee type. Returns
/// the variant's field (name, [`TypeId`]) pairs, with type parameters
/// substituted from the scrutinee.
fn resolve_variant_fields(
    types: &mut TypeCollection,
    variant_name: &str,
    scrutinee_ty: TypeId,
) -> Option<Vec<(String, TypeId)>> {
    let scrutinee = types.env.get_ty(scrutinee_ty).clone();

    // A struct destructure written in variant syntax (`Failure { err: x }`, the
    // parser emits a `VariantPattern` for any `UpperIdent { … }`). The scrutinee is
    // a `Ty::Struct`; substitute its type args so a generic struct's fields resolve
    // concretely (`Failure[U64]` → `err`'s field is `U64`, not the bare param `E`).
    if let Ty::Struct(struct_ty) = &scrutinee {
        let args = struct_ty.type_args.clone();
        return match types.env[struct_ty.def] {
            vx_types::TypeDef::Struct(ref sdef) => {
                let fields_snapshot: Vec<(String, TypeId)> =
                    sdef.fields.iter().map(|f| (f.name.clone(), f.ty)).collect();
                Some(
                    fields_snapshot
                        .into_iter()
                        .map(|(name, ty)| {
                            let substituted = if args.is_empty() {
                                ty
                            } else {
                                substitute_type_params(ty, &args, &mut types.env)
                            };
                            (name, substituted)
                        })
                        .collect(),
                )
            }
            _ => None,
        };
    }

    // Enum variants — substitute the enum's type args so a generic enum's payload
    // resolves concretely (`Result[U64, String]` → `Err`'s field is `String`, not
    // the bare param `E`; `Option[U64]` → `Some`'s field is `U64`).
    if let Ty::Enum(enum_ty) = scrutinee {
        let args = enum_ty.type_args.clone();
        let variants = match types.env[enum_ty.def] {
            vx_types::TypeDef::Enum(ref edef) => edef.variants.clone(),
            _ => Vec::new(),
        };
        for variant_id in variants {
            let fields_snapshot: Option<Vec<(String, TypeId)>> =
                if let vx_types::TypeDef::Struct(ref sdef) = types.env[variant_id]
                    && sdef.name == variant_name
                {
                    Some(sdef.fields.iter().map(|f| (f.name.clone(), f.ty)).collect())
                } else {
                    None
                };
            if let Some(fields_snapshot) = fields_snapshot {
                let fields = fields_snapshot
                    .into_iter()
                    .map(|(name, ty)| {
                        let substituted = if args.is_empty() {
                            ty
                        } else {
                            substitute_type_params(ty, &args, &mut types.env)
                        };
                        (name, substituted)
                    })
                    .collect();
                return Some(fields);
            }
        }
    }

    // Fallback: look up by name.
    let type_ids: Vec<vx_types::TypeDefId> = types.env.lookup_def(variant_name).to_vec();
    for id in type_ids {
        if let vx_types::TypeDef::Struct(ref sdef) = types.env[id]
            && sdef.parent_enum.is_some()
        {
            return Some(sdef.fields.iter().map(|f| (f.name.clone(), f.ty)).collect());
        }
    }

    None
}
