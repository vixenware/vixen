use std::collections::{BTreeMap, BTreeSet, VecDeque};
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
    Claim, CrossingPolicy, EvalError, EvalErrorKind, EvalEventKind, EvalProgram, EvalProgramImport,
    EvalProgramSource, EvalSession, EvalTrace, ProjectionStep, Provenance, ProvenanceRoot,
    Resolution, ReuseClass, RuntimeCallOutcome, RuntimeHooks, RuntimeInputKind, RuntimeReady,
    RuntimeSlotPrepare, SlotAction, SlotDecision, SlotEventReason, SlotResolved, SlotStore,
    SlotStoreReason, SlotVerification, SourcePoint, SourceSpan, Value, ValueRep, envelope_to_value,
    eval_source_entry_at_place_with_args_with_source_id, resolution_to_outcome, value_to_envelope,
};
use vx_eval2_proto::{
    EVAL2_PROTOCOL_VERSION, Eval2Daemon, Eval2DaemonClient, Eval2DaemonDispatcher, Eval2Version,
    EvalOutcome, EvalSubgraphRequest, FetchProxyRequest, PlaceRef, RemoteRef, ValueEnvelope,
    VerifyClaimsRequest, VerifyClaimsResult, WireValue,
};
use vx_io::net::{Listener, connect_session, default_local_endpoint, park_session};
use vx_spec::{
    CrossingPattern, EngineExpectation, EngineId, EventPattern, ExternalSpec, HostSpec,
    ObservationPattern, ObservedPoison, ObservedResult, PlaceSpec, ResolvedSpecRun, ReusePattern,
    RuntimeSpec, SlotActionSpec, SlotDomainSpec, SlotEventReasonPattern, SpecCase, SpecOutcome,
    SpecValue, StructSpec, ToyResolverSlotSpec,
};

fn run_fixture(path: &Path) -> datatest_stable::Result<()> {
    let case = SpecCase::from_path(path)?;
    match case.expectation_for(EngineId::Eval2) {
        EngineExpectation::Pass => {
            let runtime = tokio::runtime::Runtime::new()?;
            let invocations = case.invocations()?;
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
    let uses_rpc = invocations
        .iter()
        .any(|invocation| invocation.invoke.place.is_some());
    if uses_rpc {
        run_case_via_rpc(case, invocations).await
    } else {
        let slots = Arc::new(Mutex::new(FixtureSlotStore::default()));
        invocations
            .iter()
            .map(|invocation| {
                let runtime = FixtureRuntime::with_slots(
                    invocation.invoke.runtime.clone(),
                    Arc::clone(&slots),
                );
                run_case_local(case, invocation, &runtime)
            })
            .collect()
    }
}

fn run_case_local(
    case: &SpecCase,
    invocation: &ResolvedSpecRun,
    runtime: &FixtureRuntime,
) -> Result<SpecOutcome, Box<dyn std::error::Error>> {
    let args = invocation
        .invoke
        .args
        .iter()
        .cloned()
        .map(spec_value_to_eval2_value)
        .collect::<Result<Vec<_>, _>>()?;
    let entry = case_entry_name(invocation);
    let place = invocation.invoke.place.as_deref().unwrap_or("local");

    let program = case_program(case, invocation)?;
    let mut session = EvalSession::new_program(program, entry, args, runtime, place);
    match session.run() {
        Ok(resolution) => resolution_to_spec_outcome_with_trace(resolution, session.trace()),
        Err(error) => Ok(SpecOutcome::Poison(ObservedPoison {
            kind: format!("{:?}", error.kind),
            message: error.message.to_string(),
            events: Vec::new(),
        })),
    }
}

fn case_source(
    case: &SpecCase,
    invocation: &ResolvedSpecRun,
) -> Result<String, Box<dyn std::error::Error>> {
    match &invocation.invoke.slot {
        Some(slot) => synthesize_slot_source(slot),
        None => Ok(case.main_source.clone()),
    }
}

fn case_program(
    case: &SpecCase,
    invocation: &ResolvedSpecRun,
) -> Result<EvalProgram, Box<dyn std::error::Error>> {
    let main_source = case_source(case, invocation)?;
    if invocation.invoke.slot.is_some() || case.files.is_empty() {
        return Ok(EvalProgram::single_source(
            case_source_id(case),
            main_source,
        ));
    }

    let mut sources = vec![EvalProgramSource::new(case_source_id(case), main_source)];
    let mut imports = Vec::new();
    for (index, (path, source_text)) in case.files.iter().enumerate() {
        sources.push(EvalProgramSource::new(
            format!("{}#{path}", case.path.display()),
            source_text.clone(),
        ));
        imports.push(EvalProgramImport {
            from: 0,
            to: index + 1,
        });
    }
    EvalProgram::new(0, sources, imports)
        .map_err(|error| std::io::Error::other(error.message.to_string()).into())
}

fn case_entry_name(invocation: &ResolvedSpecRun) -> &str {
    if invocation.invoke.slot.is_some() {
        "build"
    } else {
        &invocation.invoke.entry
    }
}

fn resolution_to_spec_outcome_with_trace(
    resolution: Resolution,
    trace: &EvalTrace,
) -> Result<SpecOutcome, Box<dyn std::error::Error>> {
    let events = trace_events_to_spec_events(trace)?;
    match resolution {
        Resolution::Ready(value) => spec_outcome_from_value(value, events),
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
            events,
        })),
    }
}

