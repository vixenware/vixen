use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use crate::{
    Claim, CrossingPolicy, EvalCompletedWorkOutcome, EvalDependencyEdge, EvalError, EvalErrorKind,
    EvalEvent, EvalEventKind, EvalExplainTerminal, EvalNodeId, EvalNodeKind, EvalNodeState,
    EvalSession, EvalSessionStep, EvalTrace, EvalWorkKind, EvalWorkState, PendingRef,
    ProjectionStep, Provenance, Resolution, ReuseClass, RuntimeCallOutcome, RuntimeHooks,
    RuntimeInputKind, SourceSpan, Value, ValueRep, channel_trace, envelope_to_value,
    eval_source_entry_at_place_with_args_with_source_id_traced,
    eval_source_entry_with_args_with_source_id, eval_source_entry_with_args_with_source_id_traced,
    eval_source_entry_with_args_with_source_id_with_sink, value_to_envelope,
};

#[test]
fn scalar_values_can_carry_claims_without_wrapper_enums() {
    let value = Value::u64(42)
        .with_provenance(Provenance::host_probe(
            SourceSpan::synthetic("discover-toolchain"),
            "windows-1",
            "discover toolchain",
        ))
        .with_claim(Claim::Arch {
            value: "x86_64".to_string(),
        });

    assert_eq!(value.as_u64(), Some(42));
    assert_eq!(value.meta.claims.len(), 1);
    assert!(matches!(value.rep, ValueRep::U64(42)));
}

#[test]
fn projection_updates_provenance_without_changing_payload_kind() {
    let value = Value::string("clang")
        .with_provenance(Provenance::runtime_input(
            SourceSpan::synthetic("toolchain"),
            RuntimeInputKind::DiscoverResult,
            "toolchain",
        ))
        .project(ProjectionStep::Field("cc".to_string()));

    assert!(matches!(value.rep, ValueRep::String(_)));
    assert_eq!(
        value.meta.provenance.path.as_slice(),
        [ProjectionStep::Field("cc".to_string())]
    );
}

#[test]
fn readiness_is_not_encoded_inside_value() {
    let ready = Resolution::Ready(Value::bool(true));
    let pending = Resolution::Pending(PendingRef::new(7, "waiting on discover"));

    assert!(matches!(ready, Resolution::Ready(_)));
    assert!(matches!(pending, Resolution::Pending(_)));
}

#[test]
fn generic_external_values_cover_boundary_cases_without_build_nouns() {
    let stable_scalar = Value::u64(42);
    let blocked_external = Value::external("tree", "/workspace");
    let proxied_external = Value::external("tree", "remote:17")
        .with_crossing(CrossingPolicy::ByProxy)
        .with_reuse(ReuseClass::Observed);

    assert_eq!(stable_scalar.meta.reuse, ReuseClass::Stable);
    assert_eq!(stable_scalar.meta.crossing, CrossingPolicy::ByValue);
    assert_eq!(blocked_external.meta.reuse, ReuseClass::Live);
    assert_eq!(blocked_external.meta.crossing, CrossingPolicy::Blocked);
    assert_eq!(proxied_external.meta.crossing, CrossingPolicy::ByProxy);
    assert_eq!(proxied_external.meta.reuse, ReuseClass::Observed);
}

#[test]
fn wire_round_trip_preserves_value_shape_and_metadata() {
    let value = Value::structure(
        "Toolchain",
        [(
            "cc".into(),
            Value::string("cl.exe")
                .with_provenance(Provenance::host_probe(
                    SourceSpan::synthetic("toolchain-probe"),
                    "win",
                    "toolchain probe",
                ))
                .with_claim(Claim::Which {
                    name: "cl.exe".to_string(),
                    expected_path: Some("C:/VS/cl.exe".to_string()),
                })
                .with_reuse(ReuseClass::Observed),
        )],
    )
    .with_provenance(Provenance::binding(
        SourceSpan::synthetic("toolchain-binding"),
        "toolchain",
    ))
    .with_crossing(CrossingPolicy::ByValue);

    let encoded = value_to_envelope(&value).expect("encode value");
    let decoded = envelope_to_value(encoded).expect("decode value");

    assert_eq!(decoded, value);
}

