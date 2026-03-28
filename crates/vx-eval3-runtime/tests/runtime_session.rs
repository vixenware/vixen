use vx_eval3_analysis::{
    BodyId, ExtractedNodeReason, KernelOp, ResultSource, SingleSourceBridge,
    build_minimal_analysis, lower_executable_program,
};
use vx_eval3_runtime::{
    BodyMemoKey, CellState, EvalOutcome, EvalRunResult, ExecValue, RuntimeMemoKey, RuntimeSession,
    WaitReason, eval_executable_program,
};

fn lower_and_eval(
    src: &str,
) -> (
    vx_eval3_analysis::AnalysisResult,
    vx_eval3_analysis::ExecutableProgram,
    EvalOutcome,
) {
    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    analysis.validate().expect("analysis should validate");
    let program = lower_executable_program(&analysis, &bridge).expect("program should lower");
    let outcome = eval_executable_program(&program).expect("program should evaluate");
    (analysis, program, outcome)
}

#[test]
fn exec_slice_lowers_simple_addition() {
    let src = r#"
fn main() -> U64 (
  40 + 2
)
"#;

    let (_analysis, _program, outcome) = lower_and_eval(src);
    assert_eq!(outcome.entry_result, ExecValue::Int(42));
}

#[test]
fn exec_slice_lowers_tuple_projection() {
    let src = r#"
fn main() -> U64 (
  (40 + 2, 7)._0
)
"#;

    let (_analysis, _program, outcome) = lower_and_eval(src);
    assert_eq!(outcome.entry_result, ExecValue::Int(42));
}

#[test]
fn exec_slice_structural_variant_is_not_tied_to_canonical_body_ids() {
    let src = r#"
fn helper() -> U64 ( 0 )

fn main() -> U64 ( 42 )
"#;

    let (_analysis, program, outcome) = lower_and_eval(src);
    assert_eq!(outcome.entry_result, ExecValue::Int(42));
    let mut bodies = program.bodies.keys().copied().collect::<Vec<_>>();
    bodies.sort_by_key(|id| id.0);
    assert!(
        bodies.len() == 2 && bodies[0] == BodyId(0) && bodies[1] == BodyId(1),
        "variant should include multiple bodies with non-singleton id shape"
    );
}

