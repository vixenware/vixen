//! Display implementations for type representations.
//!
//! These produce Vixen-syntax type strings, e.g. `Fn(U64, U64) -> U64`,
//! `Array[String]`, `Option[T]`.

use std::fmt;

use crate::{Capability, FunctionTy, InferVar, Scalar, TupleTy, Ty, TypeParam};

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Scalar(s) => write!(f, "{s}"),
            Ty::Unit => write!(f, "()"),
            Ty::Struct(s) => {
                write!(f, "<struct#{}>", s.id.0)?;
                write_type_args(f, &s.type_args)
            }
            Ty::Enum(e) => {
                write!(f, "<enum#{}>", e.id.0)?;
                write_type_args(f, &e.type_args)
            }
            Ty::Newtype(n) => {
                write!(f, "<newtype#{}>", n.id.0)?;
                write_type_args(f, &n.type_args)
            }
            Ty::Alias(a) => {
                write!(f, "<alias#{}>", a.id.0)?;
                write_type_args(f, &a.type_args)
            }
            Ty::Tuple(t) => write!(f, "{t}"),
            Ty::Function(func) => write!(f, "{func}"),
            Ty::Optional(inner) => write!(f, "{inner}?"),
            Ty::Array(inner) => write!(f, "Array[{inner}]"),
            Ty::Map(k, v) => write!(f, "Map[{k}, {v}]"),
            Ty::Stream(inner) => write!(f, "Stream[{inner}]"),
            Ty::Param(p) => write!(f, "{p}"),
            Ty::Infer(v) => write!(f, "{v}"),
            Ty::Poison => write!(f, "<poison>"),
        }
    }
}

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

impl fmt::Display for TupleTy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "(")?;
        for (i, elem) in self.elements.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{elem}")?;
        }
        if self.elements.len() == 1 {
            write!(f, ",")?;
        }
        write!(f, ")")
    }
}

impl fmt::Display for FunctionTy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fn(")?;
        for (i, param) in self.params.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{param}")?;
        }
        write!(f, ")")?;
        for cap in &self.capabilities {
            write!(f, " {cap}")?;
        }
        write!(f, " -> {}", self.ret)
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.name)
    }
}

fn write_type_args(f: &mut fmt::Formatter<'_>, args: &[Ty]) -> fmt::Result {
    if !args.is_empty() {
        write!(f, "[")?;
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{arg}")?;
        }
        write!(f, "]")?;
    }
    Ok(())
}
