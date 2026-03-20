use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use crate::{
    Claim, CrossingPolicy, EvalBranchContext, EvalCompletedWorkOutcome, EvalDependencyEdge,
    EvalError, EvalErrorKind, EvalEvent, EvalEventKind, EvalExplainTerminal, EvalLeasedWorkState,
    EvalNodeId, EvalNodeKind, EvalNodeState, EvalSession, EvalSessionStep, EvalTrace, EvalWorkKind,
    EvalWorkState, PendingRef, ProjectionStep, Provenance, Resolution, ReuseClass,
    RuntimeCallOutcome, RuntimeHooks, RuntimeInputKind, RuntimeReady, SlotAction, SlotEventReason,
    SlotId, SlotResolutionKind, SlotResolved, SlotStore, SlotStoreReason, SlotVerification,
    SourceSpan, Value, ValueRep, channel_trace, envelope_to_value,
    eval_source_entry_at_place_with_args_with_source_id_traced,
    eval_source_entry_with_args_with_source_id, eval_source_entry_with_args_with_source_id_traced,
    eval_source_entry_with_args_with_source_id_with_sink, value_to_envelope,
};

#[test]
fn current_populates_missing_slot_and_then_reuses_it() {
    let mut slots = SlotStore::<u64, String, String, String>::default();

    let initial = slots
        .resolve(
            "toolchain",
            "w1".to_string(),
            SlotAction::<String>::Current,
            |_| SlotVerification::Valid,
            |_| Ok::<_, EvalError>(SlotResolved::new(41)),
        )
        .expect("populate missing slot");
    assert_eq!(
        initial.kind,
        SlotResolutionKind::Stored {
            reason: SlotStoreReason::Missing,
        }
    );
    assert_eq!(initial.record.slot_id, SlotId::from("toolchain"));
    assert_eq!(initial.record.value, 41);
    assert_eq!(initial.record.meta.write_count, 1);
    assert_eq!(initial.record.meta.rewrite_count, 0);

    let reused = slots
        .resolve(
            "toolchain",
            "w1".to_string(),
            SlotAction::<String>::Current,
            |_| SlotVerification::Valid,
            |_| -> Result<SlotResolved<u64, String>, EvalError> {
                panic!("reused slot should not resolve again")
            },
        )
        .expect("reuse existing slot");
    assert_eq!(reused.kind, SlotResolutionKind::Reused);
    assert_eq!(reused.record.value, 41);
    assert_eq!(reused.record.meta.write_count, 1);
    assert_eq!(reused.record.meta.rewrite_count, 0);
}

#[test]
fn witness_changes_and_failed_verification_rewrite_in_place() {
    let mut slots = SlotStore::<u64, String, String, String>::default();

    slots
        .resolve(
            "toolchain",
            "w1".to_string(),
            SlotAction::<String>::Current,
            |_| SlotVerification::Valid,
            |_| Ok::<_, EvalError>(SlotResolved::new(41)),
        )
        .expect("populate slot");

    let witness_changed = slots
        .resolve(
            "toolchain",
            "w2".to_string(),
            SlotAction::<String>::Current,
            |_| SlotVerification::Valid,
            |_| Ok::<_, EvalError>(SlotResolved::new(42)),
        )
        .expect("rewrite after witness change");
    assert_eq!(
        witness_changed.kind,
        SlotResolutionKind::Stored {
            reason: SlotStoreReason::WitnessChanged,
        }
    );
    assert_eq!(slots.len(), 1);
    assert_eq!(witness_changed.record.slot_id, SlotId::from("toolchain"));
    assert_eq!(witness_changed.record.value, 42);
    assert_eq!(witness_changed.record.meta.write_count, 2);
    assert_eq!(witness_changed.record.meta.rewrite_count, 1);

    let claims_invalid = slots
        .resolve(
            "toolchain",
            "w2".to_string(),
            SlotAction::<String>::Current,
            |_| SlotVerification::invalid("claims no longer verify"),
            |_| Ok::<_, EvalError>(SlotResolved::new(43)),
        )
        .expect("rewrite after verification failure");
    assert_eq!(slots.len(), 1);
    assert_eq!(claims_invalid.record.slot_id, SlotId::from("toolchain"));
    assert_eq!(claims_invalid.record.value, 43);
    assert_eq!(claims_invalid.record.meta.write_count, 3);
    assert_eq!(claims_invalid.record.meta.rewrite_count, 2);
    assert_eq!(
        claims_invalid.record.meta.last_store_reason,
        SlotStoreReason::ClaimsInvalid {
            reason: "claims no longer verify".into(),
        }
    );
}

#[test]
fn refresh_forces_recompute_and_exposes_current_value_to_resolver() {
    let mut slots = SlotStore::<u64, String, String, String>::default();
    slots
        .resolve(
            "toolchain",
            "w1".to_string(),
            SlotAction::<String>::Current,
            |_| SlotVerification::Valid,
            |_| Ok::<_, EvalError>(SlotResolved::new(41)),
        )
        .expect("populate slot");

    let seen_current = Arc::new(Mutex::new(Vec::new()));
    let seen_current_clone = Arc::clone(&seen_current);
    let refreshed = slots
        .resolve(
            "toolchain",
            "w1".to_string(),
            SlotAction::Refresh("force-rediscover".to_string()),
            |_| SlotVerification::Valid,
            move |request| {
                seen_current_clone
                    .lock()
                    .expect("capture lock poisoned")
                    .push((
                        request.current.map(|record| record.value),
                        request.action.clone(),
                    ));
                Ok::<_, EvalError>(SlotResolved::new(99))
            },
        )
        .expect("refresh slot");

    assert_eq!(
        refreshed.kind,
        SlotResolutionKind::Stored {
            reason: SlotStoreReason::Refresh {
                intent: "force-rediscover".to_string(),
            },
        }
    );
    assert_eq!(slots.len(), 1);
    assert_eq!(refreshed.record.slot_id, SlotId::from("toolchain"));
    assert_eq!(refreshed.record.value, 99);
    assert_eq!(refreshed.record.meta.write_count, 2);
    assert_eq!(refreshed.record.meta.rewrite_count, 1);
    assert_eq!(
        refreshed.record.meta.last_store_reason,
        SlotStoreReason::Refresh {
            intent: "force-rediscover".to_string(),
        }
    );
    assert_eq!(
        seen_current
            .lock()
            .expect("capture lock poisoned")
            .as_slice(),
        &[(
            Some(41),
            SlotAction::Refresh("force-rediscover".to_string())
        )]
    );

    let current = slots
        .resolve(
            "toolchain",
            "w1".to_string(),
            SlotAction::<String>::Current,
            |_| SlotVerification::Valid,
            |_| -> Result<SlotResolved<u64, String>, EvalError> {
                panic!("refreshed slot should reuse its new current value")
            },
        )
        .expect("reuse refreshed slot");
    assert_eq!(current.kind, SlotResolutionKind::Reused);
    assert_eq!(current.record.value, 99);
}

