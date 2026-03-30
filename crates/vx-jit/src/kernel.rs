//! Tiny strict-kernel IR shared by the interpreter and Kajit backend.

use std::fmt::{self, Display, Formatter};

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KernelLocalId(pub u32);

impl Display for KernelLocalId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelType {
    U64,
    Bool,
    String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelParam {
    pub local: KernelLocalId,
    pub name: String,
    pub ty: KernelType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelLocal {
    pub local: KernelLocalId,
    pub name: String,
    pub ty: KernelType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelBinaryOp {
    Add,
    Eq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelIntrinsic {
    StringLen,
    StringSlice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelExpr {
    Local(KernelLocalId),
    U64(u64),
    Bool(bool),
    String(String),
    Binary {
        op: KernelBinaryOp,
        lhs: Box<KernelExpr>,
        rhs: Box<KernelExpr>,
    },
    Intrinsic {
        intrinsic: KernelIntrinsic,
        args: Vec<KernelExpr>,
    },
    Concat(Vec<KernelExpr>),
    If {
        condition: Box<KernelExpr>,
        then_expr: Box<KernelExpr>,
        else_expr: Box<KernelExpr>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelStmt {
    Let {
        local: KernelLocal,
        value: KernelExpr,
    },
    Return(KernelExpr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelFunction {
    pub name: String,
    pub params: Vec<KernelParam>,
    pub return_type: KernelType,
    pub body: Vec<KernelStmt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum KernelInterpretError {
    #[error("arity mismatch: expected {expected}, got {actual}")]
    ArityMismatch { expected: usize, actual: usize },
    #[error("missing local l{0}")]
    MissingLocal(KernelLocalId),
    #[error("local l{local} expected {expected:?}, found {found:?}")]
    TypeMismatch {
        local: KernelLocalId,
        expected: KernelType,
        found: KernelValueKind,
    },
    #[error("binary op {op:?} does not support lhs={lhs:?} rhs={rhs:?}")]
    InvalidBinaryOp {
        op: KernelBinaryOp,
        lhs: KernelValueKind,
        rhs: KernelValueKind,
    },
    #[error("intrinsic {intrinsic:?} received invalid args {args:?}")]
    InvalidIntrinsicArgs {
        intrinsic: KernelIntrinsic,
        args: Vec<KernelValueKind>,
    },
    #[error("if condition must be Bool, found {0:?}")]
    InvalidCondition(KernelValueKind),
    #[error("expected function to return a value")]
    MissingReturn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelValueKind {
    U64,
    Bool,
    String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelValue {
    U64(u64),
    Bool(bool),
    String(String),
}

impl KernelValue {
    pub const fn kind(&self) -> KernelValueKind {
        match self {
            Self::U64(_) => KernelValueKind::U64,
            Self::Bool(_) => KernelValueKind::Bool,
            Self::String(_) => KernelValueKind::String,
        }
    }
}
