use std::path::Path;

use camino::Utf8Path;
use moire::task::FutureExt;
use roam_core::{BareConduit, acceptor};
use roam_stream::StreamLink;
use roam_types::MessageFamily;
use tempfile::TempDir;
use vx_eval2::{
    Claim, CrossingPolicy, EvalError, EvalErrorKind, ProjectionStep, Provenance, ProvenanceRoot,
    Resolution, ReuseClass, RuntimeHooks, RuntimeInputKind, SourcePoint, SourceSpan, Value,
    ValueRep, envelope_to_value, eval_source_entry_at_place_with_source_id,
    eval_source_entry_with_source_id, resolution_to_outcome,
};
use vx_eval2_proto::{
    EVAL2_PROTOCOL_VERSION, Eval2Daemon, Eval2DaemonClient, Eval2DaemonDispatcher, Eval2Version,
    EvalOutcome, EvalSubgraphRequest, FetchProxyRequest, PlaceRef, VerifyClaimsRequest,
    VerifyClaimsResult,
};
use vx_io::net::{Listener, connect_session, default_local_endpoint, park_session};
use vx_spec::{
    EngineExpectation, EngineId, ExternalSpec, HostSpec, ObservationPattern, ObservedPoison,
    ObservedResult, RuntimeSpec, SpecCase, SpecOutcome, SpecValue, StructSpec,
};

fn run_fixture(path: &Path) -> datatest_stable::Result<()> {
    let case = SpecCase::from_path(path)?;
    match case.expectation_for(EngineId::Eval2) {
        EngineExpectation::Pass => {
            let runtime = tokio::runtime::Runtime::new()?;
            let outcome = runtime.block_on(run_case(&case))?;
            case.assert_matches(&outcome)?;
            Ok(())
        }
        EngineExpectation::Todo | EngineExpectation::Skip => Ok(()),
    }
}

async fn run_case(case: &SpecCase) -> Result<SpecOutcome, Box<dyn std::error::Error>> {
    match case.invoke.place.as_deref() {
        Some(place) => run_case_via_rpc(case, place).await,
        None => run_case_local(case),
    }
}

fn run_case_local(case: &SpecCase) -> Result<SpecOutcome, Box<dyn std::error::Error>> {
    match eval_source_entry_with_source_id(
        &case.main_source,
        case_source_id(case),
        &case.invoke.entry,
    ) {
        Ok(resolution) => resolution_to_spec_outcome(resolution),
        Err(error) => Ok(SpecOutcome::Poison(ObservedPoison {
            kind: format!("{:?}", error.kind),
            message: error.message.to_string(),
        })),
    }
}

#[derive(Clone)]
struct FixtureRuntime {
    spec: RuntimeSpec,
}

impl FixtureRuntime {
    fn host(&self, place: &str) -> Result<&HostSpec, EvalError> {
        self.spec
            .places
            .get(place)
            .map(|place| &place.host)
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::Internal,
                    format!("place {place:?} was not declared in fixture runtime"),
                )
            })
    }

    fn read_hash(&self, place: &str, path: &str) -> Result<String, EvalError> {
        let host = self.host(place)?;
        host.reads
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.content_hash.clone())
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::UserFail,
                    format!("failed to read host path {path}: fixture runtime has no read entry"),
                )
            })
    }

    fn hashed_path(&self, place: &str, path: &str) -> Result<String, EvalError> {
        let host = self.host(place)?;
        host.hashes
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.content_hash.clone())
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::UserFail,
                    format!("failed to hash host path {path}: fixture runtime has no hash entry"),
                )
            })
    }
}

impl RuntimeHooks for FixtureRuntime {
    fn call(
        &self,
        place: &str,
        callee: &str,
        args: &[Value],
        source: &SourceSpan,
    ) -> Result<Option<Value>, EvalError> {
        let host = self.host(place)?;
        match callee {
            "host_os" => {
                if !args.is_empty() {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "host_os() does not take any arguments",
                    ));
                }
                Ok(Some(
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
                Ok(Some(
                    Value::string(found.path.clone())
                        .with_provenance(Provenance::host_probe(
                            source.clone(),
                            place,
                            "host_which",
                        ))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue)
                        .with_claim(Claim::Which {
                            name: name.to_string(),
                            expected_path: Some(found.path.clone()),
                        }),
                ))
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
                let content_hash = self.read_hash(place, path)?;
                Ok(Some(
                    Value::external("host_file", path.as_ref())
                        .with_provenance(Provenance::host_probe(source.clone(), place, "host_read"))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByProxy)
                        .with_claim(Claim::Read {
                            path: path.to_string(),
                            content_hash,
                        }),
                ))
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
                Ok(Some(
                    Value::string(content_hash.clone())
                        .with_provenance(Provenance::host_probe(source.clone(), place, "host_hash"))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue)
                        .with_claim(Claim::Hash {
                            path: path.to_string(),
                            content_hash,
                        }),
                ))
            }
            _ => Ok(None),
        }
    }
}