#[test]
fn slot_runtime_calls_emit_trace_events_with_refresh_intent() {
    #[derive(Clone)]
    struct SlotRuntime {
        slots: Arc<Mutex<SlotStore<Value, Value, (), Value>>>,
    }

    impl SlotRuntime {
        fn new() -> Self {
            Self {
                slots: Arc::new(Mutex::new(SlotStore::default())),
            }
        }
    }

    impl RuntimeHooks for SlotRuntime {
        fn call(
            &self,
            _place: &str,
            callee: &str,
            args: &[Value],
            _source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            match callee {
                "slot_current_pure" => {
                    let ValueRep::String(slot_id) = &args[0].rep else {
                        panic!("slot id should be a string");
                    };
                    let mut slots = self.slots.lock().expect("slot lock poisoned");
                    let resolution = slots.resolve(
                        slot_id.to_string(),
                        args[1].clone(),
                        SlotAction::Current,
                        |_| SlotVerification::Valid,
                        |_| Ok::<_, EvalError>(SlotResolved::new(args[2].clone())),
                    )?;
                    let event = match resolution.kind {
                        SlotResolutionKind::Reused => EvalEventKind::SlotReuse {
                            slot: slot_id.clone(),
                        },
                        SlotResolutionKind::Stored {
                            reason: SlotStoreReason::Missing,
                        } => EvalEventKind::SlotPopulate {
                            slot: slot_id.clone(),
                            reason: SlotEventReason::Missing,
                        },
                        SlotResolutionKind::Stored { reason } => EvalEventKind::SlotRewrite {
                            slot: slot_id.clone(),
                            reason: match reason {
                                SlotStoreReason::WitnessChanged => SlotEventReason::WitnessChanged,
                                SlotStoreReason::ClaimsInvalid { reason } => {
                                    SlotEventReason::ClaimsInvalid { reason }
                                }
                                SlotStoreReason::Refresh { intent } => {
                                    SlotEventReason::Refresh { intent }
                                }
                                SlotStoreReason::Missing => SlotEventReason::Missing,
                            },
                        },
                    };
                    Ok(RuntimeCallOutcome::ReadyWithEvents(RuntimeReady {
                        value: resolution.record.value,
                        events: vec![event],
                    }))
                }
                "slot_refresh_pure" => {
                    let ValueRep::String(slot_id) = &args[0].rep else {
                        panic!("slot id should be a string");
                    };
                    let mut slots = self.slots.lock().expect("slot lock poisoned");
                    let resolution = slots.resolve(
                        slot_id.to_string(),
                        args[1].clone(),
                        SlotAction::Refresh(args[3].clone()),
                        |_| SlotVerification::Valid,
                        |_| Ok::<_, EvalError>(SlotResolved::new(args[2].clone())),
                    )?;
                    Ok(RuntimeCallOutcome::ReadyWithEvents(RuntimeReady {
                        value: resolution.record.value,
                        events: vec![EvalEventKind::SlotRewrite {
                            slot: slot_id.clone(),
                            reason: SlotEventReason::Refresh {
                                intent: match resolution.kind {
                                    SlotResolutionKind::Stored {
                                        reason: SlotStoreReason::Refresh { intent },
                                    } => intent,
                                    other => panic!("expected refresh store reason, got {other:?}"),
                                },
                            },
                        }],
                    }))
                }
                _ => Ok(RuntimeCallOutcome::Unsupported),
            }
        }
    }

    let runtime = SlotRuntime::new();
    let source = r#"
fn build() {
  slot_current_pure("toolchain", "w1", 41)
}
"#;
    let (initial, initial_trace) = eval_source_entry_at_place_with_args_with_source_id_traced(
        source,
        "slot-trace-current",
        "build",
        &[],
        &runtime,
        "local",
    )
    .expect("evaluate initial slot current");
    let Resolution::Ready(initial_value) = initial else {
        panic!("expected ready resolution, got {initial:?}");
    };
    assert_eq!(initial_value.as_u64(), Some(41));
    assert!(initial_trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::SlotPopulate {
                slot: "toolchain".into(),
                reason: SlotEventReason::Missing,
            }
    }));

    let refresh_source = r#"
fn build() {
  slot_refresh_pure("toolchain", "w1", 99, "force-rediscover")
}
"#;
    let (refreshed, refresh_trace) = eval_source_entry_at_place_with_args_with_source_id_traced(
        refresh_source,
        "slot-trace-refresh",
        "build",
        &[],
        &runtime,
        "local",
    )
    .expect("evaluate slot refresh");
    let Resolution::Ready(refreshed_value) = refreshed else {
        panic!("expected ready resolution, got {refreshed:?}");
    };
    assert_eq!(refreshed_value.as_u64(), Some(99));
    assert!(refresh_trace.events.iter().any(|event| {
        matches!(
            &event.kind,
            EvalEventKind::SlotRewrite {
                slot,
                reason: SlotEventReason::Refresh { intent },
            } if slot.as_ref() == "toolchain"
                && matches!(&intent.rep, ValueRep::String(value) if value.as_ref() == "force-rediscover")
        )
    }));

    let stored = runtime
        .slots
        .lock()
        .expect("slot lock poisoned")
        .get(&SlotId::from("toolchain"))
        .cloned()
        .expect("stored slot");
    assert_eq!(stored.value.as_u64(), Some(99));
    match stored.meta.last_store_reason {
        SlotStoreReason::Refresh { intent } => {
            assert!(matches!(
                &intent.rep,
                ValueRep::String(value) if value.as_ref() == "force-rediscover"
            ));
        }
        other => panic!("expected refresh store reason, got {other:?}"),
    }
}

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

