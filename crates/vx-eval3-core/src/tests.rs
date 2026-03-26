use crate::{
    KernelId, LocalId, Stmt, Terminator, lower_program, program_a_module, program_b_module,
    program_c_module, program_d_module,
};
use vx_eval3_oracle::Program;

#[test]
fn program_a_shape_matches_lazy_product_plan() {
    let module = program_a_module();
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
            product,
            ..
        } if *product == LocalId::new(8)
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
    let module = program_c_module();
    let main = module
        .kernel_named("enter_main")
        .expect("enter_main kernel should exist");
    let enters = main.blocks[0]
        .stmts
        .iter()
        .filter(|stmt| matches!(stmt, Stmt::EnterU64 { reference, .. } if *reference == LocalId::new(4)))
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
    let rendered = program_c_module().to_string();

    assert!(rendered.contains("layout #0 PlusOneFrame = thunk_frame"));
    assert!(rendered.contains("x = alloc_thunk kernel#0 frame#0"));
    assert!(rendered.contains("lhs = enter_u64 x, NeedValue"));
    assert!(rendered.contains("rhs = enter_u64 x, NeedValue"));
    assert!(rendered.contains("state = thunk_state self"));
    assert!(rendered.contains("is_ready = state_is_ready state"));
}

#[test]
fn program_b_shape_matches_closure_pair_plan() {
    let module = program_b_module();
    let main = module
        .kernel_named("enter_main")
        .expect("enter_main kernel should exist");

    assert_eq!(module.layouts.len(), 3);
    assert!(
        main.blocks[0]
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::AllocEnv { .. }))
    );
    assert!(main.blocks[0].stmts.iter().any(|stmt| matches!(
        stmt,
        Stmt::AllocClosure {
            kernel,
            ..
        } if *kernel == KernelId::new(0)
    )));
    let call_result_allocs = main.blocks[0]
        .stmts
        .iter()
        .filter(
            |stmt| matches!(stmt, Stmt::AllocThunk { kernel, .. } if *kernel == KernelId::new(1)),
        )
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
    let module = program_d_module();
    let main = module
        .kernel_named("enter_main")
        .expect("enter_main kernel should exist");
    let apply = module
        .kernel_named("enter_apply")
        .expect("enter_apply kernel should exist");
    let call_result = module
        .kernel_named("call_result")
        .expect("call_result kernel should exist");

    assert!(
        main.blocks[0]
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::ReadyCallable { .. }))
    );
    assert!(
        apply.blocks[0]
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::NeedCallable { .. }))
    );
    assert!(
        call_result.blocks[0]
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::CallCallable { .. }))
    );
}

#[test]
fn canonical_program_a_lowers_to_program_a_module() {
    let lowered = lower_program(&Program::canonical_program_a())
        .expect("canonical Program A should lower successfully");

    assert_eq!(lowered, program_a_module());
}

#[test]
fn canonical_program_c_lowers_to_program_c_module() {
    let lowered = lower_program(&Program::canonical_program_c())
        .expect("canonical Program C should lower successfully");

    assert_eq!(lowered, program_c_module());
}

#[test]
fn canonical_program_b_is_not_yet_supported() {
    let error = lower_program(&Program::canonical_program_b())
        .expect_err("canonical Program B should not lower yet");

    assert!(
        error
            .to_string()
            .contains("supports only canonical Program A and Program C")
    );
}