#[test]
fn entry_arguments_are_bound_as_runtime_inputs() {
    let resolution = eval_source_entry_with_args_with_source_id(
        "fn build(value: String) { value }",
        "entry-args",
        "build",
        &[Value::string("forty-two")],
    )
    .expect("evaluate entry with args");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("forty-two".into()));
    assert_eq!(value.meta.provenance.source.source_id, "entry-args");
    assert_eq!(
        value.meta.provenance.root,
        Provenance::runtime_input(
            value.meta.provenance.source.clone(),
            RuntimeInputKind::Argument,
            "value",
        )
        .root
    );
}

#[test]
fn entry_argument_count_mismatch_is_a_type_error() {
    let error = eval_source_entry_with_args_with_source_id(
        "fn build(value: String) { value }",
        "arity",
        "build",
        &[],
    )
    .expect_err("missing arg should fail");

    assert_eq!(error.kind, crate::EvalErrorKind::TypeError);
    assert!(
        error
            .message
            .contains("expects 1 positional argument(s), got 0")
    );
}

#[test]
fn traced_entry_arguments_emit_binding_and_ready_events() {
    let (resolution, trace) = eval_source_entry_with_args_with_source_id_traced(
        "fn build(value: String) { value }",
        "trace-bindings",
        "build",
        &[Value::string("forty-two")],
    )
    .expect("evaluate traced entry");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("forty-two".into()));

    assert_eq!(
        trace.events[0].kind,
        EvalEventKind::BindArgument {
            label: "value".into(),
        },
    );
    assert_eq!(trace.events[0].node_id.0, 1);
    assert_eq!(trace.events[0].parent_id, None);
    assert_eq!(trace.events[0].work_id, None);
    assert_eq!(trace.events[0].place.as_ref(), "local");
    assert_eq!(
        trace.events[0]
            .source
            .as_ref()
            .map(|source| source.source_id.as_str()),
        Some("trace-bindings"),
    );
    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::EnterExpr {
                kind: crate::EvalExprKind::NameRef,
            }
    }));
    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::ExitReady {
                reuse: ReuseClass::Stable,
                crossing: CrossingPolicy::ByValue,
            }
    }));
    assert!(trace.events.iter().all(|event| event.work_id.is_none()));
}

struct TestRuntime;

