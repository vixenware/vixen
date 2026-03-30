use kajit::{CompiledFunction, ir::IrFunc};
use kajit_hir::{
    BinaryOp, Literal, LocalId, Module, Type, VixenLoweringError, VixenTypedExpr,
    VixenTypedFunction, VixenTypedParam, VixenTypedStmt,
};

use crate::kernel::{KernelExpr, KernelFunction, KernelParam, KernelType};

pub fn lower_hir(function: &KernelFunction) -> Result<Module, VixenLoweringError> {
    Module::new().lower_vixen_typed_function_into_module(&to_vixen_typed_function(function))
}

pub fn lower_ir(function: &KernelFunction) -> Result<IrFunc, VixenLoweringError> {
    lower_hir(function).map(|module| kajit::lower_hir_module(&module))
}

pub fn compile(function: &KernelFunction) -> Result<CompiledFunction, VixenLoweringError> {
    lower_hir(function).map(|module| kajit::compile_hir_module(&module))
}

fn to_vixen_typed_function(function: &KernelFunction) -> VixenTypedFunction {
    VixenTypedFunction {
        name: function.name.clone(),
        params: function.params.iter().map(to_vixen_typed_param).collect(),
        locals: Vec::new(),
        return_type: to_vixen_type(function.return_type),
        body: vec![VixenTypedStmt::Return(Some(to_vixen_typed_expr(
            &function.body,
        )))],
        comment: Some("lowered from vx-jit kernel IR".to_owned()),
    }
}

fn to_vixen_typed_param(param: &KernelParam) -> VixenTypedParam {
    VixenTypedParam {
        local: LocalId::new(param.local.0),
        name: param.name.clone(),
        ty: to_vixen_type(param.ty),
    }
}

fn to_vixen_typed_expr(expr: &KernelExpr) -> VixenTypedExpr {
    match expr {
        KernelExpr::Local(local) => VixenTypedExpr::Local(LocalId::new(local.0)),
        KernelExpr::U64(value) => VixenTypedExpr::Literal(Literal::Integer(*value)),
        KernelExpr::Add(lhs, rhs) => VixenTypedExpr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(to_vixen_typed_expr(lhs)),
            rhs: Box::new(to_vixen_typed_expr(rhs)),
        },
    }
}

fn to_vixen_type(ty: KernelType) -> Type {
    match ty {
        KernelType::U64 => Type::u(64),
    }
}
