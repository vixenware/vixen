use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use blake3::hash as blake3_hash;
use camino::Utf8Path;
use moire::task::FutureExt;
use roam_core::{BareConduit, acceptor};
use roam_stream::StreamLink;
use roam_types::MessageFamily;
use tempfile::TempDir;
use vx_eval2::{
    Claim, CrossingPolicy, EvalError, EvalErrorKind, ProjectionStep, Provenance, ProvenanceRoot,
    Resolution, ReuseClass, RuntimeCallOutcome, RuntimeHooks, RuntimeInputKind, SlotAction,
    SlotResolutionKind, SlotResolveRequest, SlotResolved, SlotStore, SlotStoreReason,
    SlotVerification, SourcePoint, SourceSpan, Value, ValueRep, envelope_to_value,
    eval_source_entry_at_place_with_args_with_source_id,
    eval_source_entry_with_args_with_source_id, resolution_to_outcome, value_to_envelope,
};
use vx_eval2_proto::{
    EVAL2_PROTOCOL_VERSION, Eval2Daemon, Eval2DaemonClient, Eval2DaemonDispatcher, Eval2Version,
    EvalOutcome, EvalSubgraphRequest, FetchProxyRequest, PlaceRef, RemoteRef, ValueEnvelope,
    VerifyClaimsRequest, VerifyClaimsResult, WireValue,
};
use vx_io::net::{Listener, connect_session, default_local_endpoint, park_session};
use vx_spec::{
    CrossingPattern, EngineExpectation, EngineId, EventPattern, ExternalSpec, HostSpec,
    ObservationPattern, ObservedPoison, ObservedResult, ObservedWhichSlotSpec, PlaceSpec,
    PureSlotSpec, ResolvedSpecRun, ReusePattern, RuntimeSpec, SlotActionSpec, SlotDomainSpec,
    SlotInvokeSpec, SpecCase, SpecOutcome, SpecValue, StructSpec,
};

fn run_fixture(path: &Path) -> datatest_stable::Result<()> {
    let case = SpecCase::from_path(path)?;
    match case.expectation_for(EngineId::Eval2) {
        EngineExpectation::Pass => {
            let runtime = tokio::runtime::Runtime::new()?;
            let invocations = case.invocations();
            let outcomes = runtime.block_on(run_case(&case, &invocations))?;
            for (index, (invocation, outcome)) in
                invocations.iter().zip(outcomes.iter()).enumerate()
            {
                invocation.expect.assert_matches(outcome).map_err(|error| {
                    std::io::Error::other(format!("run #{} expectation failed: {error}", index + 1))
                })?;
            }
            Ok(())
        }
        EngineExpectation::Todo | EngineExpectation::Skip => Ok(()),
    }
}

async fn run_case(
    case: &SpecCase,
    invocations: &[ResolvedSpecRun],
) -> Result<Vec<SpecOutcome>, Box<dyn std::error::Error>> {
    let uses_slots = invocations
        .iter()
        .any(|invocation| invocation.invoke.slot.is_some());
    if uses_slots {
        if invocations
            .iter()
            .any(|invocation| invocation.invoke.slot.is_none())
        {
            return Err(std::io::Error::other(
                "slot fixture cases must declare invoke.slot on every run",
            )
            .into());
        }
        return run_slot_case(case, invocations);
    }

    let uses_rpc = invocations
        .iter()
        .any(|invocation| invocation.invoke.place.is_some());
    if uses_rpc {
        run_case_via_rpc(case, invocations).await
    } else {
        invocations
            .iter()
            .map(|invocation| run_case_local(case, invocation))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FixtureSlotVerifierPayload {
    ObservedWhich { place: String, name: String },
}

fn run_slot_case(
    case: &SpecCase,
    invocations: &[ResolvedSpecRun],
) -> Result<Vec<SpecOutcome>, Box<dyn std::error::Error>> {
    let runtime = FixtureRuntime {
        spec: case.invoke.runtime.clone(),
    };
    let mut slots = SlotStore::<Value, SpecValue, FixtureSlotVerifierPayload>::default();
    invocations
        .iter()
        .map(|invocation| run_slot_invocation(case, &runtime, invocation, &mut slots))
        .collect()
}

fn run_slot_invocation(
    case: &SpecCase,
    runtime: &FixtureRuntime,
    invocation: &ResolvedSpecRun,
    slots: &mut SlotStore<Value, SpecValue, FixtureSlotVerifierPayload>,
) -> Result<SpecOutcome, Box<dyn std::error::Error>> {
    let slot = invocation
        .invoke
        .slot
        .as_ref()
        .ok_or_else(|| std::io::Error::other("slot invocation is missing invoke.slot"))?;
    let slot_action = spec_slot_action_to_eval(&slot.action);
    let witness = slot_witness(slot, invocation)?;
    let slot_id = vx_eval2::SlotId::from(slot.id.clone());
    let previous = slots.get(&slot_id).cloned();
    let resolution = slots
        .resolve(
            slot.id.clone(),
            witness.clone(),
            slot_action,
            |record| verify_slot_record(runtime, invocation, record),
            |request| resolve_slot(case, runtime, invocation, slot, request),
        )
        .map_err(|error| std::io::Error::other(error.message.to_string()))?;

    let mut events = Vec::new();
    if matches!(slot.domain, SlotDomainSpec::ObservedWhich(_))
        && matches!(slot.action, SlotActionSpec::Current)
        && previous
            .as_ref()
            .is_some_and(|record| record.witness == witness)
    {
        match &resolution.kind {
            SlotResolutionKind::Reused => events.push(EventPattern::VerifyClaimsOk),
            SlotResolutionKind::Stored {
                reason: SlotStoreReason::ClaimsInvalid { .. },
            } => events.push(EventPattern::VerifyClaimsFail),
            SlotResolutionKind::Stored { .. } => {}
        }
    }

    match &resolution.kind {
        SlotResolutionKind::Reused => events.push(EventPattern::SlotReuse {
            slot: slot.id.clone(),
        }),
        SlotResolutionKind::Stored {
            reason: SlotStoreReason::Missing,
        } => events.push(EventPattern::SlotPopulate {
            slot: slot.id.clone(),
        }),
        SlotResolutionKind::Stored { .. } => events.push(EventPattern::SlotRewrite {
            slot: slot.id.clone(),
        }),
    }

    spec_outcome_from_value(resolution.record.value, events)
}

fn spec_slot_action_to_eval(action: &SlotActionSpec) -> SlotAction<SpecValue> {
    match action {
        SlotActionSpec::Current => SlotAction::Current,
        SlotActionSpec::Refresh { intent } => SlotAction::Refresh(intent.clone()),
    }
}

fn slot_witness(
    slot: &SlotInvokeSpec,
    invocation: &ResolvedSpecRun,
) -> Result<SpecValue, Box<dyn std::error::Error>> {
    match &slot.domain {
        SlotDomainSpec::Pure(PureSlotSpec { witness, .. }) => Ok(witness.clone()),
        SlotDomainSpec::ObservedWhich(ObservedWhichSlotSpec { name }) => {
            if invocation.invoke.place.is_none() {
                return Err(std::io::Error::other(format!(
                    "observed slot {:?} requires invoke.place",
                    slot.id
                ))
                .into());
            }
            Ok(SpecValue::String(format!("observed_which:{name}")))
        }
    }
}

fn verify_slot_record(
    runtime: &FixtureRuntime,
    invocation: &ResolvedSpecRun,
    record: &vx_eval2::SlotRecord<Value, SpecValue, FixtureSlotVerifierPayload>,
) -> SlotVerification {
    if record.claims.is_empty() {
        return SlotVerification::Valid;
    }

    let Some(place) = invocation.invoke.place.as_deref() else {
        return SlotVerification::invalid("claim-bearing slots require invoke.place");
    };
    let verification = verify_claims_against_runtime(&runtime.spec, place, &record.claims);
    if verification.ok {
        SlotVerification::Valid
    } else {
        SlotVerification::invalid(
            verification
                .error
                .unwrap_or_else(|| "claim verification failed".to_string()),
        )
    }
}

fn resolve_slot(
    case: &SpecCase,
    runtime: &FixtureRuntime,
    invocation: &ResolvedSpecRun,
    slot: &SlotInvokeSpec,
    _request: SlotResolveRequest<'_, Value, SpecValue, SpecValue, FixtureSlotVerifierPayload>,
) -> Result<SlotResolved<Value, FixtureSlotVerifierPayload>, EvalError> {
    match &slot.domain {
        SlotDomainSpec::Pure(PureSlotSpec { value, .. }) => Ok(SlotResolved::new(
            spec_value_to_eval2_value(value.clone()).map_err(|error| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!(
                        "invalid pure slot value in {}: {error}",
                        case.path.display()
                    ),
                )
            })?,
        )),
        SlotDomainSpec::ObservedWhich(ObservedWhichSlotSpec { name }) => {
            let place = invocation.invoke.place.as_deref().ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("observed slot {:?} requires invoke.place", slot.id),
                )
            })?;
            let value = runtime.discover_which(
                place,
                name,
                &SourceSpan::synthetic(format!("slot:{}:{}", slot.id, name)),
            )?;
            let claims = value.meta.claims.clone();
            Ok(SlotResolved::new(value)
                .with_claims(claims)
                .with_verifier_payload(FixtureSlotVerifierPayload::ObservedWhich {
                    place: place.to_string(),
                    name: name.clone(),
                }))
        }
    }
}