impl RuntimeHooks for TestRuntime {
    fn call(
        &self,
        place: &str,
        callee: &str,
        args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match (place, callee, args) {
            ("win", "host_os", []) => Ok(RuntimeCallOutcome::Ready(
                Value::string("windows")
                    .with_provenance(Provenance::host_probe(source.clone(), place, "host_os"))
                    .with_reuse(ReuseClass::Observed)
                    .with_crossing(CrossingPolicy::ByValue),
            )),
            ("win", "host_wait", []) => Ok(RuntimeCallOutcome::Pending {
                description: "waiting for remote host observation".into(),
            }),
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
    }
}

struct CountingRuntime {
    host_os_calls: Arc<AtomicUsize>,
    host_wait_calls: Arc<AtomicUsize>,
}

impl RuntimeHooks for CountingRuntime {
    fn call(
        &self,
        place: &str,
        callee: &str,
        _args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match (place, callee) {
            ("win", "host_os") => {
                self.host_os_calls.fetch_add(1, Ordering::SeqCst);
                Ok(RuntimeCallOutcome::Ready(
                    Value::string("windows")
                        .with_provenance(Provenance::host_probe(source.clone(), place, "host_os"))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue),
                ))
            }
            ("win", "host_wait") => {
                self.host_wait_calls.fetch_add(1, Ordering::SeqCst);
                Ok(RuntimeCallOutcome::Pending {
                    description: "waiting for remote host observation".into(),
                })
            }
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
    }
}

struct DoublePendingRuntime {
    host_os_calls: Arc<AtomicUsize>,
    wait_one_calls: Arc<AtomicUsize>,
    wait_two_calls: Arc<AtomicUsize>,
}

impl RuntimeHooks for DoublePendingRuntime {
    fn call(
        &self,
        place: &str,
        callee: &str,
        _args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match (place, callee) {
            ("win", "host_os") => {
                self.host_os_calls.fetch_add(1, Ordering::SeqCst);
                Ok(RuntimeCallOutcome::Ready(
                    Value::string("windows")
                        .with_provenance(Provenance::host_probe(source.clone(), place, "host_os"))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue),
                ))
            }
            ("win", "wait_one") => {
                self.wait_one_calls.fetch_add(1, Ordering::SeqCst);
                Ok(RuntimeCallOutcome::Pending {
                    description: "waiting for first remote result".into(),
                })
            }
            ("win", "wait_two") => {
                self.wait_two_calls.fetch_add(1, Ordering::SeqCst);
                Ok(RuntimeCallOutcome::Pending {
                    description: "waiting for second remote result".into(),
                })
            }
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
    }
}

#[test]
fn traced_runtime_calls_emit_start_and_finish_events() {
    let (resolution, trace) = eval_source_entry_at_place_with_args_with_source_id_traced(
        "fn build() { host_os() }",
        "trace-runtime",
        "build",
        &[],
        &TestRuntime,
        "win",
    )
    .expect("evaluate traced runtime call");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("windows".into()));

    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::EnterExpr {
                kind: crate::EvalExprKind::Call,
            }
    }));
    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::RuntimeCallStarted {
                callee: "host_os".into(),
                argc: 0,
            }
    }));
    assert!(
        trace
            .events
            .iter()
            .any(|event| matches!(event.kind, EvalEventKind::DependOn { .. }))
    );
    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::RuntimeCallFinishedReady {
                reuse: ReuseClass::Observed,
                crossing: CrossingPolicy::ByValue,
            }
    }));
    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::ExitReady {
                reuse: ReuseClass::Observed,
                crossing: CrossingPolicy::ByValue,
            }
    }));
    assert!(
        trace
            .events
            .iter()
            .all(|event| event.place.as_ref() == "win")
    );
}

#[test]
fn traced_fail_expression_emits_poison_exit() {
    let (resolution, trace) = eval_source_entry_with_args_with_source_id_traced(
        "fn build() { fail \"boom\" }",
        "trace-poison",
        "build",
        &[],
    )
    .expect("evaluate traced failure");

    let Resolution::Poison(error) = resolution else {
        panic!("expected poison resolution, got {resolution:?}");
    };
    assert_eq!(error.kind, EvalErrorKind::UserFail);
    assert_eq!(error.message.as_ref(), "boom");

    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::EnterExpr {
                kind: crate::EvalExprKind::Fail,
            }
    }));
    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::ExitPoison {
                kind: EvalErrorKind::UserFail,
                message: "boom".into(),
            }
    }));
}

#[test]
fn traced_runtime_calls_can_produce_pending_nodes() {
    let (resolution, trace) = eval_source_entry_at_place_with_args_with_source_id_traced(
        "fn build() { host_wait() }",
        "trace-pending",
        "build",
        &[],
        &TestRuntime,
        "win",
    )
    .expect("evaluate traced pending runtime call");

    let Resolution::Pending(pending) = resolution else {
        panic!("expected pending resolution, got {resolution:?}");
    };
    assert_eq!(
        pending.description.as_ref(),
        "waiting for remote host observation"
    );

    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::RegisterPending {
                pending: pending.clone(),
            }
    }));
    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::RuntimeCallFinishedPending {
                pending: pending.clone(),
            }
    }));
    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::ExitPending {
                pending: pending.clone(),
            }
    }));
}