struct ParallelObserveRuntime {
    active_calls: Arc<AtomicUsize>,
    max_calls: Arc<AtomicUsize>,
}

impl RuntimeHooks for ParallelObserveRuntime {
    fn call(
        &self,
        place: &str,
        callee: &str,
        _args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match (place, callee) {
            ("win", "host_os") => {
                let active = self.active_calls.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_calls.fetch_max(active, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(40));
                self.active_calls.fetch_sub(1, Ordering::SeqCst);
                Ok(RuntimeCallOutcome::Ready(
                    Value::string("windows")
                        .with_provenance(Provenance::host_probe(source.clone(), place, "host_os"))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue),
                ))
            }
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
    }
}

struct StaggeredRuntime;

impl RuntimeHooks for StaggeredRuntime {
    fn call(
        &self,
        place: &str,
        callee: &str,
        _args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match (place, callee) {
            ("win", "slow_value") => {
                thread::sleep(Duration::from_millis(50));
                Ok(RuntimeCallOutcome::Ready(
                    Value::string("slow")
                        .with_provenance(Provenance::host_probe(
                            source.clone(),
                            place,
                            "slow_value",
                        ))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue),
                ))
            }
            ("win", "fast_value") => {
                thread::sleep(Duration::from_millis(5));
                Ok(RuntimeCallOutcome::Ready(
                    Value::string("fast")
                        .with_provenance(Provenance::host_probe(
                            source.clone(),
                            place,
                            "fast_value",
                        ))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue),
                ))
            }
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
    }
}

struct PanicRuntime;