fn run_case_local(
    case: &SpecCase,
    invocation: &ResolvedSpecRun,
) -> Result<SpecOutcome, Box<dyn std::error::Error>> {
    let args = invocation
        .invoke
        .args
        .iter()
        .cloned()
        .map(spec_value_to_eval2_value)
        .collect::<Result<Vec<_>, _>>()?;
    match eval_source_entry_with_args_with_source_id(
        &case.main_source,
        case_source_id(case),
        &invocation.invoke.entry,
        &args,
    ) {
        Ok(resolution) => resolution_to_spec_outcome(resolution),
        Err(error) => Ok(SpecOutcome::Poison(ObservedPoison {
            kind: format!("{:?}", error.kind),
            message: error.message.to_string(),
            events: Vec::new(),
        })),
    }
}

#[derive(Clone)]
struct FixtureRuntime {
    spec: RuntimeSpec,
}

impl FixtureRuntime {
    fn place(&self, place: &str) -> Result<&PlaceSpec, EvalError> {
        self.spec.places.get(place).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("place {place:?} was not declared in fixture runtime"),
            )
        })
    }

    fn host(&self, place: &str) -> Result<&HostSpec, EvalError> {
        self.place(place).map(|place| &place.host)
    }

    fn tracked_file_hash(&self, place: &str, path: &str) -> Option<String> {
        self.place(place)
            .ok()
            .and_then(|place| place.tracked_file_contents(path))
            .map(hash_string)
    }

    fn opaque_bundle_file(&self, place: &str, path: &str) -> Option<(String, String, String)> {
        let place = self.place(place).ok()?;
        place.opaque_bundles.iter().find_map(|(name, bundle)| {
            bundle.files.get(path).map(|contents| {
                (
                    name.clone(),
                    opaque_bundle_identity(bundle),
                    contents.to_string(),
                )
            })
        })
    }

    fn bundle_identity(&self, place: &str, bundle_name: &str) -> Result<String, EvalError> {
        let place_spec = self.place(place)?;
        let bundle = place_spec.opaque_bundles.get(bundle_name).ok_or_else(|| {
            EvalError::new(
                EvalErrorKind::Internal,
                format!("place {place:?} does not define opaque bundle {bundle_name:?}"),
            )
        })?;
        Ok(opaque_bundle_identity(bundle))
    }

    fn read_hash(&self, place: &str, path: &str) -> Result<String, EvalError> {
        let host = self.host(place)?;
        host.reads
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.content_hash.clone())
            .or_else(|| self.tracked_file_hash(place, path))
            .or_else(|| {
                self.opaque_bundle_file(place, path)
                    .map(|(_, _, contents)| hash_string(&contents))
            })
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::UserFail,
                    format!(
                        "failed to read host path {path}: fixture runtime has no read entry or file"
                    ),
                )
            })
    }

    fn hashed_path(&self, place: &str, path: &str) -> Result<String, EvalError> {
        let host = self.host(place)?;
        host.hashes
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.content_hash.clone())
            .or_else(|| self.tracked_file_hash(place, path))
            .or_else(|| {
                self.opaque_bundle_file(place, path)
                    .map(|(_, _, contents)| hash_string(&contents))
            })
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::UserFail,
                    format!(
                        "failed to hash host path {path}: fixture runtime has no hash entry or file"
                    ),
                )
            })
    }

    fn discover_which(
        &self,
        place: &str,
        name: &str,
        source: &SourceSpan,
    ) -> Result<Value, EvalError> {
        let host = self.host(place)?;
        let Some(found) = host.which.iter().find(|entry| entry.name == name) else {
            return Err(EvalError::new(
                EvalErrorKind::UserFail,
                format!("host.which could not find {name}"),
            ));
        };

        let mut value = Value::string(found.path.clone())
            .with_provenance(Provenance::host_probe(source.clone(), place, "host_which"))
            .with_reuse(ReuseClass::Observed)
            .with_crossing(CrossingPolicy::ByValue)
            .with_claim(Claim::Which {
                name: name.to_string(),
                expected_path: Some(found.path.clone()),
            });
        if let Some(bundle_name) = &found.bundle {
            value = value.with_claim(Claim::Bundle {
                name: bundle_name.clone(),
                identity: self.bundle_identity(place, bundle_name)?,
            });
        }
        Ok(value)
    }
}