#[test]
fn session_can_resume_pending_runtime_call() {
    let mut session = EvalSession::new(
        "fn build() { host_wait() }",
        "session-pending",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );
    let queued_before_run = session.queued_work();
    assert_eq!(queued_before_run.len(), 1);
    assert_eq!(queued_before_run[0].kind, EvalWorkKind::StartEntry);
    assert_eq!(queued_before_run[0].node_id, EvalNodeId(0));
    assert_eq!(
        session.node_state(EvalNodeId(0)),
        Some(&EvalNodeState::Runnable {
            work: queued_before_run[0].id,
            pending: None,
        }),
    );

    let first = session.run().expect("first session run");
    let Resolution::Pending(pending) = first else {
        panic!("expected pending resolution, got {first:?}");
    };
    assert_eq!(session.pending_nodes(), vec![pending.clone()]);
    assert_eq!(
        session.node_state(EvalNodeId(0)),
        Some(&EvalNodeState::Pending(pending.clone()))
    );
    assert_eq!(
        session.node_state(EvalNodeId(1)),
        Some(&EvalNodeState::BlockedOnNode {
            dependency: EvalNodeId(2),
        })
    );
    assert_eq!(
        session.node_state(EvalNodeId(2)),
        Some(&EvalNodeState::Pending(pending.clone()))
    );
    assert_eq!(
        session.node_state(EvalNodeId(3)),
        Some(&EvalNodeState::Pending(pending.clone()))
    );

    session
        .resolve_pending(&pending, Value::string("windows"))
        .expect("resolve pending node");

    let second = session.run().expect("second session run");
    let Resolution::Ready(value) = second else {
        panic!("expected ready resolution, got {second:?}");
    };
    assert_eq!(value.rep, ValueRep::String("windows".into()));
    assert!(session.pending_nodes().is_empty());
    assert_eq!(
        session.node_state(EvalNodeId(0)),
        Some(&EvalNodeState::Ready(Value::string("windows")))
    );
    assert_eq!(
        session.node_state(EvalNodeId(1)),
        Some(&EvalNodeState::Ready(Value::string("windows")))
    );
    assert_eq!(
        session.node_state(EvalNodeId(pending.node_id)),
        Some(&EvalNodeState::Ready(Value::string("windows")))
    );
    assert!(
        session
            .trace()
            .events
            .iter()
            .any(|event| matches!(event.kind, EvalEventKind::RegisterPending { .. })),
        "session trace should retain the first suspension",
    );
}

