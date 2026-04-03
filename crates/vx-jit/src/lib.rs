//! Tiny Vixen -> Kajit JIT experiments.

mod abi;
pub use abi::{
    CompiledKernelOutput, KernelAbiError, MAX_ABI_WORDS, UnmanagedKernelString, compile_kernel,
    execute_compiled_copying_strings, execute_compiled_unmanaged,
};

mod analysis_adapter;
pub use analysis_adapter::{
    SourceLoweringError, compile_source_text_function, compile_source_text_u64_add_function,
    interpret_source_text_function, interpret_source_text_string_function,
    interpret_source_text_u64_add_function, lower_source_text_hir, lower_source_text_ir,
    lower_source_text_kernel, lower_source_text_u64_add_hir, lower_source_text_u64_add_ir,
    lower_source_text_u64_add_kernel, run_source_text_u64_add_function,
};

mod interpreter;
pub use interpreter::{interpret, interpret_string, interpret_u64};

mod kajit_backend;
pub use kajit_backend::KajitBackendError;

mod kernel;
pub use kernel::{
    KernelBinaryOp, KernelExpr, KernelFunction, KernelInterpretError, KernelIntrinsic, KernelLocal,
    KernelLocalId, KernelParam, KernelStmt, KernelType, KernelValue, KernelValueKind,
};

use kajit::{CompiledFunction, ir::IrFunc};
use kajit_hir::Module;

pub fn lower_u64_add_kernel() -> KernelFunction {
    KernelFunction {
        name: "add_u64".to_owned(),
        params: vec![
            KernelParam {
                local: KernelLocalId(0),
                name: "lhs".to_owned(),
                ty: KernelType::U64,
            },
            KernelParam {
                local: KernelLocalId(1),
                name: "rhs".to_owned(),
                ty: KernelType::U64,
            },
        ],
        return_type: KernelType::U64,
        body: vec![KernelStmt::Return(KernelExpr::Binary {
            op: KernelBinaryOp::Add,
            lhs: Box::new(KernelExpr::Local(KernelLocalId(0))),
            rhs: Box::new(KernelExpr::Local(KernelLocalId(1))),
        })],
    }
}

pub fn lower_u64_add_hir() -> Result<Module, KajitBackendError> {
    kajit_backend::lower_hir(&lower_u64_add_kernel())
}

pub fn lower_u64_add_hir_text() -> Result<String, KajitBackendError> {
    lower_u64_add_hir().map(|module| module.to_string())
}

pub fn lower_u64_add_ir() -> Result<IrFunc, KajitBackendError> {
    kajit_backend::lower_ir(&lower_u64_add_kernel())
}

pub fn lower_u64_add_ir_text() -> Result<String, KajitBackendError> {
    lower_u64_add_ir().map(|func| func.to_string())
}

pub fn compile_u64_add() -> Result<CompiledFunction, KajitBackendError> {
    kajit_backend::compile(&lower_u64_add_kernel())
}

pub fn interpret_u64_add(lhs: u64, rhs: u64) -> Result<u64, KernelInterpretError> {
    interpret_u64(&lower_u64_add_kernel(), &[lhs, rhs])
}

pub unsafe fn run_u64_add(lhs: u64, rhs: u64) -> Result<u64, KajitBackendError> {
    let compiled = compile_u64_add()?;
    let add: unsafe extern "C" fn(u64, u64) -> u64 =
        unsafe { core::mem::transmute(compiled.as_ptr()) };
    Ok(unsafe { add(lhs, rhs) })
}

#[cfg(test)]
mod tests;
