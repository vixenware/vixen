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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelParam {
    pub local: KernelLocalId,
    pub name: String,
    pub ty: KernelType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelExpr {
    Local(KernelLocalId),
    U64(u64),
    Add(Box<KernelExpr>, Box<KernelExpr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelFunction {
    pub name: String,
    pub params: Vec<KernelParam>,
    pub return_type: KernelType,
    pub body: KernelExpr,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum KernelInterpretError {
    #[error("arity mismatch: expected {expected}, got {actual}")]
    ArityMismatch { expected: usize, actual: usize },
    #[error("missing local l{0}")]
    MissingLocal(KernelLocalId),
}
