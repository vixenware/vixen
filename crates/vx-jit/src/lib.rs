//! Tiny Vixen -> Kajit JIT experiments.

mod abi;
pub use abi::{
    CompiledStrictFragmentOutput, MAX_STRICT_FRAGMENT_ABI_WORDS, StrictFragmentAbiError,
    StrictFragmentAbiLayout, StrictFragmentValue, StrictFragmentValueKind,
    UnmanagedStrictFragmentString, compile_strict_fragment,
    execute_compiled_copying_strict_fragment_strings, execute_compiled_unmanaged_strict_fragment,
};

mod kajit_backend;
pub use kajit_backend::KajitBackendError;
pub use kajit_backend::{lower_hir as lower_vtf_to_hir, lower_ir as lower_vtf_to_ir};
mod vixen_hir;
pub use vixen_hir::*;

pub mod legacy;
pub mod strict_from_ops;

use kajit::CompiledFunction;
use thiserror::Error;
use vx_jit_types::VixenTypedFunction;

/// Error from the unified strict-fragment pipeline.
#[derive(Debug, Error)]
pub enum UnifiedStrictError {
    #[error("function `{function_name}` was not lowered as strict")]
    NotStrict { function_name: String },
    #[error("unified lowering failed: {0}")]
    Lowering(String),
    #[error("compilation failed: {0}")]
    Compile(#[from] KajitBackendError),
    #[error("ABI error: {0}")]
    Abi(#[from] StrictFragmentAbiError),
}

/// Result of lowering source text through the unified path: the VTF plus
/// the module it was built on (needed for compilation).
pub struct UnifiedLowered {
    pub vtf: VixenTypedFunction,
    pub module: kajit_reprs::hir::Module,
}

/// Lower source text for a named function through the unified path.
/// Returns the `VixenTypedFunction` and its module if the function is strict.
pub fn lower_source_text_unified(
    source: &str,
    function_name: &str,
) -> Result<UnifiedLowered, UnifiedStrictError> {
    let output = vx_vir::compile::compile_source_with_types(source, function_name)
        .map_err(|e| UnifiedStrictError::Lowering(format!("{e}")))?;

    // Try strict lowering on each body, find the one matching function_name.
    eprintln!(
        "[unified] entry={:?}, original_body_count={}, bodies: {:?}",
        output.program.entry,
        output.original_body_count,
        output.program.bodies.keys().collect::<Vec<_>>()
    );
    for body in output.program.bodies.values() {
        if let Some(mut lowered) =
            strict_from_ops::try_strict_lower(body, &output.slot_types, output.original_body_count)
        {
            // The entry body is the named function.
            if body.id == output.program.entry {
                lowered.vtf.name = function_name.to_owned();
                eprintln!("[unified] VTF body:\n{:#?}", lowered.vtf.body);
                return Ok(UnifiedLowered {
                    vtf: lowered.vtf,
                    module: lowered.module,
                });
            }
        }
    }

    Err(UnifiedStrictError::NotStrict {
        function_name: function_name.to_owned(),
    })
}

/// Compile source text for a named function through the unified path.
pub fn compile_source_text_unified(
    source: &str,
    function_name: &str,
) -> Result<(CompiledFunction, VixenTypedFunction), UnifiedStrictError> {
    let lowered = lower_source_text_unified(source, function_name)?;
    let compiled = kajit_backend::compile(&lowered.vtf, &lowered.module)?;
    Ok((compiled, lowered.vtf))
}

/// Compile and execute a strict function from source text.
///
/// # Safety
///
/// Compiled code executes with full memory access.
pub unsafe fn execute_source_text_unified(
    source: &str,
    function_name: &str,
    args: &[StrictFragmentValue],
) -> Result<StrictFragmentValue, UnifiedStrictError> {
    let (compiled, vtf) = compile_source_text_unified(source, function_name)?;
    unsafe {
        execute_compiled_copying_strict_fragment_strings(&vtf, &compiled, args)
            .map_err(UnifiedStrictError::Abi)
    }
}

#[cfg(test)]
mod tests;