fn hash_string(contents: &str) -> String {
    blake3_hash(contents.as_bytes()).to_hex().to_string()
}

fn opaque_bundle_identity(bundle: &vx_spec::OpaqueBundleSpec) -> String {
    if let Some(identity) = &bundle.identity {
        return identity.clone();
    }

    let mut hasher = blake3::Hasher::new();
    for (path, contents) in &bundle.files {
        hasher.update(path.as_bytes());
        hasher.update(&[0]);
        hasher.update(contents.as_bytes());
        hasher.update(&[0xff]);
    }
    hasher.finalize().to_hex().to_string()
}

fn tracked_place_file_hash(runtime: &RuntimeSpec, place: &str, path: &str) -> Option<String> {
    runtime
        .places
        .get(place)
        .and_then(|place| place.tracked_file_contents(path))
        .map(hash_string)
}

fn runtime_bundle_identity(
    runtime: &RuntimeSpec,
    place: &str,
    bundle_name: &str,
) -> Option<String> {
    runtime
        .places
        .get(place)
        .and_then(|place| place.opaque_bundles.get(bundle_name))
        .map(opaque_bundle_identity)
}

impl RuntimeHooks for FixtureRuntime {
    fn call(
        &self,
        place: &str,
        callee: &str,
        args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        let host = self.host(place)?;
        match callee {
            "host_os" => {
                if !args.is_empty() {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "host_os() does not take any arguments",
                    ));
                }
                Ok(RuntimeCallOutcome::Ready(
                    Value::string(host.os.clone())
                        .with_provenance(Provenance::host_probe(source.clone(), place, "host_os"))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue)
                        .with_claim(Claim::Os {
                            value: host.os.clone(),
                        }),
                ))
            }
            "host_which" => {
                if args.len() != 1 {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "host_which() requires exactly one string argument",
                    ));
                }
                let ValueRep::String(name) = &args[0].rep else {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "host_which() argument must evaluate to String",
                    ));
                };
                let Some(found) = host.which.iter().find(|entry| entry.name == name.as_ref())
                else {
                    return Err(EvalError::new(
                        EvalErrorKind::UserFail,
                        format!("host.which could not find {}", name.as_ref()),
                    ));
                };
                let mut value = Value::string(found.path.clone())
                    .with_provenance(Provenance::host_probe(source.clone(), place, "host_which"))
                    .with_reuse(ReuseClass::Observed)
                    .with_crossing(CrossingPolicy::ByValue)
                    .with_claim(Claim::Which {
                        name: name.to_string(),
                        expected_path: Some(found.path.clone()),
                    });
                if let Some(bundle_name) = &found.bundle {
                    value = value.with_claim(Claim::Bundle {
                        name: bundle_name.clone(),
                        identity: self.bundle_identity(place, bundle_name)?,
                    });
                }
                Ok(RuntimeCallOutcome::Ready(value))
            }
            "host_read" => {
                if args.len() != 1 {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "host_read() requires exactly one string argument",
                    ));
                }
                let ValueRep::String(path) = &args[0].rep else {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "host_read() argument must evaluate to String",
                    ));
                };
                let mut value = Value::external("host_file", path.as_ref())
                    .with_provenance(Provenance::host_probe(source.clone(), place, "host_read"))
                    .with_reuse(ReuseClass::Observed)
                    .with_crossing(CrossingPolicy::ByProxy);
                if let Some(entry) = host.reads.iter().find(|entry| entry.path == path.as_ref()) {
                    if let Some(bundle_name) = &entry.bundle {
                        value = value.with_claim(Claim::Bundle {
                            name: bundle_name.clone(),
                            identity: self.bundle_identity(place, bundle_name)?,
                        });
                    } else {
                        value = value.with_claim(Claim::Read {
                            path: path.to_string(),
                            content_hash: entry.content_hash.clone(),
                        });
                    }
                } else if let Some((bundle_name, identity, _)) =
                    self.opaque_bundle_file(place, path)
                {
                    value = value.with_claim(Claim::Bundle {
                        name: bundle_name,
                        identity,
                    });
                } else {
                    let content_hash = self.read_hash(place, path)?;
                    value = value.with_claim(Claim::Read {
                        path: path.to_string(),
                        content_hash,
                    });
                }
                Ok(RuntimeCallOutcome::Ready(value))
            }
            "host_hash" => {
                if args.len() != 1 {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "host_hash() requires exactly one string argument",
                    ));
                }
                let ValueRep::String(path) = &args[0].rep else {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "host_hash() argument must evaluate to String",
                    ));
                };
                let content_hash = self.hashed_path(place, path)?;
                let mut value = Value::string(content_hash.clone())
                    .with_provenance(Provenance::host_probe(source.clone(), place, "host_hash"))
                    .with_reuse(ReuseClass::Observed)
                    .with_crossing(CrossingPolicy::ByValue);
                if let Some(entry) = host.hashes.iter().find(|entry| entry.path == path.as_ref()) {
                    if let Some(bundle_name) = &entry.bundle {
                        value = value.with_claim(Claim::Bundle {
                            name: bundle_name.clone(),
                            identity: self.bundle_identity(place, bundle_name)?,
                        });
                    } else {
                        value = value.with_claim(Claim::Hash {
                            path: path.to_string(),
                            content_hash,
                        });
                    }
                } else if let Some((bundle_name, identity, _)) =
                    self.opaque_bundle_file(place, path)
                {
                    value = value.with_claim(Claim::Bundle {
                        name: bundle_name,
                        identity,
                    });
                } else {
                    value = value.with_claim(Claim::Hash {
                        path: path.to_string(),
                        content_hash,
                    });
                }
                Ok(RuntimeCallOutcome::Ready(value))
            }
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
    }
}

