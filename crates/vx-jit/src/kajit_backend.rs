use kajit::{CompiledFunction, ir::IrFunc};
use kajit_hir::{
    BinaryOp, FieldDef, LocalId, Module, Type, TypeDef, TypeDefId, TypeDefKind, VixenLoweringError,
    VixenTypedExpr, VixenTypedFunction, VixenTypedLocal, VixenTypedParam, VixenTypedStmt,
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
    let lowerer = KajitLowerer::new();
    lowerer
        .module
        .lower_vixen_typed_function_into_module(&lowerer.to_vixen_typed_function(function)?)
        .map_err(KajitBackendError::Kajit)
}

pub fn lower_ir(function: &KernelFunction) -> Result<IrFunc, KajitBackendError> {
    lower_hir(function).map(|module| kajit::lower_hir_module(&module))
}

pub fn compile(function: &KernelFunction) -> Result<CompiledFunction, KajitBackendError> {
    lower_hir(function).map(|module| kajit::compile_hir_module(&module))
}

struct KajitLowerer {
    module: Module,
    str_def: TypeDefId,
}

impl KajitLowerer {
    fn new() -> Self {
        let mut module = Module::new();
        let str_def = module.add_type_def(TypeDef {
            name: "Str".to_owned(),
            generic_params: Vec::new(),
            kind: TypeDefKind::Struct {
                fields: vec![
                    FieldDef {
                        name: "ptr".to_owned(),
                        ty: Type::u(64),
                        offset: None,
                    },
                    FieldDef {
                        name: "len".to_owned(),
                        ty: Type::u(64),
                        offset: None,
                    },
                ],
            },
            size: None,
            transparent: false,
        });
        Self { module, str_def }
    }

    fn str_type(&self) -> Type {
        Type::named(self.str_def, Vec::new())
    }

    fn to_vixen_typed_function(
        &self,
        function: &KernelFunction,
    ) -> Result<VixenTypedFunction, KajitBackendError> {
        Ok(VixenTypedFunction {
            name: function.name.clone(),
            params: function
                .params
                .iter()
                .map(|param| self.to_vixen_typed_param(param))
                .collect(),
            locals: self.collect_locals(function),
            return_type: self.to_vixen_type(function.return_type),
            body: function
                .body
                .iter()
                .map(|stmt| self.to_vixen_typed_stmt(stmt))
                .collect::<Result<Vec<_>, _>>()?,
            comment: Some("lowered from vx-jit kernel IR".to_owned()),
        })
    }

    fn collect_locals(&self, function: &KernelFunction) -> Vec<VixenTypedLocal> {
        let mut locals = Vec::new();
        self.collect_locals_from_stmts(&function.body, &mut locals);
        locals
    }

    fn collect_locals_from_stmts(&self, stmts: &[KernelStmt], out: &mut Vec<VixenTypedLocal>) {
        for stmt in stmts {
            match stmt {
                KernelStmt::Let { local, .. } => out.push(self.to_vixen_typed_local(local)),
                KernelStmt::Return(_) => {}
            }
        }
    }

    fn to_vixen_typed_param(&self, param: &KernelParam) -> VixenTypedParam {
        VixenTypedParam {
            local: LocalId::new(param.local.0),
            name: param.name.clone(),
            ty: self.to_vixen_type(param.ty),
        }
    }

    fn to_vixen_typed_local(&self, local: &KernelLocal) -> VixenTypedLocal {
        VixenTypedLocal {
            local: LocalId::new(local.local.0),
            name: local.name.clone(),
            ty: self.to_vixen_type(local.ty),
        }
    }

    fn to_vixen_typed_stmt(&self, stmt: &KernelStmt) -> Result<VixenTypedStmt, KajitBackendError> {
        match stmt {
            KernelStmt::Let { local, value } => Ok(VixenTypedStmt::Let {
                local: LocalId::new(local.local.0),
                value: self.to_vixen_typed_expr(value)?,
            }),
            KernelStmt::Return(expr) => self.to_vixen_typed_return_stmt(expr),
        }
    }