fn trace_events_to_spec_events(
    trace: &EvalTrace,
) -> Result<Vec<EventPattern>, Box<dyn std::error::Error>> {
    let mut events = Vec::new();
    for event in &trace.events {
        match &event.kind {
            EvalEventKind::SlotPopulate { slot, reason } => {
                events.push(EventPattern::SlotPopulate {
                    slot: slot.to_string(),
                    reason: slot_event_reason_to_pattern(reason)?,
                })
            }
            EvalEventKind::SlotReuse { slot } => events.push(EventPattern::SlotReuse {
                slot: slot.to_string(),
            }),
            EvalEventKind::SlotRewrite { slot, reason } => events.push(EventPattern::SlotRewrite {
                slot: slot.to_string(),
                reason: slot_event_reason_to_pattern(reason)?,
            }),
            _ => {}
        }
    }
    Ok(events)
}

fn slot_event_reason_to_pattern(
    reason: &SlotEventReason,
) -> Result<SlotEventReasonPattern, Box<dyn std::error::Error>> {
    Ok(match reason {
        SlotEventReason::Missing => SlotEventReasonPattern::Missing,
        SlotEventReason::WitnessChanged => SlotEventReasonPattern::WitnessChanged,
        SlotEventReason::ClaimsInvalid { reason } => SlotEventReasonPattern::ClaimsInvalid {
            reason_contains: Some(reason.to_string()),
        },
        SlotEventReason::Refresh { intent } => SlotEventReasonPattern::Refresh {
            intent: eval2_value_to_spec(intent)?,
        },
    })
}

fn synthesize_slot_source(
    slot: &vx_spec::SlotInvokeSpec,
) -> Result<String, Box<dyn std::error::Error>> {
    let body = match (&slot.domain, &slot.action) {
        (SlotDomainSpec::Pure(spec), SlotActionSpec::Current) => format!(
            "slot_current_pure({}, {}, {})",
            string_literal(&slot.id),
            spec_value_to_source_expr(&spec.witness)?,
            spec_value_to_source_expr(&spec.value)?,
        ),
        (SlotDomainSpec::Pure(spec), SlotActionSpec::Refresh { intent }) => format!(
            "slot_refresh_pure({}, {}, {}, {})",
            string_literal(&slot.id),
            spec_value_to_source_expr(&spec.witness)?,
            spec_value_to_source_expr(&spec.value)?,
            spec_value_to_source_expr(intent)?,
        ),
        (SlotDomainSpec::ObservedWhich(spec), SlotActionSpec::Current) => format!(
            "slot_current_observed_which({}, {})",
            string_literal(&slot.id),
            string_literal(&spec.name),
        ),
        (SlotDomainSpec::ObservedWhich(spec), SlotActionSpec::Refresh { intent }) => format!(
            "slot_refresh_observed_which({}, {}, {})",
            string_literal(&slot.id),
            string_literal(&spec.name),
            spec_value_to_source_expr(intent)?,
        ),
        (SlotDomainSpec::ToyResolver(spec), SlotActionSpec::Current) => format!(
            "slot_current_toy_resolver({}, {}, {})",
            string_literal(&slot.id),
            spec_value_to_source_expr(&spec.witness)?,
            string_string_map_to_source_expr(&spec.requirements),
        ),
        (SlotDomainSpec::ToyResolver(spec), SlotActionSpec::Refresh { intent }) => {
            synthesize_toy_resolver_refresh_source(&slot.id, spec, intent)?
        }
    };
    Ok(format!("fn build() {{\n  {body}\n}}\n"))
}