#[derive(Clone)]
struct FixtureDaemon {
    runtime: FixtureRuntime,
    proxies: ProxyStore,
}

#[derive(Clone, PartialEq, Eq)]
struct CachedInvocation {
    entry: String,
    args: Vec<ValueEnvelope>,
    outcome: EvalOutcome,
}

#[derive(Clone, Default)]
struct ProxyStore {
    next_handle: Arc<AtomicU64>,
    entries: Arc<Mutex<Vec<(RemoteRef, ValueEnvelope)>>>,
}

impl ProxyStore {
    fn insert(&self, owner_place: &str, mut envelope: ValueEnvelope) -> RemoteRef {
        envelope.meta.crossing = CrossingPolicy::ByValue;
        let handle_id = self.next_handle.fetch_add(1, Ordering::Relaxed) + 1;
        let reference = RemoteRef {
            owner_place: owner_place.to_string(),
            handle_id,
        };
        self.entries
            .lock()
            .expect("proxy store lock poisoned")
            .push((reference.clone(), envelope));
        reference
    }

    fn get(&self, reference: &RemoteRef) -> Option<ValueEnvelope> {
        self.entries
            .lock()
            .expect("proxy store lock poisoned")
            .iter()
            .find(|(stored, _)| stored == reference)
            .map(|(_, envelope)| envelope.clone())
    }
}

impl Eval2Daemon for FixtureDaemon {
    async fn version(&self) -> Eval2Version {
        Eval2Version {
            protocol_version: EVAL2_PROTOCOL_VERSION,
        }
    }

    async fn eval_subgraph(&self, request: EvalSubgraphRequest) -> EvalOutcome {
        let args = match request
            .args
            .into_iter()
            .map(envelope_to_value)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(args) => args,
            Err(error) => {
                return EvalOutcome::Poison {
                    kind: "Internal".to_string(),
                    message: format!("invalid eval2 subgraph args: {error:?}"),
                };
            }
        };
        match eval_source_entry_at_place_with_args_with_source_id(
            &request.source,
            &request.source_id,
            &request.entry,
            &args,
            &self.runtime,
            &request.place.id,
        ) {
            Ok(resolution) => proxify_outcome(
                &self.proxies,
                &request.place.id,
                resolution_to_outcome(resolution).expect("serialize eval2 result"),
            ),
            Err(error) => EvalOutcome::Poison {
                kind: format!("{:?}", error.kind),
                message: error.message.to_string(),
            },
        }
    }

    async fn verify_claims(&self, request: VerifyClaimsRequest) -> VerifyClaimsResult {
        verify_claims_against_runtime(&self.runtime.spec, &request.place.id, &request.claims)
    }

    async fn fetch_proxy(&self, request: FetchProxyRequest) -> EvalOutcome {
        match self.proxies.get(&request.reference) {
            Some(envelope) => EvalOutcome::Ready(envelope),
            None => EvalOutcome::Poison {
                kind: "Internal".to_string(),
                message: format!(
                    "proxy {}:{} was not found",
                    request.reference.owner_place, request.reference.handle_id
                ),
            },
        }
    }
}

async fn run_case_via_rpc(
    case: &SpecCase,
    invocations: &[ResolvedSpecRun],
) -> Result<Vec<SpecOutcome>, Box<dyn std::error::Error>> {
    for invocation in invocations {
        let Some(place) = invocation.invoke.place.as_deref() else {
            return Err(std::io::Error::other(
                "eval2 RPC fixture runs require invoke.place on every run",
            )
            .into());
        };
        if !case.invoke.runtime.places.contains_key(place) {
            return Err(std::io::Error::other(format!(
                "invoke.place {place:?} was not declared under invoke.runtime.places"
            ))
            .into());
        }
    }

    let runtime = FixtureRuntime {
        spec: case.invoke.runtime.clone(),
    };
    let proxies = ProxyStore::default();

    let root = TempDir::new()?;
    let root = Utf8Path::from_path(root.path()).ok_or_else(|| {
        std::io::Error::other("temporary directory for eval2 fixture daemon is not valid UTF-8")
    })?;
    let endpoint = default_local_endpoint(root, "vx-eval2-fixture-daemon");
    let mut listener = Listener::bind(&endpoint).await?;

    let accept_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept local connection");
        moire::task::spawn(
            async move {
                let (reader, writer) = tokio::io::split(stream);
                let conduit = BareConduit::<MessageFamily, _>::new(StreamLink::new(reader, writer));
                let dispatcher = Eval2DaemonDispatcher::new(FixtureDaemon { runtime, proxies });
                let (caller, session_handle) = acceptor(conduit)
                    .establish::<roam_core::NoopCaller>(dispatcher)
                    .await
                    .expect("roam session establish");
                park_session(caller, session_handle).await;
            }
            .named("vx_eval2_fixture_connection"),
        );
    });

    let (caller, session_handle) = connect_session(&endpoint).await?;
    let client = Eval2DaemonClient::new(caller.clone());
    let mut cached = Vec::new();
    let mut outcomes = Vec::with_capacity(invocations.len());
    for invocation in invocations {
        outcomes.push(run_rpc_invocation(&client, case, invocation, &mut cached).await?);
    }

    drop(client);
    drop(caller);
    drop(session_handle);
    accept_task.abort();

    Ok(outcomes)
}

async fn eval_subgraph(
    client: &Eval2DaemonClient,
    case: &SpecCase,
    invocation: &ResolvedSpecRun,
    args: &[ValueEnvelope],
) -> Result<EvalOutcome, Box<dyn std::error::Error>> {
    let place = invocation
        .invoke
        .place
        .as_deref()
        .ok_or_else(|| std::io::Error::other("eval2 RPC invocation is missing invoke.place"))?;
    eval_subgraph_at_place(client, case, &invocation.invoke.entry, place, args).await
}

