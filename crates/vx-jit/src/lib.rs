//! Cranelift JIT backend for Vixen strict IR.

mod codegen;
mod fragment;
pub mod lower_to_strict;

pub use codegen::compile;
pub use fragment::{CompiledFragment, FragmentArg, FragmentResult};

pub use vx_jit_types::{
    LocalId, LocalInfo, StrictBinOp, StrictExpr, StrictFunction, StrictLiteral, StrictParam,
    StrictStmt, StrictType, StringOwnership,
};

#[cfg(test)]
mod tests;
