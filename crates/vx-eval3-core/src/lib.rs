//! Slice-0 lazy-core IR for `eval3`.

mod examples;
pub use examples::{program_a_module, program_b_module, program_c_module, program_d_module};

mod ir;
pub use ir::{
    Block, BlockId, CallableSig, FieldKey, Kernel, KernelId, Layout, LayoutId, LayoutKind,
    LocalDecl, LocalId, Module, NamedField, ObsLiteral, ProductField, ReturnType, Stmt, Terminator,
    Type,
};

mod lowering;
pub use lowering::{LoweringError, lower_program};

#[cfg(test)]
mod tests;
