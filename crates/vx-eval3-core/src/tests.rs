use crate::{KernelId, LocalId, Stmt, Terminator, lower_program};
use vx_eval3_oracle::{Expr, FieldKey as OracleFieldKey, FunctionDef, Program};

#[test]
fn program_a_shape_matches_lazy_product_plan() {
    let module = lower_program(&Program::canonical_program_a())
        .expect("canonical Program A should lower successfully");
    let main = module
        .kernel_named("enter_main")
        .expect("enter_main kernel should exist");

    assert_eq!(module.layouts.len(), 2);
    assert!(main.blocks[0].stmts.iter().any(|stmt| matches!(
        stmt,
        Stmt::AllocThunk {
            kernel,
            ..
        } if *kernel == KernelId::new(1)
    )));
    assert!(
        main.blocks[0]
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::AllocProduct { .. }))
    );
    assert!(main.blocks[0].stmts.iter().any(|stmt| matches!(
        stmt,
        Stmt::ProjectField {
            field,
            ..
        } if *field == crate::FieldKey::index(0)
    )));
    assert!(matches!(
        module
            .kernel_named("thunk_bomb")
            .expect("thunk_bomb kernel should exist")
            .blocks[0]
            .terminator,
        Terminator::Trap { .. }
    ));
}

#[test]
fn program_c_shape_matches_shared_thunk_plan() {
    let module = lower_program(&Program::canonical_program_c())
        .expect("canonical Program C should lower successfully");
    let main = module
        .kernel_named("enter_main")
        .expect("enter_main kernel should exist");
    let enters = main.blocks[0]
        .stmts
        .iter()
        .filter(
            |stmt| matches!(stmt, Stmt::EnterU64 { reference, .. } if *reference == LocalId::new(4)),
        )
        .count();
    assert_eq!(enters, 2);

    let plus_one = module
        .kernel_named("enter_plus_one")
        .expect("enter_plus_one kernel should exist");
    assert_eq!(plus_one.blocks.len(), 3);
    assert!(matches!(
        plus_one.blocks[0].terminator,
        Terminator::Branch { .. }
    ));
    assert!(
        plus_one.blocks[1]
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::LoadCachedU64 { .. }))
    );
    assert!(
        plus_one.blocks[2]
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::MarkEvaluating { .. }))
    );
}

#[test]
fn display_renders_expected_ops() {
    let rendered = lower_program(&Program::canonical_program_c())
        .expect("canonical Program C should lower successfully")
        .to_string();

    assert!(rendered.contains("thunk_frame"));
    assert!(rendered.contains("alloc_thunk"));
    assert!(rendered.contains("enter_u64"));
    assert!(rendered.contains("NeedValue"));
    assert!(rendered.contains("state = thunk_state self"));
    assert!(rendered.contains("is_ready = state_is_ready state"));
}

#[test]
fn program_b_shape_matches_closure_pair_plan() {
    let module = lower_program(&Program::canonical_program_b())
        .expect("canonical Program B should lower successfully");
    let main = module
        .kernel_named("enter_main")
        .expect("enter_main kernel should exist");

    assert!(module.layouts.len() >= 3);
    assert!(
        main.blocks[0]
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::AllocEnv { .. }))
    );
    assert!(
        main.blocks[0]
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::AllocClosure { .. }))
    );
    let call_result_allocs = main.blocks[0]
        .stmts
        .iter()
        .filter(|stmt| matches!(stmt, Stmt::AllocThunk { .. }))
        .count();
    assert_eq!(call_result_allocs, 2);
    assert!(main.blocks[0].stmts.iter().any(|stmt| matches!(
        stmt,
        Stmt::ProjectField {
            field,
            ..
        } if *field == crate::FieldKey::index(1)
    )));
    assert_eq!(
        main.blocks[0]
            .stmts
            .iter()
            .filter(|stmt| matches!(stmt, Stmt::EnterU64 { .. }))
            .count(),
        1
    );
}

#[test]
fn program_d_shape_matches_higher_order_plan() {
    let module = lower_program(&Program::canonical_program_d())
        .expect("canonical Program D should lower successfully");
    let main = module
        .kernel_named("enter_main")
        .expect("enter_main kernel should exist");

    assert!(
        main.blocks[0]
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::ReadyCallable { .. }))
    );
    assert!(
        module
            .kernels
            .iter()
            .any(|kernel| kernel.blocks.iter().any(|block| block
                .stmts
                .iter()
                .any(|stmt| matches!(stmt, Stmt::NeedCallable { .. }))))
    );
    assert!(
        module
            .kernels
            .iter()
            .any(|kernel| kernel.blocks.iter().any(|block| block
                .stmts
                .iter()
                .any(|stmt| matches!(stmt, Stmt::CallCallable { .. }))))
    );
}

#[test]
fn non_canonical_if_program_lowers_structurally() {
    let program = Program::new([FunctionDef {
        name: "main".to_owned(),
        params: vec!["b".to_owned()],
        body: Expr::If {
            condition: Box::new(Expr::Local("b".to_owned())),
            then_branch: Box::new(Expr::U64(1)),
            else_branch: Box::new(Expr::U64(0)),
        },
    }]);
    let module = lower_program(&program).expect("non-canonical if program should lower");
    let main = module
        .kernel_named("enter_main")
        .expect("enter_main kernel should exist");

    assert!(module.layouts.is_empty());
    assert!(
        main.blocks[0]
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::EnterBool { .. }))
    );
    assert!(matches!(
        main.blocks[0].terminator,
        Terminator::Branch { .. }
    ));
    assert_eq!(main.blocks.len(), 4);
    assert!(
        main.blocks[1]
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::ReadyU64 { .. }))
    );
    assert!(
        main.blocks[2]
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::ReadyU64 { .. }))
    );
    assert!(
        main.blocks[3]
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::EnterU64 { .. }))
    );
}

#[test]
fn non_canonical_projection_program_lowers_structurally() {
    let program = Program::new([FunctionDef {
        name: "main".to_owned(),
        params: vec!["b".to_owned()],
        body: Expr::Let {
            name: "pair".to_owned(),
            value: Box::new(Expr::Tuple(vec![
                Expr::U64(1),
                Expr::If {
                    condition: Box::new(Expr::Local("b".to_owned())),
                    then_branch: Box::new(Expr::U64(2)),
                    else_branch: Box::new(Expr::U64(3)),
                },
            ])),
            body: Box::new(Expr::Field {
                base: Box::new(Expr::Local("pair".to_owned())),
                field: OracleFieldKey::index(1),
            }),
        },
    }]);
    let module = lower_program(&program).expect("non-canonical projection program should lower");
    let main = module
        .kernel_named("enter_main")
        .expect("enter_main kernel should exist");

    assert_eq!(module.layouts.len(), 1);
    assert!(main.blocks.iter().any(|block| {
        block
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::AllocProduct { .. }))
    }));
    assert!(main.blocks.iter().any(|block| {
        block
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::ProjectField { .. }))
    }));
    assert!(main.blocks.iter().any(|block| {
        block
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::EnterBool { .. }))
    }));
}
