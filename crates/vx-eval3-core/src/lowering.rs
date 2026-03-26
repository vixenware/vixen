use std::{error::Error, fmt};

use vx_eval3_oracle::{Expr as OracleExpr, FieldKey as OracleFieldKey, Program};

use crate::{
    Block, BlockId, FieldKey, Kernel, KernelId, Layout, LayoutId, LayoutKind, LocalDecl, LocalId,
    Module, NamedField, ObsLiteral, ProductField, ReturnType, Stmt, Terminator, Type,
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
    if let Some(info) = match_program_a(program) {
        return Ok(lower_program_a(info));
    }

    if let Some(info) = match_program_c(program) {
        return Ok(lower_program_c(info));
    }

    Err(LoweringError::UnsupportedProgram {
        reason: "slice-0 lowering currently supports only canonical Program A and Program C"
            .to_owned(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProgramAInfo {
    trap_name: String,
    trap_message: String,
    lhs: u64,
    rhs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProgramCInfo {
    helper_name: String,
    increment: u64,
}

fn match_program_a(program: &Program) -> Option<ProgramAInfo> {
    let main = program.functions.get("main")?;
    if !main.params.is_empty() {
        return None;
    }

    let (pair_name, tuple_fields, selected_field) = match &main.body {
        OracleExpr::Let { name, value, body } => match (&**value, &**body) {
            (OracleExpr::Tuple(fields), OracleExpr::Field { base, field }) => {
                let OracleExpr::Local(base_name) = &**base else {
                    return None;
                };
                if base_name != name {
                    return None;
                }
                (name.as_str(), fields, field)
            }
            _ => return None,
        },
        _ => return None,
    };

    if pair_name != "pair" || *selected_field != OracleFieldKey::index(0) || tuple_fields.len() != 2
    {
        return None;
    }

    let (lhs, rhs) = match &tuple_fields[0] {
        OracleExpr::Add(lhs, rhs) => match (&**lhs, &**rhs) {
            (OracleExpr::U64(lhs), OracleExpr::U64(rhs)) => (*lhs, *rhs),
            _ => return None,
        },
        _ => return None,
    };

    let trap_name = match &tuple_fields[1] {
        OracleExpr::CallNamed { name, args } if args.is_empty() => name.clone(),
        _ => return None,
    };

    let trap = trap_function(program, &trap_name)?;
    Some(ProgramAInfo {
        trap_name,
        trap_message: trap.clone(),
        lhs,
        rhs,
    })
}

fn match_program_c(program: &Program) -> Option<ProgramCInfo> {
    let main = program.functions.get("main")?;
    if main.params != ["n".to_owned()] {
        return None;
    }

    let (shared_name, helper_name) = match &main.body {
        OracleExpr::Let { name, value, body } => {
            let OracleExpr::CallNamed {
                name: helper_name,
                args,
            } = &**value
            else {
                return None;
            };
            if args.len() != 1 || args[0] != OracleExpr::Local("n".to_owned()) {
                return None;
            }
            let OracleExpr::Add(lhs, rhs) = &**body else {
                return None;
            };
            if **lhs != OracleExpr::Local(name.clone()) || **rhs != OracleExpr::Local(name.clone())
            {
                return None;
            }
            (name.as_str(), helper_name.as_str())
        }
        _ => return None,
    };

    if shared_name != "x" {
        return None;
    }

    let helper = program.functions.get(helper_name)?;
    if helper.params.len() != 1 {
        return None;
    }

    let increment = match_add_local_and_const(&helper.body, helper.params[0].as_str())?;
    Some(ProgramCInfo {
        helper_name: helper.name.clone(),
        increment,
    })
}

fn trap_function(program: &Program, name: &str) -> Option<String> {
    let function = program.functions.get(name)?;
    if !function.params.is_empty() {
        return None;
    }

    let OracleExpr::Trap(message) = &function.body else {
        return None;
    };
    Some(message.clone())
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

fn lower_program_a(info: ProgramAInfo) -> Module {
    let pair_layout = Layout {
        id: LayoutId::new(0),
        name: "PairU64U64".to_owned(),
        kind: LayoutKind::Product {
            fields: vec![
                ProductField {
                    key: FieldKey::index(0),
                    ty: Type::ref_of(Type::U64),
                },
                ProductField {
                    key: FieldKey::index(1),
                    ty: Type::ref_of(Type::U64),
                },
            ],
        },
    };
    let bomb_frame = Layout {
        id: LayoutId::new(1),
        name: "BombFrame".to_owned(),
        kind: LayoutKind::ThunkFrame { fields: vec![] },
    };

    let enter_main = Kernel {
        id: KernelId::new(0),
        name: "enter_main".to_owned(),
        params: vec![
            local(0, "ctx", Type::RuntimeCtx),
            local(1, "self", Type::thunk_handle(Type::U64)),
            local(2, "obs", Type::obs_of(Type::U64)),
        ],
        locals: vec![
            local(3, "forty_one", Type::U64),
            local(4, "one", Type::U64),
            local(5, "forty_two", Type::U64),
            local(6, "field0", Type::ref_of(Type::U64)),
            local(7, "field1", Type::ref_of(Type::U64)),
            local(
                8,
                "pair",
                Type::ref_of(Type::ProductHandle(LayoutId::new(0))),
            ),
            local(9, "selected", Type::ref_of(Type::U64)),
            local(10, "out", Type::U64),
        ],
        return_type: ReturnType::Never,
        entry: BlockId::new(0),
        blocks: vec![Block {
            id: BlockId::new(0),
            stmts: vec![
                Stmt::ConstU64 {
                    dst: LocalId::new(3),
                    value: info.lhs,
                },
                Stmt::ConstU64 {
                    dst: LocalId::new(4),
                    value: info.rhs,
                },
                Stmt::AddU64 {
                    dst: LocalId::new(5),
                    lhs: LocalId::new(3),
                    rhs: LocalId::new(4),
                },
                Stmt::ReadyU64 {
                    dst: LocalId::new(6),
                    value: LocalId::new(5),
                },
                Stmt::AllocThunk {
                    dst: LocalId::new(7),
                    kernel: KernelId::new(1),
                    frame_layout: LayoutId::new(1),
                    frame: vec![],
                },
                Stmt::AllocProduct {
                    dst: LocalId::new(8),
                    layout: LayoutId::new(0),
                    fields: vec![
                        (FieldKey::index(0), LocalId::new(6)),
                        (FieldKey::index(1), LocalId::new(7)),
                    ],
                },
                Stmt::ProjectField {
                    dst: LocalId::new(9),
                    product: LocalId::new(8),
                    field: FieldKey::index(0),
                },
                Stmt::EnterU64 {
                    dst: LocalId::new(10),
                    reference: LocalId::new(9),
                    obs: ObsLiteral::NeedValue,
                },
                Stmt::CacheReadyU64 {
                    thunk: LocalId::new(1),
                    value: LocalId::new(10),
                },
            ],
            terminator: Terminator::Done,
        }],
    };

    let trap_kernel = Kernel {
        id: KernelId::new(1),
        name: format!("thunk_{}", info.trap_name),
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
            terminator: Terminator::Trap {
                message: info.trap_message,
            },
        }],
    };

    Module {
        layouts: vec![pair_layout, bomb_frame],
        kernels: vec![enter_main, trap_kernel],
    }
}

fn lower_program_c(info: ProgramCInfo) -> Module {
    let plus_one_frame = Layout {
        id: LayoutId::new(0),
        name: "PlusOneFrame".to_owned(),
        kind: LayoutKind::ThunkFrame {
            fields: vec![NamedField {
                name: "n_ref".to_owned(),
                ty: Type::ref_of(Type::U64),
            }],
        },
    };

    let enter_plus_one = Kernel {
        id: KernelId::new(0),
        name: format!("enter_{}", info.helper_name),
        params: vec![
            local(0, "ctx", Type::RuntimeCtx),
            local(1, "self", Type::thunk_handle(Type::U64)),
            local(2, "n_ref", Type::ref_of(Type::U64)),
            local(3, "obs", Type::obs_of(Type::U64)),
        ],
        locals: vec![
            local(4, "state", Type::ThunkState),
            local(5, "is_ready", Type::Bool),
            local(6, "out", Type::U64),
            local(7, "n", Type::U64),
            local(8, "one", Type::U64),
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
                        value: info.increment,
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
    };

    let enter_main = Kernel {
        id: KernelId::new(1),
        name: "enter_main".to_owned(),
        params: vec![
            local(0, "ctx", Type::RuntimeCtx),
            local(1, "self", Type::thunk_handle(Type::U64)),
            local(2, "n_ref", Type::ref_of(Type::U64)),
            local(3, "obs", Type::obs_of(Type::U64)),
        ],
        locals: vec![
            local(4, "x", Type::ref_of(Type::U64)),
            local(5, "lhs", Type::U64),
            local(6, "rhs", Type::U64),
            local(7, "sum", Type::U64),
        ],
        return_type: ReturnType::Never,
        entry: BlockId::new(0),
        blocks: vec![Block {
            id: BlockId::new(0),
            stmts: vec![
                Stmt::AllocThunk {
                    dst: LocalId::new(4),
                    kernel: KernelId::new(0),
                    frame_layout: LayoutId::new(0),
                    frame: vec![("n_ref".to_owned(), LocalId::new(2))],
                },
                Stmt::EnterU64 {
                    dst: LocalId::new(5),
                    reference: LocalId::new(4),
                    obs: ObsLiteral::NeedValue,
                },
                Stmt::EnterU64 {
                    dst: LocalId::new(6),
                    reference: LocalId::new(4),
                    obs: ObsLiteral::NeedValue,
                },
                Stmt::AddU64 {
                    dst: LocalId::new(7),
                    lhs: LocalId::new(5),
                    rhs: LocalId::new(6),
                },
                Stmt::CacheReadyU64 {
                    thunk: LocalId::new(1),
                    value: LocalId::new(7),
                },
            ],
            terminator: Terminator::Done,
        }],
    };

    Module {
        layouts: vec![plus_one_frame],
        kernels: vec![enter_plus_one, enter_main],
    }
}

fn local(id: u32, name: &str, ty: Type) -> LocalDecl {
    LocalDecl {
        id: LocalId::new(id),
        name: name.to_owned(),
        ty,
    }
}