#[test]
fn session_step_queues_root_expression_work_before_evaluating_it() {
    let mut session = EvalSession::new(
        "fn build() { host_wait() }",
        "session-step-root",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    let queued_before = session.queued_work();
    assert_eq!(queued_before.len(), 1);
    assert_eq!(queued_before[0].kind, EvalWorkKind::StartEntry);
    assert_eq!(queued_before[0].node_id, EvalNodeId(0));

    let first = session.step().expect("first session step");
    assert_eq!(first, EvalSessionStep::Progressed);
    assert_eq!(session.completed_work().len(), 1);
    assert_eq!(
        session.completed_work()[0].outcome,
        EvalCompletedWorkOutcome::Progressed
    );
    assert_eq!(
        session.completed_work()[0].work.kind,
        EvalWorkKind::StartEntry
    );

    let queued_after = session.queued_work();
    assert_eq!(queued_after.len(), 1);
    assert_eq!(queued_after[0].kind, EvalWorkKind::EvaluateExpr);
    assert_eq!(queued_after[0].node_id, EvalNodeId(1));
    assert_eq!(
        session.node_state(EvalNodeId(0)),
        Some(&EvalNodeState::Runnable {
            work: queued_after[0].id,
            pending: None,
        }),
    );
    assert_eq!(
        session.node_state(EvalNodeId(1)),
        Some(&EvalNodeState::Runnable {
            work: queued_after[0].id,
            pending: None,
        }),
    );

    let second = session.step().expect("second session step");
    assert_eq!(second, EvalSessionStep::Progressed);
    let child_work = session.queued_work();
    assert_eq!(child_work.len(), 1);
    assert_eq!(child_work[0].kind, EvalWorkKind::EvaluateExpr);
    assert_eq!(child_work[0].node_id, EvalNodeId(2));
    assert_eq!(session.completed_work().len(), 2);
    assert_eq!(
        session.completed_work()[1].work.kind,
        EvalWorkKind::EvaluateExpr
    );
    assert_eq!(
        session.completed_work()[1].outcome,
        EvalCompletedWorkOutcome::BlockedOnNode(EvalNodeId(2)),
    );
    assert!(
        session
            .trace()
            .events
            .iter()
            .any(|event| event.work_id == Some(queued_after[0].id))
    );
    let third = session.step().expect("third session step");
    let EvalSessionStep::Pending(pending) = third else {
        panic!("expected pending step, got {third:?}");
    };
    assert_eq!(
        pending.description.as_ref(),
        "waiting for remote host observation"
    );
    assert!(session.queued_work().is_empty());
    assert_eq!(session.completed_work().len(), 3);
    assert_eq!(
        session.completed_work()[2].work.kind,
        EvalWorkKind::EvaluateExpr
    );
    assert_eq!(
        session.completed_work()[2].outcome,
        EvalCompletedWorkOutcome::Pending(pending.clone()),
    );
    assert!(
        session
            .trace()
            .events
            .iter()
            .any(|event| event.work_id == Some(child_work[0].id))
    );
}

#[test]
fn session_queues_child_expression_work_and_resumes_parent_block() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls: host_os_calls.clone(),
        host_wait_calls: host_wait_calls.clone(),
    };
    let mut session = EvalSession::new(
        "fn build() { let os = host_os(); os }",
        "session-child-queue",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );

    let root_work = session.queued_work();
    assert_eq!(root_work.len(), 1);
    assert_eq!(root_work[0].kind, EvalWorkKind::EvaluateExpr);
    let root_node = root_work[0].node_id;

    assert_eq!(
        session.step().expect("root expression step"),
        EvalSessionStep::Progressed
    );
    let child_work = session.queued_work();
    assert_eq!(child_work.len(), 1);
    assert_eq!(child_work[0].kind, EvalWorkKind::EvaluateExpr);
    let child_node = child_work[0].node_id;
    assert_ne!(child_node, root_node);
    assert_eq!(
        session.node_state(root_node),
        Some(&EvalNodeState::BlockedOnNode {
            dependency: child_node,
        }),
    );
    assert_eq!(
        session.completed_work()[1].outcome,
        EvalCompletedWorkOutcome::BlockedOnNode(child_node),
    );

    assert_eq!(
        session.step().expect("child expression step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(host_os_calls.load(Ordering::SeqCst), 1);
    assert_eq!(host_wait_calls.load(Ordering::SeqCst), 0);
    let resumed = session.queued_work();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].kind, EvalWorkKind::ResumePending);
    assert_eq!(resumed[0].node_id, root_node);
    let Some(EvalNodeState::Ready(value)) = session.node_state(child_node) else {
        panic!("expected child node to be ready");
    };
    assert_eq!(value.rep, ValueRep::String("windows".into()));

    let mut progressed_after_child = 0;
    let value = loop {
        match session.step().expect("finish queued parent work") {
            EvalSessionStep::Progressed => {
                progressed_after_child += 1;
            }
            EvalSessionStep::Ready(value) => break value,
            other => panic!("expected progress or ready, got {other:?}"),
        }
    };
    assert!(progressed_after_child >= 1);
    assert_eq!(value.rep, ValueRep::String("windows".into()));
    assert_eq!(host_os_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn session_exposes_dependency_edges_and_blocked_node_explanations() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls,
        host_wait_calls,
    };
    let mut session = EvalSession::new(
        "fn build() { let os = host_os(); os }",
        "session-explain",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    let root_work = session.queued_work();
    let root_node = root_work[0].node_id;

    assert_eq!(
        session.step().expect("root expression step"),
        EvalSessionStep::Progressed
    );
    let child_work = session.queued_work();
    let child_node = child_work[0].node_id;

    assert!(session.dependency_edges().contains(&EvalDependencyEdge {
        parent: root_node,
        child: child_node,
    }));

    let explanation = session.explain_node(root_node);
    assert_eq!(explanation.chain, vec![root_node, child_node]);
    assert_eq!(
        explanation.terminal,
        EvalExplainTerminal::Runnable {
            node_id: child_node,
            work: child_work[0].id,
            pending: None,
        },
    );
}

