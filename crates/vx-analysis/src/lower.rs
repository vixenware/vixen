//! Unified lowering entry point.
//!
//! For each kernel, attempts strict lowering (→ Kajit `VixenTypedFunction`) first.
//! Falls back to interpreted `KernelOp` bytecode if the kernel contains anything
//! Kajit can't compile.

use std::collections::BTreeMap;

use crate::{
    AnalysisResult, BodyId, SingleSourceBridge,
    exec_slice::{
        self, ExecSliceError, ExecutableBody, ExecutableProgram, ExecutableRuntimeNode, KernelKind,
        ResultSource, SlotId,
    },
    lower_strict::StrictLowerer,
};

/// Lower an analysis result into an `ExecutableProgram`, trying strict (Kajit)
/// compilation for each body first and falling back to interpreted bytecode.
pub fn lower_executable_program(
    analysis: &AnalysisResult,
    bridge: &SingleSourceBridge,
) -> Result<ExecutableProgram, ExecSliceError> {
    // Try the full interpreted lowering. If it succeeds, upgrade strict bodies.
    if let Ok(mut program) = exec_slice::lower_executable_program(analysis, bridge) {
        upgrade_strict_kernels(&mut program, analysis, bridge);
        return Ok(program);
    }

    // Interpreted lowering failed (some bodies use ops only strict supports).
    // Build program body-by-body: try strict, skip on failure.
    let lowerer = StrictLowerer::new();
    let mut bodies = BTreeMap::new();

    for analyzed in &analysis.bodies {
        let kernel = analysis.kernels.iter().find(|k| k.body == analyzed.id);
        let kernel_id = analyzed
            .kernel_entries
            .first()
            .map(|e| e.kernel)
            .unwrap_or(crate::KernelId(analyzed.id.0));

        if let Some(kernel) = kernel {
            if let Ok(vtf) = lowerer.try_lower_kernel(kernel, bridge) {
                bodies.insert(
                    analyzed.id,
                    ExecutableBody {
                        id: analyzed.id,
                        kernel: kernel_id,
                        kernel_kind: KernelKind::Strict { vtf },
                        params: vec![],
                        ops: vec![],
                        result_source: ResultSource::PendingRuntimeBoundary,
                        runtime_boundary_extractions: vec![],
                        local_call_dependencies: vec![],
                    },
                );
            }
        }
    }

    let entry = pick_entry(
        &analysis.bodies.iter().map(|b| b.id).collect::<Vec<_>>(),
        bridge,
    );

    Ok(ExecutableProgram {
        entry,
        bodies,
        runtime_nodes: BTreeMap::new(),
    })
}

fn upgrade_strict_kernels(
    program: &mut ExecutableProgram,
    analysis: &AnalysisResult,
    bridge: &SingleSourceBridge,
) {
    let lowerer = StrictLowerer::new();
    for kernel in &analysis.kernels {
        let Some(body) = program.bodies.get_mut(&kernel.body) else {
            continue;
        };
        if body.kernel_kind != KernelKind::Interpreted {
            continue;
        }
        if let Ok(vtf) = lowerer.try_lower_kernel(kernel, bridge) {
            body.kernel_kind = KernelKind::Strict { vtf };
        }
    }
}

fn pick_entry(body_ids: &[BodyId], bridge: &SingleSourceBridge) -> BodyId {
    // Prefer main, then first body
    for body in &bridge.bodies {
        if body.name == "main" {
            return body.id;
        }
    }
    body_ids.first().copied().unwrap_or(BodyId(0))
}
