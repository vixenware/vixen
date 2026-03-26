use super::{Obs, OracleError, Program, ReadyValue, Ref, TraceEvent};

#[test]
fn program_a_keeps_bomb_unentered() {
    let run = Program::canonical_program_a()
        .run_entry("main", vec![], Obs::NeedValue)
        .expect("program A should succeed");

    assert_eq!(run.result_u64(), Some(42));
    let bomb = run
        .node_named("call bomb")
        .expect("bomb call node should exist");
    assert_eq!(bomb.enter_count, 0);
    assert!(
        !run.trace
            .iter()
            .any(|event| matches!(event, TraceEvent::TrapEntered { message } if message == "bomb"))
    );
}

#[test]
fn program_b_allocates_one_closure_and_enters_one_sibling() {
    let run = Program::canonical_program_b()
        .run_entry("main", vec![Ref::ready_u64(10)], Obs::NeedValue)
        .expect("program B should succeed");

    assert_eq!(run.result_u64(), Some(12));
    let closure_allocs = run
        .trace
        .iter()
        .filter(|event| matches!(event, TraceEvent::ClosureAllocated { .. }))
        .count();
    assert_eq!(closure_allocs, 1);

    let env_allocs = run
        .trace
        .iter()
        .filter(|event| matches!(event, TraceEvent::EnvAllocated { .. }))
        .count();
    assert_eq!(env_allocs, 1);

    let call_enters = run
        .trace
        .iter()
        .filter(|event| matches!(event, TraceEvent::NodeEntered { label, .. } if label == "call"))
        .count();
    assert_eq!(call_enters, 1);
}

#[test]
fn program_c_reuses_shared_call_result() {
    let run = Program::canonical_program_c()
        .run_entry("main", vec![Ref::ready_u64(41)], Obs::NeedValue)
        .expect("program C should succeed");

    assert_eq!(run.result_u64(), Some(84));
    let plus_one = run
        .node_named("call plus_one")
        .expect("plus_one call node should exist");
    assert_eq!(plus_one.enter_count, 1);
    assert!(run.trace.iter().any(|event| matches!(
        event,
        TraceEvent::NodeReused { label, .. } if label == "call plus_one"
    )));
}

#[test]
fn program_d_supports_higher_order_local_call() {
    let run = Program::canonical_program_d()
        .run_entry("main", vec![Ref::ready_u64(10)], Obs::NeedValue)
        .expect("program D should succeed");

    assert_eq!(run.result_u64(), Some(51));
    let lambda = run.node_named("lambda").expect("lambda node should exist");
    assert_eq!(lambda.enter_count, 1);
    assert!(
        run.trace
            .iter()
            .any(|event| matches!(event, TraceEvent::ClosureAllocated { .. }))
    );
}

#[test]
fn program_a_traps_if_bomb_is_observed_directly() {
    let program = Program::canonical_program_a();
    let bomb = program.run_entry("bomb", vec![], Obs::NeedValue);
    assert_eq!(
        bomb.expect_err("bomb should trap"),
        OracleError::Trap {
            message: "bomb".to_owned()
        }
    );
}

#[test]
fn direct_callable_observation_is_explicit() {
    let run = Program::canonical_program_b()
        .run_entry("main", vec![Ref::Ready(ReadyValue::U64(0))], Obs::NeedValue)
        .expect("program B should still succeed");
    assert!(
        run.trace
            .iter()
            .any(|event| matches!(event, TraceEvent::NodeEntered { label, obs, .. } if label == "lambda" && obs == &Obs::NeedCallable))
    );
}
