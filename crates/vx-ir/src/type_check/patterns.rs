//! Pattern typing — assign types to bindings introduced by patterns.

use vx_types::{Ty, TypeId};

use crate::PatternId;
use crate::pattern::PatternKind;

use super::TypeChecker;
use super::subst::substitute_type_params;

/// Assign types to all bindings introduced by a pattern, given the type of the matched value.
pub(crate) fn type_pattern(tc: &mut TypeChecker, pat_id: PatternId, ty: TypeId) {
    let kind = tc.arena[pat_id].kind.clone();
    tc.arena[pat_id].ty = Some(ty);
    let is_poison = tc.types.env.get_ty(ty).is_poison();

    match kind {
        PatternKind::Binding(bp) => {
            if !is_poison {
                tc.arena[bp.binding].ty = Some(ty);
            }
        }
        PatternKind::Wildcard
        | PatternKind::Literal(_)
        | PatternKind::Unit
        | PatternKind::Error => {
            // No bindings to type.
        }
        PatternKind::Tuple(tp) => {
            let ty_clone = tc.types.env.get_ty(ty).clone();
            if let Ty::Tuple(tuple_ty) = ty_clone {
                for (elem_pat, elem_ty) in tp.elements.iter().zip(tuple_ty.0.iter()) {
                    type_pattern(tc, *elem_pat, *elem_ty);
                }
            }
        }
        PatternKind::Struct(sp) => {
            let ty_clone = tc.types.env.get_ty(ty).clone();
            if let Ty::Struct(struct_ty) = ty_clone
                && let vx_types::TypeDef::Struct(ref sdef) = tc.types.env[struct_ty.def]
            {
                // Snapshot to release the immutable borrow.
                let fields_snapshot: Vec<(String, TypeId)> =
                    sdef.fields.iter().map(|f| (f.name.clone(), f.ty)).collect();
                for field in &sp.fields {
                    let field_ty = fields_snapshot
                        .iter()
                        .find(|(n, _)| *n == field.name)
                        .map(|(_, t)| *t)
                        .unwrap_or_else(|| tc.poison());
                    type_pattern(tc, field.pattern, field_ty);
                }
            }
        }
        PatternKind::Variant(vp) => {
            let variant_fields = resolve_variant_fields(tc, &vp.name, ty);
            if let Some(ref vp_fields) = vp.fields {
                for field in vp_fields {
                    let field_ty = variant_fields
                        .as_ref()
                        .and_then(|vf| vf.iter().find(|(n, _)| *n == field.name))
                        .map(|(_, t)| *t)
                        .unwrap_or_else(|| tc.poison());
                    type_pattern(tc, field.pattern, field_ty);
                }
            }
        }
        PatternKind::Array(arr) => {
            let ty_clone = tc.types.env.get_ty(ty).clone();
            if let Ty::Array(arr_ty) = ty_clone {
                for &elem_pat in &arr.elements {
                    type_pattern(tc, elem_pat, arr_ty.0);
                }
            }
        }
        PatternKind::ArrayRest(rest) => {
            if let Some(binding_id) = rest.binding {
                // Rest binding gets the full Array type.
                if matches!(tc.types.env.get_ty(ty), Ty::Array(_)) {
                    tc.arena[binding_id].ty = Some(ty);
                }
            }
        }
        PatternKind::At(at) => {
            if !is_poison {
                tc.arena[at.binding].ty = Some(ty);
            }
            type_pattern(tc, at.inner, ty);
        }
        PatternKind::Or(or) => {
            for &alt in &or.alternatives {
                type_pattern(tc, alt, ty);
            }
        }
    }
}

/// Resolve a variant pattern's field types from the scrutinee type. Returns
/// the variant's field (name, [`TypeId`]) pairs, with type parameters
/// substituted from the scrutinee.
fn resolve_variant_fields(
    tc: &mut TypeChecker,
    variant_name: &str,
    scrutinee_ty: TypeId,
) -> Option<Vec<(String, TypeId)>> {
    let scrutinee = tc.types.env.get_ty(scrutinee_ty).clone();
    // Optional variants: substitute type param.
    if let Ty::Optional(opt) = scrutinee {
        let inner = opt.0;
        let option_id = tc
            .types
            .env
            .lookup_def("Option")
            .iter()
            .copied()
            .find(|&id| matches!(tc.types.env.get_def(id), vx_types::TypeDef::Enum(_)))?;
        let vx_types::TypeDef::Enum(enum_def) = tc.types.env.get_def(option_id) else {
            return None;
        };
        let variant_id = enum_def.variants.iter().copied().find(|vid| {
            matches!(
                tc.types.env.get_def(*vid),
                vx_types::TypeDef::Struct(s) if s.name == variant_name
            )
        })?;
        let vx_types::TypeDef::Struct(ref sdef) = tc.types.env[variant_id] else {
            return None;
        };
        let fields_snapshot: Vec<(String, TypeId)> =
            sdef.fields.iter().map(|f| (f.name.clone(), f.ty)).collect();
        let substitutions = [inner];
        let fields = fields_snapshot
            .into_iter()
            .map(|(name, ty)| {
                let substituted = substitute_type_params(ty, &substitutions, &mut tc.types.env);
                (name, substituted)
            })
            .collect();
        return Some(fields);
    }

    // Enum variants — substitute the enum's type args so a generic enum's payload
    // resolves concretely (`Result[U64, String]` → `Err`'s field is `String`, not
    // the bare param `E`), mirroring the Optional path above.
    if let Ty::Enum(enum_ty) = scrutinee {
        let args = enum_ty.type_args.clone();
        let variants = match tc.types.env[enum_ty.def] {
            vx_types::TypeDef::Enum(ref edef) => edef.variants.clone(),
            _ => Vec::new(),
        };
        for variant_id in variants {
            let fields_snapshot: Option<Vec<(String, TypeId)>> =
                if let vx_types::TypeDef::Struct(ref sdef) = tc.types.env[variant_id]
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
                            substitute_type_params(ty, &args, &mut tc.types.env)
                        };
                        (name, substituted)
                    })
                    .collect();
                return Some(fields);
            }
        }
    }

    // Fallback: look up by name.
    let type_ids: Vec<vx_types::TypeDefId> = tc.types.env.lookup_def(variant_name).to_vec();
    for id in type_ids {
        if let vx_types::TypeDef::Struct(ref sdef) = tc.types.env[id]
            && sdef.parent_enum.is_some()
        {
            return Some(sdef.fields.iter().map(|f| (f.name.clone(), f.ty)).collect());
        }
    }

    None
}
