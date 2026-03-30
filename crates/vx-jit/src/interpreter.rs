use std::collections::BTreeMap;

use crate::kernel::{KernelExpr, KernelFunction, KernelInterpretError, KernelLocalId, KernelType};

pub fn interpret_u64(function: &KernelFunction, args: &[u64]) -> Result<u64, KernelInterpretError> {
    if function.return_type != KernelType::U64 {
        return Err(KernelInterpretError::ArityMismatch {
            expected: function.params.len(),
            actual: args.len(),
        });
    }
    if function.params.len() != args.len() {
        return Err(KernelInterpretError::ArityMismatch {
            expected: function.params.len(),
            actual: args.len(),
        });
    }

    let locals = function
        .params
        .iter()
        .zip(args.iter().copied())
        .map(|(param, value)| (param.local, value))
        .collect::<BTreeMap<KernelLocalId, u64>>();

    interpret_expr_u64(&function.body, &locals)
}

fn interpret_expr_u64(
    expr: &KernelExpr,
    locals: &BTreeMap<KernelLocalId, u64>,
) -> Result<u64, KernelInterpretError> {
    match expr {
        KernelExpr::Local(local) => locals
            .get(local)
            .copied()
            .ok_or(KernelInterpretError::MissingLocal(*local)),
        KernelExpr::U64(value) => Ok(*value),
        KernelExpr::Add(lhs, rhs) => {
            let lhs = interpret_expr_u64(lhs, locals)?;
            let rhs = interpret_expr_u64(rhs, locals)?;
            Ok(lhs.wrapping_add(rhs))
        }
    }
}