#[test]
fn session_retains_node_records_with_kind_parent_and_source() {
    let mut session = EvalSession::new(
        "fn build(value: String) { value }",
        "session-node-records",
        "build",
        vec![Value::string("windows")],
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    let records = session.node_records();
    assert!(records.iter().any(|record| {
        record.node_id == EvalNodeId(0)
            && record.parent_id.is_none()
            && record.kind == EvalNodeKind::SyntheticRoot
    }));
    assert!(records.iter().any(|record| {
        record.node_id == EvalNodeId(1)
            && record.parent_id.is_none()
            && record.kind
                == EvalNodeKind::ArgumentBinding {
                    label: "value".into(),
                }
            && record
                .source
                .as_ref()
                .map(|source| source.source_id.as_str())
                == Some("session-node-records")
    }));
    assert!(records.iter().any(|record| {
        record.node_id == EvalNodeId(2)
            && record.parent_id == Some(EvalNodeId(0))
            && record.kind
                == EvalNodeKind::Expr {
                    kind: crate::EvalExprKind::Block,
                }
            && record
                .source
                .as_ref()
                .map(|source| source.source_id.as_str())
                == Some("session-node-records")
    }));
}

#[test]
fn session_graph_snapshot_tracks_node_and_work_lifecycle() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls,
        host_wait_calls,
    };
    let mut session = EvalSession::new(
        "fn build() { let os = host_os(); os }",
        "session-graph-snapshot",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root expression step"),
        EvalSessionStep::Progressed
    );

    let snapshot = session.graph_snapshot();
    assert!(snapshot.pending.is_empty());
    assert!(snapshot.completed.is_none());

    let root_node = snapshot
        .nodes
        .iter()
        .find(|node| {
            node.record.as_ref().is_some_and(|record| {
                record.kind
                    == EvalNodeKind::Expr {
                        kind: crate::EvalExprKind::Block,
                    }
                    && record.parent_id == Some(EvalNodeId(0))
            })
        })
        .expect("root block node should be recorded");
    let child_node = snapshot
        .nodes
        .iter()
        .find(|node| {
            node.record.as_ref().is_some_and(|record| {
                record.kind
                    == EvalNodeKind::Expr {
                        kind: crate::EvalExprKind::Call,
                    }
                    && record.parent_id == Some(root_node.node_id)
            })
        })
        .expect("child call node should be recorded");

    assert_eq!(
        root_node.state,
        Some(EvalNodeState::BlockedOnNode {
            dependency: child_node.node_id,
        }),
    );
    assert_eq!(root_node.dependencies, vec![child_node.node_id]);
    assert_eq!(child_node.dependents, vec![root_node.node_id]);
    assert_eq!(
        child_node.state,
        Some(EvalNodeState::Runnable {
            work: snapshot.work[2].work.id,
            pending: None,
        }),
    );

    assert_eq!(snapshot.work.len(), 3);
    assert_eq!(snapshot.work[0].work.kind, EvalWorkKind::StartEntry);
    assert_eq!(
        snapshot.work[0].state,
        EvalWorkState::Completed(EvalCompletedWorkOutcome::Progressed)
    );
    assert_eq!(snapshot.work[1].work.kind, EvalWorkKind::EvaluateExpr);
    assert_eq!(
        snapshot.work[1].state,
        EvalWorkState::Completed(EvalCompletedWorkOutcome::BlockedOnNode(child_node.node_id)),
    );
    assert_eq!(snapshot.work[2].work.kind, EvalWorkKind::EvaluateExpr);
    assert_eq!(snapshot.work[2].state, EvalWorkState::Queued);
    assert_eq!(snapshot.work[2].work.node_id, child_node.node_id);
}

#[test]
fn session_can_step_back_to_a_prior_scheduler_state() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls,
        host_wait_calls,
    };
    let mut session = EvalSession::new(
        "fn build() { let os = host_os(); os }",
        "session-step-back",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    let root_work = session.queued_work();
    let root_node = root_work[0].node_id;

    assert_eq!(
        session.step().expect("root expression step"),
        EvalSessionStep::Progressed
    );
    let child_work = session.queued_work();
    let child_node = child_work[0].node_id;
    assert_eq!(
        session.node_state(root_node),
        Some(&EvalNodeState::BlockedOnNode {
            dependency: child_node,
        }),
    );
    assert!(session.can_step_back());

    assert!(session.step_back());
    let queued_after_rewind = session.queued_work();
    assert_eq!(queued_after_rewind.len(), 1);
    assert_eq!(queued_after_rewind[0].kind, EvalWorkKind::EvaluateExpr);
    assert_eq!(queued_after_rewind[0].node_id, root_node);
    assert_eq!(
        session.node_state(root_node),
        Some(&EvalNodeState::Runnable {
            work: queued_after_rewind[0].id,
            pending: None,
        }),
    );
    assert_eq!(
        session.dependency_edges(),
        &[EvalDependencyEdge {
            parent: EvalNodeId(0),
            child: root_node,
        }]
    );
    assert_eq!(session.completed_work().len(), 1);

    assert!(session.step_back());
    let queued_at_start = session.queued_work();
    assert_eq!(queued_at_start.len(), 1);
    assert_eq!(queued_at_start[0].kind, EvalWorkKind::StartEntry);
    assert_eq!(queued_at_start[0].node_id, EvalNodeId(0));
    assert!(session.completed_work().is_empty());
    assert!(!session.step_back());
}

