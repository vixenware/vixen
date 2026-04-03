//! Tiny Vixen -> Kajit JIT experiments.

mod abi;
pub use abi::{
    CompiledStrictFragmentOutput, MAX_STRICT_FRAGMENT_ABI_WORDS, StrictFragmentAbiError,
    StrictFragmentAbiLayout, UnmanagedStrictFragmentString, compile_strict_fragment,
    execute_compiled_copying_strict_fragment_strings, execute_compiled_unmanaged_strict_fragment,
};

mod analysis_adapter;
pub use analysis_adapter::{
    SourceLoweringError, compile_source_text_function, compile_source_text_u64_add_function,
    interpret_source_text_function, interpret_source_text_string_function,
    interpret_source_text_u64_add_function, lower_source_text_hir, lower_source_text_ir,
    lower_source_text_strict_fragment, lower_source_text_u64_add_hir, lower_source_text_u64_add_ir,
    lower_source_text_u64_add_strict_fragment, run_source_text_u64_add_function,
};

mod interpreter;
pub use interpreter::{interpret, interpret_string, interpret_u64};

mod kajit_backend;
pub use kajit_backend::KajitBackendError;

mod strict_fragment;
pub use strict_fragment::{
    StrictFragmentBinaryOp, StrictFragmentExpr, StrictFragmentFunction,
    StrictFragmentInterpretError, StrictFragmentIntrinsic, StrictFragmentLocal,
    StrictFragmentLocalId, StrictFragmentParam, StrictFragmentStmt, StrictFragmentType,
    StrictFragmentValue, StrictFragmentValueKind,
};

use kajit::{CompiledFunction, ir::IrFunc};
use kajit_hir::Module;

pub fn lower_u64_add_strict_fragment() -> StrictFragmentFunction {
    StrictFragmentFunction {
        name: "add_u64".to_owned(),
        params: vec![
            StrictFragmentParam {
                local: StrictFragmentLocalId(0),
                name: "lhs".to_owned(),
                ty: StrictFragmentType::U64,
            },
            StrictFragmentParam {
                local: StrictFragmentLocalId(1),
                name: "rhs".to_owned(),
                ty: StrictFragmentType::U64,
            },
        ],
        return_type: StrictFragmentType::U64,
        body: vec![StrictFragmentStmt::Return(StrictFragmentExpr::Binary {
            op: StrictFragmentBinaryOp::Add,
            lhs: Box::new(StrictFragmentExpr::Local(StrictFragmentLocalId(0))),
            rhs: Box::new(StrictFragmentExpr::Local(StrictFragmentLocalId(1))),
        })],
    }
}

pub fn lower_u64_add_hir() -> Result<Module, KajitBackendError> {
    kajit_backend::lower_hir(&lower_u64_add_strict_fragment())
}

pub fn lower_u64_add_hir_text() -> Result<String, KajitBackendError> {
    lower_u64_add_hir().map(|module| module.to_string())
}

pub fn lower_u64_add_ir() -> Result<IrFunc, KajitBackendError> {
    kajit_backend::lower_ir(&lower_u64_add_strict_fragment())
}

pub fn lower_u64_add_ir_text() -> Result<String, KajitBackendError> {
    lower_u64_add_ir().map(|func| func.to_string())
}

pub fn compile_u64_add() -> Result<CompiledFunction, KajitBackendError> {
    kajit_backend::compile(&lower_u64_add_strict_fragment())
}

pub fn interpret_u64_add(lhs: u64, rhs: u64) -> Result<u64, StrictFragmentInterpretError> {
    interpret_u64(&lower_u64_add_strict_fragment(), &[lhs, rhs])
}

pub unsafe fn run_u64_add(lhs: u64, rhs: u64) -> Result<u64, KajitBackendError> {
    let compiled = compile_u64_add()?;
    let add: unsafe extern "C" fn(u64, u64) -> u64 =
        unsafe { core::mem::transmute(compiled.as_ptr()) };
    Ok(unsafe { add(lhs, rhs) })
}

#[cfg(test)]
mod tests;
