//! Unified lowering entry point.
//!
//! For each kernel, attempts strict lowering (→ Kajit `VixenTypedFunction`) first.
//! Falls back to interpreted `KernelOp` bytecode if the kernel contains anything
//! Kajit can't compile.

use crate::{
    AnalysisResult, SingleSourceBridge,
    exec_slice::{self, ExecSliceError, ExecutableProgram, KernelKind},
    lower_strict::StrictLowerer,
};

/// Lower an analysis result into an `ExecutableProgram`, trying strict (Kajit)
/// compilation for each body first and falling back to interpreted bytecode.
pub fn lower_executable_program(
    analysis: &AnalysisResult,
    bridge: &SingleSourceBridge,
) -> Result<ExecutableProgram, ExecSliceError> {
    // First, do the full interpreted lowering as a baseline.
    let mut program = exec_slice::lower_executable_program(analysis, bridge)?;

    // Now try to upgrade each body to strict where possible.
    let lowerer = StrictLowerer::new();
    for kernel in &analysis.kernels {
        let Some(body) = program.bodies.get_mut(&kernel.body) else {
            continue;
        };

        if body.kernel_kind != KernelKind::Interpreted {
            continue;
        }

        // Attempt strict lowering — if it fails, keep the interpreted version
        if let Ok(vtf) = lowerer.try_lower_kernel(kernel, bridge) {
            body.kernel_kind = KernelKind::Strict { vtf };
        }
    }

    Ok(program)
}
