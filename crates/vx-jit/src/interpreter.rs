use std::collections::BTreeMap;

use crate::strict_fragment::{
    StrictFragmentBinaryOp, StrictFragmentExpr, StrictFragmentFunction,
    StrictFragmentInterpretError, StrictFragmentIntrinsic, StrictFragmentLocal,
    StrictFragmentLocalId, StrictFragmentStmt, StrictFragmentType, StrictFragmentValue,
    StrictFragmentValueKind,
};

pub fn interpret(
    function: &StrictFragmentFunction,
    args: &[StrictFragmentValue],
) -> Result<StrictFragmentValue, StrictFragmentInterpretError> {
    if function.params.len() != args.len() {
        return Err(StrictFragmentInterpretError::ArityMismatch {
            expected: function.params.len(),
            actual: args.len(),
        });
    }

    let mut locals = BTreeMap::new();
    for (param, value) in function.params.iter().zip(args.iter().cloned()) {
        ensure_value_type(param.local, param.ty, &value)?;
        locals.insert(param.local, value);
    }

    eval_block(&function.body, &mut locals)?.ok_or(StrictFragmentInterpretError::MissingReturn)
}

pub fn interpret_u64(
    function: &StrictFragmentFunction,
    args: &[u64],
) -> Result<u64, StrictFragmentInterpretError> {
    let args = args
        .iter()
        .copied()
        .map(StrictFragmentValue::U64)
        .collect::<Vec<_>>();
    match interpret(function, &args)? {
        StrictFragmentValue::U64(value) => Ok(value),
        other => Err(StrictFragmentInterpretError::TypeMismatch {
            local: StrictFragmentLocalId(u32::MAX),
            expected: StrictFragmentType::U64,
            found: other.kind(),
        }),
    }
}

pub fn interpret_string(
    function: &StrictFragmentFunction,
    args: &[StrictFragmentValue],
) -> Result<String, StrictFragmentInterpretError> {
    match interpret(function, args)? {
        StrictFragmentValue::String(value) => Ok(value),
        other => Err(StrictFragmentInterpretError::TypeMismatch {
            local: StrictFragmentLocalId(u32::MAX),
            expected: StrictFragmentType::String,
            found: other.kind(),
        }),
    }
}

fn eval_block(
    stmts: &[StrictFragmentStmt],
    locals: &mut BTreeMap<StrictFragmentLocalId, StrictFragmentValue>,
) -> Result<Option<StrictFragmentValue>, StrictFragmentInterpretError> {
    for stmt in stmts {
        match stmt {
            StrictFragmentStmt::Let { local, value } => {
                let value = eval_expr(value, locals)?;
                bind_local(local, value, locals)?;
            }
            StrictFragmentStmt::Return(expr) => {
                return eval_expr(expr, locals).map(Some);
            }
        }
    }

    Ok(None)
}