impl RuntimeHooks for PanicRuntime {
    fn call(
        &self,
        place: &str,
        callee: &str,
        _args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match (place, callee) {
            ("win", "panic_now") => panic!("panic runtime hook exploded"),
            ("win", "host_os") => Ok(RuntimeCallOutcome::Ready(
                Value::string("windows")
                    .with_provenance(Provenance::host_probe(source.clone(), place, "host_os"))
                    .with_reuse(ReuseClass::Observed)
                    .with_crossing(CrossingPolicy::ByValue),
            )),
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
fn session_can_expose_multiple_runnable_array_children_at_once() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls,
        host_wait_calls,
    };
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_wait()] }",
        "session-array-frontier",
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
        session.step().expect("root array step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array expression step"),
        EvalSessionStep::Progressed
    );

    let queued = session.queued_work();
    assert_eq!(queued.len(), 2);
    assert!(
        queued
            .iter()
            .all(|work| work.kind == EvalWorkKind::EvaluateExpr)
    );

    let snapshot = session.graph_snapshot();
    let array_node = snapshot
        .nodes
        .iter()
        .find(|node| {
            node.record.as_ref().is_some_and(|record| {
                record.kind
                    == EvalNodeKind::Expr {
                        kind: crate::EvalExprKind::Array,
                    }
            })
        })
        .expect("array node should be recorded");

    let child_call_nodes = snapshot
        .nodes
        .iter()
        .filter(|node| {
            node.record.as_ref().is_some_and(|record| {
                record.parent_id == Some(array_node.node_id)
                    && record.kind
                        == EvalNodeKind::Expr {
                            kind: crate::EvalExprKind::Call,
                        }
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(child_call_nodes.len(), 2);
    assert_eq!(
        array_node.dependencies,
        child_call_nodes
            .iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        array_node.state,
        Some(EvalNodeState::BlockedOnNode {
            dependency: child_call_nodes[0].node_id,
        }),
    );
    assert!(
        child_call_nodes
            .iter()
            .all(|node| matches!(node.state, Some(EvalNodeState::Runnable { .. })))
    );
}

#[test]
fn session_can_expose_multiple_runnable_call_arguments_at_once() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls,
        host_wait_calls,
    };
    let mut session = EvalSession::new(
        "fn build() { pair(host_os(), host_wait()) }",
        "session-call-frontier",
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
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("pair call step"),
        EvalSessionStep::Progressed
    );

    let queued = session.queued_work();
    assert_eq!(queued.len(), 2);
    assert!(
        queued
            .iter()
            .all(|work| work.kind == EvalWorkKind::EvaluateExpr)
    );

    let snapshot = session.graph_snapshot();
    let call_node = snapshot
        .nodes
        .iter()
        .find(|node| {
            node.record.as_ref().is_some_and(|record| {
                record.kind
                    == EvalNodeKind::Expr {
                        kind: crate::EvalExprKind::Call,
                    }
            }) && node.dependencies.len() == 2
        })
        .expect("pair call node should be recorded");
    let arg_call_nodes = snapshot
        .nodes
        .iter()
        .filter(|node| {
            node.record.as_ref().is_some_and(|record| {
                record.parent_id == Some(call_node.node_id)
                    && record.kind
                        == EvalNodeKind::Expr {
                            kind: crate::EvalExprKind::Call,
                        }
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(arg_call_nodes.len(), 2);
    assert_eq!(
        call_node.dependencies,
        arg_call_nodes
            .iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        call_node.state,
        Some(EvalNodeState::BlockedOnNode {
            dependency: arg_call_nodes[0].node_id,
        }),
    );
    assert!(
        arg_call_nodes
            .iter()
            .all(|node| matches!(node.state, Some(EvalNodeState::Runnable { .. })))
    );
}

#[test]
fn session_can_expose_multiple_runnable_struct_fields_at_once() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls,
        host_wait_calls,
    };
    let mut session = EvalSession::new(
        "fn build() { Pair { left: host_os(), right: host_wait() } }",
        "session-struct-frontier",
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
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("struct literal step"),
        EvalSessionStep::Progressed
    );

    let queued = session.queued_work();
    assert_eq!(queued.len(), 2);
    assert!(
        queued
            .iter()
            .all(|work| work.kind == EvalWorkKind::EvaluateExpr)
    );

    let snapshot = session.graph_snapshot();
    let struct_node = snapshot
        .nodes
        .iter()
        .find(|node| {
            node.record.as_ref().is_some_and(|record| {
                record.kind
                    == EvalNodeKind::Expr {
                        kind: crate::EvalExprKind::StructLiteral,
                    }
            }) && node.dependencies.len() == 2
        })
        .expect("struct node should be recorded");
    let field_call_nodes = snapshot
        .nodes
        .iter()
        .filter(|node| {
            node.record.as_ref().is_some_and(|record| {
                record.parent_id == Some(struct_node.node_id)
                    && record.kind
                        == EvalNodeKind::Expr {
                            kind: crate::EvalExprKind::Call,
                        }
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(field_call_nodes.len(), 2);
    assert_eq!(
        struct_node.dependencies,
        field_call_nodes
            .iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        struct_node.state,
        Some(EvalNodeState::BlockedOnNode {
            dependency: field_call_nodes[0].node_id,
        }),
    );
    assert!(
        field_call_nodes
            .iter()
            .all(|node| matches!(node.state, Some(EvalNodeState::Runnable { .. })))
    );
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
fn prepared_work_can_execute_on_a_scoped_worker_thread() {
    let runtime = TestRuntime;
    let mut session = EvalSession::new(
        "fn build() { host_wait() }",
        "prepared-worker",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    let prepared = session
        .prepare_step()
        .expect("start-entry work should exist");
    let completion = thread::scope(|scope| {
        scope
            .spawn(move || prepared.execute().expect("prepared work should execute"))
            .join()
            .expect("worker thread should not panic")
    });
    assert_eq!(completion.work().kind, EvalWorkKind::StartEntry);
    assert_eq!(*completion.outcome(), EvalCompletedWorkOutcome::Progressed);

    let step = session
        .apply_completion(completion)
        .expect("completion should apply");
    assert_eq!(step, EvalSessionStep::Progressed);
    assert_eq!(session.completed_work().len(), 1);
    assert_eq!(
        session.completed_work()[0].work.kind,
        EvalWorkKind::StartEntry
    );
    let queued = session.queued_work();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].kind, EvalWorkKind::EvaluateExpr);
}

#[test]
fn sibling_prepared_block_work_survives_checkpoint_restore_and_resolves_handles() {
    let mut session = EvalSession::new(
        "fn build() { [{ let os = host_os(); os }, { let os = host_os(); os }] }",
        "prepared-checkpoint-blocks",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(session.program_parse_count(), 1);
    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root expression step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("first leased sibling");
    let second = session.prepare_step().expect("second leased sibling");
    let checkpoint = session.checkpoint();

    let (first_completion, second_completion) = thread::scope(|scope| {
        let first_join = scope.spawn(move || first.execute().expect("first work should execute"));
        let second_join =
            scope.spawn(move || second.execute().expect("second work should execute"));
        (
            first_join.join().expect("first worker should not panic"),
            second_join.join().expect("second worker should not panic"),
        )
    });

    session.restore(checkpoint);
    assert_eq!(session.program_parse_count(), 1);

    assert_eq!(
        session
            .apply_completion(first_completion)
            .expect("first completion should apply"),
        EvalSessionStep::Progressed,
    );
    assert_eq!(
        session
            .apply_completion(second_completion)
            .expect("second completion should apply"),
        EvalSessionStep::Progressed,
    );

    let resolution = session.run().expect("resume queued parent work");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    let ValueRep::Array(items) = value.rep else {
        panic!("expected array value");
    };
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|item| item.rep == ValueRep::String("windows".into()))
    );
    assert_eq!(session.program_parse_count(), 1);
}

#[test]
fn stale_prepared_work_completion_is_rejected() {
    let runtime = TestRuntime;
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "stale-prepared-work",
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
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let completion = session
        .prepare_step()
        .expect("leased sibling should exist")
        .execute()
        .expect("prepared work should execute");
    assert!(completion.lease_sequence() > 0);

    assert!(session.step_back());

    let error = session
        .apply_completion(completion)
        .expect_err("stale completion should be rejected");
    assert_eq!(error.kind, EvalErrorKind::Internal);
    assert!(
        error.message.contains("stale eval work completion"),
        "unexpected error message: {}",
        error.message
    );
}

#[test]
fn old_completion_is_rejected_after_step_back_and_re_lease() {
    let mut session = EvalSession::new(
        "fn build() { host_os() }",
        "prepared-release-stale",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("leased root work");
    let first_work = first.work().clone();
    let old_completion = first.execute().expect("old completion should execute");

    assert!(session.step_back());

    let released = session.prepare_step().expect("re-leased root work");
    assert_eq!(released.work().node_id, first_work.node_id);
    assert!(
        released.lease_sequence() > old_completion.lease_sequence(),
        "re-leased work must receive a fresh lease identity",
    );

    let error = session
        .apply_completion(old_completion)
        .expect_err("old completion should be stale after re-lease");
    assert_eq!(error.kind, EvalErrorKind::Internal);
    assert!(
        error.message.contains("stale eval work completion"),
        "unexpected error message: {}",
        error.message
    );

    assert_eq!(
        session
            .apply_completion(released.execute().expect("fresh completion"))
            .expect("fresh completion should apply"),
        EvalSessionStep::Progressed,
    );
}

#[test]
fn preparing_twice_never_returns_the_same_work_twice() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "prepared-distinct-work",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(session.queued_work().len(), 2);
    assert!(session.in_flight_work().is_empty());

    let first = session.prepare_step().expect("first leased sibling");
    let first_work = first.work().clone();
    assert_eq!(session.queued_work().len(), 1);
    assert_eq!(session.in_flight_work(), vec![first_work.clone()]);
    assert!(session.work_records().iter().any(|record| {
        record.work.id == first_work.id && record.state == EvalWorkState::InFlight
    }));

    let second = session.prepare_step().expect("second leased sibling");
    let second_work = second.work().clone();
    assert_ne!(first_work.id, second_work.id);
    assert!(session.queued_work().is_empty());
    assert_eq!(session.in_flight_work().len(), 2);
    assert_eq!(
        session
            .graph_snapshot()
            .work
            .iter()
            .filter(|record| record.state == EvalWorkState::InFlight)
            .count(),
        2
    );
    assert!(session.prepare_step().is_none());
}

#[test]
fn run_returns_waiting_when_only_in_flight_work_remains() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "prepared-run-waiting",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let _first = session.prepare_step().expect("first leased sibling");
    let _second = session.prepare_step().expect("second leased sibling");

    assert!(session.queued_work().is_empty());
    assert!(session.pending_nodes().is_empty());
    assert_eq!(
        session.step().expect("waiting session step"),
        EvalSessionStep::WaitingOnInFlight
    );
    assert_eq!(session.run().expect("waiting run"), Resolution::Waiting);
}

#[test]
fn sibling_prepared_work_can_execute_on_threads_and_preserve_live_state() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "prepared-sibling-threads",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("first leased sibling");
    let first_work = first.work().clone();
    let second = session.prepare_step().expect("second leased sibling");
    let second_work = second.work().clone();

    let (first_completion, second_completion) = thread::scope(|scope| {
        let first_join = scope.spawn(move || first.execute().expect("first work should execute"));
        let second_join =
            scope.spawn(move || second.execute().expect("second work should execute"));
        (
            first_join.join().expect("first worker should not panic"),
            second_join.join().expect("second worker should not panic"),
        )
    });

    assert_eq!(
        session
            .apply_completion(first_completion)
            .expect("first completion should apply"),
        EvalSessionStep::Progressed,
    );
    assert_eq!(session.in_flight_work(), vec![second_work.clone()]);
    let queued = session.queued_work();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].kind, EvalWorkKind::ResumePending);
    assert_ne!(queued[0].node_id, first_work.node_id);

    assert_eq!(
        session
            .apply_completion(second_completion)
            .expect("second completion should apply"),
        EvalSessionStep::Progressed,
    );
    assert!(session.in_flight_work().is_empty());
    assert_eq!(session.queued_work().len(), 1);
    assert_eq!(session.queued_work()[0].kind, EvalWorkKind::ResumePending);
    assert_ne!(session.queued_work()[0].id, first_work.id);

    let resolution = session.run().expect("resume queued parent work");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    let ValueRep::Array(items) = value.rep else {
        panic!("expected array value");
    };
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|item| item.rep == ValueRep::String("windows".into()))
    );
}

