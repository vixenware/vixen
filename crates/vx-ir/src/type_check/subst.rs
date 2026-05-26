//! Type parameter substitution helpers.
//!
//! Operate over interned [`TypeId`]s, fetching `Ty` shapes from the
//! [`TypeEnv`] and re-interning rewritten types.

use vx_types::{ArrayTy, FunctionTy, MapTy, OptionalTy, StreamTy, TupleTy, Ty, TypeEnv, TypeId};

/// Replace `Ty::Param(index)` with the concrete type at that index in `substitutions`.
/// Used after all type params have been resolved to concrete types.
pub fn substitute_type_params(ty: TypeId, substitutions: &[TypeId], env: &mut TypeEnv) -> TypeId {
    match env.get_ty(ty).clone() {
        Ty::Param(param) => substitutions
            .get(param.index as usize)
            .copied()
            .unwrap_or(ty),
        Ty::Optional(OptionalTy(inner)) => {
            let inner = substitute_type_params(inner, substitutions, env);
            env.intern_optional(inner)
        }
        Ty::Array(ArrayTy(inner)) => {
            let inner = substitute_type_params(inner, substitutions, env);
            env.intern_array(inner)
        }
        Ty::Map(MapTy { key, value }) => {
            let key = substitute_type_params(key, substitutions, env);
            let value = substitute_type_params(value, substitutions, env);
            env.intern_map(key, value)
        }
        Ty::Stream(StreamTy(inner)) => {
            let inner = substitute_type_params(inner, substitutions, env);
            env.intern_stream(inner)
        }
        Ty::Tuple(TupleTy(elems)) => {
            let elems: Vec<TypeId> = elems
                .into_iter()
                .map(|e| substitute_type_params(e, substitutions, env))
                .collect();
            env.intern_tuple(elems)
        }
        Ty::Function(FunctionTy {
            params,
            ret,
            capabilities,
        }) => {
            let params: Vec<TypeId> = params
                .into_iter()
                .map(|p| substitute_type_params(p, substitutions, env))
                .collect();
            let ret = substitute_type_params(ret, substitutions, env);
            env.intern_function(params, ret, capabilities)
        }
        Ty::Struct(s) => {
            let args: Vec<TypeId> = s
                .type_args
                .into_iter()
                .map(|a| substitute_type_params(a, substitutions, env))
                .collect();
            env.intern_struct(s.def, args)
        }
        Ty::Enum(e) => {
            let args: Vec<TypeId> = e
                .type_args
                .into_iter()
                .map(|a| substitute_type_params(a, substitutions, env))
                .collect();
            env.intern_enum(e.def, args)
        }
        Ty::Newtype(n) => {
            let args: Vec<TypeId> = n
                .type_args
                .into_iter()
                .map(|a| substitute_type_params(a, substitutions, env))
                .collect();
            env.intern_newtype(n.def, args)
        }
        Ty::Alias(a) => {
            let args: Vec<TypeId> = a
                .type_args
                .into_iter()
                .map(|x| substitute_type_params(x, substitutions, env))
                .collect();
            env.intern_alias(a.def, args)
        }
        Ty::Opaque(o) => {
            let args: Vec<TypeId> = o
                .type_args
                .into_iter()
                .map(|x| substitute_type_params(x, substitutions, env))
                .collect();
            env.intern_opaque(o.def, args)
        }
        Ty::Scalar(_) | Ty::Unit | Ty::Poison | Ty::Infer(_) => ty,
    }
}