#[derive(Clone)]
struct FixtureDaemon {
    runtime: FixtureRuntime,
}

impl Eval2Daemon for FixtureDaemon {
    async fn version(&self) -> Eval2Version {
        Eval2Version {
            protocol_version: EVAL2_PROTOCOL_VERSION,
        }
    }

    async fn eval_subgraph(&self, request: EvalSubgraphRequest) -> EvalOutcome {
        match eval_source_entry_at_place_with_source_id(
            &request.source,
            &request.source_id,
            &request.entry,
            &self.runtime,
            &request.place.id,
        ) {
            Ok(resolution) => resolution_to_outcome(resolution).expect("serialize eval2 result"),
            Err(error) => EvalOutcome::Poison {
                kind: format!("{:?}", error.kind),
                message: error.message.to_string(),
            },
        }
    }

    async fn verify_claims(&self, request: VerifyClaimsRequest) -> VerifyClaimsResult {
        verify_claims_against_runtime(&self.runtime.spec, &request.place.id, &request.claims)
    }

    async fn fetch_proxy(&self, _request: FetchProxyRequest) -> EvalOutcome {
        panic!("proxy fetching is not implemented in the eval2 fixture daemon")
    }
}

async fn run_case_via_rpc(
    case: &SpecCase,
    place: &str,
) -> Result<SpecOutcome, Box<dyn std::error::Error>> {
    if !case.invoke.runtime.places.contains_key(place) {
        return Err(std::io::Error::other(format!(
            "invoke.place {place:?} was not declared under invoke.runtime.places"
        ))
        .into());
    }
    if !case.invoke.args.is_empty() {
        return Err(std::io::Error::other(
            "eval2 RPC fixture runner does not support invoke.args yet",
        )
        .into());
    }

    let runtime = FixtureRuntime {
        spec: case.invoke.runtime.clone(),
    };

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
                let dispatcher = Eval2DaemonDispatcher::new(FixtureDaemon { runtime });
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

    let outcome = client
        .eval_subgraph(EvalSubgraphRequest {
            place: PlaceRef {
                id: place.to_string(),
            },
            source_id: case_source_id(case),
            source: case.main_source.clone(),
            entry: case.invoke.entry.clone(),
            args: Vec::new(),
        })
        .await
        .map_err(|error| std::io::Error::other(format!("rpc eval_subgraph failed: {error:?}")))?;

    if let Some(verify_place) = &case.invoke.verify_place {
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
                return Ok(SpecOutcome::Poison(ObservedPoison {
                    kind: "VerificationFailure".to_string(),
                    message: verification
                        .error
                        .unwrap_or_else(|| "claim verification failed".to_string()),
                }));
            }
        }
    }

    drop(client);
    drop(caller);
    drop(session_handle);
    accept_task.abort();

    eval_outcome_to_spec(outcome)
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
            Claim::Read { path, content_hash } => {
                let actual = host
                    .reads
                    .iter()
                    .find(|entry| entry.path == *path)
                    .map(|entry| entry.content_hash.as_str())
                    .ok_or_else(|| VerifyClaimsResult {
                        ok: false,
                        error: Some(format!(
                            "host.read verification failed for {path}: missing fixture read entry"
                        )),
                    });
                let Ok(actual) = actual else {
                    return actual.unwrap_err();
                };
                if actual != content_hash {
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
                    .ok_or_else(|| VerifyClaimsResult {
                        ok: false,
                        error: Some(format!(
                            "host.hash verification failed for {path}: missing fixture hash entry"
                        )),
                    });
                let Ok(actual) = actual else {
                    return actual.unwrap_err();
                };
                if actual != content_hash {
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
        Resolution::Ready(value) => spec_outcome_from_value(value),
        Resolution::Pending(pending) => Err(std::io::Error::other(format!(
            "eval2 returned pending at the external boundary: {pending:?}"
        ))
        .into()),
        Resolution::Poison(error) => Ok(SpecOutcome::Poison(ObservedPoison {
            kind: format!("{:?}", error.kind),
            message: error.message.to_string(),
        })),
    }
}

fn eval_outcome_to_spec(outcome: EvalOutcome) -> Result<SpecOutcome, Box<dyn std::error::Error>> {
    match outcome {
        EvalOutcome::Ready(result) => {
            spec_outcome_from_value(envelope_to_value(result).map_err(|error| {
                std::io::Error::other(format!("invalid daemon result: {error:?}"))
            })?)
        }
        EvalOutcome::Poison { kind, message } => {
            Ok(SpecOutcome::Poison(ObservedPoison { kind, message }))
        }
    }
}

fn spec_outcome_from_value(value: Value) -> Result<SpecOutcome, Box<dyn std::error::Error>> {
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
    }))
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
