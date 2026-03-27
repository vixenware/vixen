use std::{collections::BTreeMap, error::Error, fmt};

use vx_eval3_oracle::{Expr as OracleExpr, FieldKey as OracleFieldKey, FunctionDef, Program};

use crate::{
    Block, BlockId, CallableSig, FieldKey, Kernel, KernelId, Layout, LayoutId, LayoutKind,
    LocalDecl, LocalId, Module, NamedField, ObsLiteral, ProductField, ReturnType, Stmt, Terminator,
    Type,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoweringError {
    UnsupportedProgram { reason: String },
}

impl fmt::Display for LoweringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedProgram { reason } => write!(f, "{reason}"),
        }
    }
}

impl Error for LoweringError {}

pub fn lower_program(program: &Program) -> Result<Module, LoweringError> {
    if let Some(module) = lower_structural_single_main(program)? {
        return Ok(module);
    }

    Err(LoweringError::UnsupportedProgram {
        reason:
            "slice-0 structural lowering currently supports main-driven programs over literals/local/let/if/add/tuple/struct/field, plus lambda and call forms used by the slice-0 corpus"
                .to_owned(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum StructuralTy {
    U64,
    Bool,
    Callable(Vec<StructuralTy>, Box<StructuralTy>),
    Product(Vec<(FieldKey, StructuralTy)>),
}

impl StructuralTy {
    fn as_runtime_type(&self, lowering: &mut StructuralLowering) -> Type {
        match self {
            Self::U64 => Type::U64,
            Self::Bool => Type::Bool,
            Self::Callable(params, ret) => Type::Callable(CallableSig::new(
                params
                    .iter()
                    .map(|param| param.as_runtime_type(lowering))
                    .collect(),
                ret.as_runtime_type(lowering),
            )),
            Self::Product(fields) => Type::ProductHandle(lowering.layout_for_product(fields)),
        }
    }
}

#[derive(Debug, Clone)]
struct Binding {
    reference: LocalId,
    ty: StructuralTy,
}

#[derive(Debug, Default)]
struct BlockBuild {
    stmts: Vec<Stmt>,
    terminator: Option<Terminator>,
}

#[derive(Debug)]
struct StructuralLowering {
    params: Vec<LocalDecl>,
    locals: Vec<LocalDecl>,
    blocks: Vec<BlockBuild>,
    layouts: Vec<Layout>,
    kernels: Vec<Kernel>,
    layout_ids: BTreeMap<Vec<(FieldKey, StructuralTy)>, LayoutId>,
    next_local: u32,
    next_kernel: u32,
}

impl StructuralLowering {
    fn new(params: Vec<LocalDecl>, first_local: u32) -> Self {
        Self {
            params,
            locals: Vec::new(),
            blocks: vec![BlockBuild::default()],
            layouts: Vec::new(),
            kernels: Vec::new(),
            layout_ids: BTreeMap::new(),
            next_local: first_local,
            next_kernel: 1,
        }
    }

    fn alloc_local(&mut self, name: &str, ty: Type) -> LocalId {
        let id = LocalId::new(self.next_local);
        self.next_local += 1;
        self.locals.push(LocalDecl {
            id,
            name: name.to_owned(),
            ty,
        });
        id
    }

    fn emit(&mut self, block: BlockId, stmt: Stmt) {
        self.blocks[block.0 as usize].stmts.push(stmt);
    }

    fn terminate(&mut self, block: BlockId, terminator: Terminator) {
        self.blocks[block.0 as usize].terminator = Some(terminator);
    }

    fn new_block(&mut self) -> BlockId {
        let id = BlockId::new(self.blocks.len() as u32);
        self.blocks.push(BlockBuild::default());
        id
    }

    fn alloc_kernel_id(&mut self) -> KernelId {
        let id = KernelId::new(self.next_kernel);
        self.next_kernel += 1;
        id
    }

    fn push_kernel(&mut self, kernel: Kernel) {
        self.kernels.push(kernel);
    }

    fn layout_for_product(&mut self, fields: &[(FieldKey, StructuralTy)]) -> LayoutId {
        if let Some(id) = self.layout_ids.get(fields) {
            return *id;
        }

        let id = LayoutId::new(self.layouts.len() as u32);
        let mut layout_fields = Vec::with_capacity(fields.len());
        for (field, field_ty) in fields {
            layout_fields.push(ProductField {
                key: field.clone(),
                ty: Type::ref_of(field_ty.as_runtime_type(self)),
            });
        }

        self.layouts.push(Layout {
            id,
            name: format!("Product{}", id.0),
            kind: LayoutKind::Product {
                fields: layout_fields,
            },
        });
        self.layout_ids.insert(fields.to_vec(), id);
        id
    }

    fn finish(self, return_ty: Type) -> Result<Module, LoweringError> {
        let mut blocks = Vec::with_capacity(self.blocks.len());
        for (index, block) in self.blocks.into_iter().enumerate() {
            let Some(terminator) = block.terminator else {
                return Err(LoweringError::UnsupportedProgram {
                    reason: format!("structural lowering left block {index} without terminator"),
                });
            };
            blocks.push(Block {
                id: BlockId::new(index as u32),
                stmts: block.stmts,
                terminator,
            });
        }

        let kernel = Kernel {
            id: KernelId::new(0),
            name: "enter_main".to_owned(),
            params: self.params,
            locals: self.locals,
            return_type: ReturnType::Never,
            entry: BlockId::new(0),
            blocks,
        };

        let _ = return_ty;
        Ok(Module {
            layouts: self.layouts,
            kernels: {
                let mut kernels = vec![kernel];
                kernels.extend(self.kernels);
                kernels
            },
        })
    }
}

fn lower_structural_single_main(program: &Program) -> Result<Option<Module>, LoweringError> {
    let Some(main) = program.functions.get("main") else {
        return Ok(None);
    };

    let mut param_tys = BTreeMap::new();
    for param in &main.params {
        param_tys.insert(param.clone(), None);
    }

    let mut type_env = BTreeMap::new();
    let Some(return_ty) = infer_expr_type(&main.body, &mut type_env, &mut param_tys, None) else {
        return Ok(None);
    };

    if return_ty == StructuralTy::Bool {
        return Ok(None);
    }

    let Some(return_value_ty) = runtime_type_without_layout(&return_ty) else {
        return Ok(None);
    };
    if return_value_ty != Type::U64 {
        return Ok(None);
    }

    let mut resolved_param_tys = BTreeMap::new();
    for param in &main.params {
        let ty = param_tys
            .get(param)
            .and_then(|ty| ty.clone())
            .unwrap_or(StructuralTy::U64);
        resolved_param_tys.insert(param.clone(), ty);
    }

    let mut params = vec![
        local(0, "ctx", Type::RuntimeCtx),
        local(1, "self", Type::thunk_handle(return_value_ty.clone())),
    ];
    for (index, param) in main.params.iter().enumerate() {
        let ty = resolved_param_tys
            .get(param)
            .expect("param type should be resolved");
        let Some(param_value_ty) = runtime_type_without_layout(ty) else {
            return Ok(None);
        };
        let ref_ty = Type::ref_of(param_value_ty);
        params.push(local(2 + index as u32, &format!("{param}_ref"), ref_ty));
    }
    params.push(local(
        2 + main.params.len() as u32,
        "obs",
        Type::obs_of(return_value_ty.clone()),
    ));

    let mut lowering = StructuralLowering::new(params, 3 + main.params.len() as u32);
    let mut value_env = BTreeMap::new();
    let mut value_type_env = BTreeMap::new();
    for (index, param) in main.params.iter().enumerate() {
        let ty = resolved_param_tys
            .get(param)
            .expect("param type should be resolved")
            .clone();
        value_env.insert(
            param.clone(),
            Binding {
                reference: LocalId::new(2 + index as u32),
                ty: ty.clone(),
            },
        );
        value_type_env.insert(param.clone(), ty);
    }

    let (tail_block, result_ref) = match lower_expr_ref(
        program,
        &main.body,
        &return_ty,
        BlockId::new(0),
        &mut value_env,
        &mut value_type_env,
        &resolved_param_tys,
        &mut lowering,
    ) {
        Some(value) => value,
        None => return Ok(None),
    };

    match &return_ty {
        StructuralTy::U64 => {
            let out = lowering.alloc_local("out", Type::U64);
            lowering.emit(
                tail_block,
                Stmt::EnterU64 {
                    dst: out,
                    reference: result_ref,
                    obs: ObsLiteral::NeedValue,
                },
            );
            lowering.emit(
                tail_block,
                Stmt::CacheReadyU64 {
                    thunk: LocalId::new(1),
                    value: out,
                },
            );
        }
        StructuralTy::Product(_) => return Ok(None),
        StructuralTy::Callable(_, _) => return Ok(None),
        StructuralTy::Bool => return Ok(None),
    }
    lowering.terminate(tail_block, Terminator::Done);

    let module = lowering.finish(return_value_ty)?;
    Ok(Some(module))
}

fn infer_expr_type(
    expr: &OracleExpr,
    env: &mut BTreeMap<String, StructuralTy>,
    params: &mut BTreeMap<String, Option<StructuralTy>>,
    expected: Option<&StructuralTy>,
) -> Option<StructuralTy> {
    let ty = match expr {
        OracleExpr::U64(_) => StructuralTy::U64,
        OracleExpr::Bool(_) => StructuralTy::Bool,
        OracleExpr::Local(name) => {
            if let Some(local_ty) = env.get(name) {
                local_ty.clone()
            } else {
                let slot = params.get_mut(name)?;
                if let Some(existing) = slot.clone() {
                    existing
                } else {
                    let inferred = expected?.clone();
                    *slot = Some(inferred.clone());
                    inferred
                }
            }
        }
        OracleExpr::Let { name, value, body } => {
            let value_ty = infer_expr_type(value, env, params, None)?;
            let replaced = env.insert(name.clone(), value_ty);
            let body_ty = infer_expr_type(body, env, params, expected)?;
            if let Some(previous) = replaced {
                env.insert(name.clone(), previous);
            } else {
                env.remove(name);
            }
            body_ty
        }
        OracleExpr::If {
            condition,
            then_branch,
            else_branch,
        } => {
            if infer_expr_type(condition, env, params, Some(&StructuralTy::Bool))?
                != StructuralTy::Bool
            {
                return None;
            }
            let then_ty = infer_expr_type(then_branch, env, params, expected)?;
            let else_ty = infer_expr_type(else_branch, env, params, Some(&then_ty))?;
            if then_ty != else_ty {
                return None;
            }
            then_ty
        }
        OracleExpr::Tuple(fields) => {
            let expected_fields = match expected {
                Some(StructuralTy::Product(fields)) => Some(fields),
                Some(_) => return None,
                None => None,
            };

            let mut resolved = Vec::with_capacity(fields.len());
            for (index, field_expr) in fields.iter().enumerate() {
                let expected_field_ty =
                    expected_fields.and_then(|fields| fields.get(index).map(|(_, ty)| ty));
                let field_ty = infer_expr_type(field_expr, env, params, expected_field_ty)?;
                resolved.push((FieldKey::index(index), field_ty));
            }
            StructuralTy::Product(resolved)
        }
        OracleExpr::Struct(fields) => {
            let expected_fields = match expected {
                Some(StructuralTy::Product(fields)) => Some(fields),
                Some(_) => return None,
                None => None,
            };

            let mut resolved = Vec::with_capacity(fields.len());
            for (name, field_expr) in fields {
                let expected_field_ty = expected_fields.and_then(|fields| {
                    fields
                        .iter()
                        .find(|(key, _)| *key == FieldKey::Name(name.clone()))
                        .map(|(_, ty)| ty)
                });
                let field_ty = infer_expr_type(field_expr, env, params, expected_field_ty)?;
                resolved.push((FieldKey::Name(name.clone()), field_ty));
            }
            StructuralTy::Product(resolved)
        }
        OracleExpr::Lambda {
            params: lambda_params,
            body,
        } => {
            let expected_callable = match expected {
                Some(StructuralTy::Callable(params, ret)) => Some((params, ret)),
                Some(_) => return None,
                None => None,
            };

            let mut param_types = Vec::with_capacity(lambda_params.len());
            let mut inserted = Vec::with_capacity(lambda_params.len());
            for (index, param) in lambda_params.iter().enumerate() {
                let param_ty = expected_callable
                    .and_then(|(params, _)| params.get(index).cloned())
                    .unwrap_or(StructuralTy::U64);
                param_types.push(param_ty.clone());
                inserted.push((param.clone(), env.insert(param.clone(), param_ty)));
            }

            let body_expected = expected_callable.map(|(_, ret)| ret.as_ref());
            let body_ty =
                infer_expr_type(body, env, params, body_expected).unwrap_or(StructuralTy::U64);

            for (param, previous) in inserted {
                if let Some(previous) = previous {
                    env.insert(param, previous);
                } else {
                    env.remove(&param);
                }
            }

            StructuralTy::Callable(param_types, Box::new(body_ty))
        }
        OracleExpr::Call { callee, args } => {
            let arg_tys = args
                .iter()
                .map(|arg| infer_expr_type(arg, env, params, None).unwrap_or(StructuralTy::U64))
                .collect::<Vec<_>>();
            let ret_ty = expected.cloned().unwrap_or(StructuralTy::U64);
            let callable_ty = StructuralTy::Callable(arg_tys.clone(), Box::new(ret_ty.clone()));
            let inferred_callee = infer_expr_type(callee, env, params, Some(&callable_ty))
                .unwrap_or(callable_ty.clone());
            match inferred_callee {
                StructuralTy::Callable(_, ret) => *ret,
                _ => return None,
            }
        }
        OracleExpr::CallNamed { args, .. } => {
            for arg in args {
                let _ = infer_expr_type(arg, env, params, None);
            }
            expected.cloned().unwrap_or(StructuralTy::U64)
        }
        OracleExpr::Field { base, field } => {
            let base_ty = infer_expr_type(base, env, params, None)?;
            let StructuralTy::Product(fields) = base_ty else {
                return None;
            };
            let (_, ty) = fields
                .into_iter()
                .find(|(key, _)| *key == convert_field_key(field))?;
            ty
        }
        OracleExpr::Add(lhs, rhs) => {
            if infer_expr_type(lhs, env, params, Some(&StructuralTy::U64))? != StructuralTy::U64 {
                return None;
            }
            if infer_expr_type(rhs, env, params, Some(&StructuralTy::U64))? != StructuralTy::U64 {
                return None;
            }
            StructuralTy::U64
        }
        _ => return None,
    };

    if let Some(expected_ty) = expected {
        if expected_ty != &ty {
            return None;
        }
    }
    Some(ty)
}

fn lower_expr_ref(
    program: &Program,
    expr: &OracleExpr,
    expected: &StructuralTy,
    block: BlockId,
    value_env: &mut BTreeMap<String, Binding>,
    value_type_env: &mut BTreeMap<String, StructuralTy>,
    params: &BTreeMap<String, StructuralTy>,
    lowering: &mut StructuralLowering,
) -> Option<(BlockId, LocalId)> {
    match expr {
        OracleExpr::U64(value) => {
            if expected != &StructuralTy::U64 {
                return None;
            }
            let value_local = lowering.alloc_local("lit_u64", Type::U64);
            lowering.emit(
                block,
                Stmt::ConstU64 {
                    dst: value_local,
                    value: *value,
                },
            );
            let reference = lowering.alloc_local("lit_u64_ref", Type::ref_of(Type::U64));
            lowering.emit(
                block,
                Stmt::ReadyU64 {
                    dst: reference,
                    value: value_local,
                },
            );
            Some((block, reference))
        }
        OracleExpr::Bool(value) => {
            if expected != &StructuralTy::Bool {
                return None;
            }
            let value_local = lowering.alloc_local("lit_bool", Type::Bool);
            lowering.emit(
                block,
                Stmt::ConstBool {
                    dst: value_local,
                    value: *value,
                },
            );
            let reference = lowering.alloc_local("lit_bool_ref", Type::ref_of(Type::Bool));
            lowering.emit(
                block,
                Stmt::ReadyBool {
                    dst: reference,
                    value: value_local,
                },
            );
            Some((block, reference))
        }
        OracleExpr::Local(name) => {
            let binding = value_env.get(name)?;
            if &binding.ty != expected {
                return None;
            }
            Some((block, binding.reference))
        }
        OracleExpr::Let { name, value, body } => {
            let value_ty = infer_expr_type_resolved(value, value_type_env, params)?;
            let (after_value, value_ref) = lower_expr_ref(
                program,
                value,
                &value_ty,
                block,
                value_env,
                value_type_env,
                params,
                lowering,
            )?;

            let previous_binding = value_env.insert(
                name.clone(),
                Binding {
                    reference: value_ref,
                    ty: value_ty.clone(),
                },
            );
            let previous_ty = value_type_env.insert(name.clone(), value_ty);

            let result = lower_expr_ref(
                program,
                body,
                expected,
                after_value,
                value_env,
                value_type_env,
                params,
                lowering,
            );

            if let Some(binding) = previous_binding {
                value_env.insert(name.clone(), binding);
            } else {
                value_env.remove(name);
            }
            if let Some(ty) = previous_ty {
                value_type_env.insert(name.clone(), ty);
            } else {
                value_type_env.remove(name);
            }

            result
        }
        OracleExpr::If {
            condition,
            then_branch,
            else_branch,
        } => {
            let (after_cond, condition_value) = lower_expr_bool(
                program,
                condition,
                block,
                value_env,
                value_type_env,
                params,
                lowering,
            )?;
            let result_ref_ty = Type::ref_of(expected.as_runtime_type(lowering));
            let result_ref = lowering.alloc_local("if_result_ref", result_ref_ty);

            let then_block = lowering.new_block();
            let else_block = lowering.new_block();
            let join_block = lowering.new_block();
            lowering.terminate(
                after_cond,
                Terminator::Branch {
                    cond: condition_value,
                    then_block,
                    else_block,
                },
            );

            let (then_tail, then_ref) = lower_expr_ref(
                program,
                then_branch,
                expected,
                then_block,
                value_env,
                value_type_env,
                params,
                lowering,
            )?;
            if then_ref != result_ref {
                lowering.emit(
                    then_tail,
                    Stmt::Copy {
                        dst: result_ref,
                        src: then_ref,
                    },
                );
            }
            lowering.terminate(then_tail, Terminator::Goto(join_block));

            let (else_tail, else_ref) = lower_expr_ref(
                program,
                else_branch,
                expected,
                else_block,
                value_env,
                value_type_env,
                params,
                lowering,
            )?;
            if else_ref != result_ref {
                lowering.emit(
                    else_tail,
                    Stmt::Copy {
                        dst: result_ref,
                        src: else_ref,
                    },
                );
            }
            lowering.terminate(else_tail, Terminator::Goto(join_block));

            Some((join_block, result_ref))
        }
        OracleExpr::Add(lhs, rhs) => {
            if expected != &StructuralTy::U64 {
                return None;
            }
            let (after_lhs, lhs_value) = lower_expr_u64(
                program,
                lhs,
                block,
                value_env,
                value_type_env,
                params,
                lowering,
            )?;
            let (after_rhs, rhs_value) = lower_expr_u64(
                program,
                rhs,
                after_lhs,
                value_env,
                value_type_env,
                params,
                lowering,
            )?;
            let sum = lowering.alloc_local("sum", Type::U64);
            lowering.emit(
                after_rhs,
                Stmt::AddU64 {
                    dst: sum,
                    lhs: lhs_value,
                    rhs: rhs_value,
                },
            );
            let sum_ref = lowering.alloc_local("sum_ref", Type::ref_of(Type::U64));
            lowering.emit(
                after_rhs,
                Stmt::ReadyU64 {
                    dst: sum_ref,
                    value: sum,
                },
            );
            Some((after_rhs, sum_ref))
        }
        OracleExpr::Tuple(fields) => {
            let StructuralTy::Product(expected_fields) = expected else {
                return None;
            };
            if fields.len() != expected_fields.len() {
                return None;
            }

            let mut at = block;
            let mut lowered_fields = Vec::with_capacity(fields.len());
            for (index, field_expr) in fields.iter().enumerate() {
                let (field_key, field_ty) = &expected_fields[index];
                if *field_key != FieldKey::index(index) {
                    return None;
                }
                let (next_block, field_ref) = lower_expr_ref(
                    program,
                    field_expr,
                    field_ty,
                    at,
                    value_env,
                    value_type_env,
                    params,
                    lowering,
                )?;
                at = next_block;
                lowered_fields.push((field_key.clone(), field_ref));
            }

            let layout_id = lowering.layout_for_product(expected_fields);
            let product_ref =
                lowering.alloc_local("tuple_ref", Type::ref_of(Type::ProductHandle(layout_id)));
            lowering.emit(
                at,
                Stmt::AllocProduct {
                    dst: product_ref,
                    layout: layout_id,
                    fields: lowered_fields,
                },
            );
            Some((at, product_ref))
        }
        OracleExpr::Struct(fields) => {
            let StructuralTy::Product(expected_fields) = expected else {
                return None;
            };
            if fields.len() != expected_fields.len() {
                return None;
            }

            let mut at = block;
            let mut lowered_fields = Vec::with_capacity(fields.len());
            for (field_name, field_expr) in fields {
                let key = FieldKey::Name(field_name.clone());
                let (_, field_ty) = expected_fields
                    .iter()
                    .find(|(expected_key, _)| *expected_key == key)?;
                let (next_block, field_ref) = lower_expr_ref(
                    program,
                    field_expr,
                    field_ty,
                    at,
                    value_env,
                    value_type_env,
                    params,
                    lowering,
                )?;
                at = next_block;
                lowered_fields.push((key, field_ref));
            }

            let layout_id = lowering.layout_for_product(expected_fields);
            let product_ref =
                lowering.alloc_local("struct_ref", Type::ref_of(Type::ProductHandle(layout_id)));
            lowering.emit(
                at,
                Stmt::AllocProduct {
                    dst: product_ref,
                    layout: layout_id,
                    fields: lowered_fields,
                },
            );
            Some((at, product_ref))
        }
        OracleExpr::Lambda {
            params: lambda_params,
            body,
        } => {
            let StructuralTy::Callable(param_tys, ret_ty) =
                infer_expr_type_resolved(expr, value_type_env, params)?
            else {
                return None;
            };
            if param_tys.len() != 1 || *ret_ty != StructuralTy::U64 {
                return None;
            }

            let capture_name = match_add_capture_body(body, &lambda_params[0])?;
            let capture = value_env.get(&capture_name)?;
            let env_layout = Layout {
                id: LayoutId::new(lowering.layouts.len() as u32),
                name: format!("ClosureEnv{}", lowering.layouts.len()),
                kind: LayoutKind::Env {
                    fields: vec![NamedField {
                        name: "n_ref".to_owned(),
                        ty: Type::ref_of(capture.ty.as_runtime_type(lowering)),
                    }],
                },
            };
            let env_layout_id = env_layout.id;
            lowering.layouts.push(env_layout);

            let kernel_id = lowering.alloc_kernel_id();
            lowering.push_kernel(enter_add_capture_kernel_named(
                kernel_id,
                format!("lambda_{}", kernel_id.0),
                env_layout_id,
                "n_ref".to_owned(),
            ));

            let env_local = lowering.alloc_local("env", Type::EnvHandle(env_layout_id));
            lowering.emit(
                block,
                Stmt::AllocEnv {
                    dst: env_local,
                    layout: env_layout_id,
                    captures: vec![("n_ref".to_owned(), capture.reference)],
                },
            );

            let callable_ty = Type::Callable(CallableSig::new(
                param_tys
                    .iter()
                    .map(|param| param.as_runtime_type(lowering))
                    .collect(),
                Type::U64,
            ));
            let closure_local = lowering.alloc_local("closure", callable_ty.clone());
            lowering.emit(
                block,
                Stmt::AllocClosure {
                    dst: closure_local,
                    kernel: kernel_id,
                    env: Some(env_local),
                },
            );

            let closure_ref = lowering.alloc_local("closure_ref", Type::ref_of(callable_ty));
            lowering.emit(
                block,
                Stmt::ReadyCallable {
                    dst: closure_ref,
                    callable: closure_local,
                },
            );
            Some((block, closure_ref))
        }
        OracleExpr::CallNamed { name, args } => {
            if expected != &StructuralTy::U64 {
                return None;
            }
            let arg_types = args
                .iter()
                .map(|arg| {
                    infer_expr_type_resolved(arg, value_type_env, params)
                        .unwrap_or(StructuralTy::U64)
                })
                .collect::<Vec<_>>();
            let mut arg_refs = Vec::with_capacity(args.len());
            let mut at = block;
            for (arg, arg_ty) in args.iter().zip(arg_types.iter()) {
                let (next, arg_ref) = lower_expr_ref(
                    program,
                    arg,
                    arg_ty,
                    at,
                    value_env,
                    value_type_env,
                    params,
                    lowering,
                )?;
                at = next;
                arg_refs.push(arg_ref);
            }

            let function_def = program.functions.get(name)?;
            let (kernel, frame_layout, frame_names) =
                make_named_call_kernel(lowering, name, function_def, &arg_types)?;
            let call_ref = lowering.alloc_local("named_call_ref", Type::ref_of(Type::U64));
            lowering.emit(
                at,
                Stmt::AllocThunk {
                    dst: call_ref,
                    kernel,
                    frame_layout,
                    frame: frame_names.into_iter().zip(arg_refs).collect(),
                },
            );
            Some((at, call_ref))
        }
        OracleExpr::Call { callee, args } => {
            if expected != &StructuralTy::U64 || args.len() != 1 {
                return None;
            }

            let callable_ty = infer_expr_type_resolved(callee, value_type_env, params)?;
            let StructuralTy::Callable(arg_tys, ret_ty) = callable_ty else {
                return None;
            };
            if arg_tys.len() != 1 || *ret_ty != StructuralTy::U64 {
                return None;
            }

            let (after_callee, callee_ref) = lower_expr_ref(
                program,
                callee,
                &StructuralTy::Callable(arg_tys.clone(), ret_ty),
                block,
                value_env,
                value_type_env,
                params,
                lowering,
            )?;
            let (after_arg, arg_ref) = lower_expr_ref(
                program,
                &args[0],
                &arg_tys[0],
                after_callee,
                value_env,
                value_type_env,
                params,
                lowering,
            )?;

            let strict_callable_ty = Type::Callable(CallableSig::new(
                arg_tys
                    .iter()
                    .map(runtime_type_without_layout)
                    .collect::<Option<Vec<_>>>()?,
                Type::U64,
            ));
            let strict_callable = lowering.alloc_local("callable", strict_callable_ty);
            lowering.emit(
                after_arg,
                Stmt::NeedCallable {
                    dst: strict_callable,
                    reference: callee_ref,
                },
            );

            let frame_layout =
                call_result_frame_layout(LayoutId::new(lowering.layouts.len() as u32));
            let frame_layout_id = frame_layout.id;
            lowering.layouts.push(frame_layout);
            let kernel_id = lowering.alloc_kernel_id();
            lowering.push_kernel(call_result_kernel_named(
                kernel_id,
                format!("call_result_{}", kernel_id.0),
            ));

            let result_ref = lowering.alloc_local("call_result_ref", Type::ref_of(Type::U64));
            lowering.emit(
                after_arg,
                Stmt::AllocThunk {
                    dst: result_ref,
                    kernel: kernel_id,
                    frame_layout: frame_layout_id,
                    frame: vec![
                        ("callable".to_owned(), strict_callable),
                        ("arg0_ref".to_owned(), arg_ref),
                    ],
                },
            );
            Some((after_arg, result_ref))
        }
        OracleExpr::Field { base, field } => {
            let base_ty = infer_expr_type_resolved(base, value_type_env, params)?;
            let StructuralTy::Product(fields) = base_ty else {
                return None;
            };
            let key = convert_field_key(field);
            let (_, field_ty) = fields.iter().find(|(field_key, _)| *field_key == key)?;
            if field_ty != expected {
                return None;
            }

            let (after_base, base_ref) = lower_expr_ref(
                program,
                base,
                &StructuralTy::Product(fields.clone()),
                block,
                value_env,
                value_type_env,
                params,
                lowering,
            )?;
            let projected_ty = Type::ref_of(expected.as_runtime_type(lowering));
            let projected = lowering.alloc_local("projected_ref", projected_ty);
            lowering.emit(
                after_base,
                Stmt::ProjectField {
                    dst: projected,
                    product: base_ref,
                    field: key,
                },
            );
            Some((after_base, projected))
        }
        _ => None,
    }
}

fn lower_expr_u64(
    program: &Program,
    expr: &OracleExpr,
    block: BlockId,
    value_env: &mut BTreeMap<String, Binding>,
    value_type_env: &mut BTreeMap<String, StructuralTy>,
    params: &BTreeMap<String, StructuralTy>,
    lowering: &mut StructuralLowering,
) -> Option<(BlockId, LocalId)> {
    let (next, reference) = lower_expr_ref(
        program,
        expr,
        &StructuralTy::U64,
        block,
        value_env,
        value_type_env,
        params,
        lowering,
    )?;
    let out = lowering.alloc_local("entered_u64", Type::U64);
    lowering.emit(
        next,
        Stmt::EnterU64 {
            dst: out,
            reference,
            obs: ObsLiteral::NeedValue,
        },
    );
    Some((next, out))
}

fn lower_expr_bool(
    program: &Program,
    expr: &OracleExpr,
    block: BlockId,
    value_env: &mut BTreeMap<String, Binding>,
    value_type_env: &mut BTreeMap<String, StructuralTy>,
    params: &BTreeMap<String, StructuralTy>,
    lowering: &mut StructuralLowering,
) -> Option<(BlockId, LocalId)> {
    let (next, reference) = lower_expr_ref(
        program,
        expr,
        &StructuralTy::Bool,
        block,
        value_env,
        value_type_env,
        params,
        lowering,
    )?;
    let out = lowering.alloc_local("entered_bool", Type::Bool);
    lowering.emit(
        next,
        Stmt::EnterBool {
            dst: out,
            reference,
            obs: ObsLiteral::NeedValue,
        },
    );
    Some((next, out))
}

fn infer_expr_type_resolved(
    expr: &OracleExpr,
    env: &BTreeMap<String, StructuralTy>,
    params: &BTreeMap<String, StructuralTy>,
) -> Option<StructuralTy> {
    match expr {
        OracleExpr::U64(_) => Some(StructuralTy::U64),
        OracleExpr::Bool(_) => Some(StructuralTy::Bool),
        OracleExpr::Local(name) => env.get(name).cloned().or_else(|| params.get(name).cloned()),
        OracleExpr::Let { name, value, body } => {
            let value_ty = infer_expr_type_resolved(value, env, params)?;
            let mut extended = env.clone();
            extended.insert(name.clone(), value_ty);
            infer_expr_type_resolved(body, &extended, params)
        }
        OracleExpr::If {
            condition,
            then_branch,
            else_branch,
        } => {
            if infer_expr_type_resolved(condition, env, params)? != StructuralTy::Bool {
                return None;
            }
            let then_ty = infer_expr_type_resolved(then_branch, env, params)?;
            let else_ty = infer_expr_type_resolved(else_branch, env, params)?;
            if then_ty == else_ty {
                Some(then_ty)
            } else {
                None
            }
        }
        OracleExpr::Tuple(fields) => {
            let mut typed_fields = Vec::with_capacity(fields.len());
            for (index, field_expr) in fields.iter().enumerate() {
                typed_fields.push((
                    FieldKey::index(index),
                    infer_expr_type_resolved(field_expr, env, params)?,
                ));
            }
            Some(StructuralTy::Product(typed_fields))
        }
        OracleExpr::Struct(fields) => {
            let mut typed_fields = Vec::with_capacity(fields.len());
            for (name, field_expr) in fields {
                typed_fields.push((
                    FieldKey::Name(name.clone()),
                    infer_expr_type_resolved(field_expr, env, params)?,
                ));
            }
            Some(StructuralTy::Product(typed_fields))
        }
        OracleExpr::Lambda {
            params: lambda_params,
            body,
        } => {
            let mut extended = env.clone();
            for param in lambda_params {
                extended.insert(param.clone(), StructuralTy::U64);
            }
            Some(StructuralTy::Callable(
                vec![StructuralTy::U64; lambda_params.len()],
                Box::new(
                    infer_expr_type_resolved(body, &extended, params).unwrap_or(StructuralTy::U64),
                ),
            ))
        }
        OracleExpr::Call { callee, args } => {
            let callee_ty = infer_expr_type_resolved(callee, env, params)?;
            for arg in args {
                let _ = infer_expr_type_resolved(arg, env, params);
            }
            match callee_ty {
                StructuralTy::Callable(_, ret) => Some(*ret),
                _ => Some(StructuralTy::U64),
            }
        }
        OracleExpr::CallNamed { args, .. } => {
            for arg in args {
                let _ = infer_expr_type_resolved(arg, env, params);
            }
            Some(StructuralTy::U64)
        }
        OracleExpr::Field { base, field } => {
            let StructuralTy::Product(fields) = infer_expr_type_resolved(base, env, params)? else {
                return None;
            };
            fields
                .into_iter()
                .find(|(key, _)| *key == convert_field_key(field))
                .map(|(_, ty)| ty)
        }
        OracleExpr::Add(lhs, rhs) => {
            if infer_expr_type_resolved(lhs, env, params)? == StructuralTy::U64
                && infer_expr_type_resolved(rhs, env, params)? == StructuralTy::U64
            {
                Some(StructuralTy::U64)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn convert_field_key(field: &OracleFieldKey) -> FieldKey {
    match field {
        OracleFieldKey::Index(index) => FieldKey::index(*index),
        OracleFieldKey::Name(name) => FieldKey::Name(name.clone()),
    }
}

fn runtime_type_without_layout(ty: &StructuralTy) -> Option<Type> {
    match ty {
        StructuralTy::U64 => Some(Type::U64),
        StructuralTy::Bool => Some(Type::Bool),
        StructuralTy::Callable(params, ret) => Some(Type::Callable(CallableSig::new(
            params
                .iter()
                .map(runtime_type_without_layout)
                .collect::<Option<Vec<_>>>()?,
            runtime_type_without_layout(ret)?,
        ))),
        StructuralTy::Product(_) => None,
    }
}

fn match_add_capture_body(body: &OracleExpr, param_name: &str) -> Option<String> {
    let OracleExpr::Add(lhs, rhs) = body else {
        return None;
    };

    match (&**lhs, &**rhs) {
        (OracleExpr::Local(lhs), OracleExpr::Local(rhs)) if lhs == param_name => Some(rhs.clone()),
        (OracleExpr::Local(lhs), OracleExpr::Local(rhs)) if rhs == param_name => Some(lhs.clone()),
        _ => None,
    }
}

fn make_named_call_kernel(
    lowering: &mut StructuralLowering,
    name: &str,
    function: &FunctionDef,
    arg_types: &[StructuralTy],
) -> Option<(KernelId, LayoutId, Vec<String>)> {
    match &function.body {
        OracleExpr::Trap(message) if function.params.is_empty() => {
            let frame_layout = Layout {
                id: LayoutId::new(lowering.layouts.len() as u32),
                name: format!("{name}Frame"),
                kind: LayoutKind::ThunkFrame { fields: vec![] },
            };
            let frame_layout_id = frame_layout.id;
            lowering.layouts.push(frame_layout);
            let kernel_id = lowering.alloc_kernel_id();
            lowering.push_kernel(trap_kernel_named(
                kernel_id,
                format!("thunk_{name}"),
                message.clone(),
            ));
            Some((kernel_id, frame_layout_id, vec![]))
        }
        OracleExpr::Add(_, _) if function.params.len() == 1 && arg_types == [StructuralTy::U64] => {
            let increment = match_add_local_and_const(&function.body, &function.params[0])?;
            let frame_layout = Layout {
                id: LayoutId::new(lowering.layouts.len() as u32),
                name: format!("{name}Frame"),
                kind: LayoutKind::ThunkFrame {
                    fields: vec![NamedField {
                        name: "arg0_ref".to_owned(),
                        ty: Type::ref_of(Type::U64),
                    }],
                },
            };
            let frame_layout_id = frame_layout.id;
            lowering.layouts.push(frame_layout);
            let kernel_id = lowering.alloc_kernel_id();
            lowering.push_kernel(enter_add_const_kernel_named(
                kernel_id,
                format!("enter_{name}"),
                increment,
            ));
            Some((kernel_id, frame_layout_id, vec!["arg0_ref".to_owned()]))
        }
        OracleExpr::Call { callee, args }
            if function.params.len() == 2
                && args.len() == 1
                && matches!(&**callee, OracleExpr::Local(local) if local == &function.params[0])
                && matches!(&args[0], OracleExpr::Local(local) if local == &function.params[1]) =>
        {
            let call_result_layout =
                call_result_frame_layout(LayoutId::new(lowering.layouts.len() as u32));
            let call_result_layout_id = call_result_layout.id;
            lowering.layouts.push(call_result_layout);
            let call_result_kernel_id = lowering.alloc_kernel_id();
            lowering.push_kernel(call_result_kernel_named(
                call_result_kernel_id,
                format!("call_result_{}", call_result_kernel_id.0),
            ));

            let apply_frame =
                apply_frame_layout_named(LayoutId::new(lowering.layouts.len() as u32));
            let apply_frame_id = apply_frame.id;
            lowering.layouts.push(apply_frame);
            let apply_kernel_id = lowering.alloc_kernel_id();
            lowering.push_kernel(enter_apply_kernel_named(
                apply_kernel_id,
                format!("enter_{name}"),
                call_result_layout_id,
                call_result_kernel_id,
            ));

            Some((
                apply_kernel_id,
                apply_frame_id,
                vec!["f_ref".to_owned(), "x_ref".to_owned()],
            ))
        }
        _ => None,
    }
}

fn match_add_local_and_const(expr: &OracleExpr, local_name: &str) -> Option<u64> {
    let OracleExpr::Add(lhs, rhs) = expr else {
        return None;
    };

    match (&**lhs, &**rhs) {
        (OracleExpr::Local(name), OracleExpr::U64(value)) if name == local_name => Some(*value),
        (OracleExpr::U64(value), OracleExpr::Local(name)) if name == local_name => Some(*value),
        _ => None,
    }
}
fn call_result_frame_layout(id: LayoutId) -> Layout {
    Layout {
        id,
        name: "CallResultFrame".to_owned(),
        kind: LayoutKind::ThunkFrame {
            fields: vec![
                NamedField {
                    name: "callable".to_owned(),
                    ty: Type::Callable(u64_to_u64_sig()),
                },
                NamedField {
                    name: "arg0_ref".to_owned(),
                    ty: Type::ref_of(Type::U64),
                },
            ],
        },
    }
}

fn trap_kernel_named(id: KernelId, name: String, message: String) -> Kernel {
    Kernel {
        id,
        name,
        params: vec![
            local(0, "ctx", Type::RuntimeCtx),
            local(1, "self", Type::thunk_handle(Type::U64)),
            local(2, "obs", Type::obs_of(Type::U64)),
        ],
        locals: vec![],
        return_type: ReturnType::Never,
        entry: BlockId::new(0),
        blocks: vec![Block {
            id: BlockId::new(0),
            stmts: vec![],
            terminator: Terminator::Trap { message },
        }],
    }
}

fn enter_add_const_kernel_named(id: KernelId, name: String, increment: u64) -> Kernel {
    Kernel {
        id,
        name,
        params: vec![
            local(0, "ctx", Type::RuntimeCtx),
            local(1, "self", Type::thunk_handle(Type::U64)),
            local(2, "arg0_ref", Type::ref_of(Type::U64)),
            local(3, "obs", Type::obs_of(Type::U64)),
        ],
        locals: vec![
            local(4, "state", Type::ThunkState),
            local(5, "is_ready", Type::Bool),
            local(6, "out", Type::U64),
            local(7, "arg0", Type::U64),
            local(8, "increment", Type::U64),
        ],
        return_type: ReturnType::Never,
        entry: BlockId::new(0),
        blocks: vec![
            Block {
                id: BlockId::new(0),
                stmts: vec![
                    Stmt::ThunkState {
                        dst: LocalId::new(4),
                        thunk: LocalId::new(1),
                    },
                    Stmt::StateIsReady {
                        dst: LocalId::new(5),
                        state: LocalId::new(4),
                    },
                ],
                terminator: Terminator::Branch {
                    cond: LocalId::new(5),
                    then_block: BlockId::new(1),
                    else_block: BlockId::new(2),
                },
            },
            Block {
                id: BlockId::new(1),
                stmts: vec![
                    Stmt::LoadCachedU64 {
                        dst: LocalId::new(6),
                        thunk: LocalId::new(1),
                    },
                    Stmt::CacheReadyU64 {
                        thunk: LocalId::new(1),
                        value: LocalId::new(6),
                    },
                ],
                terminator: Terminator::Done,
            },
            Block {
                id: BlockId::new(2),
                stmts: vec![
                    Stmt::MarkEvaluating {
                        thunk: LocalId::new(1),
                    },
                    Stmt::EnterU64 {
                        dst: LocalId::new(7),
                        reference: LocalId::new(2),
                        obs: ObsLiteral::NeedValue,
                    },
                    Stmt::ConstU64 {
                        dst: LocalId::new(8),
                        value: increment,
                    },
                    Stmt::AddU64 {
                        dst: LocalId::new(6),
                        lhs: LocalId::new(7),
                        rhs: LocalId::new(8),
                    },
                    Stmt::CacheReadyU64 {
                        thunk: LocalId::new(1),
                        value: LocalId::new(6),
                    },
                ],
                terminator: Terminator::Done,
            },
        ],
    }
}

fn enter_add_capture_kernel_named(
    id: KernelId,
    name: String,
    env_layout: LayoutId,
    capture_name: String,
) -> Kernel {
    Kernel {
        id,
        name,
        params: vec![
            local(0, "ctx", Type::RuntimeCtx),
            local(1, "self", Type::thunk_handle(Type::U64)),
            local(2, "env", Type::EnvHandle(env_layout)),
            local(3, "x_ref", Type::ref_of(Type::U64)),
            local(4, "obs", Type::obs_of(Type::U64)),
        ],
        locals: vec![
            local(5, "n_ref", Type::ref_of(Type::U64)),
            local(6, "x", Type::U64),
            local(7, "n", Type::U64),
            local(8, "out", Type::U64),
        ],
        return_type: ReturnType::Never,
        entry: BlockId::new(0),
        blocks: vec![Block {
            id: BlockId::new(0),
            stmts: vec![
                Stmt::EnvGetCapture {
                    dst: LocalId::new(5),
                    env: LocalId::new(2),
                    capture: capture_name,
                },
                Stmt::EnterU64 {
                    dst: LocalId::new(6),
                    reference: LocalId::new(3),
                    obs: ObsLiteral::NeedValue,
                },
                Stmt::EnterU64 {
                    dst: LocalId::new(7),
                    reference: LocalId::new(5),
                    obs: ObsLiteral::NeedValue,
                },
                Stmt::AddU64 {
                    dst: LocalId::new(8),
                    lhs: LocalId::new(6),
                    rhs: LocalId::new(7),
                },
                Stmt::CacheReadyU64 {
                    thunk: LocalId::new(1),
                    value: LocalId::new(8),
                },
            ],
            terminator: Terminator::Done,
        }],
    }
}

fn call_result_kernel_named(id: KernelId, name: String) -> Kernel {
    Kernel {
        id,
        name,
        params: vec![
            local(0, "ctx", Type::RuntimeCtx),
            local(1, "self", Type::thunk_handle(Type::U64)),
            local(2, "callable", Type::Callable(u64_to_u64_sig())),
            local(3, "arg0_ref", Type::ref_of(Type::U64)),
            local(4, "obs", Type::obs_of(Type::U64)),
        ],
        locals: vec![],
        return_type: ReturnType::Never,
        entry: BlockId::new(0),
        blocks: vec![Block {
            id: BlockId::new(0),
            stmts: vec![Stmt::CallCallable {
                callable: LocalId::new(2),
                self_thunk: LocalId::new(1),
                args: vec![LocalId::new(3)],
                obs: ObsLiteral::NeedValue,
            }],
            terminator: Terminator::Done,
        }],
    }
}

fn apply_frame_layout_named(id: LayoutId) -> Layout {
    Layout {
        id,
        name: format!("ApplyFrame{}", id.0),
        kind: LayoutKind::ThunkFrame {
            fields: vec![
                NamedField {
                    name: "f_ref".to_owned(),
                    ty: Type::ref_of(Type::Callable(u64_to_u64_sig())),
                },
                NamedField {
                    name: "x_ref".to_owned(),
                    ty: Type::ref_of(Type::U64),
                },
            ],
        },
    }
}

fn enter_apply_kernel_named(
    id: KernelId,
    name: String,
    call_result_frame: LayoutId,
    call_result_kernel: KernelId,
) -> Kernel {
    Kernel {
        id,
        name,
        params: vec![
            local(0, "ctx", Type::RuntimeCtx),
            local(1, "self", Type::thunk_handle(Type::U64)),
            local(2, "f_ref", Type::ref_of(Type::Callable(u64_to_u64_sig()))),
            local(3, "x_ref", Type::ref_of(Type::U64)),
            local(4, "obs", Type::obs_of(Type::U64)),
        ],
        locals: vec![
            local(5, "callable", Type::Callable(u64_to_u64_sig())),
            local(6, "result", Type::ref_of(Type::U64)),
            local(7, "out", Type::U64),
        ],
        return_type: ReturnType::Never,
        entry: BlockId::new(0),
        blocks: vec![Block {
            id: BlockId::new(0),
            stmts: vec![
                Stmt::NeedCallable {
                    dst: LocalId::new(5),
                    reference: LocalId::new(2),
                },
                Stmt::AllocThunk {
                    dst: LocalId::new(6),
                    kernel: call_result_kernel,
                    frame_layout: call_result_frame,
                    frame: vec![
                        ("callable".to_owned(), LocalId::new(5)),
                        ("arg0_ref".to_owned(), LocalId::new(3)),
                    ],
                },
                Stmt::EnterU64 {
                    dst: LocalId::new(7),
                    reference: LocalId::new(6),
                    obs: ObsLiteral::NeedValue,
                },
                Stmt::CacheReadyU64 {
                    thunk: LocalId::new(1),
                    value: LocalId::new(7),
                },
            ],
            terminator: Terminator::Done,
        }],
    }
}

fn u64_to_u64_sig() -> CallableSig {
    CallableSig::new(vec![Type::U64], Type::U64)
}

fn local(id: u32, name: &str, ty: Type) -> LocalDecl {
    LocalDecl {
        id: LocalId::new(id),
        name: name.to_owned(),
        ty,
    }
}
