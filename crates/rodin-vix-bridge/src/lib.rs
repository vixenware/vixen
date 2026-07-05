//! Spike A bridge: Rodin fact lifecycle as vix-machine-shaped values.
//!
//! The Rodin kernel remains `rodin-core`; this crate only puts a coarse
//! `solve()` capability boundary around it and projects learned facts into
//! content-addressed store values. Kernel propagation stays opaque.

use std::collections::{BTreeMap, BTreeSet};

use eyre::{Context, Result, bail};
use facet::Facet;
use rodin_core::{
    CompiledIndex, Decree, LearnedKind, LearnedNoGood, Problem, RootReq, SolveError, SolveResult,
    SyntheticDep,
};
use rodin_facts::{
    DerivationId, DerivationStore, Premise, Region, RuleKind, VersionSet, WidenCert, WidenEvidence,
};
use semver::{Version, VersionReq};
use vix::machine::{DriveEvent, Machine, MachineArg, NamedArg, StoreValue, ValueBundle};

pub const CAPABILITY_SCHEMA: &str = "rodin.kernel.capability.v0";
pub const PREMISE_BUNDLE_SCHEMA: &str = "rodin.premise_bundle.v0";
pub const LEARNED_FACT_SCHEMA: &str = "rodin.learned_no_good.v0";
pub const SOLVE_OUTPUT_SCHEMA: &str = "rodin.solve_output.v0";
pub const RODIN_SOLVE_COMPARATOR: &str =
    "old.monotone_safe && new.monotone_safe && new.req.subset(old.req)";

const RODIN_MACHINE_SOURCE: &str = r#"
use vix::{Version, VersionSet};

struct RodinPremise {
    req: VersionSet,
    monotone_safe: Bool,
}

pub fn premise(req: VersionSet, monotone_safe: Bool) -> RodinPremise {
    RodinPremise { req: req, monotone_safe: monotone_safe }
}

fn solve_kernel(req: VersionSet) -> Int {
    match req.contains(version("1.0.0")) {
        true => 1,
        false => 0,
    }
}

pub fn solve(premise: RodinPremise) -> Int {
    solve_kernel(premise.req)
}

