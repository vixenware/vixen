use kajit::{CompiledFunction, ir::IrFunc};
use kajit_hir::{
    BinaryOp, FieldDef, Literal, LocalId, Module, Type, TypeDef, TypeDefId, TypeDefKind,
    VixenCallableRef, VixenLoweringError, VixenTypedExpr, VixenTypedFunction, VixenTypedLocal,
    VixenTypedParam, VixenTypedStmt,
};
use thiserror::Error;

use crate::strict_fragment::{
    StrictFragmentBinaryOp, StrictFragmentExpr, StrictFragmentFunction, StrictFragmentIntrinsic,
    StrictFragmentLocal, StrictFragmentParam, StrictFragmentStmt, StrictFragmentType,
};

#[derive(Debug, Error)]
pub enum KajitBackendError {
    #[error("strict fragment IR feature not yet supported by Kajit backend: {0}")]
    Unsupported(&'static str),
    #[error("Kajit lowering failed: {0:?}")]
    Kajit(VixenLoweringError),
}

pub fn lower_hir(function: &StrictFragmentFunction) -> Result<Module, KajitBackendError> {
    let lowerer = KajitLowerer::new();
    lowerer
        .module
        .lower_vixen_typed_function_into_module(&lowerer.to_vixen_typed_function(function)?)
        .map_err(KajitBackendError::Kajit)
}

pub fn lower_ir(function: &StrictFragmentFunction) -> Result<IrFunc, KajitBackendError> {
    lower_hir(function).map(|module| kajit::lower_hir_module(&module))
}

pub fn compile(function: &StrictFragmentFunction) -> Result<CompiledFunction, KajitBackendError> {
    lower_hir(function).map(|module| kajit::compile_hir_module(&module))
}

struct KajitLowerer {
    module: Module,
    str_def: TypeDefId,
}

struct LoweringState {
    synthetic_locals: Vec<VixenTypedLocal>,
    next_local_id: u32,
}

impl KajitLowerer {
    fn new() -> Self {
        let mut module = Module::new();
        let _ = module.install_runtime_memory_callables();
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
        function: &StrictFragmentFunction,
    ) -> Result<VixenTypedFunction, KajitBackendError> {
        let original_locals = self.collect_locals(function);
        let mut state = LoweringState {
            synthetic_locals: Vec::new(),
            next_local_id: next_local_id(function),
        };
        let mut body = Vec::new();
        for stmt in &function.body {
            body.extend(self.lower_stmt(stmt, &mut state)?);
        }
        let mut locals = original_locals;
        locals.extend(state.synthetic_locals);
        Ok(VixenTypedFunction {
            name: function.name.clone(),
            params: function
                .params
                .iter()
                .map(|param| self.to_vixen_typed_param(param))
                .collect(),
            locals,
            return_type: self.to_vixen_type(function.return_type),
            body,
            comment: Some("lowered from vx-jit strict fragment IR".to_owned()),
        })
    }

    fn collect_locals(&self, function: &StrictFragmentFunction) -> Vec<VixenTypedLocal> {
        let mut locals = Vec::new();
        self.collect_locals_from_stmts(&function.body, &mut locals);
        locals
    }

    fn collect_locals_from_stmts(
        &self,
        stmts: &[StrictFragmentStmt],
        out: &mut Vec<VixenTypedLocal>,
    ) {
        for stmt in stmts {
            match stmt {
                StrictFragmentStmt::Let { local, .. } => out.push(self.to_vixen_typed_local(local)),
                StrictFragmentStmt::Return(_) => {}
            }
        }
    }

    fn to_vixen_typed_param(&self, param: &StrictFragmentParam) -> VixenTypedParam {
        VixenTypedParam {
            local: LocalId::new(param.local.0),
            name: param.name.clone(),
            ty: self.to_vixen_type(param.ty),
        }
    }

    fn to_vixen_typed_local(&self, local: &StrictFragmentLocal) -> VixenTypedLocal {
        VixenTypedLocal {
            local: LocalId::new(local.local.0),
            name: local.name.clone(),
            ty: self.to_vixen_type(local.ty),
        }
    }

    fn lower_stmt(
        &self,
        stmt: &StrictFragmentStmt,
        state: &mut LoweringState,
    ) -> Result<Vec<VixenTypedStmt>, KajitBackendError> {
        match stmt {
            StrictFragmentStmt::Let { local, value } => Ok(vec![VixenTypedStmt::Let {
                local: LocalId::new(local.local.0),
                value: self.to_vixen_typed_expr(value)?,
            }]),
            StrictFragmentStmt::Return(expr) => self.lower_return_stmt(expr, state),
        }
    }