fn synthesize_toy_resolver_refresh_source(
    slot_id: &str,
    spec: &ToyResolverSlotSpec,
    intent: &SpecValue,
) -> Result<String, Box<dyn std::error::Error>> {
    match toy_resolver_intent_spec(intent)? {
        ToyResolverIntentSpec::UpdateAll => Ok(format!(
            "slot_refresh_toy_resolver_update_all({}, {}, {})",
            string_literal(slot_id),
            spec_value_to_source_expr(&spec.witness)?,
            string_string_map_to_source_expr(&spec.requirements),
        )),
        ToyResolverIntentSpec::UpdatePackage { name } => Ok(format!(
            "slot_refresh_toy_resolver_update_package({}, {}, {}, {})",
            string_literal(slot_id),
            spec_value_to_source_expr(&spec.witness)?,
            string_string_map_to_source_expr(&spec.requirements),
            string_literal(&name),
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ToyResolverIntentSpec {
    UpdateAll,
    UpdatePackage { name: String },
}

fn toy_resolver_intent_spec(
    intent: &SpecValue,
) -> Result<ToyResolverIntentSpec, Box<dyn std::error::Error>> {
    let SpecValue::Struct(struct_spec) = intent else {
        return Err(
            std::io::Error::other("toy resolver refresh intent must be a struct value").into(),
        );
    };
    match struct_spec.r#type.as_str() {
        "UpdateAll" => Ok(ToyResolverIntentSpec::UpdateAll),
        "UpdatePackage" => {
            let Some(SpecValue::String(name)) = struct_spec.fields.get("name") else {
                return Err(std::io::Error::other(
                    "UpdatePackage intent requires a string field `name`",
                )
                .into());
            };
            Ok(ToyResolverIntentSpec::UpdatePackage { name: name.clone() })
        }
        other => Err(std::io::Error::other(format!(
            "unsupported toy resolver refresh intent type {other:?}"
        ))
        .into()),
    }
}

fn string_string_map_to_source_expr(entries: &BTreeMap<String, String>) -> String {
    format!(
        "%{{ {} }}",
        entries
            .iter()
            .map(|(key, value)| format!("{} => {}", string_literal(key), string_literal(value)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn spec_value_to_source_expr(value: &SpecValue) -> Result<String, Box<dyn std::error::Error>> {
    match value {
        SpecValue::Unit(()) => Ok("()".to_string()),
        SpecValue::Bool(value) => Ok(value.to_string()),
        SpecValue::U64(value) => Ok(value.to_string()),
        SpecValue::String(value) => Ok(string_literal(value)),
        SpecValue::Array(items) => Ok(format!(
            "[{}]",
            items
                .iter()
                .map(spec_value_to_source_expr)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        )),
        SpecValue::Map(entries) => Ok(format!(
            "%{{ {} }}",
            entries
                .iter()
                .map(|(key, value)| Ok(format!(
                    "{} => {}",
                    string_literal(key),
                    spec_value_to_source_expr(value)?
                )))
                .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?
                .join(", ")
        )),
        other => Err(std::io::Error::other(format!(
            "slot source synthesis does not support spec value {other:?}"
        ))
        .into()),
    }
}

fn string_literal(value: &str) -> String {
    format!("{value:?}")
}

#[derive(Clone)]
struct FixtureRuntime {
    spec: RuntimeSpec,
    slots: Arc<Mutex<FixtureSlotStore>>,
}

type FixtureSlotStore = SlotStore<Value, Value, (), Value>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ToyResolverIntent {
    UpdateAll,
    UpdatePackage { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ToyVersionKey(Vec<u64>);

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToyResolvedPackage {
    version: String,
    constraints: BTreeSet<String>,
    unlocked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToyConstraintPropagation {
    constraints: BTreeMap<String, BTreeSet<String>>,
    unlocked: BTreeMap<String, bool>,
}

impl FixtureRuntime {
    fn with_slots(spec: RuntimeSpec, slots: Arc<Mutex<FixtureSlotStore>>) -> Self {
        Self { spec, slots }
    }

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

    fn slot_current_toy_resolver(
        &self,
        slot_id: &str,
        witness: Value,
        requirements: BTreeMap<String, String>,
    ) -> Result<RuntimeReady, EvalError> {
        self.resolve_slot(
            "local",
            slot_id,
            normalized_slot_witness(witness),
            SlotAction::Current,
            |current| {
                let graph =
                    self.resolve_toy_graph(&requirements, current, ToyResolverIntent::UpdateAll)?;
                Ok(SlotResolved::new(toy_graph_to_value(&graph)))
            },
        )
    }

    fn slot_refresh_toy_resolver(
        &self,
        slot_id: &str,
        witness: Value,
        requirements: BTreeMap<String, String>,
        intent: ToyResolverIntent,
    ) -> Result<RuntimeReady, EvalError> {
        self.resolve_slot(
            "local",
            slot_id,
            normalized_slot_witness(witness),
            SlotAction::Refresh(toy_resolver_intent_to_value(&intent)),
            |current| {
                let graph = self.resolve_toy_graph(&requirements, current, intent)?;
                Ok(SlotResolved::new(toy_graph_to_value(&graph)))
            },
        )
    }

    fn resolve_toy_graph(
        &self,
        requirements: &BTreeMap<String, String>,
        current: Option<&vx_eval2::SlotRecord<Value, Value, (), Value>>,
        intent: ToyResolverIntent,
    ) -> Result<BTreeMap<String, String>, EvalError> {
        let current_graph = current
            .map(|current| toy_graph_from_value(&current.value))
            .transpose()?;
        let current_graph = current_graph.unwrap_or_default();
        let root_unlocks = requirements
            .keys()
            .map(|package| {
                (
                    package.clone(),
                    match &intent {
                        ToyResolverIntent::UpdateAll => true,
                        ToyResolverIntent::UpdatePackage { name } => name == package,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let root_constraints = requirements
            .iter()
            .map(|(package, constraint)| {
                let mut set = BTreeSet::new();
                set.insert(constraint.clone());
                (package.clone(), set)
            })
            .collect::<BTreeMap<_, _>>();
        let mut selected =
            self.select_toy_packages(&root_constraints, &current_graph, &root_unlocks)?;

        loop {
            let propagated =
                self.propagate_toy_constraints(requirements, &selected, &root_unlocks)?;
            let next = self.select_toy_packages(
                &propagated.constraints,
                &current_graph,
                &propagated.unlocked,
            )?;
            if next == selected {
                return Ok(next
                    .into_iter()
                    .map(|(package, resolved)| (package, resolved.version))
                    .collect());
            }
            selected = next;
        }
    }

    fn select_toy_packages(
        &self,
        constraints: &BTreeMap<String, BTreeSet<String>>,
        current_graph: &BTreeMap<String, String>,
        unlocked: &BTreeMap<String, bool>,
    ) -> Result<BTreeMap<String, ToyResolvedPackage>, EvalError> {
        let mut selected = BTreeMap::new();
        for (package, package_constraints) in constraints {
            let is_unlocked = unlocked.get(package).copied().unwrap_or(false);
            let version = if is_unlocked {
                self.latest_matching_version(package, package_constraints)?
            } else if let Some(current) = current_graph
                .get(package)
                .filter(|version| self.version_satisfies_all(version, package_constraints))
            {
                current.clone()
            } else {
                self.latest_matching_version(package, package_constraints)?
            };
            selected.insert(
                package.clone(),
                ToyResolvedPackage {
                    version,
                    constraints: package_constraints.clone(),
                    unlocked: is_unlocked,
                },
            );
        }
        Ok(selected)
    }

    fn propagate_toy_constraints(
        &self,
        requirements: &BTreeMap<String, String>,
        selected: &BTreeMap<String, ToyResolvedPackage>,
        root_unlocks: &BTreeMap<String, bool>,
    ) -> Result<ToyConstraintPropagation, EvalError> {
        let mut constraints = requirements
            .iter()
            .map(|(package, constraint)| {
                let mut set = BTreeSet::new();
                set.insert(constraint.clone());
                (package.clone(), set)
            })
            .collect::<BTreeMap<_, _>>();
        let mut unlocked = root_unlocks.clone();
        let mut queue = requirements.keys().cloned().collect::<VecDeque<_>>();

        while let Some(package) = queue.pop_front() {
            let Some(resolved) = selected.get(&package) else {
                continue;
            };
            for (dep_name, dep_constraint) in self.package_deps(&package, &resolved.version)? {
                let mut changed = false;
                changed |= constraints
                    .entry(dep_name.clone())
                    .or_default()
                    .insert(dep_constraint.clone());
                let dep_unlocked = unlocked.entry(dep_name.clone()).or_insert(false);
                if resolved.unlocked && !*dep_unlocked {
                    *dep_unlocked = true;
                    changed = true;
                }
                if changed {
                    queue.push_back(dep_name.clone());
                }
            }
        }

        Ok(ToyConstraintPropagation {
            constraints,
            unlocked,
        })
    }

    fn latest_matching_version(
        &self,
        package: &str,
        constraints: &BTreeSet<String>,
    ) -> Result<String, EvalError> {
        let package_spec = self
            .spec
            .package_registry
            .packages
            .get(package)
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::UserFail,
                    format!("toy registry does not contain package {package:?}"),
                )
            })?;
        package_spec
            .versions
            .iter()
            .filter(|version| self.version_satisfies_all(&version.version, constraints))
            .max_by_key(|version| toy_version_key(&version.version))
            .map(|version| version.version.clone())
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::UserFail,
                    format!(
                        "toy registry has no version of {package:?} satisfying constraints {constraints:?}"
                    ),
                )
            })
    }

    fn package_deps(
        &self,
        package: &str,
        version: &str,
    ) -> Result<&BTreeMap<String, String>, EvalError> {
        let package_spec = self
            .spec
            .package_registry
            .packages
            .get(package)
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::UserFail,
                    format!("toy registry does not contain package {package:?}"),
                )
            })?;
        package_spec
            .versions
            .iter()
            .find(|candidate| candidate.version == version)
            .map(|candidate| &candidate.deps)
            .ok_or_else(|| {
                EvalError::new(
                    EvalErrorKind::UserFail,
                    format!("toy registry does not contain {package:?} version {version:?}"),
                )
            })
    }

    fn version_satisfies(&self, version: &str, constraint: &str) -> bool {
        if constraint.contains('.') {
            return version == constraint;
        }
        version
            .split('.')
            .next()
            .is_some_and(|major| major == constraint)
    }

    fn version_satisfies_all(&self, version: &str, constraints: &BTreeSet<String>) -> bool {
        constraints
            .iter()
            .all(|constraint| self.version_satisfies(version, constraint))
    }

    fn slot_current_pure(
        &self,
        slot_id: &str,
        witness: Value,
        value: Value,
    ) -> Result<RuntimeReady, EvalError> {
        self.resolve_slot(
            "local",
            slot_id,
            normalized_slot_witness(witness),
            SlotAction::Current,
            |_| Ok(SlotResolved::new(value)),
        )
    }

    fn slot_refresh_pure(
        &self,
        slot_id: &str,
        witness: Value,
        value: Value,
        intent: Value,
    ) -> Result<RuntimeReady, EvalError> {
        self.resolve_slot(
            "local",
            slot_id,
            normalized_slot_witness(witness),
            SlotAction::Refresh(intent),
            |_| Ok(SlotResolved::new(value)),
        )
    }

    fn slot_current_observed_which(
        &self,
        place: &str,
        slot_id: &str,
        name: &str,
        source: &SourceSpan,
    ) -> Result<RuntimeReady, EvalError> {
        self.resolve_slot(
            place,
            slot_id,
            normalized_slot_witness(Value::string(format!("observed_which:{name}"))),
            SlotAction::Current,
            |_| {
                let value = self.discover_which(place, name, source)?;
                let claims = value.meta.claims.clone();
                Ok(SlotResolved::new(value).with_claims(claims))
            },
        )
    }

    fn slot_refresh_observed_which(
        &self,
        place: &str,
        slot_id: &str,
        name: &str,
        intent: Value,
        source: &SourceSpan,
    ) -> Result<RuntimeReady, EvalError> {
        self.resolve_slot(
            place,
            slot_id,
            normalized_slot_witness(Value::string(format!("observed_which:{name}"))),
            SlotAction::Refresh(intent),
            |_| {
                let value = self.discover_which(place, name, source)?;
                let claims = value.meta.claims.clone();
                Ok(SlotResolved::new(value).with_claims(claims))
            },
        )
    }

    fn resolve_slot(
        &self,
        place: &str,
        slot_id: &str,
        witness: Value,
        action: SlotAction<Value>,
        resolve: impl FnOnce(
            Option<&vx_eval2::SlotRecord<Value, Value, (), Value>>,
        ) -> Result<SlotResolved<Value, ()>, EvalError>,
    ) -> Result<RuntimeReady, EvalError> {
        let mut slots = self.slots.lock().expect("slot store lock poisoned");
        let resolution = slots.resolve(
            slot_id.to_string(),
            witness,
            action,
            |record| self.verify_slot_record(place, record),
            |request| resolve(request.current),
        )?;
        Ok(RuntimeReady {
            value: resolution.record.value.clone(),
            events: vec![slot_resolution_event(slot_id, &resolution.kind)],
        })
    }

    fn verify_slot_record(
        &self,
        place: &str,
        record: &vx_eval2::SlotRecord<Value, Value, (), Value>,
    ) -> SlotVerification {
        if record.claims.is_empty() {
            return SlotVerification::Valid;
        }

        let verification = verify_claims_against_runtime(&self.spec, place, &record.claims);
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
}

fn hash_string(contents: &str) -> String {
    blake3_hash(contents.as_bytes()).to_hex().to_string()
}

fn runtime_fingerprint(runtime: &RuntimeSpec) -> String {
    hash_string(&format!("{runtime:?}"))
}

fn normalized_slot_witness(value: Value) -> Value {
    match value.rep {
        ValueRep::Unit => Value::unit(),
        ValueRep::Bool(value) => Value::bool(value),
        ValueRep::U64(value) => Value::u64(value),
        ValueRep::String(value) => Value::string(value),
        ValueRep::Array(items) => Value::array(
            items
                .into_iter()
                .cloned()
                .map(normalized_slot_witness)
                .collect::<Vec<_>>(),
        ),
        ValueRep::Map(map) => Value::map(
            map.entries
                .into_iter()
                .map(|(key, value)| {
                    (
                        normalized_slot_witness(key.clone()),
                        normalized_slot_witness(value.clone()),
                    )
                })
                .collect::<Vec<_>>(),
        ),
        ValueRep::Struct(struct_value) => Value::structure(
            struct_value.type_name,
            struct_value
                .fields
                .into_iter()
                .map(|(name, value)| (name.clone(), normalized_slot_witness(value.clone())))
                .collect::<Vec<_>>(),
        ),
        ValueRep::External(external) => Value::external(external.kind, external.descriptor),
        ValueRep::Function(function) => Value::new(ValueRep::Function(function)),
    }
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

fn slot_resolution_event(
    slot_id: &str,
    kind: &vx_eval2::SlotResolutionKind<Value>,
) -> EvalEventKind {
    match kind {
        vx_eval2::SlotResolutionKind::Reused => EvalEventKind::SlotReuse {
            slot: Arc::from(slot_id),
        },
        vx_eval2::SlotResolutionKind::Stored {
            reason: SlotStoreReason::Missing,
        } => EvalEventKind::SlotPopulate {
            slot: Arc::from(slot_id),
            reason: SlotEventReason::Missing,
        },
        vx_eval2::SlotResolutionKind::Stored { reason } => EvalEventKind::SlotRewrite {
            slot: Arc::from(slot_id),
            reason: slot_store_reason_to_event_reason(reason),
        },
    }
}

fn slot_store_reason_to_event_reason(reason: &SlotStoreReason<Value>) -> SlotEventReason {
    match reason {
        SlotStoreReason::Missing => SlotEventReason::Missing,
        SlotStoreReason::WitnessChanged => SlotEventReason::WitnessChanged,
        SlotStoreReason::ClaimsInvalid { reason } => SlotEventReason::ClaimsInvalid {
            reason: reason.clone(),
        },
        SlotStoreReason::Refresh { intent } => SlotEventReason::Refresh {
            intent: intent.clone(),
        },
    }
}

fn expect_string_arg<'a>(callee: &str, value: &'a Value) -> Result<&'a Arc<str>, EvalError> {
    let ValueRep::String(value) = &value.rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{callee}() string arguments must evaluate to String"),
        ));
    };
    Ok(value)
}

fn expect_string_map_arg(
    callee: &str,
    value: &Value,
) -> Result<std::collections::BTreeMap<String, String>, EvalError> {
    let ValueRep::Map(map) = &value.rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{callee}() map arguments must evaluate to Map"),
        ));
    };
    map.entries
        .iter()
        .map(|(key, value)| {
            let ValueRep::String(key) = &key.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    format!("{callee}() map keys must evaluate to String"),
                ));
            };
            let ValueRep::String(value) = &value.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    format!("{callee}() map values must evaluate to String"),
                ));
            };
            Ok((key.to_string(), value.to_string()))
        })
        .collect()
}

