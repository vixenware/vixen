use std::collections::BTreeMap;

use crate::kernel::{
    KernelBinaryOp, KernelExpr, KernelFunction, KernelInterpretError, KernelIntrinsic, KernelLocal,
    KernelLocalId, KernelStmt, KernelType, KernelValue, KernelValueKind,
};

pub fn interpret(
    function: &KernelFunction,
    args: &[KernelValue],
) -> Result<KernelValue, KernelInterpretError> {
    if function.params.len() != args.len() {
        return Err(KernelInterpretError::ArityMismatch {
            expected: function.params.len(),
            actual: args.len(),
        });
    }

    let mut locals = BTreeMap::new();
    for (param, value) in function.params.iter().zip(args.iter().cloned()) {
        ensure_value_type(param.local, param.ty, &value)?;
        locals.insert(param.local, value);
    }

    eval_block(&function.body, &mut locals)?.ok_or(KernelInterpretError::MissingReturn)
}

pub fn interpret_u64(function: &KernelFunction, args: &[u64]) -> Result<u64, KernelInterpretError> {
    let args = args
        .iter()
        .copied()
        .map(KernelValue::U64)
        .collect::<Vec<_>>();
    match interpret(function, &args)? {
        KernelValue::U64(value) => Ok(value),
        other => Err(KernelInterpretError::TypeMismatch {
            local: KernelLocalId(u32::MAX),
            expected: KernelType::U64,
            found: other.kind(),
        }),
    }
}

pub fn interpret_string(
    function: &KernelFunction,
    args: &[KernelValue],
) -> Result<String, KernelInterpretError> {
    match interpret(function, args)? {
        KernelValue::String(value) => Ok(value),
        other => Err(KernelInterpretError::TypeMismatch {
            local: KernelLocalId(u32::MAX),
            expected: KernelType::String,
            found: other.kind(),
        }),
    }
}

fn eval_block(
    stmts: &[KernelStmt],
    locals: &mut BTreeMap<KernelLocalId, KernelValue>,
) -> Result<Option<KernelValue>, KernelInterpretError> {
    for stmt in stmts {
        match stmt {
            KernelStmt::Let { local, value } => {
                let value = eval_expr(value, locals)?;
                bind_local(local, value, locals)?;
            }
            KernelStmt::Return(expr) => {
                return eval_expr(expr, locals).map(Some);
            }
        }
    }

    Ok(None)
}

fn eval_expr(
    expr: &KernelExpr,
    locals: &BTreeMap<KernelLocalId, KernelValue>,
) -> Result<KernelValue, KernelInterpretError> {
    match expr {
        KernelExpr::Local(local) => locals
            .get(local)
            .cloned()
            .ok_or(KernelInterpretError::MissingLocal(*local)),
        KernelExpr::U64(value) => Ok(KernelValue::U64(*value)),
        KernelExpr::Bool(value) => Ok(KernelValue::Bool(*value)),
        KernelExpr::String(value) => Ok(KernelValue::String(value.clone())),
        KernelExpr::Binary { op, lhs, rhs } => {
            let lhs = eval_expr(lhs, locals)?;
            let rhs = eval_expr(rhs, locals)?;
            eval_binary(*op, lhs, rhs)
        }
        KernelExpr::Intrinsic { intrinsic, args } => {
            let args = args
                .iter()
                .map(|arg| eval_expr(arg, locals))
                .collect::<Result<Vec<_>, _>>()?;
            eval_intrinsic(*intrinsic, args)
        }
        KernelExpr::Concat(parts) => {
            let mut out = String::new();
            for part in parts {
                let value = eval_expr(part, locals)?;
                match value {
                    KernelValue::String(value) => out.push_str(&value),
                    KernelValue::U64(value) => out.push_str(&value.to_string()),
                    KernelValue::Bool(value) => out.push_str(if value { "true" } else { "false" }),
                }
            }
            Ok(KernelValue::String(out))
        }
        KernelExpr::If {
            condition,
            then_expr,
            else_expr,
        } => {
            let condition = eval_expr(condition, locals)?;
            let KernelValue::Bool(condition) = condition else {
                return Err(KernelInterpretError::InvalidCondition(condition.kind()));
            };
            if condition {
                eval_expr(then_expr, locals)
            } else {
                eval_expr(else_expr, locals)
            }
        }
    }
}

fn eval_binary(
    op: KernelBinaryOp,
    lhs: KernelValue,
    rhs: KernelValue,
) -> Result<KernelValue, KernelInterpretError> {
    match (op, lhs, rhs) {
        (KernelBinaryOp::Add, KernelValue::U64(lhs), KernelValue::U64(rhs)) => {
            Ok(KernelValue::U64(lhs.wrapping_add(rhs)))
        }
        (KernelBinaryOp::Eq, KernelValue::U64(lhs), KernelValue::U64(rhs)) => {
            Ok(KernelValue::Bool(lhs == rhs))
        }
        (op, lhs, rhs) => Err(KernelInterpretError::InvalidBinaryOp {
            op,
            lhs: lhs.kind(),
            rhs: rhs.kind(),
        }),
    }
}

fn eval_intrinsic(
    intrinsic: KernelIntrinsic,
    args: Vec<KernelValue>,
) -> Result<KernelValue, KernelInterpretError> {
    match intrinsic {
        KernelIntrinsic::StringLen => match args.as_slice() {
            [KernelValue::String(value)] => Ok(KernelValue::U64(value.len() as u64)),
            _ => Err(KernelInterpretError::InvalidIntrinsicArgs {
                intrinsic,
                args: args.iter().map(KernelValue::kind).collect(),
            }),
        },
        KernelIntrinsic::StringSlice => match args.as_slice() {
            [
                KernelValue::String(value),
                KernelValue::U64(start),
                KernelValue::U64(end),
            ] => {
                let start = (*start as usize).min(value.len());
                let end = (*end as usize).min(value.len());
                let start = start.min(end);
                Ok(KernelValue::String(value[start..end].to_owned()))
            }
            _ => Err(KernelInterpretError::InvalidIntrinsicArgs {
                intrinsic,
                args: args.iter().map(KernelValue::kind).collect(),
            }),
        },
    }
}

fn bind_local(
    local: &KernelLocal,
    value: KernelValue,
    locals: &mut BTreeMap<KernelLocalId, KernelValue>,
) -> Result<(), KernelInterpretError> {
    ensure_value_type(local.local, local.ty, &value)?;
    locals.insert(local.local, value);
    Ok(())
}

fn ensure_value_type(
    local: KernelLocalId,
    expected: KernelType,
    value: &KernelValue,
) -> Result<(), KernelInterpretError> {
    let found = value.kind();
    let matches = match (expected, found) {
        (KernelType::U64, KernelValueKind::U64) => true,
        (KernelType::Bool, KernelValueKind::Bool) => true,
        (KernelType::String, KernelValueKind::String) => true,
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err(KernelInterpretError::TypeMismatch {
            local,
            expected,
            found,
        })
    }
}
