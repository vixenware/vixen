use crate::{
    Block, BlockId, FieldKey, Kernel, KernelId, Layout, LayoutId, LayoutKind, LocalDecl, LocalId,
    Module, NamedField, ObsLiteral, ProductField, ReturnType, Stmt, Terminator, Type,
};

pub fn program_a_module() -> Module {
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
                    value: 41,
                },
                Stmt::ConstU64 {
                    dst: LocalId::new(4),
                    value: 1,
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

    let thunk_bomb = Kernel {
        id: KernelId::new(1),
        name: "thunk_bomb".to_owned(),
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
                message: "bomb".to_owned(),
            },
        }],
    };

    Module {
        layouts: vec![pair_layout, bomb_frame],
        kernels: vec![enter_main, thunk_bomb],
    }
}

pub fn program_c_module() -> Module {
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
        name: "enter_plus_one".to_owned(),
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
                        value: 1,
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

pub fn program_b_module() -> Module {
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
    let add_n_env = Layout {
        id: LayoutId::new(1),
        name: "AddNEnv".to_owned(),
        kind: LayoutKind::Env {
            fields: vec![NamedField {
                name: "n_ref".to_owned(),
                ty: Type::ref_of(Type::U64),
            }],
        },
    };
    let call_result_frame = Layout {
        id: LayoutId::new(2),
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
    };

    let enter_add_n = Kernel {
        id: KernelId::new(0),
        name: "enter_add_n".to_owned(),
        params: vec![
            local(0, "ctx", Type::RuntimeCtx),
            local(1, "self", Type::thunk_handle(Type::U64)),
            local(2, "env", Type::EnvHandle(LayoutId::new(1))),
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
                    capture: "n_ref".to_owned(),
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
    };

    let call_result = Kernel {
        id: KernelId::new(1),
        name: "call_result".to_owned(),
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
    };

    let enter_main = Kernel {
        id: KernelId::new(2),
        name: "enter_main".to_owned(),
        params: vec![
            local(0, "ctx", Type::RuntimeCtx),
            local(1, "self", Type::thunk_handle(Type::U64)),
            local(2, "n_ref", Type::ref_of(Type::U64)),
            local(3, "obs", Type::obs_of(Type::U64)),
        ],
        locals: vec![
            local(4, "env", Type::EnvHandle(LayoutId::new(1))),
            local(5, "add_n", Type::Callable(u64_to_u64_sig())),
            local(6, "one", Type::U64),
            local(7, "two", Type::U64),
            local(8, "one_ref", Type::ref_of(Type::U64)),
            local(9, "two_ref", Type::ref_of(Type::U64)),
            local(10, "call1", Type::ref_of(Type::U64)),
            local(11, "call2", Type::ref_of(Type::U64)),
            local(
                12,
                "pair",
                Type::ref_of(Type::ProductHandle(LayoutId::new(0))),
            ),
            local(13, "selected", Type::ref_of(Type::U64)),
            local(14, "out", Type::U64),
        ],
        return_type: ReturnType::Never,
        entry: BlockId::new(0),
        blocks: vec![Block {
            id: BlockId::new(0),
            stmts: vec![
                Stmt::AllocEnv {
                    dst: LocalId::new(4),
                    layout: LayoutId::new(1),
                    captures: vec![("n_ref".to_owned(), LocalId::new(2))],
                },
                Stmt::AllocClosure {
                    dst: LocalId::new(5),
                    kernel: KernelId::new(0),
                    env: Some(LocalId::new(4)),
                },
                Stmt::ConstU64 {
                    dst: LocalId::new(6),
                    value: 1,
                },
                Stmt::ConstU64 {
                    dst: LocalId::new(7),
                    value: 2,
                },
                Stmt::ReadyU64 {
                    dst: LocalId::new(8),
                    value: LocalId::new(6),
                },
                Stmt::ReadyU64 {
                    dst: LocalId::new(9),
                    value: LocalId::new(7),
                },
                Stmt::AllocThunk {
                    dst: LocalId::new(10),
                    kernel: KernelId::new(1),
                    frame_layout: LayoutId::new(2),
                    frame: vec![
                        ("callable".to_owned(), LocalId::new(5)),
                        ("arg0_ref".to_owned(), LocalId::new(8)),
                    ],
                },
                Stmt::AllocThunk {
                    dst: LocalId::new(11),
                    kernel: KernelId::new(1),
                    frame_layout: LayoutId::new(2),
                    frame: vec![
                        ("callable".to_owned(), LocalId::new(5)),
                        ("arg0_ref".to_owned(), LocalId::new(9)),
                    ],
                },
                Stmt::AllocProduct {
                    dst: LocalId::new(12),
                    layout: LayoutId::new(0),
                    fields: vec![
                        (FieldKey::index(0), LocalId::new(10)),
                        (FieldKey::index(1), LocalId::new(11)),
                    ],
                },
                Stmt::ProjectField {
                    dst: LocalId::new(13),
                    product: LocalId::new(12),
                    field: FieldKey::index(1),
                },
                Stmt::EnterU64 {
                    dst: LocalId::new(14),
                    reference: LocalId::new(13),
                    obs: ObsLiteral::NeedValue,
                },
                Stmt::CacheReadyU64 {
                    thunk: LocalId::new(1),
                    value: LocalId::new(14),
                },
            ],
            terminator: Terminator::Done,
        }],
    };

    Module {
        layouts: vec![pair_layout, add_n_env, call_result_frame],
        kernels: vec![enter_add_n, call_result, enter_main],
    }
}

pub fn program_d_module() -> Module {
    let add_n_env = Layout {
        id: LayoutId::new(0),
        name: "AddNEnv".to_owned(),
        kind: LayoutKind::Env {
            fields: vec![NamedField {
                name: "n_ref".to_owned(),
                ty: Type::ref_of(Type::U64),
            }],
        },
    };
    let call_result_frame = Layout {
        id: LayoutId::new(1),
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
    };
    let apply_frame = Layout {
        id: LayoutId::new(2),
        name: "ApplyFrame".to_owned(),
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
    };

    let enter_add_n = Kernel {
        id: KernelId::new(0),
        name: "enter_add_n".to_owned(),
        params: vec![
            local(0, "ctx", Type::RuntimeCtx),
            local(1, "self", Type::thunk_handle(Type::U64)),
            local(2, "env", Type::EnvHandle(LayoutId::new(0))),
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
                    capture: "n_ref".to_owned(),
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
    };

    let call_result = Kernel {
        id: KernelId::new(1),
        name: "call_result".to_owned(),
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
    };

    let enter_apply = Kernel {
        id: KernelId::new(2),
        name: "enter_apply".to_owned(),
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
                    kernel: KernelId::new(1),
                    frame_layout: LayoutId::new(1),
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
    };

    let enter_main = Kernel {
        id: KernelId::new(3),
        name: "enter_main".to_owned(),
        params: vec![
            local(0, "ctx", Type::RuntimeCtx),
            local(1, "self", Type::thunk_handle(Type::U64)),
            local(2, "n_ref", Type::ref_of(Type::U64)),
            local(3, "obs", Type::obs_of(Type::U64)),
        ],
        locals: vec![
            local(4, "env", Type::EnvHandle(LayoutId::new(0))),
            local(5, "add_n", Type::Callable(u64_to_u64_sig())),
            local(
                6,
                "add_n_ref",
                Type::ref_of(Type::Callable(u64_to_u64_sig())),
            ),
            local(7, "forty_one", Type::U64),
            local(8, "forty_one_ref", Type::ref_of(Type::U64)),
            local(9, "apply_result", Type::ref_of(Type::U64)),
            local(10, "out", Type::U64),
        ],
        return_type: ReturnType::Never,
        entry: BlockId::new(0),
        blocks: vec![Block {
            id: BlockId::new(0),
            stmts: vec![
                Stmt::AllocEnv {
                    dst: LocalId::new(4),
                    layout: LayoutId::new(0),
                    captures: vec![("n_ref".to_owned(), LocalId::new(2))],
                },
                Stmt::AllocClosure {
                    dst: LocalId::new(5),
                    kernel: KernelId::new(0),
                    env: Some(LocalId::new(4)),
                },
                Stmt::ReadyCallable {
                    dst: LocalId::new(6),
                    callable: LocalId::new(5),
                },
                Stmt::ConstU64 {
                    dst: LocalId::new(7),
                    value: 41,
                },
                Stmt::ReadyU64 {
                    dst: LocalId::new(8),
                    value: LocalId::new(7),
                },
                Stmt::AllocThunk {
                    dst: LocalId::new(9),
                    kernel: KernelId::new(2),
                    frame_layout: LayoutId::new(2),
                    frame: vec![
                        ("f_ref".to_owned(), LocalId::new(6)),
                        ("x_ref".to_owned(), LocalId::new(8)),
                    ],
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

    Module {
        layouts: vec![add_n_env, call_result_frame, apply_frame],
        kernels: vec![enter_add_n, call_result, enter_apply, enter_main],
    }
}

fn local(id: u32, name: &str, ty: Type) -> LocalDecl {
    LocalDecl {
        id: LocalId::new(id),
        name: name.to_owned(),
        ty,
    }
}

fn u64_to_u64_sig() -> crate::CallableSig {
    crate::CallableSig::new(vec![Type::U64], Type::U64)
}