#[test]
fn apply_completion_is_transactional_when_delta_merge_fails() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "prepared-transactional-merge",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("first leased sibling");
    let before_graph = session.graph_snapshot();
    let before_trace = session.trace().clone();
    let before_completed = session.completed_work().to_vec();
    let before_work = session.work_records();
    let duplicate = session.first_queued_work_for_testing();

    let mut completion = first.execute().expect("completion should execute");
    completion.inject_duplicate_queued_work(duplicate);

    let error = session
        .apply_completion(completion)
        .expect_err("corrupted delta merge should fail");
    assert_eq!(error.kind, EvalErrorKind::Internal);
    assert!(
        error
            .message
            .contains("already exists during completion merge"),
        "unexpected error message: {}",
        error.message
    );
    assert_eq!(session.graph_snapshot(), before_graph);
    assert_eq!(session.trace(), &before_trace);
    assert_eq!(session.completed_work(), before_completed.as_slice());
    assert_eq!(session.work_records(), before_work);
}

#[test]
fn out_of_order_sibling_completion_is_buffered_until_earlier_lease_commits() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "prepared-out-of-order",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("first leased sibling");
    let first_work = first.work().clone();
    let second = session.prepare_step().expect("second leased sibling");
    let second_work = second.work().clone();
    let first_completion = first.execute().expect("first completion");
    let second_completion = second.execute().expect("second completion");
    let second_wave_id = second_completion.wave_id();
    let second_lease_sequence = second_completion.lease_sequence();
    let second_outcome = second_completion.outcome().clone();

    assert_eq!(
        session
            .apply_completion(second_completion)
            .expect("out-of-order completion should buffer"),
        EvalSessionStep::WaitingOnInFlight,
    );
    assert_eq!(session.in_flight_work(), vec![first_work.clone()]);
    assert!(session.queued_work().is_empty());
    assert_eq!(session.buffered_work().len(), 1);
    assert_eq!(session.buffered_work()[0].wave_id, second_wave_id);
    assert_eq!(
        session.buffered_work()[0].lease_sequence,
        second_lease_sequence
    );
    assert_eq!(session.buffered_work()[0].work, second_work.clone());
    assert_eq!(session.buffered_work()[0].outcome, second_outcome.clone());
    let active_wave = session
        .active_wave()
        .expect("buffered siblings should retain a wave");
    assert_eq!(active_wave.wave_id, second_wave_id);
    assert_eq!(active_wave.work.len(), 2);
    assert_eq!(active_wave.work[0].work.id, first_work.id);
    assert_eq!(active_wave.work[0].state, EvalLeasedWorkState::InFlight);
    assert_eq!(active_wave.work[1].work.id, second_work.id);
    assert_eq!(
        active_wave.work[1].state,
        EvalLeasedWorkState::Buffered(second_outcome.clone()),
    );
    assert!(session.work_records().iter().any(|record| {
        record.work.id == second_work.id
            && record.state == EvalWorkState::Buffered(second_outcome.clone())
    }));
    assert_eq!(
        session.explain_node(second_work.node_id).terminal,
        EvalExplainTerminal::Buffered {
            node_id: second_work.node_id,
            work: second_work.id,
            pending: None,
            lease_sequence: second_lease_sequence,
            outcome: second_outcome.clone(),
        },
    );
    assert_eq!(
        session.run().expect("session should still be waiting"),
        Resolution::Waiting
    );
    assert!(
        session
            .completed_work()
            .iter()
            .all(|completed| completed.work.id != second_work.id),
        "buffered completion must not be visible as committed work yet",
    );

    assert_eq!(
        session
            .apply_completion(first_completion)
            .expect("first completion should commit and drain"),
        EvalSessionStep::Progressed,
    );
    assert!(session.active_wave().is_none());
    assert!(session.buffered_work().is_empty());
    assert!(session.in_flight_work().is_empty());
    assert_eq!(
        session
            .completed_work()
            .iter()
            .rev()
            .take(2)
            .map(|completed| completed.work.id)
            .collect::<Vec<_>>(),
        vec![second_work.id, first_work.id],
    );

    let resolution = session.run().expect("resume queued parent work");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    let ValueRep::Array(items) = value.rep else {
        panic!("expected array value");
    };
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|item| item.rep == ValueRep::String("windows".into()))
    );
}