fn toy_resolver_intent_to_value(intent: &ToyResolverIntent) -> Value {
    match intent {
        ToyResolverIntent::UpdateAll => Value::structure("UpdateAll", Vec::new()),
        ToyResolverIntent::UpdatePackage { name } => Value::structure(
            "UpdatePackage",
            vec![("name".into(), Value::string(name.clone()))],
        ),
    }
}

fn toy_graph_to_value(graph: &BTreeMap<String, String>) -> Value {
    Value::map(
        graph
            .iter()
            .map(|(package, version)| {
                (
                    Value::string(package.clone()),
                    Value::string(version.clone()),
                )
            })
            .collect::<Vec<_>>(),
    )
}

fn toy_graph_from_value(value: &Value) -> Result<BTreeMap<String, String>, EvalError> {
    expect_string_map_arg("toy resolver current graph", value)
}

fn toy_version_key(version: &str) -> ToyVersionKey {
    ToyVersionKey(
        version
            .split('.')
            .map(|segment| segment.parse::<u64>().unwrap_or(0))
            .collect(),
    )
}

impl RuntimeHooks for FixtureRuntime {
    fn call(
        &self,
        place: &str,
        callee: &str,
        args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match callee {
            "slot_current_pure" => {
                if args.len() != 3 {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "slot_current_pure() requires slot id, witness, and value",
                    ));
                }
                let slot_id = expect_string_arg(callee, &args[0])?;
                Ok(RuntimeCallOutcome::ReadyWithEvents(
                    self.slot_current_pure(slot_id.as_ref(), args[1].clone(), args[2].clone())?,
                ))
            }
            "slot_refresh_pure" => {
                if args.len() != 4 {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "slot_refresh_pure() requires slot id, witness, value, and intent",
                    ));
                }
                let slot_id = expect_string_arg(callee, &args[0])?;
                Ok(RuntimeCallOutcome::ReadyWithEvents(
                    self.slot_refresh_pure(
                        slot_id.as_ref(),
                        args[1].clone(),
                        args[2].clone(),
                        args[3].clone(),
                    )?,
                ))
            }
            "slot_current_observed_which" => {
                if args.len() != 2 {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "slot_current_observed_which() requires slot id and tool name",
                    ));
                }
                let slot_id = expect_string_arg(callee, &args[0])?;
                let name = expect_string_arg(callee, &args[1])?;
                Ok(RuntimeCallOutcome::ReadyWithEvents(
                    self.slot_current_observed_which(
                        place,
                        slot_id.as_ref(),
                        name.as_ref(),
                        source,
                    )?,
                ))
            }
            "slot_refresh_observed_which" => {
                if args.len() != 3 {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "slot_refresh_observed_which() requires slot id, tool name, and intent",
                    ));
                }
                let slot_id = expect_string_arg(callee, &args[0])?;
                let name = expect_string_arg(callee, &args[1])?;
                Ok(RuntimeCallOutcome::ReadyWithEvents(
                    self.slot_refresh_observed_which(
                        place,
                        slot_id.as_ref(),
                        name.as_ref(),
                        args[2].clone(),
                        source,
                    )?,
                ))
            }
            "slot_current_toy_resolver" => {
                if args.len() != 3 {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "slot_current_toy_resolver() requires slot id, witness, and requirements",
                    ));
                }
                let slot_id = expect_string_arg(callee, &args[0])?;
                let requirements = expect_string_map_arg(callee, &args[2])?;
                Ok(RuntimeCallOutcome::ReadyWithEvents(
                    self.slot_current_toy_resolver(
                        slot_id.as_ref(),
                        args[1].clone(),
                        requirements,
                    )?,
                ))
            }
            "slot_refresh_toy_resolver_update_all" => {
                if args.len() != 3 {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "slot_refresh_toy_resolver_update_all() requires slot id, witness, and requirements",
                    ));
                }
                let slot_id = expect_string_arg(callee, &args[0])?;
                let requirements = expect_string_map_arg(callee, &args[2])?;
                Ok(RuntimeCallOutcome::ReadyWithEvents(
                    self.slot_refresh_toy_resolver(
                        slot_id.as_ref(),
                        args[1].clone(),
                        requirements,
                        ToyResolverIntent::UpdateAll,
                    )?,
                ))
            }
            "slot_refresh_toy_resolver_update_package" => {
                if args.len() != 4 {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "slot_refresh_toy_resolver_update_package() requires slot id, witness, requirements, and package name",
                    ));
                }
                let slot_id = expect_string_arg(callee, &args[0])?;
                let requirements = expect_string_map_arg(callee, &args[2])?;
                let package = expect_string_arg(callee, &args[3])?;
                Ok(RuntimeCallOutcome::ReadyWithEvents(
                    self.slot_refresh_toy_resolver(
                        slot_id.as_ref(),
                        args[1].clone(),
                        requirements,
                        ToyResolverIntent::UpdatePackage {
                            name: package.to_string(),
                        },
                    )?,
                ))
            }
            "host_os" => {
                let host = self.host(place)?;
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
                let host = self.host(place)?;
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
                let host = self.host(place)?;
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
                let host = self.host(place)?;
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

    fn prepare_slot(
        &self,
        place: &str,
        slot_id: &str,
        witness: &Value,
        action: &SlotAction<Value>,
    ) -> Result<RuntimeSlotPrepare, EvalError> {
        let slots = self.slots.lock().expect("slot store lock poisoned");
        Ok(
            match slots.decide(
                slot_id.to_string(),
                &normalized_slot_witness(witness.clone()),
                action,
                |record| self.verify_slot_record(place, record),
            ) {
                SlotDecision::Reused(record) => RuntimeSlotPrepare::Reused(RuntimeReady {
                    value: record.value,
                    events: vec![slot_resolution_event(
                        slot_id,
                        &vx_eval2::SlotResolutionKind::Reused,
                    )],
                }),
                SlotDecision::Store { current, .. } => RuntimeSlotPrepare::Resolve {
                    current: current.map(|record| record.value),
                },
            },
        )
    }

    fn store_slot(
        &self,
        place: &str,
        slot_id: &str,
        witness: &Value,
        action: &SlotAction<Value>,
        resolved: &Value,
    ) -> Result<RuntimeReady, EvalError> {
        let mut slots = self.slots.lock().expect("slot store lock poisoned");
        let resolution = slots.resolve(
            slot_id.to_string(),
            normalized_slot_witness(witness.clone()),
            action.clone(),
            |record| self.verify_slot_record(place, record),
            |_| Ok(SlotResolved::new(resolved.clone()).with_claims(resolved.meta.claims.clone())),
        )?;
        Ok(RuntimeReady {
            value: resolution.record.value.clone(),
            events: vec![slot_resolution_event(slot_id, &resolution.kind)],
        })
    }
}