    fn lower_return_stmt(
        &self,
        expr: &StrictFragmentExpr,
        state: &mut LoweringState,
    ) -> Result<Vec<VixenTypedStmt>, KajitBackendError> {
        match expr {
            StrictFragmentExpr::If {
                condition,
                then_expr,
                else_expr,
            } => Ok(vec![VixenTypedStmt::If {
                condition: self.to_vixen_typed_expr(condition)?,
                then_body: self.lower_return_stmt(then_expr, state)?,
                else_body: self.lower_return_stmt(else_expr, state)?,
            }]),
            StrictFragmentExpr::Concat(parts) => self.lower_concat_return(parts, state),
            _ => Ok(vec![VixenTypedStmt::Return(Some(
                self.to_vixen_typed_expr(expr)?,
            ))]),
        }
    }

    fn to_vixen_typed_expr(
        &self,
        expr: &StrictFragmentExpr,
    ) -> Result<VixenTypedExpr, KajitBackendError> {
        match expr {
            StrictFragmentExpr::Local(local) => Ok(VixenTypedExpr::Local(LocalId::new(local.0))),
            StrictFragmentExpr::U64(value) => {
                Ok(VixenTypedExpr::Literal(kajit_hir::Literal::Integer(*value)))
            }
            StrictFragmentExpr::Bool(value) => Ok(VixenTypedExpr::Literal(Literal::Bool(*value))),
            StrictFragmentExpr::String(value) => Ok(self.lower_string_literal(value)),
            StrictFragmentExpr::Binary { op, lhs, rhs } => Ok(VixenTypedExpr::Binary {
                op: to_vixen_binary_op(*op),
                lhs: Box::new(self.to_vixen_typed_expr(lhs)?),
                rhs: Box::new(self.to_vixen_typed_expr(rhs)?),
            }),
            StrictFragmentExpr::Intrinsic { intrinsic, args } => {
                self.lower_intrinsic(*intrinsic, args)
            }
            StrictFragmentExpr::Concat(_) => {
                Err(KajitBackendError::Unsupported("string concatenation"))
            }
            StrictFragmentExpr::If { .. } => Ok(self.lower_if_expr(expr)?),
        }
    }

    fn lower_string_literal(&self, value: &str) -> VixenTypedExpr {
        VixenTypedExpr::TypedLiteral {
            literal: Literal::String(value.to_owned()),
            ty: self.str_type(),
        }
    }

    fn lower_if_expr(
        &self,
        expr: &StrictFragmentExpr,
    ) -> Result<VixenTypedExpr, KajitBackendError> {
        let StrictFragmentExpr::If {
            condition,
            then_expr,
            else_expr,
        } = expr
        else {
            unreachable!();
        };
        Ok(VixenTypedExpr::If {
            condition: Box::new(self.to_vixen_typed_expr(condition)?),
            then_expr: Box::new(self.to_vixen_typed_expr(then_expr)?),
            else_expr: Box::new(self.to_vixen_typed_expr(else_expr)?),
        })
    }

    fn lower_concat_return(
        &self,
        parts: &[StrictFragmentExpr],
        state: &mut LoweringState,
    ) -> Result<Vec<VixenTypedStmt>, KajitBackendError> {
        if parts.is_empty() {
            return Ok(vec![VixenTypedStmt::Return(Some(
                self.lower_string_literal(""),
            ))]);
        }

        let total_len = state.new_temp(self, "__concat_total_len", StrictFragmentType::U64);
        let buf = state.new_temp(self, "__concat_buf", StrictFragmentType::U64);
        let mut stmts = vec![
            VixenTypedStmt::Let {
                local: total_len,
                value: sum_expr(
                    parts
                        .iter()
                        .map(|part| self.string_len_expr(part))
                        .collect::<Result<Vec<_>, _>>()?,
                )?,
            },
            VixenTypedStmt::Let {
                local: buf,
                value: VixenTypedExpr::Call {
                    callee: VixenCallableRef::Named("runtime.alloc_transient".to_owned()),
                    args: vec![
                        VixenTypedExpr::Local(total_len),
                        VixenTypedExpr::Literal(Literal::Integer(1)),
                    ],
                },
            },
        ];

        let mut cursor = VixenTypedExpr::Local(buf);
        for (index, part) in parts.iter().enumerate() {
            let next = state.new_temp(
                self,
                &format!("__concat_cursor_{index}"),
                StrictFragmentType::U64,
            );
            stmts.push(VixenTypedStmt::Let {
                local: next,
                value: VixenTypedExpr::Call {
                    callee: VixenCallableRef::Named("runtime.memcpy".to_owned()),
                    args: vec![
                        cursor,
                        self.string_ptr_expr(part)?,
                        self.string_len_expr(part)?,
                    ],
                },
            });
            cursor = VixenTypedExpr::Local(next);
        }

        stmts.push(VixenTypedStmt::Return(Some(VixenTypedExpr::Struct {
            def: self.str_def,
            fields: vec![
                ("ptr".to_owned(), VixenTypedExpr::Local(buf)),
                ("len".to_owned(), VixenTypedExpr::Local(total_len)),
            ],
        })));
        Ok(stmts)
    }

