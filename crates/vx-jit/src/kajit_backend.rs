//! Kajit backend: compiles a `VixenTypedFunction` (from kajit-hir) into
//! executable machine code via the Kajit pipeline.

use kajit::{CompiledFunction, ir::IrFunc};
use kajit_hir::{Module, VixenLoweringError, VixenTypedFunction};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum KajitBackendError {
    #[error("Kajit lowering failed: {0:?}")]
    Kajit(VixenLoweringError),
}

/// Lower a `VixenTypedFunction` into a Kajit HIR `Module`.
///
/// The function must already have its Str type registered on a module —
/// the `StrictLowerer` in `vx-analysis` handles that. This function creates
/// a fresh module and lowers the VTF into it.
pub fn lower_hir(vtf: &VixenTypedFunction, module: &Module) -> Result<Module, KajitBackendError> {
    module
        .lower_vixen_typed_function_into_module(vtf)
        .map_err(KajitBackendError::Kajit)
}

/// Lower a `VixenTypedFunction` into Kajit IR.
pub fn lower_ir(vtf: &VixenTypedFunction, module: &Module) -> Result<IrFunc, KajitBackendError> {
    lower_hir(vtf, module).map(|m| kajit::lower_hir_module(&m))
}

/// Compile a `VixenTypedFunction` into executable machine code.
pub fn compile(
    vtf: &VixenTypedFunction,
    module: &Module,
) -> Result<CompiledFunction, KajitBackendError> {
    lower_hir(vtf, module).map(|m| kajit::compile_hir_module(&m))
}
