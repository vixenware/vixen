//! Structured executor requests for the `exec` builtin.

use std::sync::Arc;

use crate::poison::PoisonError;
use crate::transport::{ShippedFunction, TransportModule};
use crate::value::{PathValue, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecArgValue {
    Literal(String),
    Path(PathValue),
}

impl ExecArgValue {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Literal(value) => value,
            Self::Path(path) => path.as_str(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecMount {
    pub path: PathValue,
    pub value: Value,
}

#[derive(Debug, Clone)]
pub struct ExecRequest {
    pub args: Vec<ExecArgValue>,
    pub env: Vec<(String, ExecArgValue)>,
    pub mounts: Vec<ExecMount>,
    pub writable_roots: Vec<PathValue>,
    pub module: TransportModule,
    pub observer: ShippedFunction,
}

pub type ExecHandler = dyn Fn(ExecRequest) -> Result<Value, PoisonError> + Send + Sync + 'static;

pub type SharedExecHandler = Arc<ExecHandler>;