#[test]
fn exec_slice_runtime_boundary_remains_pending() {
    let src = r#"
fn main() -> U64 (
  stabilize(() => 1, (w) => w)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    analysis.validate().expect("analysis should validate");
    let program = lower_executable_program(&analysis, &bridge).expect("program should lower");

    let entry_body = program
        .bodies
        .get(&program.entry)
        .expect("entry body should exist");
    assert!(matches!(entry_body.result_source, ResultSource::Slot(_)));
    assert!(
        entry_body
            .ops
            .iter()
            .any(|op| matches!(op, KernelOp::AwaitRuntime { .. })),
        "runtime boundary bodies should preserve local op structure with explicit await ops"
    );
    assert!(
        !program.runtime_nodes.is_empty(),
        "runtime boundary nodes should be preserved in executable static schema"
    );

    let expected_boundaries = analysis
        .bodies
        .iter()
        .find(|body| body.id == program.entry)
        .expect("entry analysis body exists")
        .extracted_nodes
        .iter()
        .filter_map(|node| match node.reason {
            ExtractedNodeReason::RuntimeBoundary { .. } => Some(node.id),
            _ => None,
        })
        .collect::<Vec<_>>();

    let mut session = RuntimeSession::new(&program).expect("session should start");
    let blocked = match session
        .run_until_blocked()
        .expect("run should reach blocked state")
    {
        EvalRunResult::Blocked(blocked) => blocked,
        EvalRunResult::Complete(outcome) => {
            panic!(
                "expected blocked runtime state, got complete {:?}",
                outcome.entry_result
            )
        }
    };
    assert_eq!(blocked.blocked_runtime_nodes, expected_boundaries);
    assert!(
        blocked
            .wait_events
            .iter()
            .any(|event| matches!(event, WaitReason::RuntimeNode(_))),
        "runtime await should produce explicit runtime wait events"
    );
    assert!(
        blocked.blocked_runtime_nodes.iter().all(|id| matches!(
            blocked.runtime_cells.get(&RuntimeMemoKey { node: *id }),
            Some(CellState::Running { .. })
        )),
        "runtime cells should remain running/pending while blocked"
    );

    for id in &blocked.blocked_runtime_nodes {
        session
            .complete_runtime_node(*id, ExecValue::PendingRuntimeBoundaries(vec![*id]))
            .expect("manual runtime completion should work");
    }
    let completed = match session
        .run_until_blocked()
        .expect("run should complete after manual runtime completion")
    {
        EvalRunResult::Complete(outcome) => outcome,
        EvalRunResult::Blocked(blocked) => {
            panic!(
                "expected completion after runtime completion, still blocked: {:?}",
                blocked
            )
        }
    };
    assert_eq!(
        completed.entry_result,
        ExecValue::PendingRuntimeBoundaries(expected_boundaries.clone())
    );
}

#[test]
fn exec_slice_handles_shadowing_with_typed_binding_identity() {
    let src = r#"
fn main() -> U64 (
  let x = 1;
  let x = 2;
  x
)
"#;

    let (_analysis, _program, outcome) = lower_and_eval(src);
    assert_eq!(outcome.entry_result, ExecValue::Int(2));
}

#[test]
fn exec_slice_keeps_local_ops_when_runtime_boundary_exists_in_body() {
    let src = r#"
fn main() -> U64 (
  let pair = (1 + 1, stabilize(() => 1, (w) => w));
  pair._0
)
"#;

    let (_analysis, program, outcome) = lower_and_eval(src);
    assert_eq!(outcome.entry_result, ExecValue::Int(2));

    let body = program
        .bodies
        .get(&program.entry)
        .expect("entry body exists");
    assert!(
        body.ops.iter().any(|op| matches!(op, KernelOp::Add { .. })),
        "local pure work should remain in executable ops even with boundary extraction in same body"
    );
}

#[test]
fn exec_slice_repeated_body_call_uses_body_cell_memo_path() {
    let src = r#"
fn tick() -> U64 ( 2 )

fn main() -> U64 (
  tick() + tick()
)
"#;

    let (_analysis, _program, outcome) = lower_and_eval(src);
    assert_eq!(outcome.entry_result, ExecValue::Int(4));

    let tick_key = BodyMemoKey {
        body: BodyId(0),
        args: vec![],
    };
    assert_eq!(
        outcome.body_eval_counts.get(&tick_key),
        Some(&1usize),
        "repeated call with identical args should evaluate body once through memo path"
    );
    assert!(
        outcome
            .wait_events
            .iter()
            .any(|event| matches!(event, WaitReason::BodyCall(_))),
        "body calls should be represented through explicit body-call wait reasons"
    );
    assert!(
        matches!(outcome.body_cells.get(&tick_key), Some(CellState::Ready(_))),
        "body memo cell should reach ready state"
    );
}

#[test]
fn runtime_session_build_shaped_source_blocks_then_resumes_after_manual_boundary_completion() {
    let src = r#"
fn build(root: Path, args: Array[String], env: Map[String, String]) #fs #discover -> Tree (
  let class = executor_class("default");
  let cc = discover(class, (host) => host.which("cc"));
  exec [cc, "-c", @root/main.c, "-o", @out/main.o] where {
    observer: (proc, tree) => tree,
  }
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    let analysis = build_minimal_analysis(&bridge);
    analysis.validate().expect("analysis should validate");
    let program = lower_executable_program(&analysis, &bridge).expect("program should lower");
    let entry_param_count = program
        .bodies
        .get(&program.entry)
        .expect("entry body should exist")
        .params
        .len();

    let mut session =
        RuntimeSession::new_with_entry_args(&program, vec![ExecValue::Int(0); entry_param_count])
            .expect("session should start");

    let blocked = match session
        .run_until_blocked()
        .expect("run should block on runtime boundaries")
    {
        EvalRunResult::Blocked(blocked) => blocked,
        EvalRunResult::Complete(outcome) => {
            panic!(
                "expected blocked runtime state for build-shaped source, got complete {:?}",
                outcome.entry_result
            )
        }
    };
    assert!(
        !blocked.blocked_runtime_nodes.is_empty(),
        "build-shaped source should block on at least one runtime node"
    );
    assert!(
        blocked
            .wait_events
            .iter()
            .any(|event| matches!(event, WaitReason::RuntimeNode(_))),
        "runtime wait should be observable in blocked session state"
    );

    for node in &blocked.blocked_runtime_nodes {
        session
            .complete_runtime_node(*node, ExecValue::PendingRuntimeBoundaries(vec![*node]))
            .expect("manual runtime node completion should succeed");
    }

    let completed = match session
        .run_until_blocked()
        .expect("run should complete after manual runtime completion")
    {
        EvalRunResult::Complete(outcome) => outcome,
        EvalRunResult::Blocked(blocked) => {
            panic!(
                "expected completion after manual runtime completion, still blocked: {:?}",
                blocked.blocked_runtime_nodes
            )
        }
    };
    assert!(
        matches!(
            completed.entry_result,
            ExecValue::PendingRuntimeBoundaries(nodes) if !nodes.is_empty()
        ),
        "final result should remain an explicit pending-runtime token shape"
    );
}
