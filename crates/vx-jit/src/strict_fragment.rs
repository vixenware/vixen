//! Tiny strict-fragment IR shared by the interpreter and Kajit backend.

use std::fmt::{self, Display, Formatter};

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StrictFragmentLocalId(pub u32);

impl Display for StrictFragmentLocalId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictFragmentType {
    U64,
    Bool,
    String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictFragmentParam {
    pub local: StrictFragmentLocalId,
    pub name: String,
    pub ty: StrictFragmentType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictFragmentLocal {
    pub local: StrictFragmentLocalId,
    pub name: String,
    pub ty: StrictFragmentType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictFragmentBinaryOp {
    Add,
    Eq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictFragmentIntrinsic {
    StringLen,
    StringSlice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrictFragmentExpr {
    Local(StrictFragmentLocalId),
    U64(u64),
    Bool(bool),
    String(String),
    Binary {
        op: StrictFragmentBinaryOp,
        lhs: Box<StrictFragmentExpr>,
        rhs: Box<StrictFragmentExpr>,
    },
    Intrinsic {
        intrinsic: StrictFragmentIntrinsic,
        args: Vec<StrictFragmentExpr>,
    },
    Concat(Vec<StrictFragmentExpr>),
    If {
        condition: Box<StrictFragmentExpr>,
        then_expr: Box<StrictFragmentExpr>,
        else_expr: Box<StrictFragmentExpr>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrictFragmentStmt {
    Let {
        local: StrictFragmentLocal,
        value: StrictFragmentExpr,
    },
    Return(StrictFragmentExpr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictFragmentFunction {
    pub name: String,
    pub params: Vec<StrictFragmentParam>,
    pub return_type: StrictFragmentType,
    pub body: Vec<StrictFragmentStmt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StrictFragmentInterpretError {
    #[error("arity mismatch: expected {expected}, got {actual}")]
    ArityMismatch { expected: usize, actual: usize },
    #[error("missing local l{0}")]
    MissingLocal(StrictFragmentLocalId),
    #[error("local l{local} expected {expected:?}, found {found:?}")]
    TypeMismatch {
        local: StrictFragmentLocalId,
        expected: StrictFragmentType,
        found: StrictFragmentValueKind,
    },
    #[error("binary op {op:?} does not support lhs={lhs:?} rhs={rhs:?}")]
    InvalidBinaryOp {
        op: StrictFragmentBinaryOp,
        lhs: StrictFragmentValueKind,
        rhs: StrictFragmentValueKind,
    },
    #[error("intrinsic {intrinsic:?} received invalid args {args:?}")]
    InvalidIntrinsicArgs {
        intrinsic: StrictFragmentIntrinsic,
        args: Vec<StrictFragmentValueKind>,
    },
    #[error("if condition must be Bool, found {0:?}")]
    InvalidCondition(StrictFragmentValueKind),
    #[error("expected function to return a value")]
    MissingReturn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictFragmentValueKind {
    U64,
    Bool,
    String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrictFragmentValue {
    U64(u64),
    Bool(bool),
    String(String),
}

impl StrictFragmentValue {
    pub const fn kind(&self) -> StrictFragmentValueKind {
        match self {
            Self::U64(_) => StrictFragmentValueKind::U64,
            Self::Bool(_) => StrictFragmentValueKind::Bool,
            Self::String(_) => StrictFragmentValueKind::String,
        }
    }
}