fn solve__memo_verify_premise(old: RodinPremise, new: RodinPremise) -> Bool {
    old.monotone_safe && new.monotone_safe && new.req.subset(old.req)
}
"#;

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct RodinKernelCapabilityValue {
    pub name: String,
    pub kernel: String,
    pub granularity: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct PremiseBundleValue {
    pub fixture: String,
    pub roots: Vec<RootRequirementValue>,
    pub decrees: Vec<DecreeValue>,
    pub learned_kind: String,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct RootRequirementValue {
    pub name: String,
    pub req: String,
    pub features: Vec<String>,
    pub default_features: bool,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct DecreeValue {
    pub name: String,
    pub req: String,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct SelectionValue {
    pub package: String,
    pub version: String,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct LearnedFactStoreValue {
    pub fact_id: String,
    pub active: bool,
    pub kind: String,
    pub region: Vec<RegionTermValue>,
    pub support_premise_identities: Vec<String>,
    pub derivation: DerivationValue,
    pub proof_root: String,
    pub proof_digest: String,
    pub widened_from: u64,
    pub widened_to: u64,
    pub verification_status: String,
    pub content_identity: String,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct RegionTermValue {
    pub package: String,
    pub versions: String,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct DerivationValue {
    pub id: String,
    pub rule: String,
    pub premises: Vec<String>,
    pub replay_visited: Vec<String>,
    pub replay_hypotheses: Vec<u32>,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct StoredValueRef {
    pub handle: u64,
    pub schema: String,
    pub content_identity: String,
    pub deduped: bool,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct SolveOutputValue {
    pub capability: StoredValueRef,
    pub premise_bundle: StoredValueRef,
    pub selected: Vec<SelectionValue>,
    pub learned_facts: Vec<StoredValueRef>,
    pub decisions: u64,
    pub conflicts: u64,
    pub restarts: u64,
    pub kernel_invocation: u64,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct KernelProjection {
    pub selected: Vec<SelectionValue>,
    pub learned_facts: Vec<LearnedFactStoreValue>,
    pub decisions: u64,
    pub conflicts: u64,
    pub restarts: u64,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct BridgeTraceEvent {
    pub kind: String,
    pub key: String,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityRun {
    pub output: SolveOutputValue,
    pub projection: KernelProjection,
    pub store: ValueBundle,
    pub trace: Vec<BridgeTraceEvent>,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct SemanticCutoffComparison {
    pub byte_equal: bool,
    pub monotone_safe: bool,
    pub equivalent_on_fixture_universe: bool,
    pub semantic_hit_candidate: bool,
    pub old_identity: String,
    pub new_identity: String,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct WarmDemoReport {
    pub first_kernel_invocations: u64,
    pub repeated_same_req_memo_hits: u64,
    pub repeated_same_req_additional_kernel_invocations: u64,
    pub narrowed_semantic_hits: u64,
    pub narrowed_additional_kernel_invocations: u64,
    pub widened_semantic_hits: u64,
    pub widened_additional_kernel_invocations: u64,
    pub comparison: SemanticCutoffComparison,
}

pub struct RodinMachine {
    semantic: Machine,
    store: RodinStore,
    exact_memo: BTreeMap<String, CapabilityRun>,
    runs: Vec<(PremiseBundleValue, CapabilityRun)>,
    trace: Vec<BridgeTraceEvent>,
    kernel_invocations: u64,
}

impl RodinMachine {
    pub fn new() -> Result<Self> {
        Ok(Self {
            semantic: Machine::load(RODIN_MACHINE_SOURCE)
                .map_err(|err| eyre::eyre!("loading Rodin vix machine: {err}"))?,
            store: RodinStore::default(),
            exact_memo: BTreeMap::new(),
            runs: Vec::new(),
            trace: Vec::new(),
            kernel_invocations: 0,
        })
    }

    pub fn demand_solve(&mut self, bundle: &PremiseBundleValue) -> Result<CapabilityRun> {
        let premise_ref = self.store.put_facet(PREMISE_BUNDLE_SCHEMA, bundle)?;
        let exact_identity = premise_ref.content_identity.clone();
        self.trace.push(BridgeTraceEvent {
            kind: "demanded".to_owned(),
            key: exact_identity.clone(),
            detail: "rodin solve premise bundle via vix VersionSet".to_owned(),
        });
        let semantic = self.demand_semantic_solve(bundle)?;
        if semantic.exact_hits > 0 {
            let hit = self
                .exact_memo
                .get(&exact_identity)
                .cloned()
                .ok_or_else(|| eyre::eyre!("vix exact memo hit without bridge output"))?;
            self.trace.push(BridgeTraceEvent {
                kind: "memo-hit".to_owned(),
                key: exact_identity,
                detail: "vix exact memo reused solve output".to_owned(),
            });
            return Ok(CapabilityRun {
                trace: self.trace.clone(),
                store: self.store.bundle(hit.store.root),
                ..hit
            });
        }
        if semantic.semantic_hits > 0 {
            let hit = self
                .runs
                .iter()
                .find(|(old, _)| {
                    compare_for_semantic_cutoff(old, bundle)
                        .is_ok_and(|cmp| cmp.semantic_hit_candidate)
                })
                .map(|(_, run)| run.clone())
                .ok_or_else(|| eyre::eyre!("vix semantic hit without compatible bridge output"))?;
            let reused = self.reuse_run_for_premise(hit, premise_ref)?;
            self.exact_memo.insert(
                reused.output.premise_bundle.content_identity.clone(),
                reused.clone(),
            );
            self.runs.push((bundle.clone(), reused.clone()));
            self.trace.push(BridgeTraceEvent {
                kind: "semantic-hit".to_owned(),
                key: reused.output.premise_bundle.content_identity.clone(),
                detail: format!(
                    "vix MemoSemanticHit verified {} comparator(s); spawned {} solve node(s)",
                    semantic.verified, semantic.spawned
                ),
            });
            return Ok(CapabilityRun {
                trace: self.trace.clone(),
                store: self.store.bundle(reused.store.root),
                ..reused
            });
        }

        self.kernel_invocations += 1;
        self.trace.push(BridgeTraceEvent {
            kind: "spawned".to_owned(),
            key: premise_ref.content_identity.clone(),
            detail: format!(
                "rodin-core solve capability invocation after vix spawned {} solve node(s)",
                semantic.spawned
            ),
        });
        let mut run = RodinKernelCapability::acquire().exec(
            bundle,
            premise_ref,
            &mut self.store,
            self.kernel_invocations,
        )?;
        self.trace.push(BridgeTraceEvent {
            kind: "completed".to_owned(),
            key: run.output.premise_bundle.content_identity.clone(),
            detail: format!("{} learned facts", run.output.learned_facts.len()),
        });
        run.trace = self.trace.clone();
        run.store = self.store.bundle(run.store.root);
        self.exact_memo.insert(
            run.output.premise_bundle.content_identity.clone(),
            run.clone(),
        );
        self.runs.push((bundle.clone(), run.clone()));
        Ok(run)
    }

    #[must_use]
    pub fn kernel_invocations(&self) -> u64 {
        self.kernel_invocations
    }

    #[must_use]
    pub fn semantic_trace(&self) -> &[DriveEvent] {
        self.semantic.trace()
    }

    fn demand_semantic_solve(&mut self, bundle: &PremiseBundleValue) -> Result<SemanticDemand> {
        let premise = semantic_premise(bundle)?;
        let premise_handle = self
            .semantic
            .call(
                "premise",
                &[
                    NamedArg {
                        name: "req".to_owned(),
                        value: MachineArg::String(premise.req),
                    },
                    NamedArg {
                        name: "monotone_safe".to_owned(),
                        value: MachineArg::Bool(premise.monotone_safe),
                    },
                ],
            )
            .map_err(|err| eyre::eyre!("building RodinPremise in vix: {err}"))?;
        self.semantic.clear_trace();
        let _ = self
            .semantic
            .call(
                "solve",
                &[NamedArg {
                    name: "premise".to_owned(),
                    value: MachineArg::Handle(premise_handle),
                }],
            )
            .map_err(|err| eyre::eyre!("demanding Rodin vix solve: {err}"))?;
        Ok(SemanticDemand {
            exact_hits: memo_hit_count(&self.semantic, "solve") as u64,
            semantic_hits: memo_semantic_hit_count(&self.semantic, "solve") as u64,
            verified: semantic_verified_count(&self.semantic, "solve")
                .into_iter()
                .sum::<usize>() as u64,
            spawned: spawned_count(&self.semantic, "solve") as u64,
        })
    }

    fn reuse_run_for_premise(
        &mut self,
        mut run: CapabilityRun,
        premise_ref: StoredValueRef,
    ) -> Result<CapabilityRun> {
        run.output.premise_bundle = premise_ref;
        let output_ref = self.store.put_facet(SOLVE_OUTPUT_SCHEMA, &run.output)?;
        run.store = self.store.bundle(output_ref.handle);
        run.trace = self.trace.clone();
        Ok(run)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SemanticPremise {
    req: String,
    monotone_safe: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SemanticDemand {
    exact_hits: u64,
    semantic_hits: u64,
    verified: u64,
    spawned: u64,
}

#[derive(Clone, Debug)]
pub struct RodinKernelCapability {
    value: RodinKernelCapabilityValue,
}

impl RodinKernelCapability {
    #[must_use]
    pub fn acquire() -> Self {
        let value = RodinKernelCapabilityValue {
            name: "RodinCoreSolve".to_owned(),
            kernel: "rodin-core".to_owned(),
            granularity: "one solve() demand node; kernel interior opaque".to_owned(),
            fingerprint: content_identity(
                CAPABILITY_SCHEMA,
                b"rodin-core solve capability v0; facts cross seam",
            ),
        };
        Self { value }
    }

    fn exec(
        &self,
        bundle: &PremiseBundleValue,
        premise_ref: StoredValueRef,
        store: &mut RodinStore,
        invocation: u64,
    ) -> Result<CapabilityRun> {
        let capability_ref = store.put_facet(CAPABILITY_SCHEMA, &self.value)?;
        let (index, problem) = fixture_problem(bundle)?;
        let projection = run_kernel(&index, problem)?;
        let learned_facts = projection
            .learned_facts
            .iter()
            .map(|fact| store.put_facet(LEARNED_FACT_SCHEMA, fact))
            .collect::<Result<Vec<_>>>()?;
        let output = SolveOutputValue {
            capability: capability_ref,
            premise_bundle: premise_ref,
            selected: projection.selected.clone(),
            learned_facts,
            decisions: projection.decisions,
            conflicts: projection.conflicts,
            restarts: projection.restarts,
            kernel_invocation: invocation,
        };
        let output_ref = store.put_facet(SOLVE_OUTPUT_SCHEMA, &output)?;
        Ok(CapabilityRun {
            output,
            projection,
            store: store.bundle(output_ref.handle),
            trace: Vec::new(),
        })
    }
}

pub fn direct_kernel_projection(bundle: &PremiseBundleValue) -> Result<KernelProjection> {
    let (index, problem) = fixture_problem(bundle)?;
    run_kernel(&index, problem)
}

#[must_use]
pub fn buried_pin_bundle(req: impl Into<String>) -> PremiseBundleValue {
    PremiseBundleValue {
        fixture: "buried-pin".to_owned(),
        roots: vec![
            RootRequirementValue {
                name: "a".to_owned(),
                req: req.into(),
                features: Vec::new(),
                default_features: true,
            },
            RootRequirementValue {
                name: "x".to_owned(),
                req: "*".to_owned(),
                features: Vec::new(),
                default_features: true,
            },
        ],
        decrees: Vec::new(),
        learned_kind: "plain".to_owned(),
    }
}

pub fn compare_for_semantic_cutoff(
    old: &PremiseBundleValue,
    new: &PremiseBundleValue,
) -> Result<SemanticCutoffComparison> {
    let old_identity = store_identity(PREMISE_BUNDLE_SCHEMA, old)?;
    let new_identity = store_identity(PREMISE_BUNDLE_SCHEMA, new)?;
    let monotone_safe = monotone_safe_roots(old, new)?;
    let equivalent_on_fixture_universe =
        old.fixture == new.fixture && roots_equal_on_fixture_universe(old, new)?;
    Ok(SemanticCutoffComparison {
        byte_equal: old_identity == new_identity,
        monotone_safe,
        equivalent_on_fixture_universe,
        semantic_hit_candidate: old_identity != new_identity
            && monotone_safe
            && equivalent_on_fixture_universe,
        old_identity,
        new_identity,
    })
}

pub fn run_warm_demo() -> Result<WarmDemoReport> {
    let base = buried_pin_bundle("*");
    let narrowed = buried_pin_bundle(">=1.0.0, <1.0.3");
    let mut machine = RodinMachine::new()?;

    machine.demand_solve(&base)?;
    let first_kernel_invocations = machine.kernel_invocations();

    machine.demand_solve(&base)?;
    let after_same_bytes = machine.kernel_invocations();
    let repeated_same_req_memo_hits = memo_hit_count(&machine.semantic, "solve") as u64;

    machine.demand_solve(&narrowed)?;
    let after_narrowed = machine.kernel_invocations();
    let narrowed_semantic_hits = memo_semantic_hit_count(&machine.semantic, "solve") as u64;

    let mut widened_machine = RodinMachine::new()?;
    widened_machine.demand_solve(&narrowed)?;
    let before_widened = widened_machine.kernel_invocations();
    widened_machine.demand_solve(&base)?;
    let after_widened = widened_machine.kernel_invocations();
    let widened_semantic_hits = memo_semantic_hit_count(&widened_machine.semantic, "solve") as u64;

    let comparison = compare_for_semantic_cutoff(&base, &narrowed)?;
    Ok(WarmDemoReport {
        first_kernel_invocations,
        repeated_same_req_memo_hits,
        repeated_same_req_additional_kernel_invocations: after_same_bytes
            - first_kernel_invocations,
        narrowed_semantic_hits,
        narrowed_additional_kernel_invocations: after_narrowed - after_same_bytes,
        widened_semantic_hits,
        widened_additional_kernel_invocations: after_widened - before_widened,
        comparison,
    })
}

fn run_kernel(index: &CompiledIndex, problem: Problem) -> Result<KernelProjection> {
    match rodin_core::solve(index, problem) {
        Ok(result) => project_result(index, result),
        Err(err) => match *err {
            SolveError::NoSolution {
                decisions,
                conflicts,
                restarts,
                learned,
                derivations,
                ..
            } => project_parts(
                index,
                BTreeMap::new(),
                learned,
                &derivations,
                decisions,
                conflicts,
                restarts,
            ),
        },
    }
}

fn project_result(index: &CompiledIndex, result: SolveResult) -> Result<KernelProjection> {
    project_parts(
        index,
        result.selected,
        result.learned,
        &result.derivations,
        result.decisions,
        result.conflicts,
        result.restarts,
    )
}

fn project_parts(
    index: &CompiledIndex,
    selected: BTreeMap<String, Version>,
    learned: Vec<LearnedNoGood>,
    derivations: &DerivationStore,
    decisions: usize,
    conflicts: usize,
    restarts: usize,
) -> Result<KernelProjection> {
    let selected = selected
        .into_iter()
        .map(|(package, version)| SelectionValue {
            package,
            version: version.to_string(),
        })
        .collect();
    let learned_facts = learned
        .iter()
        .enumerate()
        .map(|(index_in_run, fact)| project_fact(index, derivations, index_in_run, fact))
        .collect::<Result<Vec<_>>>()?;
    Ok(KernelProjection {
        selected,
        learned_facts,
        decisions: decisions as u64,
        conflicts: conflicts as u64,
        restarts: restarts as u64,
    })
}

fn project_fact(
    index: &CompiledIndex,
    derivations: &DerivationStore,
    index_in_run: usize,
    fact: &LearnedNoGood,
) -> Result<LearnedFactStoreValue> {
    let derivation = project_derivation(index, derivations, fact.derivation)?;
    let mut value = LearnedFactStoreValue {
        fact_id: format!("learned-{index_in_run}"),
        active: fact.active,
        kind: fact.kind.as_str().to_owned(),
        region: project_region(index, &fact.region),
        support_premise_identities: fact
            .support
            .iter()
            .map(|edge| format!("edge:{}->{}", edge.parent, edge.dep))
            .collect(),
        derivation,
        proof_root: fact.proof_root.0.clone(),
        proof_digest: fact.proof_digest.0.clone(),
        widened_from: fact.point_versions as u64,
        widened_to: fact.widened_versions as u64,
        verification_status: format!("{:?}", fact.verification_status),
        content_identity: String::new(),
    };
    value.content_identity = store_identity(LEARNED_FACT_SCHEMA, &value)?;
    Ok(value)
}

fn project_derivation(
    index: &CompiledIndex,
    derivations: &DerivationStore,
    id: DerivationId,
) -> Result<DerivationValue> {
    let derivation = derivations
        .get(id)
        .ok_or_else(|| eyre::eyre!("missing derivation {id:?}"))?;
    let replay = derivations
        .replay(id)
        .map_err(|err| eyre::eyre!("learned derivation {id:?} did not replay: {err:?}"))?;
    Ok(DerivationValue {
        id: derivation_id(id),
        rule: rule_identity(index, &derivation.rule),
        premises: derivation.premises.iter().map(premise_identity).collect(),
        replay_visited: replay.visited.into_iter().map(derivation_id).collect(),
        replay_hypotheses: replay.hypotheses.into_iter().collect(),
    })
}

fn project_region(index: &CompiledIndex, region: &Region) -> Vec<RegionTermValue> {
    region
        .versions
        .iter()
        .map(|(pkg, versions)| RegionTermValue {
            package: index.name(*pkg).unwrap_or("<unknown>").to_owned(),
            versions: display_version_set(versions),
        })
        .collect()
}

fn fixture_problem(bundle: &PremiseBundleValue) -> Result<(CompiledIndex, Problem)> {
    if bundle.fixture != "buried-pin" {
        bail!("unknown Rodin bridge fixture `{}`", bundle.fixture);
    }
    let mut index = CompiledIndex::empty();
    add_leaf_versions(&mut index, "a", 1, 3);
    index.add_package_version("x", rung(1), Vec::new(), BTreeMap::new());
    index.add_package_version(
        "x",
        rung(2),
        vec![SyntheticDep::normal("a", exact_rung(1))],
        BTreeMap::new(),
    );
    index.finish_synthetic()?;
    let mut problem = Problem::new(bundle.roots.iter().map(root_req).collect());
    problem.decrees = bundle
        .decrees
        .iter()
        .map(|decree| Decree::new(&decree.name, &decree.req))
        .collect();
    problem.learned_kind = learned_kind(&bundle.learned_kind)?;
    Ok((index, problem))
}

fn semantic_premise(bundle: &PremiseBundleValue) -> Result<SemanticPremise> {
    let roots = roots_by_name(bundle);
    let root = roots
        .get("a")
        .ok_or_else(|| eyre::eyre!("buried-pin semantic premise requires root `a`"))?;
    Ok(SemanticPremise {
        req: root.req.clone(),
        monotone_safe: monotone_safe_requirement_bundle(bundle),
    })
}

fn monotone_safe_requirement_bundle(bundle: &PremiseBundleValue) -> bool {
    if bundle.fixture != "buried-pin"
        || bundle.learned_kind != "plain"
        || !bundle.decrees.is_empty()
    {
        return false;
    }
    let roots = roots_by_name(bundle);
    let Some(a) = roots.get("a") else {
        return false;
    };
    let Some(x) = roots.get("x") else {
        return false;
    };
    roots.len() == 2
        && a.features.is_empty()
        && a.default_features
        && x.req == "*"
        && x.features.is_empty()
        && x.default_features
}

fn root_req(root: &RootRequirementValue) -> RootReq {
    let mut out = RootReq::new(&root.name, &root.req);
    out.default_features = root.default_features;
    out.features = root.features.iter().cloned().collect();
    out
}

fn learned_kind(value: &str) -> Result<LearnedKind> {
    match value {
        "plain" => Ok(LearnedKind::Plain),
        "feature-guarded" => Ok(LearnedKind::FeatureGuarded),
        "decree-derived" => Ok(LearnedKind::DecreeDerived),
        "hole-aware" => Ok(LearnedKind::HoleAware),
        "exclusion" => Ok(LearnedKind::Exclusion),
        "family" => Ok(LearnedKind::Family),
        other => bail!("unknown learned kind `{other}`"),
    }
}

fn add_leaf_versions(index: &mut CompiledIndex, name: &str, first: u64, last: u64) {
    for rung in first..=last {
        index.add_package_version(name, rung_version(rung), Vec::new(), BTreeMap::new());
    }
}

fn rung(rung: u64) -> Version {
    rung_version(rung)
}

fn rung_version(rung: u64) -> Version {
    Version::new(1, 0, rung.saturating_sub(1))
}

fn exact_rung(rung: u64) -> String {
    format!("={}", rung_version(rung))
}

fn display_version_set(set: &VersionSet) -> String {
    if set.is_empty() {
        return "empty".to_owned();
    }
    set.intervals()
        .iter()
        .map(|interval| {
            let lower = interval
                .lower
                .as_ref()
                .map_or("-inf".to_owned(), ToString::to_string);
            let upper = interval
                .upper
                .as_ref()
                .map_or("+inf".to_owned(), ToString::to_string);
            format!("[{lower},{upper})")
        })
        .collect::<Vec<_>>()
        .join(" U ")
}

fn derivation_id(id: DerivationId) -> String {
    format!("derivation-{}", id.0)
}

fn premise_identity(premise: &Premise) -> String {
    match premise {
        Premise::Clause(id) => format!("clause-{}", id.0),
        Premise::Exclusion(id) => format!("exclusion-{}", id.0),
        Premise::Pin(pin) => format!("pin-{}", pin.0),
        Premise::Hypothesis(id) => format!("hypothesis-{id}"),
        Premise::Derived(id) => derivation_id(*id),
    }
}

fn rule_identity(index: &CompiledIndex, rule: &RuleKind) -> String {
    match rule {
        RuleKind::Conflict => "conflict".to_owned(),
        RuleKind::Resolve => "resolve".to_owned(),
        RuleKind::Widen { certificate } => widen_cert_identity(index, certificate),
    }
}

fn widen_cert_identity(index: &CompiledIndex, cert: &WidenCert) -> String {
    let package = index.name(cert.package).unwrap_or("<unknown>");
    let evidence = match &cert.evidence {
        WidenEvidence::DeclaredStructure { manifest_hash } => {
            format!("declared:{manifest_hash}")
        }
        WidenEvidence::ObservedReadSet {
            read_set_digest,
            snapshot_id,
        } => format!("read-set:{snapshot_id}:{read_set_digest}"),
    };
    format!(
        "widen:{package}:{}:{evidence}",
        display_version_set(&cert.checked)
    )
}

fn monotone_safe_roots(old: &PremiseBundleValue, new: &PremiseBundleValue) -> Result<bool> {
    if old.fixture != new.fixture
        || old.decrees != new.decrees
        || old.learned_kind != new.learned_kind
    {
        return Ok(false);
    }
    let old_roots = roots_by_name(old);
    let new_roots = roots_by_name(new);
    if old_roots.keys().collect::<BTreeSet<_>>() != new_roots.keys().collect::<BTreeSet<_>>() {
        return Ok(false);
    }
    for (name, old_root) in old_roots {
        let new_root = new_roots[name];
        if old_root.features != new_root.features
            || old_root.default_features != new_root.default_features
        {
            return Ok(false);
        }
        if !version_req_set(&new_root.req)?.is_subset_of(&version_req_set(&old_root.req)?) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn roots_equal_on_fixture_universe(
    old: &PremiseBundleValue,
    new: &PremiseBundleValue,
) -> Result<bool> {
    let (index, _) = fixture_problem(old)?;
    let old_roots = roots_by_name(old);
    let new_roots = roots_by_name(new);
    if old_roots.keys().collect::<BTreeSet<_>>() != new_roots.keys().collect::<BTreeSet<_>>() {
        return Ok(false);
    }
    for (name, old_root) in old_roots {
        let Some(pkg) = index.package(name) else {
            return Ok(false);
        };
        let versions = index.versions(pkg);
        let old_allowed = allowed_fixture_versions(versions, &old_root.req)?;
        let new_allowed = allowed_fixture_versions(versions, &new_roots[name].req)?;
        if old_allowed != new_allowed {
            return Ok(false);
        }
    }
    Ok(true)
}

fn roots_by_name(bundle: &PremiseBundleValue) -> BTreeMap<&str, &RootRequirementValue> {
    bundle
        .roots
        .iter()
        .map(|root| (root.name.as_str(), root))
        .collect()
}

fn allowed_fixture_versions(versions: &[Version], req: &str) -> Result<Vec<String>> {
    let set = version_req_set(req)?;
    Ok(versions
        .iter()
        .filter(|version| set.contains(version))
        .map(ToString::to_string)
        .collect())
}

fn version_req_set(req: &str) -> Result<VersionSet> {
    let parsed = VersionReq::parse(req).wrap_err_with(|| format!("parsing version req {req}"))?;
    Ok(VersionSet::from_req(&parsed))
}

fn spawned_count(machine: &Machine, name: &str) -> usize {
    let hash = machine.fn_hash(name).expect("function hash");
    machine
        .trace()
        .iter()
        .filter(|event| matches!(event, DriveEvent::Spawned { fn_hash } if *fn_hash == hash))
        .count()
}

fn memo_hit_count(machine: &Machine, name: &str) -> usize {
    let hash = machine.fn_hash(name).expect("function hash");
    machine
        .trace()
        .iter()
        .filter(|event| matches!(event, DriveEvent::MemoHit { fn_hash } if *fn_hash == hash))
        .count()
}

fn memo_semantic_hit_count(machine: &Machine, name: &str) -> usize {
    let hash = machine.fn_hash(name).expect("function hash");
    machine
        .trace()
        .iter()
        .filter(|event| {
            matches!(event, DriveEvent::MemoSemanticHit { fn_hash, .. } if *fn_hash == hash)
        })
        .count()
}

fn semantic_verified_count(machine: &Machine, name: &str) -> Vec<usize> {
    let hash = machine.fn_hash(name).expect("function hash");
    machine
        .trace()
        .iter()
        .filter_map(|event| match event {
            DriveEvent::MemoSemanticHit { fn_hash, verified } if *fn_hash == hash => {
                Some(*verified)
            }
            _ => None,
        })
        .collect()
}

#[derive(Clone, Debug, Default)]
struct RodinStore {
    values: Vec<StoreValue>,
    by_content: BTreeMap<(String, String), u64>,
}

impl RodinStore {
    fn put_facet<T: for<'facet> Facet<'facet>>(
        &mut self,
        schema: &str,
        value: &T,
    ) -> Result<StoredValueRef> {
        let json = facet_json::to_string(value)
            .wrap_err_with(|| format!("serializing {schema} store value"))?;
        self.put_bytes(schema, json.into_bytes())
    }

    fn put_bytes(&mut self, schema: &str, bytes: Vec<u8>) -> Result<StoredValueRef> {
        let identity = content_identity(schema, &bytes);
        let key = (schema.to_owned(), identity.clone());
        if let Some(handle) = self.by_content.get(&key).copied() {
            return Ok(StoredValueRef {
                handle,
                schema: schema.to_owned(),
                content_identity: identity,
                deduped: true,
            });
        }
        let handle = self.values.len() as u64;
        self.values.push(StoreValue {
            handle,
            schema: schema.to_owned(),
            bytes,
            content_hash: hex_to_bytes(&identity)?,
            taint: None,
        });
        self.by_content.insert(key, handle);
        Ok(StoredValueRef {
            handle,
            schema: schema.to_owned(),
            content_identity: identity,
            deduped: false,
        })
    }

    fn bundle(&self, root: u64) -> ValueBundle {
        ValueBundle {
            root,
            values: self.values.clone(),
            code: Vec::new(),
        }
    }
}

fn store_identity<T: for<'facet> Facet<'facet>>(schema: &str, value: &T) -> Result<String> {
    let json =
        facet_json::to_string(value).wrap_err_with(|| format!("serializing {schema} identity"))?;
    Ok(content_identity(schema, json.as_bytes()))
}

fn content_identity(schema: &str, bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(schema.as_bytes());
    hasher.update(&[0]);
    hasher.update(bytes);
    hasher.finalize().to_hex().to_string()
}

fn hex_to_bytes(hex: &str) -> Result<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        bail!("hex string has odd length");
    }
    (0..hex.len())
        .step_by(2)
        .map(|offset| {
            u8::from_str_radix(&hex[offset..offset + 2], 16)
                .wrap_err_with(|| format!("parsing hex byte at {offset}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_solve_matches_rodin_core_oracle() -> Result<()> {
        let bundle = buried_pin_bundle("*");
        let direct = direct_kernel_projection(&bundle)?;

        let mut machine = RodinMachine::new()?;
        let via_machine = machine.demand_solve(&bundle)?;

        assert_eq!(via_machine.projection.selected, direct.selected);
        assert_eq!(via_machine.projection.learned_facts, direct.learned_facts);
        assert_eq!(machine.kernel_invocations(), 1);
        assert!(
            via_machine
                .output
                .learned_facts
                .iter()
                .all(|fact| fact.schema == LEARNED_FACT_SCHEMA)
        );
        assert!(
            via_machine
                .projection
                .learned_facts
                .iter()
                .all(|fact| !fact.derivation.replay_visited.is_empty())
        );
        Ok(())
    }

    #[test]
    fn learned_fact_store_values_dedupe_by_content() -> Result<()> {
        let bundle = buried_pin_bundle("*");
        let mut machine = RodinMachine::new()?;
        let first = machine.demand_solve(&bundle)?;
        let second = machine.demand_solve(&bundle)?;

        assert_eq!(machine.kernel_invocations(), 1);
        assert_eq!(first.output.learned_facts, second.output.learned_facts);
        assert!(
            second
                .trace
                .iter()
                .any(|event| event.kind == "memo-hit" && event.detail.contains("vix exact memo"))
        );
        Ok(())
    }

    #[test]
    fn warm_demo_asserts_semantic_hit_for_req_narrowing() -> Result<()> {
        let report = run_warm_demo()?;

        assert_eq!(report.first_kernel_invocations, 1);
        assert_eq!(report.repeated_same_req_memo_hits, 1);
        assert_eq!(report.repeated_same_req_additional_kernel_invocations, 0);
        assert_eq!(report.narrowed_semantic_hits, 1);
        assert_eq!(report.narrowed_additional_kernel_invocations, 0);
        assert_eq!(report.widened_semantic_hits, 0);
        assert_eq!(report.widened_additional_kernel_invocations, 1);
        assert!(!report.comparison.byte_equal);
        assert!(report.comparison.monotone_safe);
        assert!(report.comparison.equivalent_on_fixture_universe);
        assert!(report.comparison.semantic_hit_candidate);
        Ok(())
    }
}