    fn string_ptr_expr(
        &self,
        expr: &StrictFragmentExpr,
    ) -> Result<VixenTypedExpr, KajitBackendError> {
        match expr {
            StrictFragmentExpr::String(value) => {
                let leaked: &'static [u8] = Box::leak(value.as_bytes().to_vec().into_boxed_slice());
                Ok(VixenTypedExpr::Literal(Literal::Integer(
                    leaked.as_ptr() as u64
                )))
            }
            StrictFragmentExpr::Intrinsic {
                intrinsic: StrictFragmentIntrinsic::StringSlice,
                args,
            } => match args.as_slice() {
                [value, start, _end] => Ok(VixenTypedExpr::Binary {
                    op: BinaryOp::Add,
                    lhs: Box::new(self.string_ptr_expr(value)?),
                    rhs: Box::new(self.to_vixen_typed_expr(start)?),
                }),
                _ => Err(KajitBackendError::Unsupported("invalid StringSlice args")),
            },
            _ => Ok(VixenTypedExpr::Field {
                base: Box::new(self.to_vixen_typed_expr(expr)?),
                field: "ptr".to_owned(),
            }),
        }
    }

    fn string_len_expr(
        &self,
        expr: &StrictFragmentExpr,
    ) -> Result<VixenTypedExpr, KajitBackendError> {
        match expr {
            StrictFragmentExpr::String(value) => Ok(VixenTypedExpr::Literal(Literal::Integer(
                value.len() as u64,
            ))),
            StrictFragmentExpr::Intrinsic {
                intrinsic: StrictFragmentIntrinsic::StringSlice,
                args,
            } => match args.as_slice() {
                [_value, start, end] => Ok(VixenTypedExpr::Binary {
                    op: BinaryOp::Sub,
                    lhs: Box::new(self.to_vixen_typed_expr(end)?),
                    rhs: Box::new(self.to_vixen_typed_expr(start)?),
                }),
                _ => Err(KajitBackendError::Unsupported("invalid StringSlice args")),
            },
            _ => Ok(VixenTypedExpr::Field {
                base: Box::new(self.to_vixen_typed_expr(expr)?),
                field: "len".to_owned(),
            }),
        }
    }

    fn lower_intrinsic(
        &self,
        intrinsic: StrictFragmentIntrinsic,
        args: &[StrictFragmentExpr],
    ) -> Result<VixenTypedExpr, KajitBackendError> {
        match intrinsic {
            StrictFragmentIntrinsic::StringLen => match args {
                [value] => Ok(VixenTypedExpr::Field {
                    base: Box::new(self.to_vixen_typed_expr(value)?),
                    field: "len".to_owned(),
                }),
                _ => Err(KajitBackendError::Unsupported("invalid StringLen args")),
            },
            StrictFragmentIntrinsic::StringSlice => match args {
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

    fn to_vixen_type(&self, ty: StrictFragmentType) -> Type {
        match ty {
            StrictFragmentType::U64 => Type::u(64),
            StrictFragmentType::Bool => Type::bool(),
            StrictFragmentType::String => self.str_type(),
        }
    }
}

impl LoweringState {
    fn new_temp(&mut self, lowerer: &KajitLowerer, name: &str, ty: StrictFragmentType) -> LocalId {
        let local = LocalId::new(self.next_local_id);
        self.next_local_id += 1;
        self.synthetic_locals.push(VixenTypedLocal {
            local,
            name: name.to_owned(),
            ty: lowerer.to_vixen_type(ty),
        });
        local
    }
}

fn next_local_id(function: &StrictFragmentFunction) -> u32 {
    let mut max_id = function
        .params
        .iter()
        .map(|param| param.local.0)
        .max()
        .unwrap_or(0);
    for stmt in &function.body {
        if let StrictFragmentStmt::Let { local, .. } = stmt {
            max_id = max_id.max(local.local.0);
        }
    }
    max_id + 1
}

fn sum_expr(mut exprs: Vec<VixenTypedExpr>) -> Result<VixenTypedExpr, KajitBackendError> {
    if exprs.is_empty() {
        return Err(KajitBackendError::Unsupported("empty concatenation"));
    }
    let mut expr = exprs.remove(0);
    for rhs in exprs {
        expr = VixenTypedExpr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(expr),
            rhs: Box::new(rhs),
        };
    }
    Ok(expr)
}
fn to_vixen_binary_op(op: StrictFragmentBinaryOp) -> BinaryOp {
    match op {
        StrictFragmentBinaryOp::Add => BinaryOp::Add,
        StrictFragmentBinaryOp::Eq => BinaryOp::Eq,
    }
}