async fn eval_subgraph_at_place(
    client: &Eval2DaemonClient,
    case: &SpecCase,
    entry: &str,
    place: &str,
    args: &[ValueEnvelope],
) -> Result<EvalOutcome, Box<dyn std::error::Error>> {
    client
        .eval_subgraph(EvalSubgraphRequest {
            place: PlaceRef {
                id: place.to_string(),
            },
            source_id: case_source_id(case),
            source: case.main_source.clone(),
            entry: entry.to_string(),
            args: args.to_vec(),
        })
        .await
        .map_err(|error| {
            std::io::Error::other(format!("rpc eval_subgraph failed: {error:?}")).into()
        })
}

async fn run_rpc_invocation(
    client: &Eval2DaemonClient,
    case: &SpecCase,
    invocation: &ResolvedSpecRun,
    cached: &mut Vec<CachedInvocation>,
) -> Result<SpecOutcome, Box<dyn std::error::Error>> {
    let mut events = Vec::new();
    let place = invocation
        .invoke
        .place
        .as_deref()
        .ok_or_else(|| std::io::Error::other("eval2 RPC invocation is missing invoke.place"))?;
    let args = invocation
        .invoke
        .args
        .iter()
        .cloned()
        .map(spec_value_to_eval2_value)
        .collect::<Result<Vec<_>, _>>()?;
    let arg_envelopes = args
        .iter()
        .map(value_to_envelope)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| std::io::Error::other(format!("invalid invocation args: {error:?}")))?;
    let outcome = if cached_outcome(cached, &invocation.invoke.entry, &arg_envelopes).is_none()
        && let Some(replay_place) = invocation.invoke.replay_place.as_deref()
    {
        let initial = eval_subgraph(client, case, invocation, &arg_envelopes).await?;
        events.push(EventPattern::StabilizeMiss);
        let claims = match &initial {
            EvalOutcome::Ready(result) => result.meta.claims.clone(),
            EvalOutcome::Poison { .. } => Vec::new(),
        };
        if claims.is_empty() {
            events.push(EventPattern::StabilizeHit);
            if matches!(initial, EvalOutcome::Ready(_)) {
                cache_outcome(
                    cached,
                    &invocation.invoke.entry,
                    &arg_envelopes,
                    initial.clone(),
                );
            }
            initial
        } else {
            let verification = client
                .verify_claims(VerifyClaimsRequest {
                    place: PlaceRef {
                        id: replay_place.to_string(),
                    },
                    claims,
                })
                .await
                .map_err(|error| {
                    std::io::Error::other(format!("rpc verify_claims failed: {error:?}"))
                })?;
            if verification.ok {
                events.push(EventPattern::VerifyClaimsOk);
                events.push(EventPattern::StabilizeHit);
                cache_outcome(
                    cached,
                    &invocation.invoke.entry,
                    &arg_envelopes,
                    initial.clone(),
                );
                initial
            } else {
                events.push(EventPattern::VerifyClaimsFail);
                let fresh = eval_subgraph_at_place(
                    client,
                    case,
                    &invocation.invoke.entry,
                    replay_place,
                    &arg_envelopes,
                )
                .await?;
                events.push(EventPattern::StabilizeMiss);
                if matches!(fresh, EvalOutcome::Ready(_)) {
                    cache_outcome(
                        cached,
                        &invocation.invoke.entry,
                        &arg_envelopes,
                        fresh.clone(),
                    );
                }
                fresh
            }
        }
    } else if let Some(previous) = cached_outcome(cached, &invocation.invoke.entry, &arg_envelopes)
    {
        let claims = match &previous {
            EvalOutcome::Ready(result) => result.meta.claims.clone(),
            EvalOutcome::Poison { .. } => Vec::new(),
        };
        if claims.is_empty() {
            events.push(EventPattern::StabilizeHit);
            previous
        } else {
            let verification = client
                .verify_claims(VerifyClaimsRequest {
                    place: PlaceRef {
                        id: place.to_string(),
                    },
                    claims,
                })
                .await
                .map_err(|error| {
                    std::io::Error::other(format!("rpc verify_claims failed: {error:?}"))
                })?;
            if verification.ok {
                events.push(EventPattern::VerifyClaimsOk);
                events.push(EventPattern::StabilizeHit);
                previous
            } else {
                events.push(EventPattern::VerifyClaimsFail);
                let fresh = eval_subgraph(client, case, invocation, &arg_envelopes).await?;
                events.push(EventPattern::StabilizeMiss);
                if matches!(fresh, EvalOutcome::Ready(_)) {
                    cache_outcome(
                        cached,
                        &invocation.invoke.entry,
                        &arg_envelopes,
                        fresh.clone(),
                    );
                }
                fresh
            }
        }
    } else {
        let fresh = eval_subgraph(client, case, invocation, &arg_envelopes).await?;
        events.push(EventPattern::StabilizeMiss);
        if matches!(fresh, EvalOutcome::Ready(_)) {
            cache_outcome(
                cached,
                &invocation.invoke.entry,
                &arg_envelopes,
                fresh.clone(),
            );
        }
        fresh
    };

    if let Some(verify_place) = &invocation.invoke.verify_place {
        let claims = match &outcome {
            EvalOutcome::Ready(result) => result.meta.claims.clone(),
            EvalOutcome::Poison { .. } => Vec::new(),
        };
        if !claims.is_empty() {
            let verification = client
                .verify_claims(VerifyClaimsRequest {
                    place: PlaceRef {
                        id: verify_place.clone(),
                    },
                    claims,
                })
                .await
                .map_err(|error| {
                    std::io::Error::other(format!("rpc verify_claims failed: {error:?}"))
                })?;
            if !verification.ok {
                events.push(EventPattern::VerifyClaimsFail);
                return Ok(SpecOutcome::Poison(ObservedPoison {
                    kind: "VerificationFailure".to_string(),
                    message: verification
                        .error
                        .unwrap_or_else(|| "claim verification failed".to_string()),
                    events,
                }));
            }
            events.push(EventPattern::VerifyClaimsOk);
        }
    }

    eval_outcome_to_spec(client, outcome, events).await
}

fn cached_outcome(
    cached: &[CachedInvocation],
    entry: &str,
    args: &[ValueEnvelope],
) -> Option<EvalOutcome> {
    cached
        .iter()
        .find(|invocation| invocation.entry == entry && invocation.args == args)
        .map(|invocation| invocation.outcome.clone())
}