    fn to_vixen_typed_return_stmt(
        &self,
        expr: &KernelExpr,
    ) -> Result<VixenTypedStmt, KajitBackendError> {
        match expr {
            KernelExpr::If {
                condition,
                then_expr,
                else_expr,
            } => Ok(VixenTypedStmt::If {
                condition: self.to_vixen_typed_expr(condition)?,
                then_body: vec![self.to_vixen_typed_return_stmt(then_expr)?],
                else_body: vec![self.to_vixen_typed_return_stmt(else_expr)?],
            }),
            _ => Ok(VixenTypedStmt::Return(Some(
                self.to_vixen_typed_expr(expr)?,
            ))),
        }
    }

    fn to_vixen_typed_expr(&self, expr: &KernelExpr) -> Result<VixenTypedExpr, KajitBackendError> {
        match expr {
            KernelExpr::Local(local) => Ok(VixenTypedExpr::Local(LocalId::new(local.0))),
            KernelExpr::U64(value) => {
                Ok(VixenTypedExpr::Literal(kajit_hir::Literal::Integer(*value)))
            }
            KernelExpr::Bool(value) => {
                Ok(VixenTypedExpr::Literal(kajit_hir::Literal::Bool(*value)))
            }
            KernelExpr::String(value) => Ok(self.lower_string_literal(value)),
            KernelExpr::Binary { op, lhs, rhs } => Ok(VixenTypedExpr::Binary {
                op: to_vixen_binary_op(*op),
                lhs: Box::new(self.to_vixen_typed_expr(lhs)?),
                rhs: Box::new(self.to_vixen_typed_expr(rhs)?),
            }),
            KernelExpr::Intrinsic { intrinsic, args } => self.lower_intrinsic(*intrinsic, args),
            KernelExpr::Concat(_) => Err(KajitBackendError::Unsupported("string concatenation")),
            KernelExpr::If { .. } => Err(KajitBackendError::Unsupported("if expressions")),
        }
    }

    fn lower_string_literal(&self, value: &str) -> VixenTypedExpr {
        let leaked: &'static [u8] = Box::leak(value.as_bytes().to_vec().into_boxed_slice());
        VixenTypedExpr::Struct {
            def: self.str_def,
            fields: vec![
                (
                    "ptr".to_owned(),
                    VixenTypedExpr::Literal(kajit_hir::Literal::Integer(leaked.as_ptr() as u64)),
                ),
                (
                    "len".to_owned(),
                    VixenTypedExpr::Literal(kajit_hir::Literal::Integer(leaked.len() as u64)),
                ),
            ],
        }
    }

    fn lower_intrinsic(
        &self,
        intrinsic: KernelIntrinsic,
        args: &[KernelExpr],
    ) -> Result<VixenTypedExpr, KajitBackendError> {
        match intrinsic {
            KernelIntrinsic::StringLen => match args {
                [value] => Ok(VixenTypedExpr::Field {
                    base: Box::new(self.to_vixen_typed_expr(value)?),
                    field: "len".to_owned(),
                }),
                _ => Err(KajitBackendError::Unsupported("invalid StringLen args")),
            },
            KernelIntrinsic::StringSlice => match args {
                [value, start, end] => Ok(VixenTypedExpr::Struct {
                    def: self.str_def,
                    fields: vec![
                        (
                            "ptr".to_owned(),
                            VixenTypedExpr::Binary {
                                op: BinaryOp::Add,
                                lhs: Box::new(VixenTypedExpr::Field {
                                    base: Box::new(self.to_vixen_typed_expr(value)?),
                                    field: "ptr".to_owned(),
                                }),
                                rhs: Box::new(self.to_vixen_typed_expr(start)?),
                            },
                        ),
                        (
                            "len".to_owned(),
                            VixenTypedExpr::Binary {
                                op: BinaryOp::Sub,
                                lhs: Box::new(self.to_vixen_typed_expr(end)?),
                                rhs: Box::new(self.to_vixen_typed_expr(start)?),
                            },
                        ),
                    ],
                }),
                _ => Err(KajitBackendError::Unsupported("invalid StringSlice args")),
            },
        }
    }

    fn to_vixen_type(&self, ty: KernelType) -> Type {
        match ty {
            KernelType::U64 => Type::u(64),
            KernelType::Bool => Type::bool(),
            KernelType::String => self.str_type(),
        }
    }
}
fn to_vixen_binary_op(op: KernelBinaryOp) -> BinaryOp {
    match op {
        KernelBinaryOp::Add => BinaryOp::Add,
        KernelBinaryOp::Eq => BinaryOp::Eq,
    }
}