#[test]
fn buffered_sibling_completions_drain_in_lease_order_once_gap_closes() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os(), host_os()] }",
        "prepared-buffered-drain",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("first leased sibling");
    let first_work = first.work().clone();
    let second = session.prepare_step().expect("second leased sibling");
    let second_work = second.work().clone();
    let third = session.prepare_step().expect("third leased sibling");
    let third_work = third.work().clone();

    let first_completion = first.execute().expect("first completion");
    let second_completion = second.execute().expect("second completion");
    let third_completion = third.execute().expect("third completion");
    let second_lease_sequence = second_completion.lease_sequence();
    let third_lease_sequence = third_completion.lease_sequence();

    assert_eq!(
        session
            .apply_completion(third_completion)
            .expect("third completion should buffer"),
        EvalSessionStep::WaitingOnInFlight,
    );
    assert_eq!(
        session
            .apply_completion(second_completion)
            .expect("second completion should also buffer"),
        EvalSessionStep::WaitingOnInFlight,
    );
    assert_eq!(session.in_flight_work(), vec![first_work.clone()]);
    assert_eq!(
        session
            .buffered_work()
            .iter()
            .map(|buffered| buffered.lease_sequence)
            .collect::<Vec<_>>(),
        vec![second_lease_sequence, third_lease_sequence],
    );
    assert!(session.completed_work().iter().all(|completed| {
        completed.work.id != second_work.id && completed.work.id != third_work.id
    }));

    assert_eq!(
        session
            .apply_completion(first_completion)
            .expect("first completion should drain buffered siblings"),
        EvalSessionStep::Progressed,
    );
    assert!(session.buffered_work().is_empty());
    assert_eq!(
        session
            .completed_work()
            .iter()
            .rev()
            .take(3)
            .map(|completed| completed.work.id)
            .collect::<Vec<_>>(),
        vec![third_work.id, second_work.id, first_work.id],
    );

    let resolution = session.run().expect("resume queued parent work");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    let ValueRep::Array(items) = value.rep else {
        panic!("expected array value");
    };
    assert_eq!(items.len(), 3);
    assert!(
        items
            .iter()
            .all(|item| item.rep == ValueRep::String("windows".into()))
    );
}

#[test]
fn graph_snapshot_reflects_buffered_work() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "prepared-buffered-graph",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("first leased sibling");
    let second = session.prepare_step().expect("second leased sibling");
    let second_work = second.work().clone();
    let second_outcome = second.execute().expect("second completion");
    let second_wave_id = second_outcome.wave_id();
    let second_lease_sequence = second_outcome.lease_sequence();
    let second_result = second_outcome.outcome().clone();

    assert_eq!(
        session
            .apply_completion(second_outcome)
            .expect("second completion should buffer"),
        EvalSessionStep::WaitingOnInFlight,
    );

    let snapshot = session.graph_snapshot();
    let active_wave = snapshot
        .active_wave
        .clone()
        .expect("snapshot should retain the buffered wave");
    assert_eq!(active_wave.wave_id, second_wave_id);
    assert_eq!(active_wave.work.len(), 2);
    assert!(snapshot.work.iter().any(|record| {
        record.work.id == second_work.id
            && record.state == EvalWorkState::Buffered(second_result.clone())
    }));
    assert_eq!(
        session.buffered_work(),
        vec![crate::EvalBufferedWork {
            wave_id: second_wave_id,
            lease_sequence: second_lease_sequence,
            work: second_work,
            outcome: second_result,
        }],
    );

    let _first_completion = first.execute().expect("first completion");
}

#[test]
fn run_parallel_executes_sibling_frontier_on_worker_threads() {
    let active_calls = Arc::new(AtomicUsize::new(0));
    let max_calls = Arc::new(AtomicUsize::new(0));
    let runtime = ParallelObserveRuntime {
        active_calls,
        max_calls: max_calls.clone(),
    };
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "parallel-ready",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    let resolution = session
        .run_parallel(2)
        .expect("parallel run should succeed");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    let ValueRep::Array(items) = value.rep else {
        panic!("expected array value");
    };
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|item| item.rep == ValueRep::String("windows".into()))
    );
    assert!(
        max_calls.load(Ordering::SeqCst) >= 2,
        "parallel driver should overlap sibling runtime calls",
    );
}