fn cache_outcome(
    cached: &mut Vec<CachedInvocation>,
    entry: &str,
    args: &[ValueEnvelope],
    outcome: EvalOutcome,
) {
    if let Some(existing) = cached
        .iter_mut()
        .find(|invocation| invocation.entry == entry && invocation.args == args)
    {
        existing.outcome = outcome;
        return;
    }

    cached.push(CachedInvocation {
        entry: entry.to_string(),
        args: args.to_vec(),
        outcome,
    });
}

fn spec_value_to_eval2_value(value: SpecValue) -> Result<Value, std::io::Error> {
    match value {
        SpecValue::Unit(()) => Ok(Value::unit()),
        SpecValue::Bool(value) => Ok(Value::bool(value)),
        SpecValue::U64(value) => Ok(Value::u64(value)),
        SpecValue::I64(value) => Err(std::io::Error::other(format!(
            "eval2 fixture args do not support i64 values yet: {value}"
        ))),
        SpecValue::F64(value) => Err(std::io::Error::other(format!(
            "eval2 fixture args do not support f64 values yet: {value}"
        ))),
        SpecValue::String(value) => Ok(Value::string(value)),
        SpecValue::Array(items) => Ok(Value::array(
            items
                .into_iter()
                .map(spec_value_to_eval2_value)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        SpecValue::Map(entries) => Ok(Value::map(
            entries
                .into_iter()
                .map(|(key, value)| Ok((Value::string(key), spec_value_to_eval2_value(value)?)))
                .collect::<Result<Vec<_>, std::io::Error>>()?,
        )),
        SpecValue::Struct(struct_spec) => Ok(Value::structure(
            struct_spec.r#type,
            struct_spec
                .fields
                .into_iter()
                .map(|(name, value)| Ok((name.into(), spec_value_to_eval2_value(value)?)))
                .collect::<Result<Vec<_>, std::io::Error>>()?,
        )),
        SpecValue::External(external) => Ok(Value::external(external.kind, external.descriptor)),
    }
}

fn verify_claims_against_runtime(
    runtime: &RuntimeSpec,
    place: &str,
    claims: &[Claim],
) -> VerifyClaimsResult {
    let Some(host) = runtime.places.get(place).map(|place| &place.host) else {
        return VerifyClaimsResult {
            ok: false,
            error: Some(format!(
                "place {place:?} was not declared in fixture runtime"
            )),
        };
    };

    for claim in claims {
        match claim {
            Claim::Os { value } if &host.os != value => {
                return VerifyClaimsResult {
                    ok: false,
                    error: Some(format!(
                        "host.os changed: expected {value}, got {}",
                        host.os
                    )),
                };
            }
            Claim::Arch { value } if &host.arch != value => {
                return VerifyClaimsResult {
                    ok: false,
                    error: Some(format!(
                        "host.arch changed: expected {value}, got {}",
                        host.arch
                    )),
                };
            }
            Claim::Which {
                name,
                expected_path,
            } => {
                let actual = host
                    .which
                    .iter()
                    .find(|entry| entry.name == *name)
                    .map(|entry| entry.path.as_str());
                match (expected_path.as_deref(), actual) {
                    (Some(expected), Some(actual)) if expected == actual => {}
                    (Some(expected), Some(actual)) => {
                        return VerifyClaimsResult {
                            ok: false,
                            error: Some(format!(
                                "host.which({name:?}) changed: expected {expected}, got {actual}"
                            )),
                        };
                    }
                    (Some(expected), None) => {
                        return VerifyClaimsResult {
                            ok: false,
                            error: Some(format!(
                                "host.which({name:?}) changed: expected {expected}, but it is now missing"
                            )),
                        };
                    }
                    (None, None) => {}
                    (None, Some(actual)) => {
                        return VerifyClaimsResult {
                            ok: false,
                            error: Some(format!(
                                "host.which({name:?}) changed: expected missing, got {actual}"
                            )),
                        };
                    }
                }
            }
            Claim::Bundle { name, identity } => {
                let Some(actual) = runtime_bundle_identity(runtime, place, name) else {
                    return VerifyClaimsResult {
                        ok: false,
                        error: Some(format!(
                            "opaque bundle {name:?} is no longer available at place {place:?}"
                        )),
                    };
                };
                if &actual != identity {
                    return VerifyClaimsResult {
                        ok: false,
                        error: Some(format!(
                            "opaque bundle {name:?} changed: expected {identity}, got {actual}"
                        )),
                    };
                }
            }
            Claim::Read { path, content_hash } => {
                let actual = host
                    .reads
                    .iter()
                    .find(|entry| entry.path == *path)
                    .map(|entry| entry.content_hash.as_str())
                    .map(ToOwned::to_owned)
                    .or_else(|| tracked_place_file_hash(runtime, place, path));
                let Some(actual) = actual else {
                    return VerifyClaimsResult {
                        ok: false,
                        error: Some(format!(
                            "host.read verification failed for {path}: missing fixture read entry or file"
                        )),
                    };
                };
                if actual != *content_hash {
                    return VerifyClaimsResult {
                        ok: false,
                        error: Some(format!(
                            "host.read(\"{path}\") changed: expected {content_hash}, got {actual}"
                        )),
                    };
                }
            }
            Claim::Hash { path, content_hash } => {
                let actual = host
                    .hashes
                    .iter()
                    .find(|entry| entry.path == *path)
                    .map(|entry| entry.content_hash.as_str())
                    .map(ToOwned::to_owned)
                    .or_else(|| tracked_place_file_hash(runtime, place, path));
                let Some(actual) = actual else {
                    return VerifyClaimsResult {
                        ok: false,
                        error: Some(format!(
                            "host.hash verification failed for {path}: missing fixture hash entry or file"
                        )),
                    };
                };
                if actual != *content_hash {
                    return VerifyClaimsResult {
                        ok: false,
                        error: Some(format!(
                            "host.hash(\"{path}\") changed: expected {content_hash}, got {actual}"
                        )),
                    };
                }
            }
            _ => {}
        }
    }

    VerifyClaimsResult {
        ok: true,
        error: None,
    }
}

fn resolution_to_spec_outcome(
    resolution: Resolution,
) -> Result<SpecOutcome, Box<dyn std::error::Error>> {
    match resolution {
        Resolution::Ready(value) => spec_outcome_from_value(value, Vec::new()),
        Resolution::Pending(pending) => Err(std::io::Error::other(format!(
            "eval2 returned pending at the external boundary: {pending:?}"
        ))
        .into()),
        Resolution::Waiting => {
            Err(std::io::Error::other("eval2 returned waiting at the external boundary").into())
        }
        Resolution::Poison(error) => Ok(SpecOutcome::Poison(ObservedPoison {
            kind: format!("{:?}", error.kind),
            message: error.message.to_string(),
            events: Vec::new(),
        })),
    }
}

async fn eval_outcome_to_spec(
    client: &Eval2DaemonClient,
    outcome: EvalOutcome,
    events: Vec<EventPattern>,
) -> Result<SpecOutcome, Box<dyn std::error::Error>> {
    match outcome {
        EvalOutcome::Ready(result) => {
            let result = materialize_proxy_envelope(client, result).await?;
            spec_outcome_from_value(
                envelope_to_value(result).map_err(|error| {
                    std::io::Error::other(format!("invalid daemon result: {error:?}"))
                })?,
                events,
            )
        }
        EvalOutcome::Poison { kind, message } => Ok(SpecOutcome::Poison(ObservedPoison {
            kind,
            message,
            events,
        })),
    }
}

fn proxify_outcome(proxies: &ProxyStore, owner_place: &str, outcome: EvalOutcome) -> EvalOutcome {
    match outcome {
        EvalOutcome::Ready(result) => {
            EvalOutcome::Ready(proxify_envelope(proxies, owner_place, result))
        }
        EvalOutcome::Poison { .. } => outcome,
    }
}

fn proxify_envelope(
    proxies: &ProxyStore,
    owner_place: &str,
    mut envelope: ValueEnvelope,
) -> ValueEnvelope {
    envelope.value = match envelope.value {
        WireValue::Array(items) => WireValue::Array(
            items
                .into_iter()
                .map(|item| proxify_envelope(proxies, owner_place, item))
                .collect(),
        ),
        WireValue::Map(entries) => WireValue::Map(
            entries
                .into_iter()
                .map(|(key, value)| {
                    (
                        proxify_envelope(proxies, owner_place, key),
                        proxify_envelope(proxies, owner_place, value),
                    )
                })
                .collect(),
        ),
        WireValue::Struct(mut value) => {
            value.fields = value
                .fields
                .into_iter()
                .map(|(name, envelope)| (name, proxify_envelope(proxies, owner_place, envelope)))
                .collect();
            WireValue::Struct(value)
        }
        value => value,
    };

    if envelope.meta.crossing == CrossingPolicy::ByProxy {
        let reference = proxies.insert(owner_place, envelope.clone());
        envelope.value = WireValue::Proxy(reference);
    }

    envelope
}

fn materialize_proxy_envelope<'a>(
    client: &'a Eval2DaemonClient,
    envelope: ValueEnvelope,
) -> Pin<Box<dyn Future<Output = Result<ValueEnvelope, Box<dyn std::error::Error>>> + 'a>> {
    Box::pin(async move {
        let envelope = if envelope.meta.crossing == CrossingPolicy::ByProxy {
            let original_crossing = envelope.meta.crossing.clone();
            let WireValue::Proxy(reference) = envelope.value else {
                return Err(std::io::Error::other(
                    "by-proxy result crossed the RPC boundary without a proxy reference",
                )
                .into());
            };

            let fetched = client
                .fetch_proxy(FetchProxyRequest {
                    requester_place: PlaceRef {
                        id: "local".to_string(),
                    },
                    reference,
                })
                .await
                .map_err(|error| {
                    std::io::Error::other(format!("rpc fetch_proxy failed: {error:?}"))
                })?;

            match fetched {
                EvalOutcome::Ready(mut envelope) => {
                    envelope.meta.crossing = original_crossing;
                    envelope
                }
                EvalOutcome::Poison { kind, message } => {
                    return Err(std::io::Error::other(format!(
                        "proxy fetch returned poison {kind}: {message}"
                    ))
                    .into());
                }
            }
        } else {
            envelope
        };

        let value = match envelope.value {
            WireValue::Array(items) => {
                let mut materialized = Vec::with_capacity(items.len());
                for item in items {
                    materialized.push(materialize_proxy_envelope(client, item).await?);
                }
                WireValue::Array(materialized)
            }
            WireValue::Map(entries) => {
                let mut materialized = Vec::with_capacity(entries.len());
                for (key, value) in entries {
                    materialized.push((
                        materialize_proxy_envelope(client, key).await?,
                        materialize_proxy_envelope(client, value).await?,
                    ));
                }
                WireValue::Map(materialized)
            }
            WireValue::Struct(mut value) => {
                let mut fields = Vec::with_capacity(value.fields.len());
                for (name, envelope) in value.fields {
                    fields.push((name, materialize_proxy_envelope(client, envelope).await?));
                }
                value.fields = fields;
                WireValue::Struct(value)
            }
            value => value,
        };

        Ok(ValueEnvelope {
            value,
            meta: envelope.meta,
        })
    })
}