/// Like `substitute_type_params` but substitutions are optional (partially resolved).
/// Unresolved params are left as-is.
pub fn substitute_bound_type_params(
    ty: TypeId,
    substitutions: &[Option<TypeId>],
    env: &mut TypeEnv,
) -> TypeId {
    match env.get_ty(ty).clone() {
        Ty::Param(param) => substitutions
            .get(param.index as usize)
            .and_then(|x| *x)
            .unwrap_or(ty),
        Ty::Optional(OptionalTy(inner)) => {
            let inner = substitute_bound_type_params(inner, substitutions, env);
            env.intern_optional(inner)
        }
        Ty::Array(ArrayTy(inner)) => {
            let inner = substitute_bound_type_params(inner, substitutions, env);
            env.intern_array(inner)
        }
        Ty::Map(MapTy { key, value }) => {
            let key = substitute_bound_type_params(key, substitutions, env);
            let value = substitute_bound_type_params(value, substitutions, env);
            env.intern_map(key, value)
        }
        Ty::Stream(StreamTy(inner)) => {
            let inner = substitute_bound_type_params(inner, substitutions, env);
            env.intern_stream(inner)
        }
        Ty::Tuple(TupleTy(elems)) => {
            let elems: Vec<TypeId> = elems
                .into_iter()
                .map(|e| substitute_bound_type_params(e, substitutions, env))
                .collect();
            env.intern_tuple(elems)
        }
        Ty::Function(FunctionTy {
            params,
            ret,
            capabilities,
        }) => {
            let params: Vec<TypeId> = params
                .into_iter()
                .map(|p| substitute_bound_type_params(p, substitutions, env))
                .collect();
            let ret = substitute_bound_type_params(ret, substitutions, env);
            env.intern_function(params, ret, capabilities)
        }
        Ty::Struct(s) => {
            let args: Vec<TypeId> = s
                .type_args
                .into_iter()
                .map(|a| substitute_bound_type_params(a, substitutions, env))
                .collect();
            env.intern_struct(s.def, args)
        }
        Ty::Enum(e) => {
            let args: Vec<TypeId> = e
                .type_args
                .into_iter()
                .map(|a| substitute_bound_type_params(a, substitutions, env))
                .collect();
            env.intern_enum(e.def, args)
        }
        Ty::Newtype(n) => {
            let args: Vec<TypeId> = n
                .type_args
                .into_iter()
                .map(|a| substitute_bound_type_params(a, substitutions, env))
                .collect();
            env.intern_newtype(n.def, args)
        }
        Ty::Alias(a) => {
            let args: Vec<TypeId> = a
                .type_args
                .into_iter()
                .map(|x| substitute_bound_type_params(x, substitutions, env))
                .collect();
            env.intern_alias(a.def, args)
        }
        Ty::Opaque(o) => {
            let args: Vec<TypeId> = o
                .type_args
                .into_iter()
                .map(|x| substitute_bound_type_params(x, substitutions, env))
                .collect();
            env.intern_opaque(o.def, args)
        }
        Ty::Scalar(_) | Ty::Unit | Ty::Poison | Ty::Infer(_) => ty,
    }
}

/// Walk `expected` and `actual` in lockstep; when `expected` is `Ty::Param`,
/// record `actual` in `substitutions[param.index]` (first-write wins).
pub fn bind_type_params(
    expected: TypeId,
    actual: TypeId,
    substitutions: &mut [Option<TypeId>],
    env: &TypeEnv,
) {
    let expected_ty = env.get_ty(expected).clone();
    let actual_ty = env.get_ty(actual).clone();
    match expected_ty {
        Ty::Param(param) => {
            if let Some(slot) = substitutions.get_mut(param.index as usize)
                && slot.is_none()
            {
                *slot = Some(actual);
            }
        }
        Ty::Optional(OptionalTy(inner)) => {
            if let Ty::Optional(OptionalTy(actual_inner)) = actual_ty {
                bind_type_params(inner, actual_inner, substitutions, env);
            }
        }
        Ty::Array(ArrayTy(inner)) => {
            if let Ty::Array(ArrayTy(actual_inner)) = actual_ty {
                bind_type_params(inner, actual_inner, substitutions, env);
            }
        }
        Ty::Map(MapTy { key: ek, value: ev }) => {
            if let Ty::Map(MapTy { key: ak, value: av }) = actual_ty {
                bind_type_params(ek, ak, substitutions, env);
                bind_type_params(ev, av, substitutions, env);
            }
        }
        Ty::Stream(StreamTy(inner)) => {
            if let Ty::Stream(StreamTy(actual_inner)) = actual_ty {
                bind_type_params(inner, actual_inner, substitutions, env);
            }
        }
        Ty::Tuple(TupleTy(elems)) => {
            if let Ty::Tuple(TupleTy(actual_elems)) = actual_ty {
                for (e, a) in elems.iter().zip(actual_elems.iter()) {
                    bind_type_params(*e, *a, substitutions, env);
                }
            }
        }
        Ty::Function(FunctionTy {
            params: ep,
            ret: er,
            ..
        }) => {
            if let Ty::Function(FunctionTy {
                params: ap,
                ret: ar,
                ..
            }) = actual_ty
            {
                for (e, a) in ep.iter().zip(ap.iter()) {
                    bind_type_params(*e, *a, substitutions, env);
                }
                bind_type_params(er, ar, substitutions, env);
            }
        }
        _ => {}
    }
}

/// Alias for `bind_type_params` — infer type param bindings from an argument type.
pub fn infer_type_params(
    expected: TypeId,
    actual: TypeId,
    substitutions: &mut [Option<TypeId>],
    env: &TypeEnv,
) {
    bind_type_params(expected, actual, substitutions, env);
}
