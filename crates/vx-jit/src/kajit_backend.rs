use kajit::{CompiledFunction, ir::IrFunc};
use kajit_hir::{
    BinaryOp, Literal, LocalId, Module, Type, VixenLoweringError, VixenTypedExpr,
    VixenTypedFunction, VixenTypedLocal, VixenTypedParam, VixenTypedStmt,
};
use thiserror::Error;

use crate::kernel::{
    KernelBinaryOp, KernelExpr, KernelFunction, KernelIntrinsic, KernelLocal, KernelParam,
    KernelStmt, KernelType,
};

#[derive(Debug, Error)]
pub enum KajitBackendError {
    #[error("kernel IR feature not yet supported by Kajit backend: {0}")]
    Unsupported(&'static str),
    #[error("Kajit lowering failed: {0:?}")]
    Kajit(VixenLoweringError),
}

pub fn lower_hir(function: &KernelFunction) -> Result<Module, KajitBackendError> {
    Module::new()
        .lower_vixen_typed_function_into_module(&to_vixen_typed_function(function)?)
        .map_err(KajitBackendError::Kajit)
}

pub fn lower_ir(function: &KernelFunction) -> Result<IrFunc, KajitBackendError> {
    lower_hir(function).map(|module| kajit::lower_hir_module(&module))
}

pub fn compile(function: &KernelFunction) -> Result<CompiledFunction, KajitBackendError> {
    lower_hir(function).map(|module| kajit::compile_hir_module(&module))
}

fn to_vixen_typed_function(
    function: &KernelFunction,
) -> Result<VixenTypedFunction, KajitBackendError> {
    Ok(VixenTypedFunction {
        name: function.name.clone(),
        params: function.params.iter().map(to_vixen_typed_param).collect(),
        locals: collect_locals(function),
        return_type: to_vixen_type(function.return_type),
        body: function
            .body
            .iter()
            .map(to_vixen_typed_stmt)
            .collect::<Result<Vec<_>, _>>()?,
        comment: Some("lowered from vx-jit kernel IR".to_owned()),
    })
}

fn collect_locals(function: &KernelFunction) -> Vec<VixenTypedLocal> {
    let mut locals = Vec::new();
    collect_locals_from_stmts(&function.body, &mut locals);
    locals
}

fn collect_locals_from_stmts(stmts: &[KernelStmt], out: &mut Vec<VixenTypedLocal>) {
    for stmt in stmts {
        match stmt {
            KernelStmt::Let { local, .. } => out.push(to_vixen_typed_local(local)),
            KernelStmt::Return(_) => {}
        }
    }
}

fn to_vixen_typed_param(param: &KernelParam) -> VixenTypedParam {
    VixenTypedParam {
        local: LocalId::new(param.local.0),
        name: param.name.clone(),
        ty: to_vixen_type(param.ty),
    }
}

fn to_vixen_typed_local(local: &KernelLocal) -> VixenTypedLocal {
    VixenTypedLocal {
        local: LocalId::new(local.local.0),
        name: local.name.clone(),
        ty: to_vixen_type(local.ty),
    }
}

fn to_vixen_typed_stmt(stmt: &KernelStmt) -> Result<VixenTypedStmt, KajitBackendError> {
    match stmt {
        KernelStmt::Let { local, value } => Ok(VixenTypedStmt::Let {
            local: LocalId::new(local.local.0),
            value: to_vixen_typed_expr(value)?,
        }),
        KernelStmt::Return(expr) => Ok(VixenTypedStmt::Return(Some(to_vixen_typed_expr(expr)?))),
    }
}

fn to_vixen_typed_expr(expr: &KernelExpr) -> Result<VixenTypedExpr, KajitBackendError> {
    match expr {
        KernelExpr::Local(local) => Ok(VixenTypedExpr::Local(LocalId::new(local.0))),
        KernelExpr::U64(value) => Ok(VixenTypedExpr::Literal(Literal::Integer(*value))),
        KernelExpr::Bool(value) => Ok(VixenTypedExpr::Literal(Literal::Bool(*value))),
        KernelExpr::String(value) => Ok(VixenTypedExpr::Literal(Literal::String(value.clone()))),
        KernelExpr::Binary { op, lhs, rhs } => Ok(VixenTypedExpr::Binary {
            op: to_vixen_binary_op(*op)?,
            lhs: Box::new(to_vixen_typed_expr(lhs)?),
            rhs: Box::new(to_vixen_typed_expr(rhs)?),
        }),
        KernelExpr::Intrinsic { intrinsic, args: _ } => match intrinsic {
            KernelIntrinsic::StringLen | KernelIntrinsic::StringSlice => {
                Err(KajitBackendError::Unsupported("String methods"))
            }
        },
        KernelExpr::Concat(_) => Err(KajitBackendError::Unsupported("string concatenation")),
        KernelExpr::If { .. } => Err(KajitBackendError::Unsupported("if expressions")),
    }
}

fn to_vixen_binary_op(op: KernelBinaryOp) -> Result<BinaryOp, KajitBackendError> {
    match op {
        KernelBinaryOp::Add => Ok(BinaryOp::Add),
        KernelBinaryOp::Eq => Ok(BinaryOp::Eq),
    }
}

fn to_vixen_type(ty: KernelType) -> Type {
    match ty {
        KernelType::U64 => Type::u(64),
        KernelType::Bool => Type::bool(),
        KernelType::String => unreachable!("strings are not yet supported by the Kajit backend"),
    }
}