fn spec_outcome_from_value(
    value: Value,
    events: Vec<EventPattern>,
) -> Result<SpecOutcome, Box<dyn std::error::Error>> {
    Ok(SpecOutcome::Result(ObservedResult {
        value: eval2_value_to_spec(&value)?,
        provenance: Some(eval2_provenance_to_pattern(value.meta.provenance.clone())),
        obligations: value
            .meta
            .claims
            .iter()
            .cloned()
            .map(eval2_claim_to_pattern)
            .collect(),
        reuse: Some(eval2_reuse_to_pattern(value.meta.reuse.clone())),
        crossing: Some(eval2_crossing_to_pattern(value.meta.crossing.clone())),
        events,
    }))
}

fn eval2_reuse_to_pattern(reuse: ReuseClass) -> ReusePattern {
    match reuse {
        ReuseClass::Stable => ReusePattern::Stable,
        ReuseClass::Observed => ReusePattern::Observed,
        ReuseClass::Live => ReusePattern::Live,
    }
}

fn eval2_crossing_to_pattern(crossing: CrossingPolicy) -> CrossingPattern {
    match crossing {
        CrossingPolicy::ByValue => CrossingPattern::ByValue,
        CrossingPolicy::ByProxy => CrossingPattern::ByProxy,
        CrossingPolicy::Blocked => CrossingPattern::Blocked,
    }
}