#[derive(Clone)]
struct FixtureDaemon {
    runtime: FixtureRuntime,
    proxies: ProxyStore,
}

#[derive(Clone, PartialEq, Eq)]
struct CachedInvocation {
    place: String,
    program: InvocationProgram,
    runtime_fingerprint: String,
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
        if !invocation.invoke.runtime.places.contains_key(place) {
            return Err(std::io::Error::other(format!(
                "invoke.place {place:?} was not declared under invoke.runtime.places"
            ))
            .into());
        }
    }

    let slots = Arc::new(Mutex::new(FixtureSlotStore::default()));
    let proxies = ProxyStore::default();
    let mut cached = Vec::new();
    let mut outcomes = Vec::with_capacity(invocations.len());
    for invocation in invocations {
        let runtime =
            FixtureRuntime::with_slots(invocation.invoke.runtime.clone(), Arc::clone(&slots));
        outcomes.push(
            run_rpc_invocation_at_runtime(case, invocation, runtime, proxies.clone(), &mut cached)
                .await?,
        );
    }

    Ok(outcomes)
}

async fn run_rpc_invocation_at_runtime(
    case: &SpecCase,
    invocation: &ResolvedSpecRun,
    runtime: FixtureRuntime,
    proxies: ProxyStore,
    cached: &mut Vec<CachedInvocation>,
) -> Result<SpecOutcome, Box<dyn std::error::Error>> {
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
    let outcome = run_rpc_invocation(&client, case, invocation, cached).await;

    drop(client);
    drop(caller);
    drop(session_handle);
    accept_task.abort();

    outcome
}