#[test]
fn run_parallel_commits_out_of_order_worker_results_in_lease_order() {
    let mut session = EvalSession::new(
        "fn build() { [slow_value(), fast_value()] }",
        "parallel-out-of-order",
        "build",
        Vec::new(),
        &StaggeredRuntime,
        "win",
    );

    let resolution = session
        .run_parallel(2)
        .expect("parallel run should succeed");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    let ValueRep::Array(items) = value.rep else {
        panic!("expected array value");
    };
    assert_eq!(
        items
            .iter()
            .map(|item| match &item.rep {
                ValueRep::String(value) => value.as_ref().to_string(),
                other => panic!("expected string item, got {other:?}"),
            })
            .collect::<Vec<_>>(),
        vec!["slow".to_string(), "fast".to_string()],
    );
    assert_eq!(
        session
            .completed_work()
            .iter()
            .filter_map(|completed| match &completed.outcome {
                EvalCompletedWorkOutcome::Ready(value) => match &value.rep {
                    ValueRep::String(value) => Some(value.to_string()),
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec!["slow".to_string(), "fast".to_string()],
    );
}

#[test]
fn run_parallel_returns_pending_when_external_work_suspends() {
    let mut session = EvalSession::new(
        "fn build() { host_wait() }",
        "parallel-pending",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    let resolution = session
        .run_parallel(2)
        .expect("parallel run should succeed");
    let Resolution::Pending(pending) = resolution else {
        panic!("expected pending resolution, got {resolution:?}");
    };
    assert_eq!(
        pending.description.as_ref(),
        "waiting for remote host observation"
    );
    assert_eq!(session.pending_nodes(), vec![pending]);
}

#[test]
fn run_parallel_returns_poison_when_evaluation_fails() {
    let mut session = EvalSession::new(
        "fn build() { fail \"boom\" }",
        "parallel-poison",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    let resolution = session
        .run_parallel(2)
        .expect("parallel run should succeed");
    let Resolution::Poison(error) = resolution else {
        panic!("expected poison resolution, got {resolution:?}");
    };
    assert_eq!(error.kind, EvalErrorKind::UserFail);
    assert_eq!(error.message.as_ref(), "boom");
}

#[test]
fn run_parallel_returns_waiting_when_external_in_flight_work_blocks_commit() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "parallel-waiting",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("external lease should exist");
    let first_work = first.work().clone();

    assert_eq!(
        session
            .run_parallel(1)
            .expect("parallel driver should wait"),
        Resolution::Waiting,
    );
    assert_eq!(session.in_flight_work(), vec![first_work]);
    assert_eq!(session.buffered_work().len(), 1);
    let active_wave = session
        .active_wave()
        .expect("waiting session should expose its wave");
    assert_eq!(active_wave.work.len(), 2);
    assert!(matches!(
        session
            .explain_node(session.buffered_work()[0].work.node_id)
            .terminal,
        EvalExplainTerminal::Buffered { .. }
    ));
}

#[test]
fn run_parallel_leaves_wave_uncommitted_when_one_worker_returns_error() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "parallel-error-wave",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );
    session.set_branch_context(EvalBranchContext::new("__test_worker_error__"));

    let before_graph = session.graph_snapshot();
    let before_work = session.work_records();
    let before_completed = session.completed_work().to_vec();
    let before_trace = session.trace().clone();
    let before_active_wave = session.active_wave();
    let before_in_flight = session.in_flight_work();
    let before_buffered = session.buffered_work();

    let error = session
        .run_parallel(2)
        .expect_err("wave error should surface");
    assert_eq!(error.kind, EvalErrorKind::Internal);
    assert_eq!(error.message.as_ref(), "injected test worker error");
    assert_eq!(session.graph_snapshot(), before_graph);
    assert_eq!(session.work_records(), before_work);
    assert_eq!(session.completed_work(), before_completed.as_slice());
    assert_eq!(session.trace(), &before_trace);
    assert_eq!(session.active_wave(), before_active_wave);
    assert_eq!(session.in_flight_work(), before_in_flight);
    assert_eq!(session.buffered_work(), before_buffered);
}

#[test]
fn run_parallel_surfaces_worker_panics_instead_of_waiting_forever() {
    let mut session = EvalSession::new(
        "fn build() { [panic_now(), host_os()] }",
        "parallel-panic",
        "build",
        Vec::new(),
        &PanicRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let before_graph = session.graph_snapshot();
    let before_work = session.work_records();
    let before_completed = session.completed_work().to_vec();
    let before_trace = session.trace().clone();
    let before_active_wave = session.active_wave();
    let before_in_flight = session.in_flight_work();
    let before_buffered = session.buffered_work();

    let panic = catch_unwind(AssertUnwindSafe(|| session.run_parallel(2)))
        .expect_err("worker panic should unwind through run_parallel");
    let message = panic
        .downcast_ref::<&'static str>()
        .map(|message| (*message).to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .expect("panic payload should be string-like");
    assert!(
        message.contains("panic runtime hook exploded"),
        "unexpected panic payload: {message}",
    );
    assert_eq!(session.graph_snapshot(), before_graph);
    assert_eq!(session.work_records(), before_work);
    assert_eq!(session.completed_work(), before_completed.as_slice());
    assert_eq!(session.trace(), &before_trace);
    assert_eq!(session.active_wave(), before_active_wave);
    assert_eq!(session.in_flight_work(), before_in_flight);
    assert_eq!(session.buffered_work(), before_buffered);
}

#[test]
fn failed_wave_completions_do_not_become_usable_on_retry() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "parallel-retry",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );
    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );
    session.set_branch_context(EvalBranchContext::new("__test_worker_error__"));
    let before_graph = session.graph_snapshot();
    let before_work = session.work_records();
    let before_completed = session.completed_work().to_vec();

    let error = session
        .run_parallel(2)
        .expect_err("first wave should fail before commit");
    assert_eq!(error.kind, EvalErrorKind::Internal);
    assert_eq!(error.message.as_ref(), "injected test worker error");
    assert_eq!(session.graph_snapshot(), before_graph);
    assert_eq!(session.work_records(), before_work);
    assert_eq!(session.completed_work(), before_completed.as_slice());
    assert!(session.in_flight_work().is_empty());
    assert!(session.buffered_work().is_empty());
    assert!(session.active_wave().is_none());
    session.set_branch_context(EvalBranchContext::new("retry-ok"));

    let resolution = session
        .run_parallel(2)
        .expect("retry should lease fresh work and succeed");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution on retry, got {resolution:?}");
    };
    let ValueRep::Array(items) = value.rep else {
        panic!("expected array value");
    };
    assert_eq!(
        items
            .iter()
            .map(|item| match &item.rep {
                ValueRep::String(value) => value.as_ref().to_string(),
                other => panic!("expected string item, got {other:?}"),
            })
            .collect::<Vec<_>>(),
        vec!["windows".to_string(), "windows".to_string()],
    );
}