fn eval2_value_to_spec(value: &Value) -> Result<SpecValue, std::io::Error> {
    match &value.rep {
        ValueRep::Unit => Ok(SpecValue::Unit(())),
        ValueRep::Bool(value) => Ok(SpecValue::Bool(*value)),
        ValueRep::U64(value) => Ok(SpecValue::U64(*value)),
        ValueRep::String(value) => Ok(SpecValue::String(value.to_string())),
        ValueRep::Array(items) => Ok(SpecValue::Array(
            items
                .iter()
                .map(eval2_value_to_spec)
                .collect::<Result<_, std::io::Error>>()?,
        )),
        ValueRep::Map(map) => Ok(SpecValue::Map(
            map.entries
                .iter()
                .map(|(key, value)| {
                    let ValueRep::String(key) = &key.rep else {
                        return Err(std::io::Error::other(
                            "only string-keyed maps can be converted to spec values",
                        ));
                    };
                    Ok((key.to_string(), eval2_value_to_spec(value)?))
                })
                .collect::<Result<_, std::io::Error>>()?,
        )),
        ValueRep::Struct(struct_value) => Ok(SpecValue::Struct(StructSpec {
            r#type: struct_value.type_name.to_string(),
            fields: struct_value
                .fields
                .iter()
                .map(|(name, value)| Ok((name.to_string(), eval2_value_to_spec(value)?)))
                .collect::<Result<_, std::io::Error>>()?,
        })),
        ValueRep::External(external) => Ok(SpecValue::External(ExternalSpec {
            kind: external.kind.to_string(),
            descriptor: external.descriptor.to_string(),
        })),
        other => Err(std::io::Error::other(format!(
            "eval2 result value is not supported yet: {other:?}"
        ))),
    }
}

fn eval2_provenance_to_pattern(provenance: vx_eval2::Provenance) -> vx_spec::ProvenancePattern {
    let source = Some(eval2_source_span_to_pattern(provenance.source.clone()));
    let path = provenance
        .path
        .iter()
        .cloned()
        .map(eval2_projection_to_pattern)
        .collect();
    match provenance.root {
        ProvenanceRoot::Unknown => vx_spec::ProvenancePattern::Unknown { source, path },
        ProvenanceRoot::Literal => vx_spec::ProvenancePattern::Literal { source, path },
        ProvenanceRoot::Binding { name } => vx_spec::ProvenancePattern::Binding {
            name: name.to_string(),
            source,
            path,
        },
        ProvenanceRoot::RuntimeInput { kind, label } => vx_spec::ProvenancePattern::RuntimeInput {
            kind: eval2_runtime_input_to_pattern(kind),
            label: label.to_string(),
            source,
            path,
        },
        ProvenanceRoot::HostProbe { place, description } => vx_spec::ProvenancePattern::HostProbe {
            host: place.to_string(),
            description: description.to_string(),
            source,
            path,
        },
    }
}

fn eval2_source_span_to_pattern(span: SourceSpan) -> vx_spec::SourceSpanPattern {
    vx_spec::SourceSpanPattern {
        source_id: span.source_id,
        start: eval2_source_point_to_pattern(span.start),
        end: eval2_source_point_to_pattern(span.end),
    }
}

fn eval2_source_point_to_pattern(point: SourcePoint) -> vx_spec::SourcePointPattern {
    vx_spec::SourcePointPattern {
        byte: point.byte.0,
        line: point.line.0,
        column: point.column.0,
    }
}

fn eval2_runtime_input_to_pattern(kind: RuntimeInputKind) -> vx_spec::RuntimeInputPattern {
    match kind {
        RuntimeInputKind::Argument => vx_spec::RuntimeInputPattern::Argument,
        RuntimeInputKind::AmbientEnv => vx_spec::RuntimeInputPattern::AmbientEnv,
        RuntimeInputKind::Import => vx_spec::RuntimeInputPattern::Import,
        RuntimeInputKind::LocalPath => vx_spec::RuntimeInputPattern::LocalPath,
        RuntimeInputKind::DiscoverResult => vx_spec::RuntimeInputPattern::DiscoverResult,
        RuntimeInputKind::ExecResult => vx_spec::RuntimeInputPattern::ExecResult,
    }
}

fn eval2_projection_to_pattern(step: ProjectionStep) -> vx_spec::ProjectionPattern {
    match step {
        ProjectionStep::Field(name) => vx_spec::ProjectionPattern::Field(name.to_string()),
        ProjectionStep::Index(index) => vx_spec::ProjectionPattern::Index(index as usize),
        ProjectionStep::MapKey(key) => vx_spec::ProjectionPattern::MapKey(key.to_string()),
        ProjectionStep::Path(path) => vx_spec::ProjectionPattern::Path(path.to_string()),
    }
}

fn eval2_claim_to_pattern(claim: Claim) -> ObservationPattern {
    match claim {
        Claim::Os { value } => ObservationPattern::Os {
            value: value.to_string(),
        },
        Claim::Arch { value } => ObservationPattern::Arch {
            value: value.to_string(),
        },
        Claim::Which {
            name,
            expected_path,
        } => ObservationPattern::Which {
            name: name.to_string(),
            expected_path: expected_path.map(|path| path.to_string()),
        },
        Claim::Bundle { name, identity } => ObservationPattern::Bundle {
            name: name.to_string(),
            identity: identity.to_string(),
        },
        Claim::Read { path, content_hash } => ObservationPattern::Read {
            path: path.to_string(),
            content_hash: content_hash.to_string(),
        },
        Claim::Hash { path, content_hash } => ObservationPattern::Hash {
            path: path.to_string(),
            content_hash: content_hash.to_string(),
        },
    }
}

fn case_source_id(case: &SpecCase) -> String {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("vx-eval2 crate should live under the workspace root");
    let absolute = case
        .path
        .canonicalize()
        .unwrap_or_else(|_| case.path.clone());
    let relative = absolute.strip_prefix(workspace_root).unwrap_or(&absolute);
    format!("{}#main", relative.display())
}

datatest_stable::harness! {
    { test = run_fixture, root = "../../specs/eval", pattern = r"(^|.*/)[^/]+\.styx$" },
}
