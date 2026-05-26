//! Display implementations for type representations.
//!
//! The full type tree requires a [`TypeEnv`] to resolve sub-references —
//! see [`Ty::display_with_env`]. The leaf-level display impls in this module
//! cover types that can be rendered without an env.
//!
//! [`TypeEnv`]: crate::TypeEnv
//! [`Ty::display_with_env`]: crate::Ty::display_with_env

use std::fmt;

use crate::{Capability, InferVar, Scalar, TypeParam};

impl fmt::Display for Scalar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scalar::U64 => write!(f, "U64"),
            Scalar::I64 => write!(f, "I64"),
            Scalar::F64 => write!(f, "F64"),
            Scalar::Bool => write!(f, "Bool"),
            Scalar::String => write!(f, "String"),
        }
    }
}

impl fmt::Display for TypeParam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl fmt::Display for InferVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "?{}", self.0)
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.name)
    }
}
