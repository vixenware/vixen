//! Type parameter substitution helpers.
//!
//! These are exact ports of the legacy free functions for type substitution:
//! `substitute_type_params`, `substitute_bound_type_params`, `bind_type_params`,
//! and `infer_type_params`.

use vx_types::{FunctionTy, TupleTy, Ty};

/// Replace `Ty::Param(index)` with the concrete type at that index in `substitutions`.
/// Used after all type params have been resolved to concrete types.
pub fn substitute_type_params(ty: &Ty, substitutions: &[Ty]) -> Ty {
    match ty {
        Ty::Param(param) => substitutions
            .get(param.index as usize)
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        Ty::Optional(inner) => Ty::Optional(Box::new(substitute_type_params(inner, substitutions))),
        Ty::Array(inner) => Ty::Array(Box::new(substitute_type_params(inner, substitutions))),
        Ty::Map(key, value) => Ty::Map(
            Box::new(substitute_type_params(key, substitutions)),
            Box::new(substitute_type_params(value, substitutions)),
        ),
        Ty::Stream(inner) => Ty::Stream(Box::new(substitute_type_params(inner, substitutions))),
        Ty::Tuple(tuple) => Ty::Tuple(TupleTy {
            elements: tuple
                .elements
                .iter()
                .map(|element| substitute_type_params(element, substitutions))
                .collect(),
        }),
        Ty::Function(function) => Ty::Function(FunctionTy {
            params: function
                .params
                .iter()
                .map(|param| substitute_type_params(param, substitutions))
                .collect(),
            ret: Box::new(substitute_type_params(&function.ret, substitutions)),
            capabilities: function.capabilities.clone(),
        }),
        _ => ty.clone(),
    }
}

/// Like `substitute_type_params` but substitutions are optional (partially resolved).
/// Unresolved params are left as-is.
pub fn substitute_bound_type_params(ty: &Ty, substitutions: &[Option<Ty>]) -> Ty {
    match ty {
        Ty::Param(param) => substitutions
            .get(param.index as usize)
            .and_then(|ty| ty.clone())
            .unwrap_or_else(|| ty.clone()),
        Ty::Optional(inner) => {
            Ty::Optional(Box::new(substitute_bound_type_params(inner, substitutions)))
        }
        Ty::Array(inner) => Ty::Array(Box::new(substitute_bound_type_params(inner, substitutions))),
        Ty::Map(key, value) => Ty::Map(
            Box::new(substitute_bound_type_params(key, substitutions)),
            Box::new(substitute_bound_type_params(value, substitutions)),
        ),
        Ty::Stream(inner) => {
            Ty::Stream(Box::new(substitute_bound_type_params(inner, substitutions)))
        }
        Ty::Tuple(tuple) => Ty::Tuple(TupleTy {
            elements: tuple
                .elements
                .iter()
                .map(|element| substitute_bound_type_params(element, substitutions))
                .collect(),
        }),
        Ty::Function(function) => Ty::Function(FunctionTy {
            params: function
                .params
                .iter()
                .map(|param| substitute_bound_type_params(param, substitutions))
                .collect(),
            ret: Box::new(substitute_bound_type_params(&function.ret, substitutions)),
            capabilities: function.capabilities.clone(),
        }),
        _ => ty.clone(),
    }
}

/// Walk `expected` and `actual` in lockstep; when `expected` is `Ty::Param`,
/// record `actual` in `substitutions[param.index]` (first-write wins).
pub fn bind_type_params(expected: &Ty, actual: &Ty, substitutions: &mut [Option<Ty>]) {
    match expected {
        Ty::Param(param) => {
            if let Some(slot) = substitutions.get_mut(param.index as usize)
                && slot.is_none()
            {
                *slot = Some(actual.clone());
            }
        }
        Ty::Optional(inner) => {
            if let Ty::Optional(actual_inner) = actual {
                bind_type_params(inner, actual_inner, substitutions);
            }
        }
        Ty::Array(inner) => {
            if let Ty::Array(actual_inner) = actual {
                bind_type_params(inner, actual_inner, substitutions);
            }
        }
        Ty::Map(expected_key, expected_value) => {
            if let Ty::Map(actual_key, actual_value) = actual {
                bind_type_params(expected_key, actual_key, substitutions);
                bind_type_params(expected_value, actual_value, substitutions);
            }
        }
        Ty::Stream(inner) => {
            if let Ty::Stream(actual_inner) = actual {
                bind_type_params(inner, actual_inner, substitutions);
            }
        }
        Ty::Tuple(expected_tuple) => {
            if let Ty::Tuple(actual_tuple) = actual {
                for (expected, actual) in expected_tuple.elements.iter().zip(&actual_tuple.elements)
                {
                    bind_type_params(expected, actual, substitutions);
                }
            }
        }
        Ty::Function(expected_function) => {
            if let Ty::Function(actual_function) = actual {
                for (expected, actual) in
                    expected_function.params.iter().zip(&actual_function.params)
                {
                    bind_type_params(expected, actual, substitutions);
                }
                bind_type_params(&expected_function.ret, &actual_function.ret, substitutions);
            }
        }
        _ => {}
    }
}

/// Alias for `bind_type_params` — infer type param bindings from an argument type.
pub fn infer_type_params(expected: &Ty, actual: &Ty, substitutions: &mut [Option<Ty>]) {
    bind_type_params(expected, actual, substitutions);
}
