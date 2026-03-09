//! Runtime value representation.
//!
//! Every Vixen value at runtime is a `Value`. Values are immutable and
//! cheaply cloneable (via Arc for compound types).

use std::fmt;
use std::sync::Arc;

use crate::instr::FnDef;

/// A runtime value.
#[derive(Debug, Clone)]
pub enum Value {
    /// 64-bit unsigned integer
    U64(u64),
    /// 64-bit signed integer
    I64(i64),
    /// 64-bit float
    F64(f64),
    /// Boolean
    Bool(bool),
    /// String
    String(Arc<str>),
    /// Unit `()`
    Unit,
    /// Tuple
    Tuple(Arc<[Value]>),
    /// Struct instance
    Struct(StructValue),
    /// A function (closure or named function)
    Function(FunctionValue),
}

/// A struct value: type name + fields.
#[derive(Debug, Clone)]
pub struct StructValue {
    pub type_name: Arc<str>,
    pub fields: Arc<[(Arc<str>, Value)]>,
}

/// A function value — either a named top-level function or a closure.
#[derive(Debug, Clone)]
pub struct FunctionValue {
    pub def: Arc<FnDef>,
    /// Captured bindings from the enclosing scope (for closures).
    pub captures: Arc<[(Arc<str>, Value)]>,
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::U64(_) => "U64",
            Value::I64(_) => "I64",
            Value::F64(_) => "F64",
            Value::Bool(_) => "Bool",
            Value::String(_) => "String",
            Value::Unit => "()",
            Value::Tuple(_) => "Tuple",
            Value::Struct(_) => "Struct",
            Value::Function(_) => "Fn",
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::U64(n) => write!(f, "{n}"),
            Value::I64(n) => write!(f, "{n}"),
            Value::F64(n) => write!(f, "{n}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::String(s) => write!(f, "{s}"),
            Value::Unit => write!(f, "()"),
            Value::Tuple(elems) => {
                write!(f, "(")?;
                for (i, elem) in elems.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{elem}")?;
                }
                if elems.len() == 1 {
                    write!(f, ",")?;
                }
                write!(f, ")")
            }
            Value::Struct(s) => {
                write!(f, "{} {{ ", s.type_name)?;
                for (i, (name, val)) in s.fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{name}: {val}")?;
                }
                write!(f, " }}")
            }
            Value::Function(func) => write!(f, "<fn {}>", func.def.name),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::U64(a), Value::U64(b)) => a == b,
            (Value::I64(a), Value::I64(b)) => a == b,
            (Value::F64(a), Value::F64(b)) => a.to_bits() == b.to_bits(),
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Unit, Value::Unit) => true,
            (Value::Tuple(a), Value::Tuple(b)) => a == b,
            (Value::Struct(a), Value::Struct(b)) => {
                a.type_name == b.type_name && a.fields == b.fields
            }
            _ => false,
        }
    }
}

impl Eq for Value {}
