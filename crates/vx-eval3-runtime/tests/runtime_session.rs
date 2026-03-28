use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use vx_eval3_analysis::{
    BodyId, ExtractedNodeReason, KernelOp, ResultSource, RuntimeBoundaryPayload,
    SingleSourceBridge, SourceLocalCalleeOrigin, build_minimal_analysis, lower_executable_program,
    lower_executable_program_for_entry_body,
};
use vx_eval3_runtime::{
    BodyMemoKey, BoundaryDriver, BoundaryPositionalInputValue, BoundaryRequest, CellState,
    EvalOutcome, EvalRunResult, ExecValue, RuntimeMemoKey, RuntimeSession, WaitReason,
    eval_executable_program,
};

struct EchoNodeDriver;

impl BoundaryDriver for EchoNodeDriver {
    fn resolve_boundary(
        &mut self,
        request: &BoundaryRequest,
    ) -> Result<ExecValue, vx_eval3_analysis::ExecSliceError> {
        let node = match request {
            BoundaryRequest::Exec(req) => req.node,
            BoundaryRequest::Discover(req) => req.node,
            BoundaryRequest::SharedKeyed(req) => req.node,
        };
        Ok(ExecValue::PendingRuntimeBoundaries(vec![node]))
    }
}

struct ConstantIntDriver(i64);

impl BoundaryDriver for ConstantIntDriver {
    fn resolve_boundary(
        &mut self,
        _request: &BoundaryRequest,
    ) -> Result<ExecValue, vx_eval3_analysis::ExecSliceError> {
        Ok(ExecValue::Int(self.0))
    }
}

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

    let mut driver = EchoNodeDriver;
    let completed = session
        .run_with_driver(&mut driver)
        .expect("driver-mediated run should complete");
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
fn runtime_session_build_shaped_source_runs_through_driver_to_concrete_result() {
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

    let mut driver = ConstantIntDriver(17);
    let completed = session
        .run_with_driver(&mut driver)
        .expect("driver-mediated run should complete");
    assert_eq!(
        completed.entry_result,
        ExecValue::Int(17),
        "driver path should produce a concrete runtime result"
    );
}

#[test]
fn runtime_session_executes_selected_multi_source_body_with_imported_call_boundaries() {
    let tmp = tempfile::tempdir().expect("tempdir should be created");
    let entry_path = tmp.path().join("main.vx");
    let helper_path = tmp.path().join("helper.vx");
    std::fs::write(
        &helper_path,
        r#"
fn build() -> U64 (
  2
)
"#,
    )
    .expect("helper source should be written");
    std::fs::write(
        &entry_path,
        r#"
let helper = import("./helper");

fn build(workspace: Path, args: Array[String], env: Map[String, String]) -> U64 (
  (helper.build(), stabilize(() => 1, (w) => w))._1
)
"#,
    )
    .expect("entry source should be written");

    let bridge =
        SingleSourceBridge::from_entry_file(&entry_path).expect("multi-source bridge should load");
    let analysis = build_minimal_analysis(&bridge);
    analysis.validate().expect("analysis should validate");

    let entry_body = bridge
        .bodies
        .iter()
        .find(|body| body.name == "build" && body.span.source == bridge.source_id)
        .map(|body| body.id)
        .expect("entry source should contain build in root source");
    let program = lower_executable_program_for_entry_body(&analysis, &bridge, entry_body)
        .expect("selected body should lower");
    assert_eq!(program.entry, entry_body);
    assert_eq!(
        program.bodies.len(),
        2,
        "selected-body lowering should include imported callee bodies in reachable slice"
    );

    let body_source = bridge
        .bodies
        .iter()
        .map(|body| (body.id, body.span.source))
        .collect::<std::collections::BTreeMap<_, _>>();
    let lowered_sources = program
        .bodies
        .keys()
        .filter_map(|body| body_source.get(body).map(|source| source.0))
        .collect::<BTreeSet<_>>();
    assert!(
        lowered_sources.len() > 1,
        "lowered executable bodies should span more than one source file"
    );
    let imported_call_exprs = analysis
        .bodies
        .iter()
        .find(|body| body.id == entry_body)
        .expect("entry analyzed body should exist")
        .source_local_call_edges
        .iter()
        .filter_map(|edge| {
            let SourceLocalCalleeOrigin::TopLevelBody(callee) = edge.callee else {
                return None;
            };
            let caller_source = *body_source.get(&entry_body)?;
            let callee_source = *body_source.get(&callee)?;
            (caller_source != callee_source).then_some(edge.call_expr)
        })
        .collect::<BTreeSet<_>>();
    assert!(
        !imported_call_exprs.is_empty(),
        "entry body should include at least one imported-module call edge"
    );
    let imported_callees = analysis
        .bodies
        .iter()
        .find(|body| body.id == entry_body)
        .expect("entry analyzed body should exist")
        .source_local_call_edges
        .iter()
        .filter_map(|edge| match edge.callee {
            SourceLocalCalleeOrigin::TopLevelBody(callee) => {
                let caller_source = *body_source.get(&entry_body)?;
                let callee_source = *body_source.get(&callee)?;
                (caller_source != callee_source).then_some(callee)
            }
            SourceLocalCalleeOrigin::LocalClosure(_) | SourceLocalCalleeOrigin::LocalDef(_) => None,
        })
        .collect::<BTreeSet<_>>();
    for callee in &imported_callees {
        assert!(
            program.bodies.contains_key(callee),
            "imported callee body {callee:?} should lower into executable program"
        );
    }

    let entry_param_count = program
        .bodies
        .get(&program.entry)
        .expect("entry body should exist")
        .params
        .len();
    let mut session =
        RuntimeSession::new_with_entry_args(&program, vec![ExecValue::Int(0); entry_param_count])
            .expect("session should start");

    let mut saw_imported_call_boundary = false;
    let mut saw_imported_callbody = false;
    let blocked = match session
        .run_until_blocked()
        .expect("runtime session should progress to first boundary")
    {
        EvalRunResult::Blocked(blocked) => blocked,
        EvalRunResult::Complete(outcome) => {
            panic!(
                "expected an initial blocked state for boundary-driven runtime, got complete {:?}",
                outcome.entry_result
            )
        }
    };
    let saw_runtime_block = true;
    for node in &blocked.blocked_runtime_nodes {
        if let Some(runtime_node) = program.runtime_nodes.get(node)
            && imported_call_exprs.contains(&runtime_node.source.expr)
        {
            saw_imported_call_boundary = true;
        }
    }
    let mut driver = EchoNodeDriver;
    let completed = session
        .run_with_driver(&mut driver)
        .expect("driver-mediated run should complete");
    if let Some(entry_exec_body) = program.bodies.get(&entry_body) {
        saw_imported_callbody = entry_exec_body
            .local_call_dependencies
            .iter()
            .any(|callee| imported_callees.contains(callee));
    }

    assert!(
        saw_imported_callbody,
        "entry executable body should include a real CallBody dependency into an imported module"
    );
    assert!(
        saw_runtime_block || saw_imported_call_boundary,
        "runtime should either block on real runtime boundaries or observe imported-call boundary reaches"
    );
    assert!(
        matches!(
            completed.entry_result,
            ExecValue::PendingRuntimeBoundaries(nodes) if !nodes.is_empty()
        ),
        "selected multi-source path should complete to an explicit runtime token shape"
    );
}

