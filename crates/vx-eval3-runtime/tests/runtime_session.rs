use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
use vx_platform_exec::{
    PlatformExecError, PlatformExecRequest, SandboxExemptionRequest, ToolObservationRequest,
    resolve_tool_from_path, run_platform_exec,
};

use vx_eval3_analysis::{
    BodyId, ExtractedNodeReason, KernelOp, ResultSource, SingleSourceBridge,
    SourceLocalCalleeOrigin, build_minimal_analysis, lower_executable_program,
    lower_executable_program_for_entry_body,
};
use vx_eval3_runtime::{
    BlobValue, BodyMemoKey, BoundaryDriver, BoundaryPositionalInputValue, BoundaryRequest,
    CellState, EvalOutcome, EvalRunResult, ExecValue, HostValue, ObservedToolValue, RuntimeMemoKey,
    RuntimeSession, SandboxExemptionValue, TreeValue, WaitReason, eval_executable_closure,
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

struct ConstantProcessDriver {
    exit_code: i64,
    stdout: String,
}

impl BoundaryDriver for ConstantProcessDriver {
    fn resolve_boundary(
        &mut self,
        _request: &BoundaryRequest,
    ) -> Result<ExecValue, vx_eval3_analysis::ExecSliceError> {
        Ok(ExecValue::ProcessResult {
            exit_code: self.exit_code,
            stdout: self.stdout.clone(),
        })
    }
}

struct SingleHostCcVersionDriver {
    sandbox: tempfile::TempDir,
}

impl SingleHostCcVersionDriver {
    fn new() -> Self {
        Self {
            sandbox: tempfile::tempdir().expect("sandbox tempdir should be created"),
        }
    }

    fn parse_sandbox_exemptions(
        inputs: &[BoundaryPositionalInputValue],
    ) -> Result<Vec<SandboxExemptionRequest>, vx_eval3_analysis::ExecSliceError> {
        let mut exemptions = Vec::new();
        for input in inputs {
            match input {
                BoundaryPositionalInputValue::RuntimeValue { value, .. } => {
                    exemptions.extend(Self::parse_sandbox_exemptions_from_value(value)?);
                }
                BoundaryPositionalInputValue::Array(items) => {
                    exemptions.extend(Self::parse_sandbox_exemptions(items)?);
                }
                BoundaryPositionalInputValue::MissingBinding { .. }
                | BoundaryPositionalInputValue::IntLiteral(_)
                | BoundaryPositionalInputValue::StringLiteral(_)
                | BoundaryPositionalInputValue::Closure { .. }
                | BoundaryPositionalInputValue::OpaqueExpr(_) => {}
            }
        }
        Ok(exemptions)
    }

    fn parse_sandbox_exemptions_from_value(
        value: &ExecValue,
    ) -> Result<Vec<SandboxExemptionRequest>, vx_eval3_analysis::ExecSliceError> {
        match value {
            ExecValue::SandboxExemption(SandboxExemptionValue::ExecSubpath(path)) => {
                Ok(vec![SandboxExemptionRequest::ExecSubpath(path.clone())])
            }
            ExecValue::SandboxExemption(SandboxExemptionValue::ReadSubpath(path)) => {
                Ok(vec![SandboxExemptionRequest::ReadSubpath(path.clone())])
            }
            ExecValue::Array(items) => {
                let mut out = Vec::new();
                for item in items {
                    out.extend(Self::parse_sandbox_exemptions_from_value(item)?);
                }
                Ok(out)
            }
            other => Err(vx_eval3_analysis::ExecSliceError::TypeMismatch(format!(
                "coarse_mounts expects sandbox exemption values, got {:?}",
                other
            ))),
        }
    }

    fn argv_from_inputs(
        inputs: &[BoundaryPositionalInputValue],
    ) -> Result<Vec<String>, vx_eval3_analysis::ExecSliceError> {
        let argv_inputs: Vec<BoundaryPositionalInputValue> = if inputs.len() == 1 {
            match &inputs[0] {
                BoundaryPositionalInputValue::Array(items) => items.clone(),
                other => vec![other.clone()],
            }
        } else {
            inputs.to_vec()
        };
        argv_inputs
            .iter()
            .map(Self::input_to_string)
            .collect::<Result<Vec<_>, _>>()
    }

    fn parse_tool_observations(
        inputs: &[BoundaryPositionalInputValue],
    ) -> Result<Vec<ToolObservationRequest>, vx_eval3_analysis::ExecSliceError> {
        let mut observations = Vec::new();
        for input in inputs {
            match input {
                BoundaryPositionalInputValue::RuntimeValue { value, .. } => {
                    observations.extend(Self::parse_tool_observations_from_value(value)?);
                }
                BoundaryPositionalInputValue::Array(items) => {
                    observations.extend(Self::parse_tool_observations(items)?);
                }
                BoundaryPositionalInputValue::MissingBinding { .. }
                | BoundaryPositionalInputValue::IntLiteral(_)
                | BoundaryPositionalInputValue::StringLiteral(_)
                | BoundaryPositionalInputValue::Closure { .. }
                | BoundaryPositionalInputValue::OpaqueExpr(_) => {}
            }
        }
        Ok(observations)
    }

    fn parse_tool_observations_from_value(
        value: &ExecValue,
    ) -> Result<Vec<ToolObservationRequest>, vx_eval3_analysis::ExecSliceError> {
        match value {
            ExecValue::ObservedTool(observed) => Ok(vec![ToolObservationRequest {
                program: observed.program.clone(),
                resolved_path: observed.resolved_path.clone(),
            }]),
            ExecValue::Array(items) => {
                let mut out = Vec::new();
                for item in items {
                    out.extend(Self::parse_tool_observations_from_value(item)?);
                }
                Ok(out)
            }
            ExecValue::Pair(left, right) => {
                let mut out = Self::parse_tool_observations_from_value(left)?;
                out.extend(Self::parse_tool_observations_from_value(right)?);
                Ok(out)
            }
            ExecValue::Int(_)
            | ExecValue::Bool(_)
            | ExecValue::String(_)
            | ExecValue::Host(_)
            | ExecValue::SandboxExemption(_)
            | ExecValue::SandboxTreeRoot(_)
            | ExecValue::Blob(_)
            | ExecValue::Tree(_)
            | ExecValue::ProcessResult { .. }
            | ExecValue::Callable(_)
            | ExecValue::LazyPoison(_)
            | ExecValue::PendingRuntimeBoundaries(_) => Ok(vec![]),
        }
    }

    fn input_to_string(
        input: &BoundaryPositionalInputValue,
    ) -> Result<String, vx_eval3_analysis::ExecSliceError> {
        match input {
            BoundaryPositionalInputValue::RuntimeValue { value, .. } => match value {
                ExecValue::String(text) => Ok(text.clone()),
                ExecValue::ObservedTool(observed) => Ok(observed.resolved_path.clone()),
                other => Err(vx_eval3_analysis::ExecSliceError::TypeMismatch(format!(
                    "exec argv expects string-like input, got {:?}",
                    other
                ))),
            },
            BoundaryPositionalInputValue::StringLiteral(text) => Ok(text.clone()),
            other => Err(vx_eval3_analysis::ExecSliceError::UnsupportedShape(
                format!("exec argv input shape unsupported: {:?}", other),
            )),
        }
    }

    fn map_platform_exec_error(error: PlatformExecError) -> vx_eval3_analysis::ExecSliceError {
        match error {
            PlatformExecError::Verification(message) => {
                vx_eval3_analysis::ExecSliceError::VerificationFailure(message)
            }
            PlatformExecError::Io(error) => vx_eval3_analysis::ExecSliceError::TypeMismatch(
                format!("platform exec failed: {error}"),
            ),
        }
    }
}

impl BoundaryDriver for SingleHostCcVersionDriver {
    fn resolve_boundary(
        &mut self,
        request: &BoundaryRequest,
    ) -> Result<ExecValue, vx_eval3_analysis::ExecSliceError> {
        match request {
            BoundaryRequest::Exec(req) => {
                let argv = Self::argv_from_inputs(&req.inputs.positional_inputs)?;
                let tool_observations =
                    Self::parse_tool_observations(&req.inputs.positional_inputs)?;
                let sandbox_exemptions = Self::parse_sandbox_exemptions(&req.inputs.coarse_mounts)?;
                let Some(program) = argv.first() else {
                    return Err(vx_eval3_analysis::ExecSliceError::UnsupportedShape(
                        "exec argv cannot be empty".to_owned(),
                    ));
                };
                let args = argv[1..].to_vec();
                let env = Vec::<(String, String)>::new();
                let output = run_platform_exec(PlatformExecRequest {
                    program,
                    args: &args,
                    env: &env,
                    sandbox_root: self.sandbox.path(),
                    tool_observations: &tool_observations,
                    sandbox_exemptions: &sandbox_exemptions,
                })
                .map_err(Self::map_platform_exec_error)?;
                Ok(ExecValue::ProcessResult {
                    exit_code: output.exit_code,
                    stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                })
            }
            BoundaryRequest::Discover(req) => {
                let closure = req
                    .inputs
                    .positional_inputs
                    .iter()
                    .find_map(|input| match input {
                        BoundaryPositionalInputValue::Closure { closure, .. } => Some(closure),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        vx_eval3_analysis::ExecSliceError::UnsupportedShape(
                            "discover requires shipped closure input".to_owned(),
                        )
                    })?;
                let first = req.inputs.positional_inputs.first().ok_or_else(|| {
                    vx_eval3_analysis::ExecSliceError::UnsupportedShape(
                        "discover requires at least one positional input".to_owned(),
                    )
                })?;
                let _ = first;
                let discovered =
                    eval_executable_closure(closure, vec![ExecValue::Host(HostValue::Local)])?;
                Ok(discovered)
            }
            BoundaryRequest::SharedKeyed(_) => {
                Err(vx_eval3_analysis::ExecSliceError::UnsupportedShape(
                    "shared_keyed boundary is unsupported in this test driver".to_owned(),
                ))
            }
        }
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
  exec ["cc", "-c", @root/main.c, "-o", @out/main.o]
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

    let mut driver = ConstantProcessDriver {
        exit_code: 0,
        stdout: "observer-input-tree".to_owned(),
    };
    let completed = session
        .run_with_driver(&mut driver)
        .expect("driver-mediated run should complete");
    assert_eq!(
        completed.entry_result,
        ExecValue::ProcessResult {
            exit_code: 0,
            stdout: "observer-input-tree".to_owned(),
        },
        "runtime should expose raw process completion"
    );
}

#[test]
fn runtime_session_exec_value_slice_builds_tree_from_process_result() {
    let src = r#"
fn build(root: Path, args: Array[String], env: Map[String, String]) -> Tree (
  let proc = exec ["cc", "--version"];
  if proc.exit_code != 0 (
    fail `process failed`
  ) else (
    Tree %{ @cc-version.txt => proc.stdout.collect().blob() }
  )
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
    let mut driver = ConstantProcessDriver {
        exit_code: 0,
        stdout: "cc 1.2.3".to_owned(),
    };
    let outcome = session
        .run_with_driver(&mut driver)
        .expect("driver-mediated run should complete");
    let mut files = BTreeMap::new();
    files.insert(
        "cc-version.txt".to_owned(),
        BlobValue {
            bytes: b"cc 1.2.3".to_vec(),
        },
    );
    assert_eq!(outcome.entry_result, ExecValue::Tree(TreeValue { files }));
}

#[test]
fn runtime_session_exec_value_slice_surfaces_fail_from_selected_branch() {
    let src = r#"
fn build(root: Path, args: Array[String], env: Map[String, String]) -> Tree (
  let proc = exec ["cc", "--version"];
  if proc.exit_code != 0 (
    fail `process failed`
  ) else (
    Tree %{ @cc-version.txt => proc.stdout.collect().blob() }
  )
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
    let mut driver = ConstantProcessDriver {
        exit_code: 1,
        stdout: "cc 1.2.3".to_owned(),
    };
    let err = session
        .run_with_driver(&mut driver)
        .expect_err("selected fail branch should surface runtime error");
    assert!(
        matches!(err, vx_eval3_analysis::ExecSliceError::TypeMismatch(ref message) if message.contains("process failed")),
        "expected process-failed runtime error, got: {err:?}"
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

#[test]
fn zoo_cc_version_runs_honestly_single_host() {
    if !cfg!(target_os = "macos") {
        return;
    }
    if resolve_tool_from_path("cc").is_none() {
        return;
    }

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
        .expect("cc-version should lower for single-host runtime path");
    let entry_param_count = program
        .bodies
        .get(&program.entry)
        .expect("entry body should exist")
        .params
        .len();
    let mut session =
        RuntimeSession::new_with_entry_args(&program, vec![ExecValue::Int(0); entry_param_count])
            .expect("session should start");
    let mut driver = SingleHostCcVersionDriver::new();
    let outcome = session
        .run_with_driver(&mut driver)
        .expect("cc-version should run through real single-host exec path");
    let ExecValue::Tree(tree) = outcome.entry_result else {
        panic!("cc-version should produce tree output");
    };
    let version_blob = tree
        .files
        .get("cc-version.txt")
        .expect("cc-version tree should include cc-version.txt");
    assert!(
        !version_blob.bytes.is_empty(),
        "cc-version output blob should contain real command output"
    );
}

#[test]
fn zoo_cc_version_requires_explicit_coarse_mounts() {
    if !cfg!(target_os = "macos") {
        return;
    }
    if resolve_tool_from_path("cc").is_none() {
        return;
    }

    let src = r#"
fn build(ws: Path, args: Array[String], env: Map[String, String]) #discover -> Tree (
  let class = executor_class("default");
  let cc = discover(class, (host) => host.which("cc"));

  exec [cc, "--version"] where {
    observer: (proc, _vfs) => (
      if proc.exit_code != 0 (
        fail `process failed`
      ) else (
        Tree %{
          @cc-version.txt => proc.stdout.collect().blob(),
        }
      )
    ),
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
    let mut driver = SingleHostCcVersionDriver::new();
    let err = session
        .run_with_driver(&mut driver)
        .expect_err("absolute discovered tool path should require explicit coarse mounts");
    assert!(
        matches!(
            err,
            vx_eval3_analysis::ExecSliceError::VerificationFailure(ref message)
                if message.contains("requires explicit ExecSubpath coverage")
        ),
        "expected shared platform enforcement failure, got: {err:?}"
    );
}

#[test]
fn zoo_cc_version_rechecks_observed_tool_identity() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let Some(cc_path) = resolve_tool_from_path("cc") else {
        return;
    };
    let Some(clang_path) = resolve_tool_from_path("clang") else {
        return;
    };
    if cc_path == clang_path {
        return;
    }

    struct DishonestDiscoverDriver {
        inner: SingleHostCcVersionDriver,
        fake_path: String,
    }

    impl BoundaryDriver for DishonestDiscoverDriver {
        fn resolve_boundary(
            &mut self,
            request: &BoundaryRequest,
        ) -> Result<ExecValue, vx_eval3_analysis::ExecSliceError> {
            match request {
                BoundaryRequest::Discover(_) => Ok(ExecValue::Pair(
                    Box::new(ExecValue::ObservedTool(ObservedToolValue {
                        program: "cc".to_owned(),
                        resolved_path: self.fake_path.clone(),
                    })),
                    Box::new(ExecValue::Array(vec![
                        ExecValue::SandboxExemption(SandboxExemptionValue::ExecSubpath(
                            "/usr/bin".to_owned(),
                        )),
                        ExecValue::SandboxExemption(SandboxExemptionValue::ExecSubpath(
                            "/Library/Developer/CommandLineTools/usr/bin".to_owned(),
                        )),
                        ExecValue::SandboxExemption(SandboxExemptionValue::ReadSubpath(
                            "/Library/Developer/CommandLineTools".to_owned(),
                        )),
                        ExecValue::SandboxExemption(SandboxExemptionValue::ReadSubpath(
                            "/Library/Developer/CommandLineTools/usr/lib".to_owned(),
                        )),
                        ExecValue::SandboxExemption(SandboxExemptionValue::ReadSubpath(
                            "/Library/Developer/CommandLineTools/SDKs".to_owned(),
                        )),
                    ])),
                )),
                BoundaryRequest::Exec(_) | BoundaryRequest::SharedKeyed(_) => {
                    self.inner.resolve_boundary(request)
                }
            }
        }
    }

    let zoo_file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cc-version.vx");
    let bridge = SingleSourceBridge::from_entry_file(&zoo_file).expect("zoo bridge should load");
    let analysis = build_minimal_analysis(&bridge);
    analysis.validate().expect("analysis should validate");
    let program = lower_executable_program_for_entry_body(
        &analysis,
        &bridge,
        bridge
            .bodies
            .iter()
            .find(|body| body.name == "build" && body.span.source == bridge.source_id)
            .map(|body| body.id)
            .expect("cc-version should expose build in root source"),
    )
    .expect("cc-version should lower for single-host runtime path");
    let entry_param_count = program
        .bodies
        .get(&program.entry)
        .expect("entry body should exist")
        .params
        .len();
    let mut session =
        RuntimeSession::new_with_entry_args(&program, vec![ExecValue::Int(0); entry_param_count])
            .expect("session should start");
    let mut driver = DishonestDiscoverDriver {
        inner: SingleHostCcVersionDriver::new(),
        fake_path: clang_path,
    };
    let err = session
        .run_with_driver(&mut driver)
        .expect_err("shared exec verification should reject mismatched observed tool identity");
    assert!(
        matches!(
            err,
            vx_eval3_analysis::ExecSliceError::VerificationFailure(ref message)
                if message.contains("observed tool `cc` changed")
        ),
        "expected observed-tool verification failure, got: {err:?}"
    );
}