#[test]
fn branch_context_survives_checkpoint_restore() {
    let mut session = EvalSession::new(
        "fn build() { host_os() }",
        "branch-context-checkpoint",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );
    let original = EvalBranchContext::new("alpha");
    session.set_branch_context(original.clone());
    let checkpoint = session.checkpoint();

    session.set_branch_context(EvalBranchContext::new("beta"));
    assert_ne!(session.branch_context(), &original);

    session.restore(checkpoint);
    assert_eq!(session.branch_context(), &original);
    assert_eq!(session.graph_snapshot().branch_context, original);
}

#[test]
fn branch_context_survives_prepare_execute_and_apply_completion() {
    let mut session = EvalSession::new(
        "fn build() { host_os() }",
        "branch-context-prepare",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );
    let context = EvalBranchContext::new("branch-a");
    session.set_branch_context(context.clone());

    let prepared = session
        .prepare_step()
        .expect("start-entry work should exist");
    assert_eq!(prepared.branch_context(), &context);
    let completion = prepared.execute().expect("prepared work should execute");
    assert_eq!(completion.branch_context(), &context);
    let _step = session
        .apply_completion(completion)
        .expect("completion should apply");
    assert_eq!(session.branch_context(), &context);
    assert_eq!(session.graph_snapshot().branch_context, context);
}

#[test]
fn branch_context_does_not_leak_across_unrelated_sessions() {
    let mut first = EvalSession::new(
        "fn build() { host_os() }",
        "branch-context-first",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );
    let second = EvalSession::new(
        "fn build() { host_os() }",
        "branch-context-second",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    let first_context = EvalBranchContext::new("first-branch");
    first.set_branch_context(first_context.clone());
    assert_eq!(first.branch_context(), &first_context);
    assert_eq!(second.branch_context(), &EvalBranchContext::default());
}

#[test]
fn run_parallel_can_drive_multiple_frontier_waves_to_completion() {
    let active_calls = Arc::new(AtomicUsize::new(0));
    let max_calls = Arc::new(AtomicUsize::new(0));
    let runtime = ParallelObserveRuntime {
        active_calls,
        max_calls: max_calls.clone(),
    };
    let mut session = EvalSession::new(
        "fn build() { [[host_os(), host_os()], [host_os(), host_os()]] }",
        "parallel-multi-wave",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    let resolution = session
        .run_parallel(4)
        .expect("parallel run should succeed");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    let ValueRep::Array(rows) = value.rep else {
        panic!("expected outer array value");
    };
    assert_eq!(rows.len(), 2);
    for row in rows.iter() {
        let ValueRep::Array(items) = &row.rep else {
            panic!("expected nested array row");
        };
        assert_eq!(items.len(), 2);
        assert!(
            items
                .iter()
                .all(|item| item.rep == ValueRep::String("windows".into()))
        );
    }
    assert!(
        session
            .completed_work()
            .iter()
            .filter(|completed| completed.work.kind == EvalWorkKind::ResumePending)
            .count()
            >= 2,
        "multi-wave drive should include resumed parent work",
    );
    assert!(
        max_calls.load(Ordering::SeqCst) >= 2,
        "later waves should still execute sibling runtime calls in parallel",
    );
    assert!(session.active_wave().is_none());
    assert!(session.buffered_work().is_empty());
}

#[test]
fn buffered_completion_merge_failure_leaves_live_session_state_unchanged() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os(), host_os()] }",
        "prepared-buffered-transactional-merge",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("first leased sibling");
    let first_work = first.work().clone();
    let second = session.prepare_step().expect("second leased sibling");
    let duplicate = session.first_queued_work_for_testing();

    let first_completion = first.execute().expect("first completion");
    let mut second_completion = second.execute().expect("second completion");
    second_completion.inject_duplicate_queued_work(duplicate);

    assert_eq!(
        session
            .apply_completion(second_completion)
            .expect("second completion should buffer"),
        EvalSessionStep::Progressed,
    );

    let before_graph = session.graph_snapshot();
    let before_trace = session.trace().clone();
    let before_completed = session.completed_work().to_vec();
    let before_work = session.work_records();

    let error = session
        .apply_completion(first_completion)
        .expect_err("buffered merge failure should roll back the whole apply");
    assert_eq!(error.kind, EvalErrorKind::Internal);
    assert!(
        error
            .message
            .contains("already exists during completion merge"),
        "unexpected error message: {}",
        error.message
    );
    assert_eq!(session.graph_snapshot(), before_graph);
    assert_eq!(session.trace(), &before_trace);
    assert_eq!(session.completed_work(), before_completed.as_slice());
    assert_eq!(session.work_records(), before_work);
    assert_eq!(session.in_flight_work(), vec![first_work]);
}

#[test]
fn step_back_and_checkpoint_define_in_flight_work_behavior() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "prepared-step-back",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let leased = session.prepare_step().expect("leased sibling");
    let leased_work = leased.work().clone();
    let leased_checkpoint = session.checkpoint();

    assert_eq!(session.queued_work().len(), 1);
    assert_eq!(session.in_flight_work(), vec![leased_work.clone()]);
    assert!(session.can_step_back());
    assert!(session.step_back());
    assert_eq!(session.queued_work().len(), 2);
    assert!(session.in_flight_work().is_empty());

    session.restore(leased_checkpoint);
    assert_eq!(session.queued_work().len(), 1);
    assert_eq!(session.in_flight_work(), vec![leased_work]);
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
