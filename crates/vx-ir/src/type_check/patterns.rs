//! Pattern typing — assign types to bindings introduced by patterns.

use vx_types::Ty;

use crate::PatternId;
use crate::pattern::PatternKind;

use super::TypeChecker;
use super::subst::substitute_type_params;

/// Assign types to all bindings introduced by a pattern, given the type of the matched value.
pub(crate) fn type_pattern(tc: &mut TypeChecker, pat_id: PatternId, ty: &Ty) {
    let kind = tc.arena[pat_id].kind.clone();
    tc.arena[pat_id].ty = Some(ty.clone());

    match kind {
        PatternKind::Binding(bp) => {
            if !ty.is_poison() {
                tc.arena[bp.binding].ty = Some(ty.clone());
            }
        }
        PatternKind::Wildcard
        | PatternKind::Literal(_)
        | PatternKind::Unit
        | PatternKind::Error => {
            // No bindings to type.
        }
        PatternKind::Tuple(tp) => {
            if let Ty::Tuple(tuple_ty) = ty {
                for (elem_pat, elem_ty) in tp.elements.iter().zip(tuple_ty.elements.iter()) {
                    type_pattern(tc, *elem_pat, elem_ty);
                }
            }
        }
        PatternKind::Struct(sp) => {
            if let Ty::Struct(struct_ty) = ty
                && let vx_types::TypeDef::Struct(ref sdef) = tc.types.env[struct_ty.id]
            {
                for field in &sp.fields {
                    let field_ty = sdef
                        .fields
                        .iter()
                        .find(|f| f.name == field.name)
                        .map(|f| f.ty.clone())
                        .unwrap_or(Ty::Poison);
                    type_pattern(tc, field.pattern, &field_ty);
                }
            }
        }
        PatternKind::Variant(vp) => {
            let variant_fields = resolve_variant_fields(tc, &vp.name, ty);
            if let Some(ref fields) = vp.fields {
                for field in fields {
                    let field_ty = variant_fields
                        .as_ref()
                        .and_then(|vf| vf.iter().find(|f| f.name == field.name))
                        .map(|f| f.ty.clone())
                        .unwrap_or(Ty::Poison);
                    type_pattern(tc, field.pattern, &field_ty);
                }
            }
        }
        PatternKind::Array(arr) => {
            if let Ty::Array(elem_ty) = ty {
                for &elem_pat in &arr.elements {
                    type_pattern(tc, elem_pat, elem_ty);
                }
            }
        }
        PatternKind::ArrayRest(rest) => {
            if let Some(binding_id) = rest.binding {
                // Rest binding gets Array type.
                if let Ty::Array(_) = ty {
                    tc.arena[binding_id].ty = Some(ty.clone());
                }
            }
        }
        PatternKind::At(at) => {
            if !ty.is_poison() {
                tc.arena[at.binding].ty = Some(ty.clone());
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

/// Resolve a variant pattern's field types from the scrutinee type.
fn resolve_variant_fields(
    tc: &TypeChecker,
    variant_name: &str,
    scrutinee_ty: &Ty,
) -> Option<Vec<vx_types::FieldDef>> {
    // Optional variants: substitute type param.
    if let Ty::Optional(inner) = scrutinee_ty {
        let option_id = tc
            .types
            .env
            .lookup("Option")
            .iter()
            .copied()
            .find(|&id| matches!(tc.types.env.get(id), vx_types::TypeDef::Enum(_)))?;
        let vx_types::TypeDef::Enum(enum_def) = tc.types.env.get(option_id) else {
            return None;
        };
        let variant_id = enum_def.variants.iter().copied().find(|vid| {
            matches!(
                tc.types.env.get(*vid),
                vx_types::TypeDef::Struct(s) if s.name == variant_name
            )
        })?;
        let vx_types::TypeDef::Struct(ref sdef) = tc.types.env[variant_id] else {
            return None;
        };
        let fields = sdef
            .fields
            .iter()
            .map(|f| vx_types::FieldDef {
                name: f.name.clone(),
                ty: substitute_type_params(&f.ty, &[(*inner.clone())]),
                attrs: f.attrs.clone(),
                has_default: f.has_default,
                default_value: f.default_value.clone(),
                name_span: f.name_span,
            })
            .collect();
        return Some(fields);
    }

    // Enum variants.
    if let Ty::Enum(enum_ty) = scrutinee_ty
        && let vx_types::TypeDef::Enum(ref edef) = tc.types.env[enum_ty.id]
    {
        for &variant_id in &edef.variants {
            if let vx_types::TypeDef::Struct(ref sdef) = tc.types.env[variant_id]
                && sdef.name == variant_name
            {
                return Some(sdef.fields.clone());
            }
        }
    }

    // Fallback: look up by name.
    let type_ids = tc.types.env.lookup(variant_name);
    for &id in type_ids {
        if let vx_types::TypeDef::Struct(ref sdef) = tc.types.env[id]
            && sdef.parent_enum.is_some()
        {
            return Some(sdef.fields.clone());
        }
    }

    None
}