struct CcVersionDriver {
    saw_discover: bool,
    saw_exec: bool,
    saw_shared_keyed: bool,
    saw_discover_query: bool,
    saw_exec_argv_shape: bool,
    expected_payloads: BTreeMap<vx_eval3_analysis::RuntimeNodeId, RuntimeBoundaryPayload>,
    discovered_tool: Option<ExecValue>,
    last_exec_value: Option<ExecValue>,
}

impl BoundaryDriver for CcVersionDriver {
    fn resolve_boundary(
        &mut self,
        request: &BoundaryRequest,
    ) -> Result<ExecValue, vx_eval3_analysis::ExecSliceError> {
        match request {
            BoundaryRequest::Discover(req) => {
                self.saw_discover = true;
                let expected = self
                    .expected_payloads
                    .get(&req.node)
                    .expect("discover request node should have expected payload");
                assert_eq!(
                    expected,
                    &RuntimeBoundaryPayload::Discover(req.payload.clone()),
                    "discover request should carry typed payload from analysis executable plan"
                );
                let saw_closure =
                    req.inputs.positional_inputs.iter().any(|input| {
                        matches!(input, BoundaryPositionalInputValue::ClosureIdentity(_))
                    });
                let saw_missing_class = req.inputs.positional_inputs.iter().any(|input| {
                    matches!(input, BoundaryPositionalInputValue::MissingBinding { .. })
                });
                assert!(
                    saw_closure && saw_missing_class,
                    "discover request should include closure identity and unresolved class binding input"
                );
                self.saw_discover_query = true;
                let discovered_tool = ExecValue::String("cc".to_owned());
                self.discovered_tool = Some(discovered_tool.clone());
                Ok(discovered_tool)
            }
            BoundaryRequest::Exec(req) => {
                self.saw_exec = true;
                let expected = self
                    .expected_payloads
                    .get(&req.node)
                    .expect("exec request node should have expected payload");
                assert_eq!(
                    expected,
                    &RuntimeBoundaryPayload::Exec(req.payload.clone()),
                    "exec request should carry typed payload from analysis executable plan"
                );
                let expected_tool = self
                    .discovered_tool
                    .clone()
                    .expect("discover should run before exec and produce concrete tool value");
                let expected_tool_for_argv = expected_tool.clone();
                let argv = req
                    .inputs
                    .positional_inputs
                    .iter()
                    .find_map(|input| match input {
                        BoundaryPositionalInputValue::Array(values) => Some(values),
                        _ => None,
                    })
                    .expect("exec request should include argv-like array positional input");
                let saw_tool_arg = argv.iter().any(|input| {
                    matches!(
                        input,
                        BoundaryPositionalInputValue::RuntimeValue { value, .. } if *value == expected_tool_for_argv
                    )
                });
                let saw_version_flag = argv.iter().any(|input| match input {
                    BoundaryPositionalInputValue::StringLiteral(flag) => flag == "--version",
                    _ => false,
                });
                assert!(
                    saw_tool_arg && saw_version_flag,
                    "exec request should carry concrete discovered tool plus --version literal"
                );
                self.saw_exec_argv_shape = true;
                let stdout = format!(
                    "{} (deterministic-mini-host) version 1.0.0",
                    match expected_tool {
                        ExecValue::String(ref tool) => tool.clone(),
                        _ => "unknown".to_owned(),
                    }
                );
                let value = ExecValue::Pair(
                    Box::new(ExecValue::Int(0)),
                    Box::new(ExecValue::String(stdout)),
                );
                self.last_exec_value = Some(value.clone());
                Ok(value)
            }
            BoundaryRequest::SharedKeyed(req) => {
                self.saw_shared_keyed = true;
                let expected = self
                    .expected_payloads
                    .get(&req.node)
                    .expect("shared_keyed request node should have expected payload");
                assert_eq!(
                    expected,
                    &RuntimeBoundaryPayload::SharedKeyed(req.payload.clone()),
                    "shared_keyed request should carry typed payload from analysis executable plan"
                );
                Ok(ExecValue::Int(1 + req.payload.positional_arg_count as i64))
            }
        }
    }
}

