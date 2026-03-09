//! Runtime value representation.
//!
//! Values are immutable. Compound values can carry pending children, and
//! `Value::Pending` points at a node whose result will be demanded later.

use std::fmt;
use std::sync::Arc;

use crate::instr::FnDef;
use crate::node::NodeId;

/// A runtime value.
#[derive(Debug, Clone)]
pub enum Value {
    /// A lazily computed value backed by a node.
    Pending(NodeId),
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
    /// Captured bindings from the enclosing scope, kept in sorted order.
    pub captures: Arc<[(Arc<str>, Value)]>,
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Pending(_) => "Pending",
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

    pub fn is_fully_resolved(&self) -> bool {
        match self {
            Value::Pending(_) => false,
            Value::Tuple(elems) => elems.iter().all(Self::is_fully_resolved),
            Value::Struct(s) => s.fields.iter().all(|(_, value)| value.is_fully_resolved()),
            Value::Function(f) => f
                .captures
                .iter()
                .all(|(_, value)| value.is_fully_resolved()),
            _ => true,
        }
    }

    pub fn key(&self) -> Option<String> {
        match self {
            Value::Pending(_) => None,
            Value::U64(n) => Some(format!("u64:{n}")),
            Value::I64(n) => Some(format!("i64:{n}")),
            Value::F64(n) => Some(format!("f64:{}", float_key(*n))),
            Value::Bool(b) => Some(format!("bool:{b}")),
            Value::String(s) => Some(format!("string:{s:?}")),
            Value::Unit => Some("unit".into()),
            Value::Tuple(elems) => {
                let parts = elems.iter().map(Value::key).collect::<Option<Vec<_>>>()?;
                Some(format!("tuple:[{}]", parts.join(",")))
            }
            Value::Struct(s) => {
                let parts = s
                    .fields
                    .iter()
                    .map(|(name, value)| Some(format!("{name}:{}", value.key()?)))
                    .collect::<Option<Vec<_>>>()?;
                Some(format!("struct:{}{{{}}}", s.type_name, parts.join(",")))
            }
            Value::Function(function) => Some(function.key()?),
        }
    }
}

impl StructValue {
    pub fn key(&self) -> Option<String> {
        Value::Struct(self.clone()).key()
    }
}

impl FunctionValue {
    pub fn identity(&self) -> String {
        format!("{:?}", self.def)
    }

    pub fn key(&self) -> Option<String> {
        let captures = self
            .captures
            .iter()
            .map(|(name, value)| Some(format!("{name}:{}", value.key()?)))
            .collect::<Option<Vec<_>>>()?;
        Some(format!("fn:{}[{}]", self.identity(), captures.join(",")))
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Pending(node_id) => write!(f, "<pending {}>", node_id.0),
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
            (Value::Pending(a), Value::Pending(b)) => a == b,
            (Value::U64(a), Value::U64(b)) => a == b,
            (Value::I64(a), Value::I64(b)) => a == b,
            (Value::F64(a), Value::F64(b)) => float_eq(*a, *b),
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Unit, Value::Unit) => true,
            (Value::Tuple(a), Value::Tuple(b)) => a == b,
            (Value::Struct(a), Value::Struct(b)) => {
                a.type_name == b.type_name && a.fields == b.fields
            }
            (Value::Function(a), Value::Function(b)) => {
                a.identity() == b.identity() && a.captures == b.captures
            }
            _ => false,
        }
    }
}

impl Eq for Value {}

fn float_eq(left: f64, right: f64) -> bool {
    if left.is_nan() && right.is_nan() {
        true
    } else if left == 0.0 && right == 0.0 {
        true
    } else {
        left == right
    }
}

fn float_key(value: f64) -> String {
    if value.is_nan() {
        "nan".into()
    } else if value == 0.0 {
        "0".into()
    } else {
        value.to_bits().to_string()
    }
}