#[test]
fn session_checkpoint_can_restore_graph_and_work_state() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls,
        host_wait_calls,
    };
    let mut session = EvalSession::new(
        "fn build() { let os = host_os(); os }",
        "session-checkpoint",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    let checkpoint = session.checkpoint();
    let expected_graph = session.graph_snapshot();
    let expected_trace = session.trace().clone();

    assert_eq!(
        session.step().expect("root expression step"),
        EvalSessionStep::Progressed
    );
    assert_ne!(session.graph_snapshot(), expected_graph);

    session.restore(checkpoint);
    assert_eq!(session.graph_snapshot(), expected_graph);
    assert_eq!(session.trace(), &expected_trace);
}

#[test]
fn session_resume_does_not_replay_ready_runtime_calls_before_pending_point() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls: host_os_calls.clone(),
        host_wait_calls: host_wait_calls.clone(),
    };
    let mut session = EvalSession::new(
        "fn build() { let os = host_os(); let wait = host_wait(); os }",
        "session-local-resume",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    let first = session.run().expect("first session run");
    let Resolution::Pending(pending) = first else {
        panic!("expected pending resolution, got {first:?}");
    };
    assert_eq!(host_os_calls.load(Ordering::SeqCst), 1);
    assert_eq!(host_wait_calls.load(Ordering::SeqCst), 1);

    session
        .resolve_pending(&pending, Value::string("done"))
        .expect("resolve pending node");

    let second = session.run().expect("second session run");
    let Resolution::Ready(value) = second else {
        panic!("expected ready resolution, got {second:?}");
    };
    assert_eq!(value.rep, ValueRep::String("windows".into()));
    assert_eq!(
        host_os_calls.load(Ordering::SeqCst),
        1,
        "ready runtime calls before the pending point should not rerun",
    );
    assert_eq!(
        host_wait_calls.load(Ordering::SeqCst),
        1,
        "the pending runtime call should resume from the resolved value",
    );
}

#[test]
fn resolving_pending_node_requeues_resume_work_record() {
    let mut session = EvalSession::new(
        "fn build() { host_wait() }",
        "session-queued-work",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    let first = session.run().expect("first session run");
    let Resolution::Pending(pending) = first else {
        panic!("expected pending resolution, got {first:?}");
    };
    assert!(session.queued_work().is_empty());
    let blocked_call_node = session
        .trace()
        .events
        .iter()
        .find_map(|event| match &event.kind {
            EvalEventKind::RegisterPending {
                pending: registered,
            } if registered == &pending => Some(event.node_id),
            _ => None,
        })
        .expect("trace should record the blocked call node");

    session
        .resolve_pending(&pending, Value::string("windows"))
        .expect("resolve pending node");

    let queued = session.queued_work();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].kind, EvalWorkKind::ResumePending);
    assert_eq!(queued[0].node_id, blocked_call_node);
    assert_eq!(queued[0].pending.as_ref(), Some(&pending));
    assert_eq!(
        session.node_state(blocked_call_node),
        Some(&EvalNodeState::Runnable {
            work: queued[0].id,
            pending: Some(pending.clone()),
        }),
    );
    let runnable = session.runnable_nodes();
    assert_eq!(runnable.len(), 1);
    assert_eq!(runnable[0].node_id, blocked_call_node);
    assert_eq!(runnable[0].work_id, queued[0].id);
    assert_eq!(runnable[0].kind, EvalWorkKind::ResumePending);
    assert_eq!(runnable[0].pending.as_ref(), Some(&pending));
    assert_eq!(
        session.node_state(EvalNodeId(pending.node_id)),
        Some(&EvalNodeState::Ready(Value::string("windows"))),
    );

    let second = session.run().expect("second session run");
    let Resolution::Ready(value) = second else {
        panic!("expected ready resolution, got {second:?}");
    };
    assert_eq!(value.rep, ValueRep::String("windows".into()));
    assert!(session.queued_work().is_empty());
    assert_eq!(session.completed_work().len(), 5);
    assert_eq!(
        session.completed_work()[3].work.kind,
        EvalWorkKind::ResumePending
    );
    assert_eq!(
        session.completed_work()[3].outcome,
        EvalCompletedWorkOutcome::Ready(Value::string("windows")),
    );
}