#[derive(Clone, PartialEq, Eq)]
struct InvocationProgram {
    source_id: String,
    source: String,
    entry: String,
}

fn program_uses_stateful_slots(program: &InvocationProgram) -> bool {
    program.source.contains("slot_current(") || program.source.contains("slot_refresh(")
}

fn invocation_program(
    case: &SpecCase,
    invocation: &ResolvedSpecRun,
) -> Result<InvocationProgram, Box<dyn std::error::Error>> {
    Ok(InvocationProgram {
        source_id: case_source_id(case),
        source: case_source(case, invocation)?,
        entry: case_entry_name(invocation).to_string(),
    })
}

async fn eval_subgraph_at_place(
    client: &Eval2DaemonClient,
    place: &str,
    program: &InvocationProgram,
    args: &[ValueEnvelope],
) -> Result<EvalOutcome, Box<dyn std::error::Error>> {
    client
        .eval_subgraph(EvalSubgraphRequest {
            place: PlaceRef {
                id: place.to_string(),
            },
            source_id: program.source_id.clone(),
            source: program.source.clone(),
            entry: program.entry.clone(),
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
    let program = invocation_program(case, invocation)?;
    let runtime_fingerprint = runtime_fingerprint(&invocation.invoke.runtime);
    let outcome = if invocation.invoke.slot.is_some() || program_uses_stateful_slots(&program) {
        eval_subgraph_at_place(client, place, &program, &arg_envelopes).await?
    } else if cached_outcome(
        cached,
        place,
        &program,
        &runtime_fingerprint,
        &arg_envelopes,
    )
    .is_none()
        && let Some(replay_place) = invocation.invoke.replay_place.as_deref()
    {
        let initial = eval_subgraph_at_place(client, place, &program, &arg_envelopes).await?;
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
                    place,
                    &program,
                    &runtime_fingerprint,
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
                    place,
                    &program,
                    &runtime_fingerprint,
                    &arg_envelopes,
                    initial.clone(),
                );
                initial
            } else {
                events.push(EventPattern::VerifyClaimsFail);
                let fresh =
                    eval_subgraph_at_place(client, replay_place, &program, &arg_envelopes).await?;
                events.push(EventPattern::StabilizeMiss);
                if matches!(fresh, EvalOutcome::Ready(_)) {
                    cache_outcome(
                        cached,
                        replay_place,
                        &program,
                        &runtime_fingerprint,
                        &arg_envelopes,
                        fresh.clone(),
                    );
                }
                fresh
            }
        }
    } else if let Some(previous) = cached_outcome(
        cached,
        place,
        &program,
        &runtime_fingerprint,
        &arg_envelopes,
    ) {
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
                let fresh = eval_subgraph_at_place(client, place, &program, &arg_envelopes).await?;
                events.push(EventPattern::StabilizeMiss);
                if matches!(fresh, EvalOutcome::Ready(_)) {
                    cache_outcome(
                        cached,
                        place,
                        &program,
                        &runtime_fingerprint,
                        &arg_envelopes,
                        fresh.clone(),
                    );
                }
                fresh
            }
        }
    } else {
        let fresh = eval_subgraph_at_place(client, place, &program, &arg_envelopes).await?;
        events.push(EventPattern::StabilizeMiss);
        if matches!(fresh, EvalOutcome::Ready(_)) {
            cache_outcome(
                cached,
                place,
                &program,
                &runtime_fingerprint,
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
    place: &str,
    program: &InvocationProgram,
    runtime_fingerprint: &str,
    args: &[ValueEnvelope],
) -> Option<EvalOutcome> {
    cached
        .iter()
        .find(|invocation| {
            invocation.place == place
                && invocation.program == *program
                && invocation.runtime_fingerprint == runtime_fingerprint
                && invocation.args == args
        })
        .map(|invocation| invocation.outcome.clone())
}

fn cache_outcome(
    cached: &mut Vec<CachedInvocation>,
    place: &str,
    program: &InvocationProgram,
    runtime_fingerprint: &str,
    args: &[ValueEnvelope],
    outcome: EvalOutcome,
) {
    if let Some(existing) = cached.iter_mut().find(|invocation| {
        invocation.place == place
            && invocation.program == *program
            && invocation.runtime_fingerprint == runtime_fingerprint
            && invocation.args == args
    }) {
        existing.outcome = outcome;
        return;
    }

    cached.push(CachedInvocation {
        place: place.to_string(),
        program: program.clone(),
        runtime_fingerprint: runtime_fingerprint.to_string(),
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

#[test]
fn rpc_cache_key_separates_runtime_overrides() {
    let program = InvocationProgram {
        source_id: "spec#main".to_string(),
        source: "fn build() { 1 }\n".to_string(),
        entry: "build".to_string(),
    };
    let args = vec![value_to_envelope(&Value::u64(1)).expect("serialize cache args")];
    let mut cached = Vec::new();

    let mut runtime_a = RuntimeSpec::default();
    runtime_a.package_registry.packages.insert(
        "serde".to_string(),
        vx_spec::ToyPackageSpec {
            versions: vec![vx_spec::ToyPackageVersionSpec {
                version: "1.0.0".to_string(),
                deps: BTreeMap::new(),
            }],
        },
    );
    let mut runtime_b = RuntimeSpec::default();
    runtime_b.package_registry.packages.insert(
        "serde".to_string(),
        vx_spec::ToyPackageSpec {
            versions: vec![vx_spec::ToyPackageVersionSpec {
                version: "2.0.0".to_string(),
                deps: BTreeMap::new(),
            }],
        },
    );

    cache_outcome(
        &mut cached,
        "win",
        &program,
        &runtime_fingerprint(&runtime_a),
        &args,
        EvalOutcome::Ready(value_to_envelope(&Value::u64(41)).expect("serialize ready value")),
    );

    assert!(
        cached_outcome(
            &cached,
            "win",
            &program,
            &runtime_fingerprint(&runtime_a),
            &args
        )
        .is_some(),
        "matching runtime override should still hit the RPC cache"
    );
    assert!(
        cached_outcome(
            &cached,
            "win",
            &program,
            &runtime_fingerprint(&runtime_b),
            &args
        )
        .is_none(),
        "different runtime overrides must not share the same RPC cache entry"
    );
}

#[test]
fn rpc_cache_key_separates_places() {
    let program = InvocationProgram {
        source_id: "spec#main".to_string(),
        source: "fn build() { host_os() }\n".to_string(),
        entry: "build".to_string(),
    };
    let args = Vec::new();
    let mut cached = Vec::new();
    let runtime = RuntimeSpec::default();

    cache_outcome(
        &mut cached,
        "win",
        &program,
        &runtime_fingerprint(&runtime),
        &args,
        EvalOutcome::Ready(
            value_to_envelope(&Value::string("windows")).expect("serialize ready value"),
        ),
    );

    assert!(
        cached_outcome(
            &cached,
            "win",
            &program,
            &runtime_fingerprint(&runtime),
            &args
        )
        .is_some(),
        "same place should hit the RPC cache"
    );
    assert!(
        cached_outcome(
            &cached,
            "linux",
            &program,
            &runtime_fingerprint(&runtime),
            &args
        )
        .is_none(),
        "different places must not share the same RPC cache entry"
    );
}

datatest_stable::harness! {
    { test = run_fixture, root = "../../specs/eval", pattern = r"(^|.*/)[^/]+\.styx$" },
}