fn eval_expr(
    expr: &StrictFragmentExpr,
    locals: &BTreeMap<StrictFragmentLocalId, StrictFragmentValue>,
) -> Result<StrictFragmentValue, StrictFragmentInterpretError> {
    match expr {
        StrictFragmentExpr::Local(local) => locals
            .get(local)
            .cloned()
            .ok_or(StrictFragmentInterpretError::MissingLocal(*local)),
        StrictFragmentExpr::U64(value) => Ok(StrictFragmentValue::U64(*value)),
        StrictFragmentExpr::Bool(value) => Ok(StrictFragmentValue::Bool(*value)),
        StrictFragmentExpr::String(value) => Ok(StrictFragmentValue::String(value.clone())),
        StrictFragmentExpr::Binary { op, lhs, rhs } => {
            let lhs = eval_expr(lhs, locals)?;
            let rhs = eval_expr(rhs, locals)?;
            eval_binary(*op, lhs, rhs)
        }
        StrictFragmentExpr::Intrinsic { intrinsic, args } => {
            let args = args
                .iter()
                .map(|arg| eval_expr(arg, locals))
                .collect::<Result<Vec<_>, _>>()?;
            eval_intrinsic(*intrinsic, args)
        }
        StrictFragmentExpr::Concat(parts) => {
            let mut out = String::new();
            for part in parts {
                let value = eval_expr(part, locals)?;
                match value {
                    StrictFragmentValue::String(value) => out.push_str(&value),
                    StrictFragmentValue::U64(value) => out.push_str(&value.to_string()),
                    StrictFragmentValue::Bool(value) => {
                        out.push_str(if value { "true" } else { "false" })
                    }
                }
            }
            Ok(StrictFragmentValue::String(out))
        }
        StrictFragmentExpr::If {
            condition,
            then_expr,
            else_expr,
        } => {
            let condition = eval_expr(condition, locals)?;
            let StrictFragmentValue::Bool(condition) = condition else {
                return Err(StrictFragmentInterpretError::InvalidCondition(
                    condition.kind(),
                ));
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
    op: StrictFragmentBinaryOp,
    lhs: StrictFragmentValue,
    rhs: StrictFragmentValue,
) -> Result<StrictFragmentValue, StrictFragmentInterpretError> {
    match (op, lhs, rhs) {
        (
            StrictFragmentBinaryOp::Add,
            StrictFragmentValue::U64(lhs),
            StrictFragmentValue::U64(rhs),
        ) => Ok(StrictFragmentValue::U64(lhs.wrapping_add(rhs))),
        (
            StrictFragmentBinaryOp::Eq,
            StrictFragmentValue::U64(lhs),
            StrictFragmentValue::U64(rhs),
        ) => Ok(StrictFragmentValue::Bool(lhs == rhs)),
        (op, lhs, rhs) => Err(StrictFragmentInterpretError::InvalidBinaryOp {
            op,
            lhs: lhs.kind(),
            rhs: rhs.kind(),
        }),
    }
}

fn eval_intrinsic(
    intrinsic: StrictFragmentIntrinsic,
    args: Vec<StrictFragmentValue>,
) -> Result<StrictFragmentValue, StrictFragmentInterpretError> {
    match intrinsic {
        StrictFragmentIntrinsic::StringLen => match args.as_slice() {
            [StrictFragmentValue::String(value)] => {
                Ok(StrictFragmentValue::U64(value.len() as u64))
            }
            _ => Err(StrictFragmentInterpretError::InvalidIntrinsicArgs {
                intrinsic,
                args: args.iter().map(StrictFragmentValue::kind).collect(),
            }),
        },
        StrictFragmentIntrinsic::StringSlice => match args.as_slice() {
            [
                StrictFragmentValue::String(value),
                StrictFragmentValue::U64(start),
                StrictFragmentValue::U64(end),
            ] => {
                let start = (*start as usize).min(value.len());
                let end = (*end as usize).min(value.len());
                let start = start.min(end);
                Ok(StrictFragmentValue::String(value[start..end].to_owned()))
            }
            _ => Err(StrictFragmentInterpretError::InvalidIntrinsicArgs {
                intrinsic,
                args: args.iter().map(StrictFragmentValue::kind).collect(),
            }),
        },
    }
}

fn bind_local(
    local: &StrictFragmentLocal,
    value: StrictFragmentValue,
    locals: &mut BTreeMap<StrictFragmentLocalId, StrictFragmentValue>,
) -> Result<(), StrictFragmentInterpretError> {
    ensure_value_type(local.local, local.ty, &value)?;
    locals.insert(local.local, value);
    Ok(())
}

fn ensure_value_type(
    local: StrictFragmentLocalId,
    expected: StrictFragmentType,
    value: &StrictFragmentValue,
) -> Result<(), StrictFragmentInterpretError> {
    let found = value.kind();
    let matches = match (expected, found) {
        (StrictFragmentType::U64, StrictFragmentValueKind::U64) => true,
        (StrictFragmentType::Bool, StrictFragmentValueKind::Bool) => true,
        (StrictFragmentType::String, StrictFragmentValueKind::String) => true,
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err(StrictFragmentInterpretError::TypeMismatch {
            local,
            expected,
            found,
        })
    }
}