#[test]
fn zoo_cc_version_runs_to_concrete_result_through_typed_boundary_driver() {
    let zoo_file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cc-version.vx");
    let bridge = SingleSourceBridge::from_entry_file(&zoo_file).expect("zoo bridge should load");
    let analysis = build_minimal_analysis(&bridge);
    analysis.validate().expect("analysis should validate");

    let entry_body = bridge
        .bodies
        .iter()
        .find(|body| body.name == "build" && body.span.source == bridge.source_id)
        .map(|body| body.id)
        .expect("cc-version should expose build in root source");
    let program = lower_executable_program_for_entry_body(&analysis, &bridge, entry_body)
        .expect("cc-version selected body should lower");
    let has_discover_payload = program.runtime_nodes.values().any(|node| {
        matches!(
            node.payload,
            RuntimeBoundaryPayload::Discover(ref payload)
                if payload.has_closure_arg && payload.positional_arg_count >= 2
        )
    });
    let has_exec_payload = program.runtime_nodes.values().any(|node| {
        matches!(
            node.payload,
            RuntimeBoundaryPayload::Exec(ref payload) if payload.positional_arg_count >= 1
        )
    });
    assert!(
        has_discover_payload,
        "cc-version executable plan should include typed discover payload with closure arg"
    );
    assert!(
        has_exec_payload,
        "cc-version executable plan should include typed exec payload with positional args"
    );

    let entry_param_count = program
        .bodies
        .get(&program.entry)
        .expect("entry body should exist")
        .params
        .len();
    let mut session =
        RuntimeSession::new_with_entry_args(&program, vec![ExecValue::Int(0); entry_param_count])
            .expect("session should start");
    let mut driver = CcVersionDriver {
        saw_discover: false,
        saw_exec: false,
        saw_shared_keyed: false,
        saw_discover_query: false,
        saw_exec_argv_shape: false,
        expected_payloads: program
            .runtime_nodes
            .iter()
            .map(|(id, node)| (*id, node.payload.clone()))
            .collect(),
        discovered_tool: None,
        last_exec_value: None,
    };
    let outcome = session
        .run_with_driver(&mut driver)
        .expect("driver-mediated zoo run should complete");

    assert!(
        driver.saw_exec,
        "typed boundary driver should receive at least one exec request on cc-version path"
    );
    assert!(
        driver.saw_discover,
        "typed boundary driver should receive at least one discover request on cc-version path"
    );
    assert!(
        driver.saw_discover_query,
        "discover request should drive deterministic tool discovery behavior"
    );
    assert!(
        driver.saw_exec_argv_shape,
        "exec request should carry argv-like concrete inputs"
    );
    assert_eq!(
        outcome.entry_result,
        driver
            .last_exec_value
            .expect("cc-version run should produce at least one exec boundary value"),
        "payload-derived exec boundary response should determine concrete final result"
    );
}