#[test]
fn session_can_progress_through_multiple_pending_runtime_calls() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let wait_one_calls = Arc::new(AtomicUsize::new(0));
    let wait_two_calls = Arc::new(AtomicUsize::new(0));
    let runtime = DoublePendingRuntime {
        host_os_calls: host_os_calls.clone(),
        wait_one_calls: wait_one_calls.clone(),
        wait_two_calls: wait_two_calls.clone(),
    };
    let mut session = EvalSession::new(
        "fn build() { let os = host_os(); let first = wait_one(); let second = wait_two(); os }",
        "session-two-pending",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    let first = session.run().expect("first session run");
    let Resolution::Pending(first_pending) = first else {
        panic!("expected first pending resolution, got {first:?}");
    };
    assert_eq!(host_os_calls.load(Ordering::SeqCst), 1);
    assert_eq!(wait_one_calls.load(Ordering::SeqCst), 1);
    assert_eq!(wait_two_calls.load(Ordering::SeqCst), 0);

    session
        .resolve_pending(&first_pending, Value::string("one"))
        .expect("resolve first pending node");

    let second = session.run().expect("second session run");
    let Resolution::Pending(second_pending) = second else {
        panic!("expected second pending resolution, got {second:?}");
    };
    assert_ne!(first_pending.node_id, second_pending.node_id);
    assert_eq!(
        host_os_calls.load(Ordering::SeqCst),
        1,
        "ready work before the first pending point should not replay",
    );
    assert_eq!(wait_one_calls.load(Ordering::SeqCst), 1);
    assert_eq!(wait_two_calls.load(Ordering::SeqCst), 1);

    session
        .resolve_pending(&second_pending, Value::string("two"))
        .expect("resolve second pending node");

    let third = session.run().expect("third session run");
    let Resolution::Ready(value) = third else {
        panic!("expected ready resolution, got {third:?}");
    };
    assert_eq!(value.rep, ValueRep::String("windows".into()));
    assert_eq!(host_os_calls.load(Ordering::SeqCst), 1);
    assert_eq!(wait_one_calls.load(Ordering::SeqCst), 1);
    assert_eq!(wait_two_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn eval2_core_trace_types_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<Value>();
    assert_send_sync::<Resolution>();
    assert_send_sync::<EvalEvent>();
    assert_send_sync::<EvalTrace>();
    assert_send_sync::<crate::SourceSpan>();
}

#[test]
fn evaluation_can_stream_events_from_a_background_thread() {
    let (mut sink, receiver) = channel_trace();
    let handle = thread::spawn(move || {
        eval_source_entry_with_args_with_source_id_with_sink(
            "fn build(value: String) { value }",
            "threaded-trace",
            "build",
            &[Value::string("forty-two")],
            &mut sink,
        )
    });

    let resolution = handle
        .join()
        .expect("background eval thread should not panic")
        .expect("background eval should succeed");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("forty-two".into()));

    let events = receiver.iter().collect::<Vec<_>>();
    assert!(!events.is_empty(), "channel sink should receive events");
    assert_eq!(
        events[0].kind,
        EvalEventKind::BindArgument {
            label: "value".into(),
        },
    );
    assert!(events.iter().all(|event| event.place.as_ref() == "local"));
}
