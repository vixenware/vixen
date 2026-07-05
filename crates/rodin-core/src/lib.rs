use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use eyre::{Context, Result};
use facet::Facet;
use rodin_facts::{
    Assignment, Atom, Clause, ClauseId, ClauseStore, CompatClass, Derivation, DerivationId,
    DerivationStore, Exclusion, ExclusionId, ExclusionStore, FeatIx, FeatureInterner,
    PackageInterner, PinRef, PkgId, PkgIx, Premise, Region, RuleKind, SourceInterner, SourceIx,
    VersionSet,
};
use semver::{Version, VersionReq};

pub mod cargo_evidence;

pub use cargo_evidence::{
    CargoDivergence, CargoDivergenceKind, CargoEvidenceReport, CargoExtractionGap,
    CargoFreshLockOracle, CargoOracleKind, CargoProjectionMode, CargoProjectionReport,
    CargoWarmCounterfactual, append_cargo_evidence_round, render_cargo_evidence_markdown,
    run_cargo_evidence, run_facet_cc_evidence,
};

const CRATES_IO: &str = "crates.io";
const FEATURE_SCOPES: &[&str] = &["normal", "build", "dev"];

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EdgeKey {
    pub parent: String,
    pub dep: String,
}

fn scoped_feature_name(scope: &str, feature: &str) -> String {
    if scope == "normal" {
        feature.to_owned()
    } else {
        format!("{scope}:{feature}")
    }
}

fn explicit_optional_dep_features(manifest: &Manifest) -> BTreeSet<String> {
    manifest
        .features
        .values()
        .flat_map(|enables| enables.iter())
        .filter_map(|enable| enable.strip_prefix("dep:"))
        .map(str::to_owned)
        .collect()
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EdgeSignature {
    pub req: String,
    pub features: Vec<String>,
    pub optional: bool,
    pub default_features: bool,
    pub target: Option<String>,
    pub kind: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootReq {
    pub name: String,
    pub req: String,
    pub features: BTreeSet<String>,
    pub default_features: bool,
}

impl RootReq {
    #[must_use]
    pub fn new(name: impl Into<String>, req: impl Into<String>) -> Self {
        Self {
            name: name.into().to_ascii_lowercase(),
            req: req.into(),
            features: BTreeSet::new(),
            default_features: true,
        }
    }

    #[must_use]
    pub fn feature(mut self, feature: impl Into<String>) -> Self {
        self.features.insert(feature.into());
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Decree {
    pub name: String,
    pub req: String,
}

impl Decree {
    #[must_use]
    pub fn new(name: impl Into<String>, req: impl Into<String>) -> Self {
        Self {
            name: name.into().to_ascii_lowercase(),
            req: req.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LearnedKind {
    Plain,
    FeatureGuarded,
    DecreeDerived,
    HoleAware,
    Exclusion,
    Family,
}

impl LearnedKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::FeatureGuarded => "feature-guarded",
            Self::DecreeDerived => "decree-derived",
            Self::HoleAware => "hole-aware",
            Self::Exclusion => "exclusion",
            Self::Family => "family",
        }
    }
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
#[repr(C)]
pub enum VerificationStatus {
    LocallyLearned,
    VerifiedImported,
    ShrunkInvalidWiden,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactScope {
    pub index_digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProofDigest(pub String);

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProofNodeId(pub String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WarmFactBundle {
    pub scope: FactScope,
    pub facts: Vec<WarmLearnedNoGood>,
    pub proofs: WarmProofSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WarmLearnedNoGood {
    pub fact: LearnedNoGood,
    pub proof_root: ProofNodeId,
    pub proof_digest: ProofDigest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WarmProofSource {
    Inline(Arc<ProofGraph>),
    Lazy(ProofFetchRef),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProofFetchRef {
    pub digest: ProofDigest,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VerifiedWarmFacts {
    pub facts: Vec<VerifiedWarmFact>,
    pub rejected: Vec<WarmFactRejection>,
}

impl VerifiedWarmFacts {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedWarmFact {
    pub fact: LearnedNoGood,
    pub proof_root: ProofNodeId,
    pub proofs: Arc<ProofGraph>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WarmFactRejection {
    pub proof_digest: Option<ProofDigest>,
    pub reason: WarmFactRejectionReason,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WarmFactRejectionReason {
    ScopeMismatch,
    LazyProofUnavailable,
    MissingProofNode,
    DigestMismatch,
    RebindFailed,
    ReplayFailed,
    InvalidWidenWithoutConflict,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProofGraph {
    pub nodes: BTreeMap<ProofNodeId, ProofNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProofNode {
    pub premises: Vec<ProofPremise>,
    pub rule: ProofRule,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProofRule {
    Conflict,
    Resolve,
    Widen { witness: WidenWitness },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProofPremise {
    Clause(ClauseWitness),
    Pin(PinWitness),
    Exclusion(ExclusionWitness),
    Hypothesis(HypothesisWitness),
    Derived(DerivedWitness),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClauseWitness {
    pub antecedents: Vec<AtomWitness>,
    pub consequent: Option<AtomWitness>,
    pub support: Option<EdgeKey>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AtomWitness {
    InGraph { package: String },
    Ver { package: String, versions: String },
    Feat { package: String, feature: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinWitness {
    pub pin: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExclusionWitness {
    pub proof_root: ProofNodeId,
    pub proof_digest: ProofDigest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HypothesisWitness {
    pub id: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedWitness {
    pub proof_root: ProofNodeId,
    pub proof_digest: ProofDigest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WidenWitness {
    pub support_edges: Vec<EdgeKey>,
    pub point_terms: Vec<PointTermWitness>,
    pub widened_terms: Vec<WidenedTermWitness>,
    pub edge_stability: Vec<EdgeStabilityWitness>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PointTermWitness {
    pub package: String,
    pub version: Version,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WidenedTermWitness {
    pub package: String,
    pub versions: Vec<Version>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EdgeStabilityWitness {
    pub parent: String,
    pub dep: String,
    pub selected_version: Version,
    pub stable_versions: Vec<Version>,
    pub selected_state: Vec<EdgeSignature>,
}

pub struct WarmFactVerifier {
    scope: FactScope,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Problem {
    pub roots: Vec<RootReq>,
    pub decrees: Vec<Decree>,
    pub exclusions: Vec<FeatureExclusion>,
    pub warm_facts: VerifiedWarmFacts,
    pub learned_kind: LearnedKind,
}

impl Problem {
    #[must_use]
    pub fn new(roots: Vec<RootReq>) -> Self {
        Self {
            roots,
            decrees: Vec::new(),
            exclusions: Vec::new(),
            warm_facts: VerifiedWarmFacts::new(),
            learned_kind: LearnedKind::Plain,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeatureExclusion {
    pub package: String,
    pub features: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub struct SolveResult {
    pub selected: BTreeMap<String, Version>,
    pub selected_derivations: Vec<SelectionDerivation>,
    pub enabled_features: BTreeMap<String, BTreeSet<String>>,
    pub decisions: usize,
    pub conflicts: usize,
    pub manifests_read: usize,
    pub candidates_existing: usize,
    pub steps: Vec<SolveStep>,
    pub trace: Vec<String>,
    pub final_domains: BTreeMap<String, FinalDomain>,
    pub learned: Vec<LearnedNoGood>,
    pub derivations: DerivationStore,
    pub clauses: ClauseStore,
}

#[derive(Clone, Debug)]
pub struct FinalDomain {
    pub allowed: VersionSet,
    pub reasons: Vec<Premise>,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct SolveRecord {
    pub scenario: String,
    pub shape: String,
    pub packages: Vec<RecordPackage>,
    pub steps: Vec<SolveStep>,
    pub graph: RecordGraph,
    pub facts: Vec<RecordFact>,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct RecordPackage {
    pub id: String,
    pub source: String,
    pub name: String,
    pub versions: Vec<String>,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct RecordGraph {
    pub nodes: Vec<RecordNode>,
    pub edges: Vec<RecordEdge>,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct RecordNode {
    pub pkg: String,
    pub version: String,
    pub features: Vec<String>,
    pub derivation: SelectionDerivation,
    pub ladder: Vec<RecordRung>,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct RecordEdge {
    pub id: String,
    pub from: String,
    pub to: String,
    pub requirement: String,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct RecordRung {
    pub version: String,
    pub verdict: RungVerdict,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
#[repr(C)]
pub enum RungVerdict {
    Selected { selection_digest: String },
    Killed { reason: KillReason },
    Unexplained { digest: String },
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct RecordFact {
    pub id: String,
    pub kind: String,
    pub proof_digest: String,
    pub verification_status: VerificationStatus,
    pub region: Vec<RecordRegionTerm>,
    pub support_edges: Vec<String>,
    pub widened_from: usize,
    pub widened_to: usize,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct RecordRegionTerm {
    pub pkg: String,
    pub versions: String,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct SelectionDerivation {
    pub pkg: String,
    pub version: String,
    pub digest: String,
    pub killed_higher: Vec<CandidateKill>,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct CandidateKill {
    pub version: String,
    pub reason: KillReason,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
#[repr(C)]
pub enum KillReason {
    RootConstraint {
        digest: String,
        pins: Vec<String>,
    },
    Decree {
        digest: String,
        pins: Vec<String>,
    },
    DependencyConstraint {
        digest: String,
        edges: Vec<String>,
        pins: Vec<String>,
    },
    LearnedExclusion {
        fact_id: String,
        proof_root: String,
        proof_digest: String,
    },
}

impl KillReason {
    #[must_use]
    pub fn digest(&self) -> &str {
        match self {
            Self::RootConstraint { digest, .. }
            | Self::Decree { digest, .. }
            | Self::DependencyConstraint { digest, .. } => digest,
            Self::LearnedExclusion { proof_digest, .. } => proof_digest,
        }
    }
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct RecordSelection {
    pub pkg: String,
    pub version: String,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
#[repr(C)]
pub enum CounterfactualQuery {
    DropDependency { package: String },
    DecreeVersion { package: String, version: String },
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
#[repr(C)]
pub enum ManifestEdit {
    RemoveRootDependency {
        package: String,
    },
    AddOrReplaceDecree {
        package: String,
        requirement: String,
    },
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct SelectionDiff {
    pub added: Vec<RecordSelection>,
    pub removed: Vec<RecordSelection>,
    pub changed: Vec<ChangedSelection>,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct ChangedSelection {
    pub pkg: String,
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct CounterfactualResult {
    pub query: CounterfactualQuery,
    pub manifest_edit: ManifestEdit,
    pub selected_diff: SelectionDiff,
    pub warm_decisions: usize,
    pub cold_decisions: usize,
    pub warm_conflicts: usize,
    pub cold_conflicts: usize,
    pub warm_facts_used: usize,
    pub warm_facts_scoped_out: usize,
    pub oracle_selected_equal: bool,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
#[repr(C)]
pub enum DecreeOrigin {
    Root,
    User,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
#[repr(C)]
pub enum SolveStep {
    Demand {
        pkg: String,
        requirement: String,
    },
    Decree {
        origin: DecreeOrigin,
        pkg: String,
        requirement: String,
    },
    Intersect {
        pkg: String,
        before: String,
        applied: String,
        after: String,
    },
    ForcedPick {
        pkg: String,
        version: String,
    },
    Expand {
        pkg: String,
        version: String,
        candidates_remaining: usize,
        manifests_read: usize,
    },
    Conflict {
        empty_pkg: String,
        edges_involved: Vec<String>,
    },
    Learn {
        region: Vec<RecordRegionTerm>,
        support_edges: Vec<String>,
        widened_from: usize,
        widened_to: usize,
    },
    Solution {
        packages: Vec<RecordSelection>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LearnedNoGood {
    pub region: Region,
    pub derivation: DerivationId,
    pub support: BTreeSet<EdgeKey>,
    pub point_versions: usize,
    pub widened_versions: usize,
    pub kind: LearnedKind,
    pub proof_root: ProofNodeId,
    pub proof_digest: ProofDigest,
    pub verification_status: VerificationStatus,
    pub widen_witness: Option<WidenWitness>,
}

impl LearnedNoGood {
    #[must_use]
    pub fn widening_factor(&self) -> f64 {
        if self.point_versions == 0 {
            0.0
        } else {
            self.widened_versions as f64 / self.point_versions as f64
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SolveError {
    NoSolution {
        decisions: usize,
        conflicts: usize,
        manifests_read: usize,
        candidates_existing: usize,
        trace: Vec<String>,
        learned: Vec<LearnedNoGood>,
        derivations: DerivationStore,
        clauses: ClauseStore,
    },
}

#[derive(Clone, Debug)]
pub struct CompiledIndex {
    pub sources: SourceInterner,
    pub packages: PackageInterner,
    pub features: FeatureInterner,
    source: SourceIx,
    labels: BTreeMap<PkgIx, String>,
    base_names: BTreeMap<PkgIx, String>,
    compat_classes: BTreeMap<PkgIx, CompatClass>,
    package_by_label: BTreeMap<String, PkgIx>,
    package_by_domain: BTreeMap<(String, CompatClass), PkgIx>,
    domains_by_name: BTreeMap<String, BTreeSet<PkgIx>>,
    versions: BTreeMap<PkgIx, Vec<Version>>,
    manifests: BTreeMap<(PkgIx, Version), Manifest>,
    clauses: ClauseStore,
    compiled: Vec<CompiledClause>,
    clause_support: BTreeMap<ClauseId, EdgeKey>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Manifest {
    deps: Vec<DepLine>,
    features: BTreeMap<String, Vec<String>>,
    yanked: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DepLine {
    key: String,
    actual: String,
    req: String,
    features: Vec<String>,
    optional: bool,
    default_features: bool,
    target: Option<String>,
    kind: String,
    class_hint: Option<CompatClass>,
}

#[derive(Clone, Debug)]
struct CompiledClause {
    id: ClauseId,
    antecedents: Vec<Guard>,
    consequent: Consequent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Guard {
    InGraph(PkgIx),
    Selected(PkgIx, Version),
    Feature(PkgIx, FeatIx),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Consequent {
    InGraph(PkgIx),
    VersionSet(PkgIx, VersionSet),
    Feature(PkgIx, FeatIx),
}

#[derive(Clone, Debug, Default)]
struct Domain {
    active: bool,
    allowed: VersionSet,
    reasons: Vec<Premise>,
    selected: Option<Version>,
    selected_reason: Option<Premise>,
}

#[derive(Clone, Debug, Default)]
struct State {
    domains: BTreeMap<PkgIx, Domain>,
    features: BTreeSet<(PkgIx, FeatIx)>,
    feature_reasons: BTreeMap<(PkgIx, FeatIx), Vec<Premise>>,
    hypotheses: Vec<Hypothesis>,
    applied_clauses: BTreeSet<ClauseId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Hypothesis {
    id: u32,
    pkg: PkgIx,
    version: Version,
}

#[derive(Clone, Debug)]
struct Conflict {
    premises: Vec<Premise>,
    selected: BTreeMap<PkgIx, Version>,
    decision: Option<Hypothesis>,
}

#[derive(Clone, Debug)]
struct Solver<'a> {
    index: &'a CompiledIndex,
    problem_kind: LearnedKind,
    feature_exclusions: Vec<(PkgIx, FeatIx, FeatIx)>,
    exclusions: ExclusionStore,
    derivations: DerivationStore,
    learned: Vec<LearnedNoGood>,
    conflicts: usize,
    decisions: usize,
    manifests_read: usize,
    candidates_existing: usize,
    next_hypothesis: u32,
    steps: Vec<SolveStep>,
    trace: Vec<String>,
    #[cfg(feature = "perf-diagnostics")]
    diagnostics: SolverDiagnostics,
}

#[cfg(feature = "perf-diagnostics")]
#[derive(Clone, Debug, Default)]
struct SolverDiagnostics {
    decision_by_pkg: BTreeMap<String, usize>,
    decision_candidates_by_pkg: BTreeMap<String, usize>,
    conflict_by_decision_pkg: BTreeMap<String, usize>,
    conflict_support_by_edge: BTreeMap<String, usize>,
    learned_by_region: BTreeMap<String, usize>,
    learned_by_support: BTreeMap<String, usize>,
    exclusion_scans: usize,
    exclusion_prunes: usize,
    exclusion_conflicts: usize,
    learned_point_versions: usize,
    learned_widened_versions: usize,
    widened_facts: usize,
}

#[cfg(feature = "perf-diagnostics")]
impl SolverDiagnostics {
    fn record_decision(&mut self, pkg: &str, candidate_count: usize) {
        *self.decision_by_pkg.entry(pkg.to_owned()).or_default() += 1;
        *self
            .decision_candidates_by_pkg
            .entry(pkg.to_owned())
            .or_default() += candidate_count;
    }

    fn record_conflict(&mut self, index: &CompiledIndex, conflict: &Conflict) {
        let decision_pkg = conflict
            .decision
            .as_ref()
            .and_then(|hypothesis| index.name(hypothesis.pkg))
            .unwrap_or("<root>")
            .to_owned();
        *self
            .conflict_by_decision_pkg
            .entry(decision_pkg)
            .or_default() += 1;
        for edge in support_from_premises(index, &conflict.premises) {
            let label = format!("{}->{}", edge.parent, edge.dep);
            *self.conflict_support_by_edge.entry(label).or_default() += 1;
        }
    }

    fn record_learn(
        &mut self,
        index: &CompiledIndex,
        region: &Region,
        support: &BTreeSet<EdgeKey>,
        point_versions: usize,
        widened_versions: usize,
    ) {
        *self
            .learned_by_region
            .entry(display_region(index, region))
            .or_default() += 1;
        *self
            .learned_by_support
            .entry(display_support(support))
            .or_default() += 1;
        self.learned_point_versions += point_versions;
        self.learned_widened_versions += widened_versions;
        if widened_versions > point_versions {
            self.widened_facts += 1;
        }
    }

    fn emit_snapshot(&self, decisions: usize, conflicts: usize, learned: usize) {
        let duplicate_regions = self
            .learned_by_region
            .values()
            .filter(|count| **count > 1)
            .count();
        tracing::info!(
            target: "rodin_perf_diag",
            decisions,
            conflicts,
            learned,
            exclusion_scans = self.exclusion_scans,
            exclusion_prunes = self.exclusion_prunes,
            exclusion_conflicts = self.exclusion_conflicts,
            widened_facts = self.widened_facts,
            learned_point_versions = self.learned_point_versions,
            learned_widened_versions = self.learned_widened_versions,
            duplicate_regions,
            decision_top = %top_counts(&self.decision_by_pkg, 8),
            conflict_top = %top_counts(&self.conflict_by_decision_pkg, 8),
            support_top = %top_counts(&self.conflict_support_by_edge, 8),
            learned_region_top = %top_counts(&self.learned_by_region, 5),
            "rodin perf diagnostic snapshot"
        );
    }
}

impl CompiledIndex {
    #[must_use]
    pub fn empty() -> Self {
        let mut sources = SourceInterner::new();
        let source = sources.intern(CRATES_IO);
        Self {
            sources,
            packages: PackageInterner::new(),
            features: FeatureInterner::new(),
            source,
            labels: BTreeMap::new(),
            base_names: BTreeMap::new(),
            compat_classes: BTreeMap::new(),
            package_by_label: BTreeMap::new(),
            package_by_domain: BTreeMap::new(),
            domains_by_name: BTreeMap::new(),
            versions: BTreeMap::new(),
            manifests: BTreeMap::new(),
            clauses: ClauseStore::new(),
            compiled: Vec::new(),
            clause_support: BTreeMap::new(),
        }
    }

    pub fn from_sparse(crates: &BTreeMap<String, rodin_survey::CrateData>) -> Result<Self> {
        let mut out = Self::empty();
        for crate_data in crates.values() {
            for (version, entry) in rodin_survey::sorted_non_yanked_entries(crate_data)? {
                let pkg = out.intern_package_version(&crate_data.name, &version);
                let mut deps = Vec::new();
                for dep in &entry.deps {
                    if dep.kind != "normal" {
                        continue;
                    }
                    let actual = rodin_survey::actual_dep_name(dep);
                    let mut features = dep.features.clone();
                    features.sort();
                    deps.push(DepLine {
                        key: dep.name.clone(),
                        actual,
                        req: dep.req.clone(),
                        features,
                        optional: dep.optional,
                        default_features: dep.default_features,
                        target: dep.target.clone(),
                        kind: dep.kind.clone(),
                        class_hint: None,
                    });
                }
                deps.sort_by(|left, right| {
                    left.actual
                        .cmp(&right.actual)
                        .then_with(|| left.key.cmp(&right.key))
                        .then_with(|| left.req.cmp(&right.req))
                });
                out.manifests.insert(
                    (pkg, version.clone()),
                    Manifest {
                        deps,
                        features: merged_features(entry),
                        yanked: entry.yanked,
                    },
                );
                out.versions.entry(pkg).or_default().push(version.clone());
            }
        }
        for versions in out.versions.values_mut() {
            versions.sort();
            versions.dedup();
        }
        out.compile()?;
        Ok(out)
    }

    #[must_use]
    pub fn package(&self, name: &str) -> Option<PkgIx> {
        let key = name.to_ascii_lowercase();
        if let Some(pkg) = self.package_by_label.get(&key) {
            return Some(*pkg);
        }
        let domains = self.domains_by_name.get(&key)?;
        (domains.len() == 1)
            .then(|| domains.iter().next().copied())
            .flatten()
    }

    #[must_use]
    pub fn package_for_req(&self, name: &str, req: &str) -> Option<PkgIx> {
        let set = parse_req(req).ok()?;
        self.package_for_set(name, &set, None)
    }

    fn package_for_set(
        &self,
        name: &str,
        set: &VersionSet,
        class_hint: Option<CompatClass>,
    ) -> Option<PkgIx> {
        let key = name.to_ascii_lowercase();
        if let Some(class) = class_hint {
            let pkg = self.package_by_domain.get(&(key, class)).copied()?;
            return self
                .versions(pkg)
                .iter()
                .any(|version| set.contains(version))
                .then_some(pkg);
        }
        self.domains_by_name
            .get(&key)?
            .iter()
            .copied()
            .filter(|pkg| {
                self.versions(*pkg)
                    .iter()
                    .any(|version| set.contains(version))
            })
            .max_by_key(|pkg| {
                self.versions(*pkg)
                    .iter()
                    .filter(|version| set.contains(version))
                    .max()
                    .cloned()
            })
    }

    #[must_use]
    pub fn name(&self, pkg: PkgIx) -> Option<&str> {
        self.labels.get(&pkg).map(String::as_str)
    }

    #[must_use]
    pub fn base_name(&self, pkg: PkgIx) -> Option<&str> {
        self.base_names.get(&pkg).map(String::as_str)
    }

    #[must_use]
    pub fn compat_class(&self, pkg: PkgIx) -> Option<CompatClass> {
        self.compat_classes.get(&pkg).copied()
    }

    #[must_use]
    pub fn versions(&self, pkg: PkgIx) -> &[Version] {
        self.versions.get(&pkg).map(Vec::as_slice).unwrap_or(&[])
    }

    #[must_use]
    pub fn package_count(&self) -> usize {
        self.labels.len()
    }

    #[must_use]
    pub fn version_count(&self) -> usize {
        self.versions.values().map(Vec::len).sum()
    }

    pub fn add_package_version(
        &mut self,
        name: impl Into<String>,
        version: Version,
        deps: Vec<SyntheticDep>,
        features: BTreeMap<String, Vec<String>>,
    ) {
        let name = name.into().to_ascii_lowercase();
        let pkg = self.intern_package_version(&name, &version);
        let mut lines = Vec::new();
        for dep in deps {
            let actual = dep.package.clone().unwrap_or_else(|| dep.name.clone());
            let actual = actual.to_ascii_lowercase();
            let mut features = dep.features;
            features.sort();
            lines.push(DepLine {
                key: dep.name,
                actual,
                req: dep.req,
                features,
                optional: dep.optional,
                default_features: dep.default_features,
                target: dep.target,
                kind: dep.kind,
                class_hint: dep.class_hint,
            });
        }
        lines.sort_by(|left, right| left.actual.cmp(&right.actual));
        self.versions.entry(pkg).or_default().push(version.clone());
        self.versions.get_mut(&pkg).expect("inserted").sort();
        self.manifests.insert(
            (pkg, version),
            Manifest {
                deps: lines,
                features,
                yanked: false,
            },
        );
    }

    pub fn finish_synthetic(&mut self) -> Result<()> {
        for versions in self.versions.values_mut() {
            versions.sort();
            versions.dedup();
        }
        self.compile()
    }

    fn intern_package_version(&mut self, name: &str, version: &Version) -> PkgIx {
        let key = name.to_ascii_lowercase();
        let compat = CompatClass::from_version(version);
        if let Some(pkg) = self.package_by_domain.get(&(key.clone(), compat)) {
            return *pkg;
        }
        let label = domain_label(&key, compat);
        let pkg = self
            .packages
            .intern(PkgId::domain(self.source, key.clone(), compat));
        self.package_by_label.insert(label.clone(), pkg);
        self.package_by_domain.insert((key.clone(), compat), pkg);
        self.domains_by_name
            .entry(key.clone())
            .or_default()
            .insert(pkg);
        self.labels.insert(pkg, label);
        self.base_names.insert(pkg, key);
        self.compat_classes.insert(pkg, compat);
        pkg
    }

    fn intern_feature(&mut self, feature: &str) -> FeatIx {
        self.features.intern(feature.to_owned())
    }

    fn compile(&mut self) -> Result<()> {
        self.clauses = ClauseStore::new();
        self.compiled.clear();
        self.clause_support.clear();
        let manifests: Vec<_> = self
            .manifests
            .iter()
            .map(|((pkg, version), manifest)| (*pkg, version.clone(), manifest.clone()))
            .collect();
        for (pkg, version, manifest) in manifests {
            let explicit_optional_dep_features = explicit_optional_dep_features(&manifest);
            for dep in &manifest.deps {
                let req = parse_req(&dep.req)?;
                let Some(dep_pkg) = self.package_for_set(&dep.actual, &req, dep.class_hint) else {
                    continue;
                };
                let support = EdgeKey {
                    parent: self.name(pkg).unwrap_or_default().to_owned(),
                    dep: self.name(dep_pkg).unwrap_or_default().to_owned(),
                };
                if !dep.optional {
                    self.push_clause(
                        vec![Guard::InGraph(pkg), Guard::Selected(pkg, version.clone())],
                        Consequent::InGraph(dep_pkg),
                        Some(support.clone()),
                    );
                    self.push_clause(
                        vec![Guard::InGraph(pkg), Guard::Selected(pkg, version.clone())],
                        Consequent::VersionSet(dep_pkg, req.clone()),
                        Some(support.clone()),
                    );
                    self.push_dependency_feature_clauses(pkg, &version, dep, dep_pkg, &support);
                }
                if dep.optional && !explicit_optional_dep_features.contains(&dep.key) {
                    self.push_feature_enable_clause(
                        pkg,
                        &version,
                        &dep.kind,
                        &dep.key,
                        &format!("dep:{}", dep.key),
                    )
                    .wrap_err_with(|| {
                        format!(
                            "compiling implicit optional feature {} {version} {} -> dep:{}",
                            self.name(pkg).unwrap_or_default(),
                            dep.key,
                            dep.key
                        )
                    })?;
                }
            }
            for (feature, enables) in &manifest.features {
                for scope in FEATURE_SCOPES {
                    for enable in enables {
                        self.push_feature_enable_clause(pkg, &version, scope, feature, enable)
                            .wrap_err_with(|| {
                                format!(
                                    "compiling feature enable {} {version} {scope}:{feature} -> {enable}",
                                    self.name(pkg).unwrap_or_default()
                                )
                            })?;
                    }
                }
            }
        }
        Ok(())
    }

    fn push_dependency_feature_clauses(
        &mut self,
        pkg: PkgIx,
        version: &Version,
        dep: &DepLine,
        dep_pkg: PkgIx,
        support: &EdgeKey,
    ) {
        let scope = dep.kind.as_str();
        if dep.default_features {
            let default = self.intern_feature(&scoped_feature_name(scope, "default"));
            self.push_clause(
                vec![Guard::InGraph(pkg), Guard::Selected(pkg, version.clone())],
                Consequent::Feature(dep_pkg, default),
                Some(support.clone()),
            );
        }
        for feature in &dep.features {
            let feat = self.intern_feature(&scoped_feature_name(scope, feature));
            self.push_clause(
                vec![Guard::InGraph(pkg), Guard::Selected(pkg, version.clone())],
                Consequent::Feature(dep_pkg, feat),
                Some(support.clone()),
            );
        }
    }

    fn push_feature_enable_clause(
        &mut self,
        pkg: PkgIx,
        version: &Version,
        scope: &str,
        feature: &str,
        enable: &str,
    ) -> Result<()> {
        let feat = self.intern_feature(&scoped_feature_name(scope, feature));
        if let Some(dep_name) = enable.strip_prefix("dep:") {
            let Some(dep) = self.dependency_line(pkg, version, scope, dep_name) else {
                return Ok(());
            };
            let req = parse_req(&dep.req)?;
            let Some(dep_pkg) = self.package_for_set(&dep.actual, &req, dep.class_hint) else {
                return Ok(());
            };
            let support = EdgeKey {
                parent: self.name(pkg).unwrap_or_default().to_owned(),
                dep: self.name(dep_pkg).unwrap_or_default().to_owned(),
            };
            self.push_clause(
                vec![
                    Guard::Feature(pkg, feat),
                    Guard::Selected(pkg, version.clone()),
                ],
                Consequent::InGraph(dep_pkg),
                Some(support.clone()),
            );
            self.push_clause(
                vec![
                    Guard::Feature(pkg, feat),
                    Guard::Selected(pkg, version.clone()),
                ],
                Consequent::VersionSet(dep_pkg, req),
                Some(support.clone()),
            );
            self.push_dependency_feature_clauses(pkg, version, &dep, dep_pkg, &support);
        } else if let Some((dep_name, dep_feature)) = enable.split_once("?/") {
            let Some(dep) = self.dependency_line(pkg, version, scope, dep_name) else {
                return Ok(());
            };
            let req = parse_req(&dep.req)?;
            let Some(dep_pkg) = self.package_for_set(&dep.actual, &req, dep.class_hint) else {
                return Ok(());
            };
            let dep_feat = self.intern_feature(&scoped_feature_name(scope, dep_feature));
            let support = EdgeKey {
                parent: self.name(pkg).unwrap_or_default().to_owned(),
                dep: self.name(dep_pkg).unwrap_or_default().to_owned(),
            };
            self.push_clause(
                vec![
                    Guard::Feature(pkg, feat),
                    Guard::Selected(pkg, version.clone()),
                    Guard::InGraph(dep_pkg),
                ],
                Consequent::Feature(dep_pkg, dep_feat),
                Some(support),
            );
        } else if let Some((dep_name, dep_feature)) = enable.split_once('/') {
            let Some(dep) = self.dependency_line(pkg, version, scope, dep_name) else {
                return Ok(());
            };
            let req = parse_req(&dep.req)?;
            let Some(dep_pkg) = self.package_for_set(&dep.actual, &req, dep.class_hint) else {
                return Ok(());
            };
            let dep_feat = self.intern_feature(&scoped_feature_name(scope, dep_feature));
            let support = EdgeKey {
                parent: self.name(pkg).unwrap_or_default().to_owned(),
                dep: self.name(dep_pkg).unwrap_or_default().to_owned(),
            };
            self.push_clause(
                vec![
                    Guard::Feature(pkg, feat),
                    Guard::Selected(pkg, version.clone()),
                ],
                Consequent::InGraph(dep_pkg),
                Some(support.clone()),
            );
            self.push_clause(
                vec![
                    Guard::Feature(pkg, feat),
                    Guard::Selected(pkg, version.clone()),
                ],
                Consequent::VersionSet(dep_pkg, req),
                Some(support.clone()),
            );
            self.push_clause(
                vec![
                    Guard::Feature(pkg, feat),
                    Guard::Selected(pkg, version.clone()),
                ],
                Consequent::Feature(dep_pkg, dep_feat),
                Some(support),
            );
        } else {
            let local_feat = self.intern_feature(&scoped_feature_name(scope, enable));
            self.push_clause(
                vec![
                    Guard::Feature(pkg, feat),
                    Guard::Selected(pkg, version.clone()),
                ],
                Consequent::Feature(pkg, local_feat),
                None,
            );
        }
        Ok(())
    }

    fn dependency_line(
        &self,
        pkg: PkgIx,
        version: &Version,
        scope: &str,
        key: &str,
    ) -> Option<DepLine> {
        self.manifests
            .get(&(pkg, version.clone()))
            .and_then(|manifest| {
                manifest
                    .deps
                    .iter()
                    .find(|dep| dep.key == key && dep.kind == scope)
                    .cloned()
            })
    }

    fn push_clause(
        &mut self,
        antecedents: Vec<Guard>,
        consequent: Consequent,
        support: Option<EdgeKey>,
    ) {
        let clause = Clause {
            antecedents: antecedents
                .iter()
                .map(|guard| self.guard_atom(guard))
                .collect(),
            consequent: Some(self.consequent_atom(&consequent)),
        };
        let id = self.clauses.append(clause);
        if let Some(edge) = support.clone() {
            self.clause_support.insert(id, edge);
        }
        self.compiled.push(CompiledClause {
            id,
            antecedents,
            consequent,
        });
    }

    fn guard_atom(&self, guard: &Guard) -> Atom {
        match guard {
            Guard::InGraph(pkg) => Atom::InGraph(*pkg),
            Guard::Selected(pkg, version) => Atom::Ver(*pkg, VersionSet::exact(version.clone())),
            Guard::Feature(pkg, feat) => Atom::Feat(*pkg, *feat),
        }
    }

    fn consequent_atom(&self, consequent: &Consequent) -> Atom {
        match consequent {
            Consequent::InGraph(pkg) => Atom::InGraph(*pkg),
            Consequent::VersionSet(pkg, set) => Atom::Ver(*pkg, set.clone()),
            Consequent::Feature(pkg, feat) => Atom::Feat(*pkg, *feat),
        }
    }

    #[must_use]
    pub fn edge_state(&self, parent: &str, version: &Version, dep: &str) -> Vec<EdgeSignature> {
        let Some(pkg) = self.package(parent) else {
            return Vec::new();
        };
        let Some(manifest) = self.manifests.get(&(pkg, version.clone())) else {
            return Vec::new();
        };
        let dep_actual = self
            .package(dep)
            .and_then(|pkg| self.base_name(pkg))
            .unwrap_or(dep);
        let mut out: Vec<_> = manifest
            .deps
            .iter()
            .filter(|line| line.actual == dep_actual)
            .map(|line| EdgeSignature {
                req: line.req.clone(),
                features: line.features.clone(),
                optional: line.optional,
                default_features: line.default_features,
                target: line.target.clone(),
                kind: line.kind.clone(),
            })
            .collect();
        out.sort();
        out
    }

    #[must_use]
    pub fn widen_region(
        &self,
        selected: &BTreeMap<PkgIx, Version>,
        support: &BTreeSet<EdgeKey>,
        base: &Region,
    ) -> (Region, usize, usize) {
        let mut region = base.clone();
        let mut point = 1_usize;
        let mut widened = 1_usize;
        for edge in support {
            let Some(parent_pkg) = self.package(&edge.parent) else {
                continue;
            };
            let Some(selected_version) = selected.get(&parent_pkg) else {
                continue;
            };
            let selected_state = self.edge_state(&edge.parent, selected_version, &edge.dep);
            let mut versions = Vec::new();
            for version in self.versions(parent_pkg) {
                if self.edge_state(&edge.parent, version, &edge.dep) == selected_state {
                    versions.push(version.clone());
                }
            }
            point = point.saturating_mul(1);
            widened = widened.saturating_mul(versions.len().max(1));
            region
                .versions
                .insert(parent_pkg, version_set_from_points(&versions));
        }
        (region, point, widened)
    }

    fn support_packages(&self, support: &BTreeSet<EdgeKey>) -> BTreeSet<PkgIx> {
        support
            .iter()
            .flat_map(|edge| [self.package(&edge.parent), self.package(&edge.dep)])
            .flatten()
            .collect()
    }

    #[must_use]
    pub fn fact_scope(&self) -> FactScope {
        let mut canonical = String::new();
        for (pkg, versions) in &self.versions {
            let _ = write!(canonical, "pkg:{}:", self.name(*pkg).unwrap_or("<unknown>"));
            for version in versions {
                let _ = write!(canonical, "{version},");
            }
            canonical.push('\n');
        }
        for ((pkg, version), manifest) in &self.manifests {
            let _ = write!(
                canonical,
                "manifest:{}@{}:yanked={}:",
                self.name(*pkg).unwrap_or("<unknown>"),
                version,
                manifest.yanked
            );
            for dep in &manifest.deps {
                let _ = write!(
                    canonical,
                    "{}|{}|{}|{:?}|{}|{}|{:?}|{};",
                    dep.key,
                    dep.actual,
                    dep.req,
                    dep.features,
                    dep.optional,
                    dep.default_features,
                    dep.target,
                    dep.kind
                );
                if let Some(class) = dep.class_hint {
                    let _ = write!(canonical, "class:{class};");
                }
            }
            for (feature, enables) in &manifest.features {
                let _ = write!(canonical, "feature:{feature}={enables:?};");
            }
            canonical.push('\n');
        }
        FactScope {
            index_digest: digest_str(&canonical),
        }
    }

    fn clause_witness(&self, id: ClauseId) -> Option<ClauseWitness> {
        let clause = self.clauses.get(id)?;
        let consequent = match &clause.consequent {
            Some(atom) => Some(self.atom_witness(atom)?),
            None => None,
        };
        Some(ClauseWitness {
            antecedents: clause
                .antecedents
                .iter()
                .map(|atom| self.atom_witness(atom))
                .collect::<Option<Vec<_>>>()?,
            consequent,
            support: self.clause_support.get(&id).cloned(),
        })
    }

    fn rebind_clause(&self, witness: &ClauseWitness) -> Option<ClauseId> {
        (0..self.clauses.len())
            .map(|ix| ClauseId(ix as u32))
            .find(|id| self.clause_witness(*id).as_ref() == Some(witness))
    }

    fn atom_witness(&self, atom: &Atom) -> Option<AtomWitness> {
        match atom {
            Atom::InGraph(pkg) => Some(AtomWitness::InGraph {
                package: self.name(*pkg)?.to_owned(),
            }),
            Atom::Ver(pkg, versions) => Some(AtomWitness::Ver {
                package: self.name(*pkg)?.to_owned(),
                versions: display_version_set(versions),
            }),
            Atom::Feat(pkg, feat) => Some(AtomWitness::Feat {
                package: self.name(*pkg)?.to_owned(),
                feature: self.features.resolve_str(*feat)?.to_owned(),
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntheticDep {
    pub name: String,
    pub req: String,
    pub features: Vec<String>,
    pub optional: bool,
    pub default_features: bool,
    pub package: Option<String>,
    pub kind: String,
    pub target: Option<String>,
    pub class_hint: Option<CompatClass>,
}

impl SyntheticDep {
    #[must_use]
    pub fn normal(name: impl Into<String>, req: impl Into<String>) -> Self {
        Self {
            name: name.into().to_ascii_lowercase(),
            req: req.into(),
            features: Vec::new(),
            optional: false,
            default_features: true,
            package: None,
            kind: "normal".to_owned(),
            target: None,
            class_hint: None,
        }
    }

    #[must_use]
    pub fn optional(mut self, optional: bool) -> Self {
        self.optional = optional;
        self
    }

    #[must_use]
    pub fn feature(mut self, feature: impl Into<String>) -> Self {
        self.features.push(feature.into());
        self
    }

    #[must_use]
    pub fn class_hint(mut self, class: CompatClass) -> Self {
        self.class_hint = Some(class);
        self
    }

    #[must_use]
    pub fn kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = kind.into();
        self
    }

    #[must_use]
    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }
}

pub fn solve(
    index: &CompiledIndex,
    problem: Problem,
) -> std::result::Result<SolveResult, Box<SolveError>> {
    let mut solver = Solver::new(index, problem.learned_kind);
    for fact in &problem.warm_facts.facts {
        solver.install_warm_fact(fact.clone());
    }
    let mut state = State::default();
    if let Err(conflict) = solver.seed_problem(&problem, &mut state) {
        solver.conflicts += 1;
        solver.learn(conflict);
        return Err(Box::new(solver.no_solution()));
    }
    match solver.search(state) {
        Ok(state) => Ok(solver.finish(state)),
        Err(()) => Err(Box::new(solver.no_solution())),
    }
}

impl<'a> Solver<'a> {
    fn new(index: &'a CompiledIndex, problem_kind: LearnedKind) -> Self {
        let mut derivations = DerivationStore::new();
        for id in (0..index.clauses.len()).map(|ix| ClauseId(ix as u32)) {
            derivations.note_clause(id);
        }
        Self {
            index,
            problem_kind,
            feature_exclusions: Vec::new(),
            exclusions: ExclusionStore::new(),
            derivations,
            learned: Vec::new(),
            conflicts: 0,
            decisions: 0,
            manifests_read: 0,
            candidates_existing: 0,
            next_hypothesis: 0,
            steps: Vec::new(),
            trace: Vec::new(),
            #[cfg(feature = "perf-diagnostics")]
            diagnostics: SolverDiagnostics::default(),
        }
    }

    fn install_warm_fact(&mut self, verified: VerifiedWarmFact) {
        let mut lowered = BTreeMap::new();
        let mut next_internal_exclusion = 1_000_000_u32;
        let Ok(derivation) = lower_proof_node(
            self.index,
            &verified.proofs,
            &verified.proof_root,
            &mut self.derivations,
            &mut lowered,
            &mut next_internal_exclusion,
        ) else {
            return;
        };
        let mut fact = verified.fact;
        fact.derivation = derivation;
        let id = self.exclusions.append(Exclusion {
            region: fact.region.clone(),
            derivation,
        });
        self.derivations.note_exclusion(id, derivation);
        self.learned.push(fact);
    }

    fn seed_problem(&mut self, problem: &Problem, state: &mut State) -> Result<(), Conflict> {
        for root in &problem.roots {
            let Some(pkg) = self.index.package_for_req(&root.name, &root.req) else {
                return Err(Conflict {
                    premises: Vec::new(),
                    selected: selected_versions(state),
                    decision: None,
                });
            };
            let requirement = display_req(&root.req);
            self.trace
                .push(format!("demand {} {requirement}", root.name));
            self.steps.push(SolveStep::Demand {
                pkg: root.name.clone(),
                requirement: requirement.clone(),
            });
            let activation_pin = PinRef(format!("root:{}", root.name));
            self.derivations.note_pin(activation_pin.clone());
            self.activate(state, pkg, Premise::Pin(activation_pin));
            self.steps.push(SolveStep::Decree {
                origin: DecreeOrigin::Root,
                pkg: self
                    .index
                    .name(pkg)
                    .unwrap_or(root.name.as_str())
                    .to_owned(),
                requirement,
            });
            let pin = PinRef(format!("root:{}:{}", root.name, root.req));
            self.derivations.note_pin(pin.clone());
            self.intersect_domain(
                state,
                pkg,
                parse_req(&root.req).map_err(|_| Conflict {
                    premises: vec![Premise::Pin(pin.clone())],
                    selected: selected_versions(state),
                    decision: None,
                })?,
                Premise::Pin(pin),
            )?;
            if root.default_features {
                let feat = self.index.features.get_str("default").unwrap_or(FeatIx(0));
                self.enable_feature(state, pkg, feat, Vec::new());
            }
            for feature in &root.features {
                let Some(feat) = self.index.features.get_str(feature) else {
                    continue;
                };
                self.enable_feature(state, pkg, feat, Vec::new());
            }
        }
        for decree in &problem.decrees {
            let Some(pkg) = self.index.package_for_req(&decree.name, &decree.req) else {
                continue;
            };
            let requirement = display_req(&decree.req);
            self.trace
                .push(format!("decree {} {requirement}", decree.name));
            self.steps.push(SolveStep::Decree {
                origin: DecreeOrigin::User,
                pkg: self
                    .index
                    .name(pkg)
                    .unwrap_or(decree.name.as_str())
                    .to_owned(),
                requirement,
            });
            let activation_pin = PinRef(format!("decree:{}", decree.name));
            self.derivations.note_pin(activation_pin.clone());
            self.activate(state, pkg, Premise::Pin(activation_pin));
            let pin = PinRef(format!("decree:{}:{}", decree.name, decree.req));
            self.derivations.note_pin(pin.clone());
            self.intersect_domain(
                state,
                pkg,
                parse_req(&decree.req).map_err(|_| Conflict {
                    premises: vec![Premise::Pin(pin.clone())],
                    selected: selected_versions(state),
                    decision: None,
                })?,
                Premise::Pin(pin),
            )?;
        }
        for exclusion in &problem.exclusions {
            let features: Vec<_> = exclusion.features.iter().cloned().collect();
            for (ix, left) in features.iter().enumerate() {
                for right in features.iter().skip(ix + 1) {
                    let Some(pkg) = self.index.package(&exclusion.package) else {
                        continue;
                    };
                    let Some(left_feat) = self.index.features.get_str(left) else {
                        continue;
                    };
                    let Some(right_feat) = self.index.features.get_str(right) else {
                        continue;
                    };
                    self.feature_exclusions.push((pkg, left_feat, right_feat));
                }
            }
        }
        Ok(())
    }

    fn search(&mut self, mut state: State) -> Result<State, ()> {
        if let Err(conflict) = self.propagate(&mut state) {
            self.conflicts += 1;
            self.learn(conflict);
            return Err(());
        }
        let Some(pkg) = self.next_package(&state) else {
            self.assert_solution(&state);
            return Ok(state);
        };
        let candidates = self.candidates(&state, pkg);
        if candidates.is_empty() {
            self.conflicts += 1;
            let premises = state
                .domains
                .get(&pkg)
                .map(|domain| domain.reasons.clone())
                .unwrap_or_default();
            self.steps.push(SolveStep::Conflict {
                empty_pkg: self.package_label(pkg).to_owned(),
                edges_involved: support_edge_ids(
                    self.index,
                    &support_from_premises(self.index, &premises),
                ),
            });
            self.learn(Conflict {
                premises,
                selected: selected_versions(&state),
                decision: last_hypothesis(&state),
            });
            return Err(());
        }
        let candidate_count = candidates.len();
        self.candidates_existing += candidate_count;
        for version in candidates.into_iter().rev() {
            let mut branch = state.clone();
            let hyp = self.next_hypothesis;
            self.next_hypothesis += 1;
            self.decisions += 1;
            self.manifests_read += 1;
            #[cfg(feature = "perf-diagnostics")]
            self.diagnostics
                .record_decision(self.index.name(pkg).unwrap_or_default(), candidate_count);
            if self.decisions.is_multiple_of(1000) {
                tracing::info!(
                    decisions = self.decisions,
                    conflicts = self.conflicts,
                    active_domains = branch.domains.len(),
                    enabled_features = branch.features.len(),
                    package = self.index.name(pkg).unwrap_or_default(),
                    %version,
                    "rodin solve progress"
                );
            }
            #[cfg(feature = "perf-diagnostics")]
            if self.decisions.is_multiple_of(perf_diag_interval()) {
                self.diagnostics
                    .emit_snapshot(self.decisions, self.conflicts, self.learned.len());
            }
            #[cfg(feature = "perf-diagnostics")]
            if let Some(limit) = perf_diag_decision_limit()
                && self.decisions >= limit
            {
                self.diagnostics
                    .emit_snapshot(self.decisions, self.conflicts, self.learned.len());
                tracing::warn!(
                    target: "rodin_perf_diag",
                    decisions = self.decisions,
                    conflicts = self.conflicts,
                    "rodin perf diagnostic decision limit reached"
                );
                return Err(());
            }
            self.trace.push(format!(
                "expand {} {} (highest remaining of {candidate_count} candidates) - reads 1 manifest",
                self.index.name(pkg).unwrap_or_default(),
                version
            ));
            self.steps.push(SolveStep::Expand {
                pkg: self.package_label(pkg).to_owned(),
                version: version.to_string(),
                candidates_remaining: candidate_count,
                manifests_read: 1,
            });
            branch.hypotheses.push(Hypothesis {
                id: hyp,
                pkg,
                version: version.clone(),
            });
            let domain = branch.domains.entry(pkg).or_default();
            domain.selected = Some(version.clone());
            domain.selected_reason = Some(Premise::Hypothesis(hyp));
            tracing::trace!(
                package = self.index.name(pkg).unwrap_or_default(),
                %version,
                hypothesis = hyp,
                "rodin decision"
            );
            if let Ok(solution) = self.search(branch) {
                return Ok(solution);
            }
        }
        Err(())
    }

    fn propagate(&mut self, state: &mut State) -> Result<(), Conflict> {
        loop {
            let before = state_fingerprint(state);
            self.apply_exclusions(state)?;
            self.force_singletons(state)?;
            for clause in &self.index.compiled {
                if state.applied_clauses.contains(&clause.id) {
                    continue;
                }
                if clause
                    .antecedents
                    .iter()
                    .all(|guard| self.guard_true(state, guard))
                {
                    state.applied_clauses.insert(clause.id);
                    let premise = Premise::Clause(clause.id);
                    self.apply_consequent(state, &clause.consequent, premise)?;
                }
            }
            self.check_feature_exclusions(state)?;
            if state_fingerprint(state) == before {
                break;
            }
        }
        Ok(())
    }

    fn check_feature_exclusions(&self, state: &State) -> Result<(), Conflict> {
        for &(pkg, left, right) in &self.feature_exclusions {
            if state.features.contains(&(pkg, left)) && state.features.contains(&(pkg, right)) {
                let mut premises = Vec::new();
                premises.extend(
                    state
                        .feature_reasons
                        .get(&(pkg, left))
                        .cloned()
                        .unwrap_or_default(),
                );
                premises.extend(
                    state
                        .feature_reasons
                        .get(&(pkg, right))
                        .cloned()
                        .unwrap_or_default(),
                );
                return Err(Conflict {
                    premises,
                    selected: selected_versions(state),
                    decision: last_hypothesis(state),
                });
            }
        }
        Ok(())
    }

    fn apply_exclusions(&mut self, state: &mut State) -> Result<(), Conflict> {
        let learned = self.learned.clone();
        for (ix, fact) in learned.iter().enumerate() {
            #[cfg(feature = "perf-diagnostics")]
            {
                self.diagnostics.exclusion_scans += 1;
            }
            let premise = Premise::Exclusion(ExclusionId(ix as u32));
            let mut unsatisfied = Vec::new();
            let mut disjoint = false;
            for (pkg, versions) in &fact.region.versions {
                match state
                    .domains
                    .get(pkg)
                    .and_then(|domain| domain.selected.as_ref())
                {
                    Some(version) if versions.contains(version) => {}
                    Some(_) => {
                        disjoint = true;
                        break;
                    }
                    None => unsatisfied.push((*pkg, versions.clone())),
                }
            }
            if disjoint {
                continue;
            }
            if unsatisfied.is_empty() {
                if fact.region.versions.keys().all(|pkg| {
                    state
                        .domains
                        .get(pkg)
                        .and_then(|d| d.selected.as_ref())
                        .is_some()
                }) {
                    #[cfg(feature = "perf-diagnostics")]
                    {
                        self.diagnostics.exclusion_conflicts += 1;
                    }
                    return Err(Conflict {
                        premises: vec![premise],
                        selected: selected_versions(state),
                        decision: last_hypothesis(state),
                    });
                }
                continue;
            }
            if unsatisfied.len() == 1 {
                let (pkg, excluded) = unsatisfied.pop().expect("one unsatisfied");
                if state.domains.get(&pkg).is_some_and(|domain| domain.active) {
                    let current = state
                        .domains
                        .get(&pkg)
                        .map(|domain| domain.allowed.clone())
                        .unwrap_or_else(VersionSet::universe);
                    let next = current.intersect(&excluded.complement(&current));
                    #[cfg(feature = "perf-diagnostics")]
                    if next != current {
                        self.diagnostics.exclusion_prunes += 1;
                    }
                    self.intersect_domain(state, pkg, next, premise)?;
                }
            }
        }
        Ok(())
    }

    fn force_singletons(&mut self, state: &mut State) -> Result<(), Conflict> {
        let active: Vec<_> = state
            .domains
            .iter()
            .filter_map(|(pkg, domain)| domain.active.then_some(*pkg))
            .collect();
        for pkg in active {
            let candidates = self.candidates(state, pkg);
            if candidates.is_empty() {
                let premises = state
                    .domains
                    .get(&pkg)
                    .map(|domain| domain.reasons.clone())
                    .unwrap_or_default();
                self.steps.push(SolveStep::Conflict {
                    empty_pkg: self.package_label(pkg).to_owned(),
                    edges_involved: support_edge_ids(
                        self.index,
                        &support_from_premises(self.index, &premises),
                    ),
                });
                return Err(Conflict {
                    premises,
                    selected: selected_versions(state),
                    decision: last_hypothesis(state),
                });
            }
            if candidates.len() == 1 {
                let domain = state.domains.entry(pkg).or_default();
                if domain.selected.is_none() {
                    let selected = candidates.into_iter().next();
                    if let Some(version) = &selected {
                        self.trace.push(format!(
                            "force {} {} (only candidate after propagation)",
                            self.package_label(pkg),
                            version
                        ));
                        self.steps.push(SolveStep::ForcedPick {
                            pkg: self.package_label(pkg).to_owned(),
                            version: version.to_string(),
                        });
                    }
                    domain.selected = selected;
                    domain.selected_reason = domain.reasons.last().cloned();
                }
            }
        }
        Ok(())
    }

    fn apply_consequent(
        &mut self,
        state: &mut State,
        consequent: &Consequent,
        premise: Premise,
    ) -> Result<(), Conflict> {
        match consequent {
            Consequent::InGraph(pkg) => {
                self.activate(state, *pkg, premise);
                Ok(())
            }
            Consequent::VersionSet(pkg, set) => {
                self.activate(state, *pkg, premise.clone());
                self.intersect_domain(state, *pkg, set.clone(), premise)
            }
            Consequent::Feature(pkg, feat) => {
                self.activate(state, *pkg, premise.clone());
                self.enable_feature(state, *pkg, *feat, vec![premise]);
                Ok(())
            }
        }
    }

    fn activate(&mut self, state: &mut State, pkg: PkgIx, premise: Premise) {
        let was_active = state.domains.get(&pkg).is_some_and(|domain| domain.active);
        let domain = state.domains.entry(pkg).or_insert_with(|| Domain {
            active: false,
            allowed: self.package_universe(pkg),
            reasons: Vec::new(),
            selected: None,
            selected_reason: None,
        });
        domain.active = true;
        domain.reasons.push(premise);
        if !was_active {
            self.trace
                .push(format!("domain {} becomes active", self.package_label(pkg)));
        }
    }

    fn intersect_domain(
        &mut self,
        state: &mut State,
        pkg: PkgIx,
        set: VersionSet,
        premise: Premise,
    ) -> Result<(), Conflict> {
        let old_allowed = state
            .domains
            .get(&pkg)
            .map(|domain| domain.allowed.clone())
            .unwrap_or_else(|| self.package_universe(pkg));
        let domain = state.domains.entry(pkg).or_insert_with(|| Domain {
            active: true,
            allowed: self.package_universe(pkg),
            reasons: Vec::new(),
            selected: None,
            selected_reason: None,
        });
        let next = domain.allowed.intersect(&set);
        let mut premises = domain.reasons.clone();
        premises.push(premise.clone());
        if next.is_empty()
            || self
                .index
                .versions(pkg)
                .iter()
                .all(|version| !next.contains(version))
        {
            let edges_involved =
                support_edge_ids(self.index, &support_from_premises(self.index, &premises));
            self.trace.push(format!(
                "{}: {} ∩ {} = empty -> conflict",
                self.package_label(pkg),
                display_version_set(&old_allowed),
                display_version_set(&set)
            ));
            self.steps.push(SolveStep::Conflict {
                empty_pkg: self.package_label(pkg).to_owned(),
                edges_involved,
            });
            return Err(Conflict {
                premises,
                selected: selected_versions(state),
                decision: last_hypothesis(state),
            });
        }
        if let Some(selected) = &domain.selected
            && !next.contains(selected)
        {
            let edges_involved =
                support_edge_ids(self.index, &support_from_premises(self.index, &premises));
            self.trace.push(format!(
                "{}: selected {} not in {} ∩ {} -> conflict",
                self.package_label(pkg),
                selected,
                display_version_set(&old_allowed),
                display_version_set(&set)
            ));
            self.steps.push(SolveStep::Conflict {
                empty_pkg: self.package_label(pkg).to_owned(),
                edges_involved,
            });
            return Err(Conflict {
                premises,
                selected: selected_versions(state),
                decision: last_hypothesis(state),
            });
        }
        if next != domain.allowed {
            let before = display_version_set(&old_allowed);
            let applied = display_version_set(&set);
            let after = display_version_set(&next);
            self.trace.push(format!(
                "{}: {} ∩ {} = {}",
                self.package_label(pkg),
                before,
                applied,
                after
            ));
            self.steps.push(SolveStep::Intersect {
                pkg: self.package_label(pkg).to_owned(),
                before,
                applied,
                after,
            });
        }
        domain.allowed = next;
        domain.reasons.push(premise);
        Ok(())
    }

    fn enable_feature(&self, state: &mut State, pkg: PkgIx, feat: FeatIx, reasons: Vec<Premise>) {
        if state.features.insert((pkg, feat)) {
            state.feature_reasons.insert((pkg, feat), reasons);
        }
    }

    fn guard_true(&self, state: &State, guard: &Guard) -> bool {
        match guard {
            Guard::InGraph(pkg) => state.domains.get(pkg).is_some_and(|domain| domain.active),
            Guard::Selected(pkg, version) => {
                state
                    .domains
                    .get(pkg)
                    .and_then(|domain| domain.selected.as_ref())
                    == Some(version)
            }
            Guard::Feature(pkg, feat) => state.features.contains(&(*pkg, *feat)),
        }
    }

    fn next_package(&self, state: &State) -> Option<PkgIx> {
        state
            .domains
            .iter()
            .filter(|(_, domain)| domain.active && domain.selected.is_none())
            .filter(|(pkg, _)| self.candidates(state, **pkg).len() > 1)
            .min_by_key(|(pkg, _)| self.index.name(**pkg).unwrap_or_default().to_owned())
            .map(|(pkg, _)| *pkg)
    }

    fn candidates(&self, state: &State, pkg: PkgIx) -> Vec<Version> {
        let Some(domain) = state.domains.get(&pkg) else {
            return Vec::new();
        };
        self.index
            .versions(pkg)
            .iter()
            .filter(|version| domain.allowed.contains(version))
            .cloned()
            .collect()
    }

    fn package_universe(&self, pkg: PkgIx) -> VersionSet {
        version_set_from_points(self.index.versions(pkg))
    }

    fn package_label(&self, pkg: PkgIx) -> &str {
        self.index.name(pkg).unwrap_or("<unknown>")
    }

    fn learn(&mut self, conflict: Conflict) {
        #[cfg(feature = "perf-diagnostics")]
        self.diagnostics.record_conflict(self.index, &conflict);
        let mut premises = conflict.premises;
        if let Some(hypothesis) = &conflict.decision {
            premises.push(Premise::Hypothesis(hypothesis.id));
        }
        let premises = dedup_premises(premises);
        let conflict_derivation = self.derivations.append(Derivation {
            premises: premises.clone(),
            rule: RuleKind::Conflict,
        });
        if let Err(err) = self.derivations.replay(conflict_derivation) {
            debug_assert!(
                false,
                "learned conflict derivation {conflict_derivation:?} must replay: {err:?}"
            );
        }
        let support = support_from_premises(self.index, &premises);
        let mut region = Region::new();
        let supported_packages = self.index.support_packages(&support);
        for pkg in supported_packages {
            if let Some(version) = conflict.selected.get(&pkg) {
                region = region.with_versions(pkg, VersionSet::exact(version.clone()));
            }
        }
        if region.versions.is_empty()
            && let Some(hypothesis) = conflict.decision.clone()
        {
            region = region.with_versions(hypothesis.pkg, VersionSet::exact(hypothesis.version));
        }
        let point_region = region.clone();
        let (region, point_versions, widened_versions) =
            self.index
                .widen_region(&conflict.selected, &support, &region);
        let mut derivation = conflict_derivation;
        let mut widen_witness = None;
        if widened_versions > point_versions {
            let witness = build_widen_witness(
                self.index,
                &conflict.selected,
                &support,
                &point_region,
                &region,
            );
            derivation = self.derivations.append(Derivation {
                premises: vec![Premise::Derived(conflict_derivation)],
                rule: RuleKind::Widen {
                    certificate: widen_cert_from_witness(self.index, &witness),
                },
            });
            if let Err(err) = self.derivations.replay(derivation) {
                debug_assert!(
                    false,
                    "learned widen derivation {derivation:?} must replay: {err:?}"
                );
            }
            widen_witness = Some(witness);
        }
        self.trace.push(format!(
            "learn: {} dead; support: {}; widened {}->{} versions",
            display_region(self.index, &region),
            display_support(&support),
            point_versions,
            widened_versions
        ));
        #[cfg(feature = "perf-diagnostics")]
        self.diagnostics.record_learn(
            self.index,
            &region,
            &support,
            point_versions,
            widened_versions,
        );
        self.steps.push(SolveStep::Learn {
            region: record_region(self.index, &region),
            support_edges: support_edge_ids(self.index, &support),
            widened_from: point_versions,
            widened_to: widened_versions,
        });
        let exclusion = self.exclusions.append(Exclusion {
            region: region.clone(),
            derivation,
        });
        self.derivations.note_exclusion(exclusion, derivation);
        let mut fact = LearnedNoGood {
            region,
            derivation,
            support,
            point_versions,
            widened_versions,
            kind: self.problem_kind,
            proof_root: ProofNodeId(String::new()),
            proof_digest: ProofDigest(String::new()),
            verification_status: VerificationStatus::LocallyLearned,
            widen_witness,
        };
        let mut learned = self.learned.clone();
        learned.push(fact.clone());
        let widen_witnesses: BTreeMap<_, _> = learned
            .iter()
            .filter_map(|fact| {
                fact.widen_witness
                    .clone()
                    .map(|witness| (fact.derivation, witness))
            })
            .collect();
        let mut graph = ProofGraph::default();
        if let Some(root) = build_proof_node(
            self.index,
            &self.derivations,
            &widen_witnesses,
            fact.derivation,
            &mut graph,
            &mut BTreeMap::new(),
            &mut Vec::new(),
        ) {
            fact.proof_root = root.clone();
            fact.proof_digest = ProofDigest(root.0);
        }
        self.learned.push(fact);
    }

    fn finish(mut self, state: State) -> SolveResult {
        let selected: BTreeMap<String, Version> = state
            .domains
            .iter()
            .filter_map(|(pkg, domain)| {
                domain.selected.as_ref().map(|version| {
                    (
                        self.index.name(*pkg).unwrap_or_default().to_owned(),
                        version.clone(),
                    )
                })
            })
            .collect();
        let selected_derivations =
            selection_derivations(self.index, &state, &selected, &self.learned);
        let mut enabled_features: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (pkg, feat) in state.features {
            if let (Some(name), Some(feature)) =
                (self.index.name(pkg), self.index.features.resolve_str(feat))
            {
                enabled_features
                    .entry(name.to_owned())
                    .or_default()
                    .insert(feature.to_owned());
            }
        }
        let final_domains: BTreeMap<String, FinalDomain> = state
            .domains
            .iter()
            .filter(|(_, domain)| domain.active)
            .map(|(pkg, domain)| {
                (
                    self.index.name(*pkg).unwrap_or_default().to_owned(),
                    FinalDomain {
                        allowed: domain.allowed.clone(),
                        reasons: domain.reasons.clone(),
                    },
                )
            })
            .collect();
        self.trace.push(format!(
            "solution: {}",
            selected
                .iter()
                .map(|(name, version)| format!("{name} {version}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        self.steps.push(SolveStep::Solution {
            packages: selected
                .iter()
                .map(|(pkg, version)| RecordSelection {
                    pkg: pkg.clone(),
                    version: version.to_string(),
                })
                .collect(),
        });
        self.trace.push(format!(
            "summary: manifests read {}, candidate observations {}, conflicts {}, widened versions killed {} vs {} point versions",
            self.manifests_read,
            self.candidates_existing,
            self.conflicts,
            self.learned.iter().map(|fact| fact.widened_versions).sum::<usize>(),
            self.learned.iter().map(|fact| fact.point_versions).sum::<usize>()
        ));
        SolveResult {
            selected,
            selected_derivations,
            enabled_features,
            decisions: self.decisions,
            conflicts: self.conflicts,
            manifests_read: self.manifests_read,
            candidates_existing: self.candidates_existing,
            steps: self.steps,
            trace: self.trace,
            final_domains,
            learned: self.learned,
            derivations: self.derivations,
            clauses: self.index.clauses.clone(),
        }
    }

    fn no_solution(mut self) -> SolveError {
        self.trace.push(format!(
            "no solution: manifests read {}, candidate observations {}, conflicts {}, widened versions killed {} vs {} point versions",
            self.manifests_read,
            self.candidates_existing,
            self.conflicts,
            self.learned.iter().map(|fact| fact.widened_versions).sum::<usize>(),
            self.learned.iter().map(|fact| fact.point_versions).sum::<usize>()
        ));
        SolveError::NoSolution {
            decisions: self.decisions,
            conflicts: self.conflicts,
            manifests_read: self.manifests_read,
            candidates_existing: self.candidates_existing,
            trace: self.trace,
            learned: self.learned,
            derivations: self.derivations,
            clauses: self.index.clauses.clone(),
        }
    }

    fn assert_solution(&self, state: &State) {
        for clause in &self.index.compiled {
            if clause
                .antecedents
                .iter()
                .all(|guard| self.guard_true(state, guard))
            {
                match &clause.consequent {
                    Consequent::InGraph(pkg) => {
                        assert!(state.domains.get(pkg).is_some_and(|domain| domain.active));
                    }
                    Consequent::VersionSet(pkg, set) => {
                        let selected = state
                            .domains
                            .get(pkg)
                            .and_then(|domain| domain.selected.as_ref());
                        assert!(selected.is_some_and(|version| set.contains(version)));
                    }
                    Consequent::Feature(pkg, feat) => {
                        assert!(state.features.contains(&(*pkg, *feat)));
                    }
                }
            }
        }
    }
}

fn merged_features(entry: &rodin_survey::IndexEntry) -> BTreeMap<String, Vec<String>> {
    let mut features = entry.features.clone();
    if let Some(features2) = &entry.features2 {
        for (name, values) in features2 {
            features.insert(name.clone(), values.clone());
        }
    }
    for values in features.values_mut() {
        values.sort();
    }
    features
}

fn parse_req(req: &str) -> Result<VersionSet> {
    let parsed = VersionReq::parse(req).wrap_err_with(|| format!("parsing version req {req}"))?;
    Ok(VersionSet::from_req(&parsed))
}

fn domain_label(name: &str, compat: CompatClass) -> String {
    format!("{}@{}", name.to_ascii_lowercase(), compat)
}

fn display_req(req: &str) -> String {
    VersionReq::parse(req).map_or_else(
        |_| req.to_owned(),
        |req| display_version_set(&VersionSet::from_req(&req)),
    )
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

fn display_region(index: &CompiledIndex, region: &Region) -> String {
    region
        .versions
        .iter()
        .map(|(pkg, versions)| {
            format!(
                "{} {}",
                index.name(*pkg).unwrap_or("<unknown>"),
                display_version_set(versions)
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn display_support(support: &BTreeSet<EdgeKey>) -> String {
    if support.is_empty() {
        return "pins/hypotheses only".to_owned();
    }
    support
        .iter()
        .map(|edge| format!("{}->{} edge", edge.parent, edge.dep))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(feature = "perf-diagnostics")]
fn top_counts(counts: &BTreeMap<String, usize>, limit: usize) -> String {
    let mut rows = counts.iter().collect::<Vec<_>>();
    rows.sort_by(|(left_key, left_count), (right_key, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_key.cmp(right_key))
    });
    rows.into_iter()
        .take(limit)
        .map(|(key, count)| format!("{key}:{count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(feature = "perf-diagnostics")]
fn perf_diag_interval() -> usize {
    std::env::var("RODIN_DIAG_EVERY")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1000)
}

#[cfg(feature = "perf-diagnostics")]
fn perf_diag_decision_limit() -> Option<usize> {
    std::env::var("RODIN_DIAG_DECISION_LIMIT")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
}

fn package_id(source: &str, name: &str) -> String {
    format!("{source}:{name}")
}

fn package_id_for_name(index: &CompiledIndex, name: &str) -> String {
    let Some(pkg) = index.package(name) else {
        return package_id(CRATES_IO, name);
    };
    let source = index
        .packages
        .resolve(pkg)
        .and_then(|pkg_id| index.sources.resolve_str(pkg_id.source))
        .unwrap_or(CRATES_IO);
    package_id(source, name)
}

fn edge_id(index: &CompiledIndex, parent: &str, dep: &str) -> String {
    format!(
        "edge:{}->{}",
        package_id_for_name(index, parent),
        package_id_for_name(index, dep)
    )
}

fn support_edge_ids(index: &CompiledIndex, support: &BTreeSet<EdgeKey>) -> Vec<String> {
    support
        .iter()
        .map(|edge| edge_id(index, &edge.parent, &edge.dep))
        .collect()
}

fn record_region(index: &CompiledIndex, region: &Region) -> Vec<RecordRegionTerm> {
    region
        .versions
        .iter()
        .map(|(pkg, versions)| RecordRegionTerm {
            pkg: index.name(*pkg).unwrap_or("<unknown>").to_owned(),
            versions: display_version_set(versions),
        })
        .collect()
}

fn version_set_from_points(versions: &[Version]) -> VersionSet {
    VersionSet::from_intervals(versions.iter().map(|version| rodin_facts::Interval {
        lower: Some(version.clone()),
        upper: Some(next_patch(version)),
    }))
}

fn next_patch(version: &Version) -> Version {
    Version::new(version.major, version.minor, version.patch + 1)
}

fn digest_str(input: &str) -> String {
    blake3::hash(input.as_bytes()).to_hex().to_string()
}

fn proof_node_id(node: &ProofNode) -> ProofNodeId {
    ProofNodeId(digest_str(&canonical_proof_node(node)))
}

fn canonical_proof_node(node: &ProofNode) -> String {
    let mut out = String::new();
    match &node.rule {
        ProofRule::Conflict => out.push_str("rule:conflict\n"),
        ProofRule::Resolve => out.push_str("rule:resolve\n"),
        ProofRule::Widen { witness } => {
            out.push_str("rule:widen\n");
            out.push_str(&canonical_widen_witness(witness));
        }
    }
    for premise in &node.premises {
        out.push_str("premise:");
        out.push_str(&canonical_premise(premise));
        out.push('\n');
    }
    out
}

fn canonical_premise(premise: &ProofPremise) -> String {
    match premise {
        ProofPremise::Clause(witness) => canonical_clause_witness(witness),
        ProofPremise::Pin(witness) => format!("pin:{}", witness.pin),
        ProofPremise::Exclusion(witness) => {
            format!(
                "exclusion:{}:{}",
                witness.proof_root.0, witness.proof_digest.0
            )
        }
        ProofPremise::Hypothesis(witness) => format!("hypothesis:{}", witness.id),
        ProofPremise::Derived(witness) => {
            format!(
                "derived:{}:{}",
                witness.proof_root.0, witness.proof_digest.0
            )
        }
    }
}

fn canonical_clause_witness(witness: &ClauseWitness) -> String {
    let mut out = String::from("clause:");
    for atom in &witness.antecedents {
        out.push_str(&canonical_atom(atom));
        out.push(',');
    }
    out.push_str("=>");
    if let Some(atom) = &witness.consequent {
        out.push_str(&canonical_atom(atom));
    } else {
        out.push_str("false");
    }
    if let Some(edge) = &witness.support {
        let _ = write!(out, ":support:{}->{}", edge.parent, edge.dep);
    }
    out
}

fn canonical_atom(atom: &AtomWitness) -> String {
    match atom {
        AtomWitness::InGraph { package } => format!("in:{package}"),
        AtomWitness::Ver { package, versions } => format!("ver:{package}:{versions}"),
        AtomWitness::Feat { package, feature } => format!("feat:{package}:{feature}"),
    }
}

fn canonical_widen_witness(witness: &WidenWitness) -> String {
    let mut out = String::new();
    for edge in &witness.support_edges {
        let _ = writeln!(out, "support:{}->{}", edge.parent, edge.dep);
    }
    for term in &witness.point_terms {
        let _ = writeln!(out, "point:{}@{}", term.package, term.version);
    }
    for term in &witness.widened_terms {
        let _ = write!(out, "widened:{}:", term.package);
        for version in &term.versions {
            let _ = write!(out, "{version},");
        }
        out.push('\n');
    }
    for edge in &witness.edge_stability {
        let _ = write!(
            out,
            "stable:{}->{}@{}:",
            edge.parent, edge.dep, edge.selected_version
        );
        for version in &edge.stable_versions {
            let _ = write!(out, "{version},");
        }
        out.push(':');
        for state in &edge.selected_state {
            let _ = write!(
                out,
                "{}|{:?}|{}|{}|{:?}|{};",
                state.req,
                state.features,
                state.optional,
                state.default_features,
                state.target,
                state.kind
            );
        }
        out.push('\n');
    }
    out
}

fn graph_from_learned(
    index: &CompiledIndex,
    learned: &[LearnedNoGood],
    derivations: &DerivationStore,
) -> Option<ProofGraph> {
    let widen_witnesses: BTreeMap<_, _> = learned
        .iter()
        .filter_map(|fact| {
            fact.widen_witness
                .clone()
                .map(|witness| (fact.derivation, witness))
        })
        .collect();
    let mut graph = ProofGraph::default();
    let mut memo = BTreeMap::new();
    for fact in learned {
        build_proof_node(
            index,
            derivations,
            &widen_witnesses,
            fact.derivation,
            &mut graph,
            &mut memo,
            &mut Vec::new(),
        )?;
    }
    Some(graph)
}

fn build_proof_node(
    index: &CompiledIndex,
    derivations: &DerivationStore,
    widen_witnesses: &BTreeMap<DerivationId, WidenWitness>,
    id: DerivationId,
    graph: &mut ProofGraph,
    memo: &mut BTreeMap<DerivationId, ProofNodeId>,
    stack: &mut Vec<DerivationId>,
) -> Option<ProofNodeId> {
    if let Some(node) = memo.get(&id) {
        return Some(node.clone());
    }
    if stack.contains(&id) {
        return None;
    }
    let derivation = derivations.get(id)?;
    stack.push(id);
    let mut premises = Vec::new();
    for premise in &derivation.premises {
        let proof_premise = match premise {
            Premise::Clause(id) => ProofPremise::Clause(index.clause_witness(*id)?),
            Premise::Pin(pin) => ProofPremise::Pin(PinWitness { pin: pin.0.clone() }),
            Premise::Hypothesis(id) => ProofPremise::Hypothesis(HypothesisWitness { id: *id }),
            Premise::Exclusion(id) => {
                let child = derivations.exclusion_derivation(*id)?;
                let proof_root = build_proof_node(
                    index,
                    derivations,
                    widen_witnesses,
                    child,
                    graph,
                    memo,
                    stack,
                )?;
                ProofPremise::Exclusion(ExclusionWitness {
                    proof_digest: ProofDigest(proof_root.0.clone()),
                    proof_root,
                })
            }
            Premise::Derived(id) => {
                let proof_root =
                    build_proof_node(index, derivations, widen_witnesses, *id, graph, memo, stack)?;
                ProofPremise::Derived(DerivedWitness {
                    proof_digest: ProofDigest(proof_root.0.clone()),
                    proof_root,
                })
            }
        };
        premises.push(proof_premise);
    }
    stack.pop();

    let rule = match &derivation.rule {
        RuleKind::Conflict => ProofRule::Conflict,
        RuleKind::Resolve => ProofRule::Resolve,
        RuleKind::Widen { .. } => ProofRule::Widen {
            witness: widen_witnesses.get(&id).cloned()?,
        },
    };
    let node = ProofNode { premises, rule };
    let proof_id = proof_node_id(&node);
    graph.nodes.insert(proof_id.clone(), node);
    memo.insert(id, proof_id.clone());
    Some(proof_id)
}

fn verify_graph_node_ids(graph: &ProofGraph) -> bool {
    graph
        .nodes
        .iter()
        .all(|(id, node)| proof_node_id(node) == *id)
}

impl WarmFactVerifier {
    #[must_use]
    pub fn new(scope: FactScope) -> Self {
        Self { scope }
    }

    #[must_use]
    pub fn for_index(index: &CompiledIndex) -> Self {
        Self::new(index.fact_scope())
    }

    #[must_use]
    pub fn verify(&self, index: &CompiledIndex, bundle: &WarmFactBundle) -> VerifiedWarmFacts {
        let mut out = VerifiedWarmFacts::new();
        if bundle.scope != self.scope {
            out.rejected
                .extend(bundle.facts.iter().map(|fact| WarmFactRejection {
                    proof_digest: Some(fact.proof_digest.clone()),
                    reason: WarmFactRejectionReason::ScopeMismatch,
                }));
            return out;
        }
        let WarmProofSource::Inline(graph) = &bundle.proofs else {
            out.rejected
                .extend(bundle.facts.iter().map(|fact| WarmFactRejection {
                    proof_digest: Some(fact.proof_digest.clone()),
                    reason: WarmFactRejectionReason::LazyProofUnavailable,
                }));
            return out;
        };
        if !verify_graph_node_ids(graph) {
            out.rejected
                .extend(bundle.facts.iter().map(|fact| WarmFactRejection {
                    proof_digest: Some(fact.proof_digest.clone()),
                    reason: WarmFactRejectionReason::DigestMismatch,
                }));
            return out;
        }

        for warm in &bundle.facts {
            match verify_one_warm_fact(index, graph.clone(), warm) {
                Ok(fact) => out.facts.push(fact),
                Err(reason) => out.rejected.push(WarmFactRejection {
                    proof_digest: Some(warm.proof_digest.clone()),
                    reason,
                }),
            }
        }
        out
    }
}

fn verify_one_warm_fact(
    index: &CompiledIndex,
    graph: Arc<ProofGraph>,
    warm: &WarmLearnedNoGood,
) -> Result<VerifiedWarmFact, WarmFactRejectionReason> {
    if warm.proof_digest.0 != warm.proof_root.0 || warm.fact.proof_digest != warm.proof_digest {
        return Err(WarmFactRejectionReason::DigestMismatch);
    }
    let node = graph
        .nodes
        .get(&warm.proof_root)
        .ok_or(WarmFactRejectionReason::MissingProofNode)?;
    replay_proof_root(index, &graph, &warm.proof_root)?;
    if let ProofRule::Widen { witness } = &node.rule
        && !verify_widen_witness(index, witness, &warm.fact)
    {
        let Some(conflict_root) = derived_conflict_root(node) else {
            return Err(WarmFactRejectionReason::InvalidWidenWithoutConflict);
        };
        replay_proof_root(index, &graph, &conflict_root)?;
        let mut shrunk = warm.fact.clone();
        shrunk.region = point_region_from_witness(index, witness)
            .ok_or(WarmFactRejectionReason::RebindFailed)?;
        shrunk.point_versions = 1;
        shrunk.widened_versions = shrunk.point_versions;
        shrunk.proof_root = conflict_root.clone();
        shrunk.proof_digest = ProofDigest(conflict_root.0.clone());
        shrunk.verification_status = VerificationStatus::ShrunkInvalidWiden;
        shrunk.widen_witness = None;
        return Ok(VerifiedWarmFact {
            fact: shrunk,
            proof_root: conflict_root,
            proofs: graph,
        });
    }

    let mut fact = warm.fact.clone();
    fact.verification_status = VerificationStatus::VerifiedImported;
    Ok(VerifiedWarmFact {
        fact,
        proof_root: warm.proof_root.clone(),
        proofs: graph,
    })
}

fn derived_conflict_root(node: &ProofNode) -> Option<ProofNodeId> {
    node.premises.iter().find_map(|premise| {
        if let ProofPremise::Derived(witness) = premise {
            Some(witness.proof_root.clone())
        } else {
            None
        }
    })
}

fn replay_proof_root(
    index: &CompiledIndex,
    graph: &ProofGraph,
    root: &ProofNodeId,
) -> Result<(), WarmFactRejectionReason> {
    let mut derivations = DerivationStore::new();
    for id in (0..index.clauses.len()).map(|ix| ClauseId(ix as u32)) {
        derivations.note_clause(id);
    }
    let mut lowered = BTreeMap::new();
    let mut next_internal_exclusion = 1_000_000_u32;
    let id = lower_proof_node(
        index,
        graph,
        root,
        &mut derivations,
        &mut lowered,
        &mut next_internal_exclusion,
    )?;
    derivations
        .replay(id)
        .map(|_| ())
        .map_err(|_| WarmFactRejectionReason::ReplayFailed)
}

fn lower_proof_node(
    index: &CompiledIndex,
    graph: &ProofGraph,
    root: &ProofNodeId,
    derivations: &mut DerivationStore,
    lowered: &mut BTreeMap<ProofNodeId, DerivationId>,
    next_internal_exclusion: &mut u32,
) -> Result<DerivationId, WarmFactRejectionReason> {
    if let Some(id) = lowered.get(root) {
        return Ok(*id);
    }
    let node = graph
        .nodes
        .get(root)
        .ok_or(WarmFactRejectionReason::MissingProofNode)?;
    let mut premises = Vec::new();
    for premise in &node.premises {
        premises.push(lower_premise(
            index,
            graph,
            premise,
            derivations,
            lowered,
            next_internal_exclusion,
        )?);
    }
    let rule = match &node.rule {
        ProofRule::Conflict => RuleKind::Conflict,
        ProofRule::Resolve => RuleKind::Resolve,
        ProofRule::Widen { witness } => RuleKind::Widen {
            certificate: widen_cert_from_witness(index, witness),
        },
    };
    let id = derivations.append(Derivation { premises, rule });
    lowered.insert(root.clone(), id);
    Ok(id)
}

fn lower_premise(
    index: &CompiledIndex,
    graph: &ProofGraph,
    premise: &ProofPremise,
    derivations: &mut DerivationStore,
    lowered: &mut BTreeMap<ProofNodeId, DerivationId>,
    next_internal_exclusion: &mut u32,
) -> Result<Premise, WarmFactRejectionReason> {
    match premise {
        ProofPremise::Clause(witness) => index
            .rebind_clause(witness)
            .map(Premise::Clause)
            .ok_or(WarmFactRejectionReason::RebindFailed),
        ProofPremise::Pin(witness) => {
            let pin = PinRef(witness.pin.clone());
            derivations.note_pin(pin.clone());
            Ok(Premise::Pin(pin))
        }
        ProofPremise::Hypothesis(witness) => Ok(Premise::Hypothesis(witness.id)),
        ProofPremise::Derived(witness) => {
            if witness.proof_digest.0 != witness.proof_root.0 {
                return Err(WarmFactRejectionReason::DigestMismatch);
            }
            lower_proof_node(
                index,
                graph,
                &witness.proof_root,
                derivations,
                lowered,
                next_internal_exclusion,
            )
            .map(Premise::Derived)
        }
        ProofPremise::Exclusion(witness) => {
            if witness.proof_digest.0 != witness.proof_root.0 {
                return Err(WarmFactRejectionReason::DigestMismatch);
            }
            let child = lower_proof_node(
                index,
                graph,
                &witness.proof_root,
                derivations,
                lowered,
                next_internal_exclusion,
            )?;
            let exclusion = ExclusionId(*next_internal_exclusion);
            *next_internal_exclusion += 1;
            derivations.note_exclusion(exclusion, child);
            Ok(Premise::Exclusion(exclusion))
        }
    }
}

fn widen_cert_from_witness(
    index: &CompiledIndex,
    witness: &WidenWitness,
) -> rodin_facts::WidenCert {
    let package = witness
        .edge_stability
        .first()
        .and_then(|edge| index.package(&edge.parent))
        .or_else(|| {
            witness
                .point_terms
                .first()
                .and_then(|term| index.package(&term.package))
        })
        .unwrap_or(PkgIx(0));
    let checked = witness
        .edge_stability
        .first()
        .map(|edge| version_set_from_points(&edge.stable_versions))
        .unwrap_or_else(VersionSet::empty);
    rodin_facts::WidenCert {
        package,
        checked,
        manifest_hash: digest_str(&canonical_widen_witness(witness)),
    }
}

fn verify_widen_witness(
    index: &CompiledIndex,
    witness: &WidenWitness,
    fact: &LearnedNoGood,
) -> bool {
    if witness.support_edges.iter().collect::<BTreeSet<_>>()
        != fact.support.iter().collect::<BTreeSet<_>>()
    {
        return false;
    }
    let Some(expected) = widened_region_from_witness(index, witness) else {
        return false;
    };
    if expected != fact.region {
        return false;
    }
    for edge in &witness.edge_stability {
        if index.edge_state(&edge.parent, &edge.selected_version, &edge.dep) != edge.selected_state
        {
            return false;
        }
        let Some(parent) = index.package(&edge.parent) else {
            return false;
        };
        let actual: Vec<_> = index
            .versions(parent)
            .iter()
            .filter(|version| {
                index.edge_state(&edge.parent, version, &edge.dep) == edge.selected_state
            })
            .cloned()
            .collect();
        if actual != edge.stable_versions {
            return false;
        }
    }
    true
}

fn point_region_from_witness(index: &CompiledIndex, witness: &WidenWitness) -> Option<Region> {
    let mut region = Region::new();
    for term in &witness.point_terms {
        region = region.with_versions(
            index.package(&term.package)?,
            VersionSet::exact(term.version.clone()),
        );
    }
    Some(region)
}

fn widened_region_from_witness(index: &CompiledIndex, witness: &WidenWitness) -> Option<Region> {
    let mut region = point_region_from_witness(index, witness)?;
    for term in &witness.widened_terms {
        region = region.with_versions(
            index.package(&term.package)?,
            version_set_from_points(&term.versions),
        );
    }
    Some(region)
}

fn build_widen_witness(
    index: &CompiledIndex,
    selected: &BTreeMap<PkgIx, Version>,
    support: &BTreeSet<EdgeKey>,
    point_region: &Region,
    widened_region: &Region,
) -> WidenWitness {
    let point_terms = point_region
        .versions
        .iter()
        .filter_map(|(pkg, versions)| {
            selected.get(pkg).and_then(|version| {
                versions.contains(version).then(|| PointTermWitness {
                    package: index.name(*pkg).unwrap_or("<unknown>").to_owned(),
                    version: version.clone(),
                })
            })
        })
        .collect();
    let widened_terms = widened_region
        .versions
        .iter()
        .map(|(pkg, versions)| WidenedTermWitness {
            package: index.name(*pkg).unwrap_or("<unknown>").to_owned(),
            versions: index
                .versions(*pkg)
                .iter()
                .filter(|version| versions.contains(version))
                .cloned()
                .collect(),
        })
        .collect();
    let edge_stability = support
        .iter()
        .filter_map(|edge| {
            let parent = index.package(&edge.parent)?;
            let selected_version = selected.get(&parent)?.clone();
            let selected_state = index.edge_state(&edge.parent, &selected_version, &edge.dep);
            let stable_versions = index
                .versions(parent)
                .iter()
                .filter(|version| {
                    index.edge_state(&edge.parent, version, &edge.dep) == selected_state
                })
                .cloned()
                .collect();
            Some(EdgeStabilityWitness {
                parent: edge.parent.clone(),
                dep: edge.dep.clone(),
                selected_version,
                stable_versions,
                selected_state,
            })
        })
        .collect();
    WidenWitness {
        support_edges: support.iter().cloned().collect(),
        point_terms,
        widened_terms,
        edge_stability,
    }
}

#[derive(Eq, PartialEq)]
struct StateFingerprint {
    domains: Vec<(PkgIx, bool, Vec<rodin_facts::Interval>, Option<Version>)>,
    features: BTreeSet<(PkgIx, FeatIx)>,
}

fn state_fingerprint(state: &State) -> StateFingerprint {
    let domains = state
        .domains
        .iter()
        .map(|(pkg, domain)| {
            (
                *pkg,
                domain.active,
                domain.allowed.intervals().to_vec(),
                domain.selected.clone(),
            )
        })
        .collect();
    StateFingerprint {
        domains,
        features: state.features.clone(),
    }
}

fn selected_versions(state: &State) -> BTreeMap<PkgIx, Version> {
    state
        .domains
        .iter()
        .filter_map(|(pkg, domain)| {
            domain
                .selected
                .as_ref()
                .map(|version| (*pkg, version.clone()))
        })
        .collect()
}

fn selection_derivations(
    index: &CompiledIndex,
    state: &State,
    selected: &BTreeMap<String, Version>,
    learned: &[LearnedNoGood],
) -> Vec<SelectionDerivation> {
    selected
        .iter()
        .map(|(pkg_name, selected_version)| {
            let killed_higher: Vec<_> = index
                .package(pkg_name)
                .map(|pkg| {
                    index
                        .versions(pkg)
                        .iter()
                        .rev()
                        .filter(|version| *version > selected_version)
                        .filter_map(|version| {
                            candidate_kill_reason(
                                index, state, selected, learned, pkg, pkg_name, version,
                            )
                            .map(|reason| CandidateKill {
                                version: version.to_string(),
                                reason,
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let digest = selection_digest(pkg_name, selected_version, &killed_higher);
            SelectionDerivation {
                pkg: pkg_name.clone(),
                version: selected_version.to_string(),
                digest,
                killed_higher,
            }
        })
        .collect()
}

fn candidate_kill_reason(
    index: &CompiledIndex,
    state: &State,
    selected: &BTreeMap<String, Version>,
    learned: &[LearnedNoGood],
    pkg: PkgIx,
    pkg_name: &str,
    version: &Version,
) -> Option<KillReason> {
    if let Some(domain) = state.domains.get(&pkg)
        && !domain.allowed.contains(version)
    {
        return Some(constraint_kill_reason(
            index,
            pkg_name,
            version,
            &domain.reasons,
        ));
    }

    let assignment = assignment_with_rebound_from_selected(index, selected, state, pkg, version);
    let direct_assignment = {
        let mut assignment = assignment.clone();
        apply_direct_dependency_versions(index, pkg, version, &mut assignment.versions);
        assignment
    };
    learned.iter().enumerate().find_map(|(ix, fact)| {
        (fact.region.contains_assignment(&assignment)
            || fact.region.contains_assignment(&direct_assignment))
        .then(|| KillReason::LearnedExclusion {
            fact_id: format!("fact-{ix}"),
            proof_root: fact.proof_root.0.clone(),
            proof_digest: fact.proof_digest.0.clone(),
        })
    })
}

fn constraint_kill_reason(
    index: &CompiledIndex,
    pkg_name: &str,
    version: &Version,
    premises: &[Premise],
) -> KillReason {
    let pins = pins_from_premises(premises);
    let edges = support_edge_ids(index, &support_from_premises(index, premises));
    let root_pins: Vec<_> = pins
        .iter()
        .filter(|pin| pin.starts_with("root:"))
        .cloned()
        .collect();
    let decree_pins: Vec<_> = pins
        .iter()
        .filter(|pin| pin.starts_with("decree:"))
        .cloned()
        .collect();
    if !decree_pins.is_empty() {
        let digest = constraint_digest("decree", pkg_name, version, &decree_pins, &edges);
        KillReason::Decree {
            digest,
            pins: decree_pins,
        }
    } else if !root_pins.is_empty() && edges.is_empty() {
        let digest = constraint_digest("root", pkg_name, version, &root_pins, &edges);
        KillReason::RootConstraint {
            digest,
            pins: root_pins,
        }
    } else {
        let digest = constraint_digest("dependency", pkg_name, version, &pins, &edges);
        KillReason::DependencyConstraint {
            digest,
            edges,
            pins,
        }
    }
}

fn pins_from_premises(premises: &[Premise]) -> Vec<String> {
    premises
        .iter()
        .filter_map(|premise| {
            if let Premise::Pin(pin) = premise {
                Some(pin.0.clone())
            } else {
                None
            }
        })
        .collect()
}

fn constraint_digest(
    kind: &str,
    pkg_name: &str,
    version: &Version,
    pins: &[String],
    edges: &[String],
) -> String {
    let mut out = format!("kill:{kind}:{pkg_name}@{version}\n");
    for pin in pins {
        let _ = writeln!(out, "pin:{pin}");
    }
    for edge in edges {
        let _ = writeln!(out, "edge:{edge}");
    }
    digest_str(&out)
}

fn selection_digest(pkg_name: &str, selected: &Version, killed: &[CandidateKill]) -> String {
    let mut out = format!("selection:{pkg_name}@{selected}\n");
    for kill in killed {
        let _ = writeln!(out, "kill:{}:{}", kill.version, kill.reason.digest());
    }
    digest_str(&out)
}

fn assignment_with_rebound_from_selected(
    index: &CompiledIndex,
    selected: &BTreeMap<String, Version>,
    state: &State,
    rebound_pkg: PkgIx,
    rebound_version: &Version,
) -> Assignment {
    let mut versions = BTreeMap::new();
    for (name, version) in selected {
        if let Some(pkg) = index.package(name) {
            let version = if pkg == rebound_pkg {
                rebound_version.clone()
            } else {
                version.clone()
            };
            versions.insert(pkg, version);
        }
    }
    add_forced_dependency_closure(index, &mut versions);
    Assignment {
        versions,
        features: state.features.clone(),
    }
}

fn last_hypothesis(state: &State) -> Option<Hypothesis> {
    state.hypotheses.last().cloned()
}

fn dedup_premises(premises: Vec<Premise>) -> Vec<Premise> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for premise in premises {
        let key = format!("{premise:?}");
        if seen.insert(key) {
            out.push(premise);
        }
    }
    out
}

fn support_from_premises(index: &CompiledIndex, premises: &[Premise]) -> BTreeSet<EdgeKey> {
    premises
        .iter()
        .filter_map(|premise| match premise {
            Premise::Clause(id) => index.clause_support.get(id).cloned(),
            Premise::Exclusion(_)
            | Premise::Pin(_)
            | Premise::Hypothesis(_)
            | Premise::Derived(_) => None,
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct ScenarioResult {
    pub name: String,
    pub shape: String,
    pub kind: LearnedKind,
    pub cold_decisions: usize,
    pub cold_conflicts: usize,
    pub cold_manifests_read: usize,
    pub cold_candidates_existing: usize,
    pub warm_decisions: usize,
    pub warm_conflicts: usize,
    pub learned: Vec<LearnedNoGood>,
    pub derivations: DerivationStore,
    pub trace: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Round3Report {
    pub natural: Vec<ScenarioResult>,
    pub pressure: Vec<ScenarioResult>,
}

impl Round3Report {
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("## Round 3\n\n");
        out.push_str("Round 3 measures dynamic derivation-support widening. The core used for this round propagates version domains as sets and only picks a point version when an active package must be expanded and multiple real candidates remain; conflicts below are therefore late-discovery conflicts, not avoidable set-intersection artifacts.\n\n");
        out.push_str("Natural workloads use cached crates.io sparse-index rows with each requested root pinned to its cached latest exact version and a bounded v0 mini-closure. The pressure suite is synthetic where needed so every burial shape has an explicit late-discovery choice point.\n\n");
        out.push_str("### Natural workloads\n\n");
        out.push_str("| workload | shape | learned kind | cold decisions | cold conflicts | learned no-goods |\n");
        out.push_str("|---|---|---|---:|---:|---:|\n");
        for row in &self.natural {
            push_scenario_row(&mut out, row, false);
        }
        out.push_str("\n### Pressure workloads\n\n");
        out.push_str("| workload | shape | learned kind | cold decisions | cold conflicts | warm decisions | warm conflicts | decisions saved | conflicts saved | learned no-goods |\n");
        out.push_str("|---|---|---|---:|---:|---:|---:|---:|---:|---:|\n");
        for row in &self.pressure {
            push_scenario_row(&mut out, row, true);
        }
        out.push_str("\n### Widening by learned-fact kind\n\n");
        out.push_str("| kind | no-goods | widened share | median factor | p90 factor | max factor | point versions | widened versions |\n");
        out.push_str("|---|---:|---:|---:|---:|---:|---:|---:|\n");
        for (kind, learned) in learned_by_kind(self) {
            let factors = factors(&learned);
            let widened = learned
                .iter()
                .filter(|fact| fact.widened_versions > fact.point_versions)
                .count();
            let point: usize = learned.iter().map(|fact| fact.point_versions).sum();
            let wide: usize = learned.iter().map(|fact| fact.widened_versions).sum();
            out.push_str("| ");
            out.push_str(kind.as_str());
            out.push_str(" | ");
            out.push_str(&learned.len().to_string());
            out.push_str(" | ");
            out.push_str(&percent(widened, learned.len()));
            out.push_str(" | ");
            out.push_str(&decimal(percentile(&factors, 0.5)));
            out.push_str(" | ");
            out.push_str(&decimal(percentile(&factors, 0.9)));
            out.push_str(" | ");
            out.push_str(&decimal(factors.last().copied().unwrap_or(0.0)));
            out.push_str(" | ");
            out.push_str(&point.to_string());
            out.push_str(" | ");
            out.push_str(&wide.to_string());
            out.push_str(" |\n");
        }
        out.push_str("\n### Round 3 aggregate\n\n");
        let learned: Vec<_> = self
            .pressure
            .iter()
            .chain(&self.natural)
            .flat_map(|scenario| scenario.learned.iter())
            .collect();
        let total_point: usize = learned.iter().map(|fact| fact.point_versions).sum();
        let total_wide: usize = learned.iter().map(|fact| fact.widened_versions).sum();
        let pressure_cold_conflicts: usize =
            self.pressure.iter().map(|row| row.cold_conflicts).sum();
        let pressure_warm_conflicts: usize =
            self.pressure.iter().map(|row| row.warm_conflicts).sum();
        let pressure_cold_decisions: usize =
            self.pressure.iter().map(|row| row.cold_decisions).sum();
        let pressure_warm_decisions: usize =
            self.pressure.iter().map(|row| row.warm_decisions).sum();
        out.push_str(&format!(
            "- learned no-goods: {}\n- point-versions excluded: {total_point}\n- widened-versions excluded: {total_wide}\n- aggregate widening factor: {}\n- pressure decisions saved by warm facts: {}\n- pressure conflicts saved by warm facts: {}\n\n",
            learned.len(),
            decimal(total_wide as f64 / total_point.max(1) as f64),
            pressure_cold_decisions.saturating_sub(pressure_warm_decisions),
            pressure_cold_conflicts.saturating_sub(pressure_warm_conflicts),
        ));
        out.push_str("### Round 3 verdict\n\n");
        out.push_str(&round3_verdict(
            total_point,
            total_wide,
            pressure_cold_decisions,
            pressure_warm_decisions,
            pressure_cold_conflicts,
            pressure_warm_conflicts,
        ));
        out.push('\n');
        if let Some(trace) = self
            .pressure
            .iter()
            .find(|scenario| scenario.name == "downgrade-cascade")
        {
            out.push_str("\n### Appendix: downgrade-cascade full trace\n\n");
            out.push_str("This trace is emitted by the round-3 solver run for the pressure scenario, not reconstructed by hand.\n\n");
            for (ix, line) in trace.trace.iter().enumerate() {
                out.push_str(&format!("{}. {line}\n", ix + 1));
            }
            out.push('\n');
        }
        out
    }
}

pub fn append_round3_report(path: &Path) -> Result<Round3Report> {
    let report = run_round3()?;
    let existing =
        fs::read_to_string(path).wrap_err_with(|| format!("reading {}", path.display()))?;
    let base = existing
        .find("\n## Round 3\n")
        .map_or(existing.as_str(), |ix| &existing[..ix])
        .trim_end();
    fs::write(path, format!("{base}\n\n{}", report.render()))
        .wrap_err_with(|| format!("writing {}", path.display()))?;
    Ok(report)
}

pub fn run_round3() -> Result<Round3Report> {
    let mut report = Round3Report::default();
    let cache = rodin_survey::workspace_cache_dir()?;
    for (name, seed, features) in [
        ("axum-default", "axum", Vec::<&str>::new()),
        ("reqwest", "reqwest", Vec::<&str>::new()),
        ("tokio-full", "tokio", vec!["full"]),
        ("clap", "clap", Vec::<&str>::new()),
    ] {
        match rodin_survey::fetch_closure(&cache, &[seed], 12)
            .and_then(|crates| CompiledIndex::from_sparse(&crates))
        {
            Ok(index) => {
                let mut root = RootReq::new(
                    seed,
                    latest_exact_req(&index, seed).unwrap_or_else(|| "*".to_owned()),
                );
                for feature in features {
                    root = root.feature(feature);
                }
                report.natural.push(run_one(
                    &index,
                    name,
                    "natural",
                    Problem::new(vec![root]),
                    None,
                ));
            }
            Err(err) => tracing::warn!(workload = name, error = %err, "skipping natural workload"),
        }
    }
    for scenario in pressure_scenarios()? {
        let cold = run_one(
            &scenario.index,
            &scenario.name,
            &scenario.shape,
            scenario.problem.clone(),
            None,
        );
        let bundle = warm_bundle_from_learned(&scenario.index, &cold.learned, &cold.derivations)
            .wrap_err_with(|| format!("building warm proof bundle for {}", scenario.name))?;
        let warm_facts =
            WarmFactVerifier::for_index(&scenario.index).verify(&scenario.index, &bundle);
        let warm_problem = Problem {
            warm_facts,
            ..scenario.perturbed
        };
        let warm = run_one(
            &scenario.index,
            &format!("{} perturbed", scenario.name),
            &scenario.shape,
            warm_problem,
            Some(&cold),
        );
        report.pressure.push(ScenarioResult {
            name: scenario.name,
            shape: scenario.shape,
            kind: scenario.kind,
            cold_decisions: cold.cold_decisions,
            cold_conflicts: cold.cold_conflicts,
            cold_manifests_read: cold.cold_manifests_read,
            cold_candidates_existing: cold.cold_candidates_existing,
            warm_decisions: warm.cold_decisions,
            warm_conflicts: warm.cold_conflicts,
            learned: cold.learned,
            derivations: cold.derivations,
            trace: cold.trace,
        });
    }
    Ok(report)
}

fn run_one(
    index: &CompiledIndex,
    name: &str,
    shape: &str,
    problem: Problem,
    _cold: Option<&ScenarioResult>,
) -> ScenarioResult {
    let kind = problem.learned_kind;
    match solve(index, problem) {
        Ok(result) => ScenarioResult {
            name: name.to_owned(),
            shape: shape.to_owned(),
            kind,
            cold_decisions: result.decisions,
            cold_conflicts: result.conflicts,
            cold_manifests_read: result.manifests_read,
            cold_candidates_existing: result.candidates_existing,
            warm_decisions: 0,
            warm_conflicts: 0,
            trace: result.trace,
            learned: result.learned,
            derivations: result.derivations,
        },
        Err(err) => match *err {
            SolveError::NoSolution {
                decisions,
                conflicts,
                manifests_read,
                candidates_existing,
                trace,
                learned,
                derivations,
                ..
            } => ScenarioResult {
                name: name.to_owned(),
                shape: shape.to_owned(),
                kind,
                cold_decisions: decisions,
                cold_conflicts: conflicts,
                cold_manifests_read: manifests_read,
                cold_candidates_existing: candidates_existing,
                warm_decisions: 0,
                warm_conflicts: 0,
                trace,
                learned,
                derivations,
            },
        },
    }
}

pub fn warm_bundle_from_learned(
    index: &CompiledIndex,
    learned: &[LearnedNoGood],
    derivations: &DerivationStore,
) -> Result<WarmFactBundle> {
    let graph = graph_from_learned(index, learned, derivations)
        .ok_or_else(|| eyre::eyre!("building proof graph from learned facts"))?;
    let graph = Arc::new(graph);
    let facts = learned
        .iter()
        .map(|fact| WarmLearnedNoGood {
            fact: fact.clone(),
            proof_root: fact.proof_root.clone(),
            proof_digest: fact.proof_digest.clone(),
        })
        .collect();
    Ok(WarmFactBundle {
        scope: index.fact_scope(),
        facts,
        proofs: WarmProofSource::Inline(graph),
    })
}

pub fn solve_record(
    index: &CompiledIndex,
    scenario: impl Into<String>,
    shape: impl Into<String>,
    problem: Problem,
) -> Result<SolveRecord> {
    let scenario = scenario.into();
    let shape = shape.into();
    let graph_problem = problem.clone();
    let result = solve(index, problem).map_err(|err| eyre::eyre!("{err:?}"))?;
    let facts = record_facts(index, &result.learned);
    let graph = record_graph(index, &graph_problem, &result);
    Ok(SolveRecord {
        scenario,
        shape,
        packages: record_packages(index),
        steps: result.steps,
        graph,
        facts,
    })
}

pub fn round3_pressure_scenario_names() -> Result<Vec<String>> {
    Ok(pressure_scenarios()?
        .into_iter()
        .map(|scenario| scenario.name)
        .collect())
}

pub fn solve_round3_record(name: &str) -> Result<SolveRecord> {
    let scenarios = pressure_scenarios()?;
    let names = scenarios
        .iter()
        .map(|scenario| scenario.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let scenario = scenarios
        .into_iter()
        .find(|scenario| scenario.name == name)
        .ok_or_else(|| eyre::eyre!("unknown round-3 pressure scenario {name}; known: {names}"))?;
    solve_record(
        &scenario.index,
        scenario.name.clone(),
        scenario.shape.clone(),
        scenario.problem,
    )
}

pub fn counterfactual_drop_dependency(
    index: &CompiledIndex,
    base_problem: &Problem,
    completed: &SolveResult,
    warm_facts: &VerifiedWarmFacts,
    package: impl Into<String>,
) -> Result<CounterfactualResult> {
    let package = package.into().to_ascii_lowercase();
    let mut modified = base_problem.clone();
    modified.roots.retain(|root| root.name != package);
    run_counterfactual(
        index,
        completed,
        warm_facts,
        modified,
        BTreeSet::from([package.clone()]),
        CounterfactualQuery::DropDependency {
            package: package.clone(),
        },
        ManifestEdit::RemoveRootDependency { package },
    )
}

pub fn counterfactual_decree_version(
    index: &CompiledIndex,
    base_problem: &Problem,
    completed: &SolveResult,
    warm_facts: &VerifiedWarmFacts,
    package: impl Into<String>,
    version: impl Into<String>,
) -> Result<CounterfactualResult> {
    let package = package.into().to_ascii_lowercase();
    let version = version.into();
    let parsed = Version::parse(&version).wrap_err_with(|| format!("parsing version {version}"))?;
    let requirement = format!("={parsed}");
    let mut modified = base_problem.clone();
    modified.decrees.retain(|decree| decree.name != package);
    modified
        .decrees
        .push(Decree::new(package.clone(), requirement.clone()));
    run_counterfactual(
        index,
        completed,
        warm_facts,
        modified,
        BTreeSet::from([package.clone()]),
        CounterfactualQuery::DecreeVersion {
            package: package.clone(),
            version: parsed.to_string(),
        },
        ManifestEdit::AddOrReplaceDecree {
            package,
            requirement,
        },
    )
}

fn run_counterfactual(
    index: &CompiledIndex,
    completed: &SolveResult,
    warm_facts: &VerifiedWarmFacts,
    mut modified: Problem,
    touched: BTreeSet<String>,
    query: CounterfactualQuery,
    manifest_edit: ManifestEdit,
) -> Result<CounterfactualResult> {
    let scoped = scope_warm_facts_for_counterfactual(index, warm_facts, &touched);
    let warm_facts_used = scoped.facts.len();
    let warm_facts_scoped_out = warm_facts.facts.len().saturating_sub(warm_facts_used);
    modified.warm_facts = scoped;
    let warm = solve(index, modified.clone()).map_err(|err| eyre::eyre!("{err:?}"))?;

    modified.warm_facts = VerifiedWarmFacts::new();
    let cold = solve(index, modified).map_err(|err| eyre::eyre!("{err:?}"))?;
    let oracle_selected_equal = warm.selected == cold.selected;

    Ok(CounterfactualResult {
        query,
        manifest_edit,
        selected_diff: selection_diff(&completed.selected, &warm.selected),
        warm_decisions: warm.decisions,
        cold_decisions: cold.decisions,
        warm_conflicts: warm.conflicts,
        cold_conflicts: cold.conflicts,
        warm_facts_used,
        warm_facts_scoped_out,
        oracle_selected_equal,
    })
}

fn scope_warm_facts_for_counterfactual(
    index: &CompiledIndex,
    warm_facts: &VerifiedWarmFacts,
    touched: &BTreeSet<String>,
) -> VerifiedWarmFacts {
    VerifiedWarmFacts {
        facts: warm_facts
            .facts
            .iter()
            .filter(|fact| !warm_fact_touches(index, fact, touched))
            .cloned()
            .collect(),
        rejected: warm_facts.rejected.clone(),
    }
}

fn warm_fact_touches(
    index: &CompiledIndex,
    fact: &VerifiedWarmFact,
    touched: &BTreeSet<String>,
) -> bool {
    fact.fact.support.iter().any(|edge| {
        label_or_base_touched(index, &edge.parent, touched)
            || label_or_base_touched(index, &edge.dep, touched)
    }) || fact.fact.region.versions.keys().any(|pkg| {
        index
            .name(*pkg)
            .is_some_and(|name| label_or_base_touched(index, name, touched))
    })
}

fn label_or_base_touched(index: &CompiledIndex, label: &str, touched: &BTreeSet<String>) -> bool {
    touched.contains(label)
        || index
            .package(label)
            .and_then(|pkg| index.base_name(pkg))
            .is_some_and(|base| touched.contains(base))
}

fn selection_diff(
    before: &BTreeMap<String, Version>,
    after: &BTreeMap<String, Version>,
) -> SelectionDiff {
    let added = after
        .iter()
        .filter(|(pkg, _)| !before.contains_key(*pkg))
        .map(|(pkg, version)| RecordSelection {
            pkg: pkg.clone(),
            version: version.to_string(),
        })
        .collect();
    let removed = before
        .iter()
        .filter(|(pkg, _)| !after.contains_key(*pkg))
        .map(|(pkg, version)| RecordSelection {
            pkg: pkg.clone(),
            version: version.to_string(),
        })
        .collect();
    let changed = before
        .iter()
        .filter_map(|(pkg, from)| {
            after.get(pkg).and_then(|to| {
                (to != from).then(|| ChangedSelection {
                    pkg: pkg.clone(),
                    from: from.to_string(),
                    to: to.to_string(),
                })
            })
        })
        .collect();
    SelectionDiff {
        added,
        removed,
        changed,
    }
}

pub fn write_round3_record(name: &str, path: &Path) -> Result<SolveRecord> {
    let record = solve_round3_record(name)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("creating record output parent {}", parent.display()))?;
    }
    let json = facet_json::to_string(&record).wrap_err("serializing Rodin solve record")?;
    fs::write(path, format!("{json}\n"))
        .wrap_err_with(|| format!("writing Rodin solve record {}", path.display()))?;
    Ok(record)
}

fn record_packages(index: &CompiledIndex) -> Vec<RecordPackage> {
    index
        .labels
        .iter()
        .map(|(pkg, label)| {
            let source = index
                .packages
                .resolve(*pkg)
                .and_then(|pkg_id| index.sources.resolve_str(pkg_id.source))
                .unwrap_or(CRATES_IO)
                .to_owned();
            let name = index.base_name(*pkg).unwrap_or(label);
            RecordPackage {
                id: package_id(&source, label),
                source: source.clone(),
                name: name.to_owned(),
                versions: index
                    .versions(*pkg)
                    .iter()
                    .rev()
                    .map(ToString::to_string)
                    .collect(),
            }
        })
        .collect()
}

fn record_graph(index: &CompiledIndex, _problem: &Problem, result: &SolveResult) -> RecordGraph {
    let mut edges = BTreeMap::<String, RecordEdge>::new();
    for (parent, version) in &result.selected {
        let Some(parent_pkg) = index.package(parent) else {
            continue;
        };
        let Some(manifest) = index.manifests.get(&(parent_pkg, version.clone())) else {
            continue;
        };
        for dep in &manifest.deps {
            if dep.optional {
                continue;
            }
            let Ok(req) = parse_req(&dep.req) else {
                continue;
            };
            let Some(dep_pkg) = index.package_for_set(&dep.actual, &req, dep.class_hint) else {
                continue;
            };
            let Some(dep_label) = index.name(dep_pkg) else {
                continue;
            };
            if !result.selected.contains_key(dep_label) {
                continue;
            }
            let id = edge_id(index, parent, dep_label);
            edges.entry(id.clone()).or_insert_with(|| RecordEdge {
                id,
                from: package_id_for_name(index, parent),
                to: package_id_for_name(index, dep_label),
                requirement: dep.req.clone(),
            });
        }
    }

    let derivations: BTreeMap<_, _> = result
        .selected_derivations
        .iter()
        .map(|derivation| (derivation.pkg.as_str(), derivation.clone()))
        .collect();
    let nodes = result
        .selected
        .iter()
        .map(|(pkg, version)| {
            let features = result
                .enabled_features
                .get(pkg)
                .map(|features| features.iter().cloned().collect())
                .unwrap_or_default();
            RecordNode {
                pkg: package_id_for_name(index, pkg),
                version: version.to_string(),
                features,
                derivation: derivations.get(pkg.as_str()).cloned().unwrap_or_else(|| {
                    let digest = selection_digest(pkg, version, &[]);
                    SelectionDerivation {
                        pkg: pkg.clone(),
                        version: version.to_string(),
                        digest,
                        killed_higher: Vec::new(),
                    }
                }),
                ladder: record_ladder(index, result, pkg, version),
            }
        })
        .collect();

    RecordGraph {
        nodes,
        edges: edges.into_values().collect(),
    }
}

fn record_ladder(
    index: &CompiledIndex,
    result: &SolveResult,
    pkg_name: &str,
    selected: &Version,
) -> Vec<RecordRung> {
    let Some(pkg) = index.package(pkg_name) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let derivation = result
        .selected_derivations
        .iter()
        .find(|derivation| derivation.pkg == pkg_name);
    for version in index.versions(pkg).iter().rev() {
        if version < selected {
            continue;
        }
        let verdict = if version == selected {
            RungVerdict::Selected {
                selection_digest: derivation
                    .map(|derivation| derivation.digest.clone())
                    .unwrap_or_else(|| selection_digest(pkg_name, selected, &[])),
            }
        } else {
            derivation
                .and_then(|derivation| {
                    derivation
                        .killed_higher
                        .iter()
                        .find(|kill| kill.version == version.to_string())
                })
                .map_or_else(
                    || RungVerdict::Unexplained {
                        digest: constraint_digest("unexplained", pkg_name, version, &[], &[]),
                    },
                    |kill| RungVerdict::Killed {
                        reason: kill.reason.clone(),
                    },
                )
        };
        out.push(RecordRung {
            version: version.to_string(),
            verdict,
        });
    }
    out
}

fn add_forced_dependency_closure(index: &CompiledIndex, versions: &mut BTreeMap<PkgIx, Version>) {
    loop {
        let mut changed = false;
        let current: Vec<_> = versions
            .iter()
            .map(|(pkg, version)| (*pkg, version.clone()))
            .collect();
        for (pkg, version) in current {
            let Some(manifest) = index.manifests.get(&(pkg, version)) else {
                continue;
            };
            for dep in &manifest.deps {
                if dep.optional {
                    continue;
                }
                let Ok(req) = parse_req(&dep.req) else {
                    continue;
                };
                let Some(dep_pkg) = index.package_for_set(&dep.actual, &req, dep.class_hint) else {
                    continue;
                };
                if versions.contains_key(&dep_pkg) {
                    continue;
                }
                let candidates: Vec<_> = index
                    .versions(dep_pkg)
                    .iter()
                    .filter(|version| req.contains(version))
                    .cloned()
                    .collect();
                if let [candidate] = candidates.as_slice() {
                    versions.insert(dep_pkg, candidate.clone());
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

fn apply_direct_dependency_versions(
    index: &CompiledIndex,
    pkg: PkgIx,
    version: &Version,
    versions: &mut BTreeMap<PkgIx, Version>,
) {
    let Some(manifest) = index.manifests.get(&(pkg, version.clone())) else {
        return;
    };
    for dep in &manifest.deps {
        if dep.optional {
            continue;
        }
        let Ok(req) = parse_req(&dep.req) else {
            continue;
        };
        let Some(dep_pkg) = index.package_for_set(&dep.actual, &req, dep.class_hint) else {
            continue;
        };
        let candidates: Vec<_> = index
            .versions(dep_pkg)
            .iter()
            .filter(|version| req.contains(version))
            .cloned()
            .collect();
        if let [candidate] = candidates.as_slice() {
            versions.insert(dep_pkg, candidate.clone());
        }
    }
}

fn record_facts(index: &CompiledIndex, learned: &[LearnedNoGood]) -> Vec<RecordFact> {
    learned
        .iter()
        .enumerate()
        .map(|(ix, fact)| {
            let region = record_region(index, &fact.region);
            RecordFact {
                id: format!("fact-{ix}"),
                kind: fact.kind.as_str().to_owned(),
                proof_digest: fact.proof_digest.0.clone(),
                verification_status: fact.verification_status.clone(),
                support_edges: support_edge_ids(index, &fact.support),
                widened_from: fact.point_versions,
                widened_to: fact.widened_versions,
                region,
            }
        })
        .collect()
}

#[derive(Clone, Debug)]
struct PressureScenario {
    name: String,
    shape: String,
    kind: LearnedKind,
    index: CompiledIndex,
    problem: Problem,
    perturbed: Problem,
}

fn pressure_scenarios() -> Result<Vec<PressureScenario>> {
    Ok(vec![
        buried_pin(false)?,
        buried_pin(true)?,
        feature_triggered_pin()?,
        downgrade_cascade()?,
        hole_trap()?,
        exclusion_collision()?,
        lockstep_trap()?,
    ])
}

fn latest_exact_req(index: &CompiledIndex, name: &str) -> Option<String> {
    let pkg = index.package(name)?;
    index
        .versions(pkg)
        .last()
        .map(|version| format!("={version}"))
}

fn buried_pin(two_level: bool) -> Result<PressureScenario> {
    let mut index = CompiledIndex::empty();
    add_leaf_versions(&mut index, "a", 1, 3);
    if two_level {
        index.add_package_version("x", rung(1), Vec::new(), BTreeMap::new());
        index.add_package_version(
            "x",
            rung(2),
            vec![SyntheticDep::normal("mid", exact_rung(1))],
            BTreeMap::new(),
        );
        index.add_package_version(
            "mid",
            rung(1),
            vec![SyntheticDep::normal("a", exact_rung(1))],
            BTreeMap::new(),
        );
    } else {
        index.add_package_version("x", rung(1), Vec::new(), BTreeMap::new());
        index.add_package_version(
            "x",
            rung(2),
            vec![SyntheticDep::normal("a", exact_rung(1))],
            BTreeMap::new(),
        );
    }
    index.finish_synthetic()?;
    let mut problem = Problem::new(vec![RootReq::new("a", "*"), RootReq::new("x", "*")]);
    problem.learned_kind = LearnedKind::Plain;
    let mut perturbed = Problem::new(vec![
        RootReq::new("a", ">=1.0.0, <1.0.3"),
        RootReq::new("x", "*"),
    ]);
    perturbed.learned_kind = LearnedKind::Plain;
    Ok(PressureScenario {
        name: if two_level {
            "buried-pin-two-level".to_owned()
        } else {
            "buried-pin".to_owned()
        },
        shape: if two_level {
            "buried pin, two levels deep".to_owned()
        } else {
            "buried pin".to_owned()
        },
        kind: LearnedKind::Plain,
        index,
        problem,
        perturbed,
    })
}

fn feature_triggered_pin() -> Result<PressureScenario> {
    let mut index = CompiledIndex::empty();
    add_leaf_versions(&mut index, "a", 1, 3);
    index.add_package_version("x", rung(1), Vec::new(), BTreeMap::new());
    index.add_package_version(
        "x",
        rung(2),
        vec![SyntheticDep::normal("a", exact_rung(1)).optional(true)],
        BTreeMap::from([("pin".to_owned(), vec!["dep:a".to_owned()])]),
    );
    index.add_package_version("trigger", rung(1), Vec::new(), BTreeMap::new());
    index.add_package_version(
        "trigger",
        rung(2),
        vec![SyntheticDep::normal("x", exact_rung(2)).feature("pin")],
        BTreeMap::new(),
    );
    index.finish_synthetic()?;
    let mut problem = Problem::new(vec![
        RootReq::new("a", "*"),
        RootReq::new("x", "*"),
        RootReq::new("trigger", "*"),
    ]);
    problem.learned_kind = LearnedKind::FeatureGuarded;
    let mut perturbed = Problem::new(vec![
        RootReq::new("a", ">=1.0.0, <1.0.3"),
        RootReq::new("x", "*"),
        RootReq::new("trigger", "*"),
    ]);
    perturbed.learned_kind = LearnedKind::FeatureGuarded;
    Ok(PressureScenario {
        name: "feature-triggered-pin".to_owned(),
        shape: "feature-triggered pin".to_owned(),
        kind: LearnedKind::FeatureGuarded,
        index,
        problem,
        perturbed,
    })
}

fn downgrade_cascade() -> Result<PressureScenario> {
    let mut index = CompiledIndex::empty();
    add_leaf_versions(&mut index, "tokio-like", 1, 3);
    add_leaf_versions(&mut index, "codec", 1, 3);
    index.add_package_version(
        "web",
        rung(3),
        vec![
            SyntheticDep::normal("tokio-like", exact_rung(3)),
            SyntheticDep::normal("codec", exact_rung(3)),
        ],
        BTreeMap::new(),
    );
    index.add_package_version(
        "web",
        rung(2),
        vec![
            SyntheticDep::normal("tokio-like", exact_rung(1)),
            SyntheticDep::normal("codec", exact_rung(1)),
        ],
        BTreeMap::new(),
    );
    index.add_package_version(
        "web",
        rung(1),
        vec![
            SyntheticDep::normal("tokio-like", exact_rung(1)),
            SyntheticDep::normal("codec", exact_rung(3)),
        ],
        BTreeMap::new(),
    );
    index.add_package_version(
        "client",
        rung(1),
        vec![
            SyntheticDep::normal("web", "*"),
            SyntheticDep::normal("codec", exact_rung(3)),
        ],
        BTreeMap::new(),
    );
    index.finish_synthetic()?;
    let mut problem = Problem::new(vec![RootReq::new("client", "*")]);
    problem
        .decrees
        .push(Decree::new("tokio-like", exact_rung(1)));
    problem.learned_kind = LearnedKind::DecreeDerived;
    let mut perturbed = Problem::new(vec![RootReq::new("client", "*")]);
    perturbed
        .decrees
        .push(Decree::new("tokio-like", ">=1.0.0, <1.0.3"));
    perturbed.learned_kind = LearnedKind::DecreeDerived;
    Ok(PressureScenario {
        name: "downgrade-cascade".to_owned(),
        shape: "downgrade cascade".to_owned(),
        kind: LearnedKind::DecreeDerived,
        index,
        problem,
        perturbed,
    })
}

fn hole_trap() -> Result<PressureScenario> {
    let mut index = CompiledIndex::empty();
    add_leaf_versions(&mut index, "a", 1, 3);
    add_leaf_versions(&mut index, "b", 1, 3);
    index.add_package_version(
        "fallback",
        rung(1),
        vec![
            SyntheticDep::normal("a", exact_rung(3)),
            SyntheticDep::normal("b", exact_rung(3)),
        ],
        BTreeMap::new(),
    );
    index.add_package_version(
        "fallback",
        rung(2),
        vec![
            SyntheticDep::normal("a", exact_rung(3)),
            SyntheticDep::normal("b", exact_rung(1)),
        ],
        BTreeMap::new(),
    );
    index.add_package_version(
        "fallback",
        rung(3),
        vec![
            SyntheticDep::normal("a", exact_rung(3)),
            SyntheticDep::normal("b", exact_rung(3)),
        ],
        BTreeMap::new(),
    );
    index.add_package_version(
        "other",
        rung(1),
        vec![SyntheticDep::normal("b", exact_rung(3))],
        BTreeMap::new(),
    );
    index.finish_synthetic()?;
    let mut problem = Problem::new(vec![
        RootReq::new("b", "*"),
        RootReq::new("fallback", "<1.0.2"),
    ]);
    problem.learned_kind = LearnedKind::HoleAware;
    let mut perturbed = Problem::new(vec![
        RootReq::new("b", ">=1.0.0, <1.0.3"),
        RootReq::new("fallback", "<1.0.2"),
    ]);
    perturbed.learned_kind = LearnedKind::HoleAware;
    Ok(PressureScenario {
        name: "hole-trap".to_owned(),
        shape: "hole trap".to_owned(),
        kind: LearnedKind::HoleAware,
        index,
        problem,
        perturbed,
    })
}

fn exclusion_collision() -> Result<PressureScenario> {
    let mut index = CompiledIndex::empty();
    index.add_package_version(
        "mode",
        v(1, 0, 0),
        Vec::new(),
        BTreeMap::from([
            ("fast".to_owned(), Vec::new()),
            ("small".to_owned(), Vec::new()),
        ]),
    );
    index.add_package_version(
        "left",
        rung(1),
        vec![SyntheticDep::normal("mode", exact_rung(1)).feature("fast")],
        BTreeMap::new(),
    );
    index.add_package_version(
        "left",
        rung(2),
        vec![SyntheticDep::normal("mode", exact_rung(1)).feature("fast")],
        BTreeMap::new(),
    );
    index.add_package_version(
        "right",
        rung(1),
        vec![SyntheticDep::normal("mode", exact_rung(1)).feature("small")],
        BTreeMap::new(),
    );
    index.finish_synthetic()?;
    let mut problem = Problem::new(vec![RootReq::new("left", "*"), RootReq::new("right", "*")]);
    problem.exclusions.push(FeatureExclusion {
        package: "mode".to_owned(),
        features: BTreeSet::from(["fast".to_owned(), "small".to_owned()]),
    });
    problem.learned_kind = LearnedKind::Exclusion;
    let perturbed = problem.clone();
    Ok(PressureScenario {
        name: "exclusion-collision".to_owned(),
        shape: "exclusion collision".to_owned(),
        kind: LearnedKind::Exclusion,
        index,
        problem,
        perturbed,
    })
}

fn lockstep_trap() -> Result<PressureScenario> {
    let mut index = CompiledIndex::empty();
    add_leaf_versions(&mut index, "core", 1, 3);
    index.add_package_version(
        "derive",
        rung(1),
        vec![SyntheticDep::normal("core", exact_rung(1))],
        BTreeMap::new(),
    );
    index.add_package_version(
        "derive",
        rung(2),
        vec![SyntheticDep::normal("core", exact_rung(2))],
        BTreeMap::new(),
    );
    index.add_package_version(
        "floor",
        rung(1),
        vec![SyntheticDep::normal("core", exact_rung(2))],
        BTreeMap::new(),
    );
    index.finish_synthetic()?;
    let mut problem = Problem::new(vec![
        RootReq::new("core", "*"),
        RootReq::new("derive", exact_rung(1)),
        RootReq::new("floor", "*"),
    ]);
    problem.learned_kind = LearnedKind::Family;
    let mut perturbed = Problem::new(vec![
        RootReq::new("core", ">=1.0.0, <1.0.3"),
        RootReq::new("derive", ">=1.0.0, <1.0.2"),
        RootReq::new("floor", "*"),
    ]);
    perturbed.decrees.push(Decree::new("derive", exact_rung(1)));
    perturbed.learned_kind = LearnedKind::Family;
    Ok(PressureScenario {
        name: "lockstep-trap".to_owned(),
        shape: "lockstep trap".to_owned(),
        kind: LearnedKind::Family,
        index,
        problem,
        perturbed,
    })
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

fn v(major: u64, minor: u64, patch: u64) -> Version {
    Version::new(major, minor, patch)
}

fn push_scenario_row(out: &mut String, row: &ScenarioResult, warm: bool) {
    out.push_str("| ");
    out.push_str(&row.name);
    out.push_str(" | ");
    out.push_str(&row.shape);
    out.push_str(" | ");
    out.push_str(row.kind.as_str());
    out.push_str(" | ");
    out.push_str(&row.cold_decisions.to_string());
    out.push_str(" | ");
    out.push_str(&row.cold_conflicts.to_string());
    out.push_str(" | ");
    if warm {
        out.push_str(&row.warm_decisions.to_string());
        out.push_str(" | ");
        out.push_str(&row.warm_conflicts.to_string());
        out.push_str(" | ");
        out.push_str(
            &row.cold_decisions
                .saturating_sub(row.warm_decisions)
                .to_string(),
        );
        out.push_str(" | ");
        out.push_str(
            &row.cold_conflicts
                .saturating_sub(row.warm_conflicts)
                .to_string(),
        );
        out.push_str(" | ");
    }
    out.push_str(&row.learned.len().to_string());
    out.push_str(" |\n");
}

fn learned_by_kind(report: &Round3Report) -> BTreeMap<LearnedKind, Vec<&LearnedNoGood>> {
    let mut out: BTreeMap<LearnedKind, Vec<&LearnedNoGood>> = BTreeMap::new();
    for scenario in report.natural.iter().chain(&report.pressure) {
        for fact in &scenario.learned {
            out.entry(fact.kind).or_default().push(fact);
        }
    }
    out
}

fn factors(learned: &[&LearnedNoGood]) -> Vec<f64> {
    let mut factors: Vec<_> = learned.iter().map(|fact| fact.widening_factor()).collect();
    factors.sort_by(|left, right| left.total_cmp(right));
    factors
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((sorted.len() - 1) as f64 * q).ceil() as usize;
    sorted[rank]
}

fn percent(numerator: usize, denominator: usize) -> String {
    decimal(if denominator == 0 {
        0.0
    } else {
        numerator as f64 * 100.0 / denominator as f64
    })
}

fn decimal(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    let mut text = format!("{value:.6}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

fn round3_verdict(
    point: usize,
    widened: usize,
    cold_decisions: usize,
    warm_decisions: usize,
    cold_conflicts: usize,
    warm_conflicts: usize,
) -> String {
    let widening_factor = widened as f64 / point.max(1) as f64;
    let decision_savings = cold_decisions.saturating_sub(warm_decisions);
    let conflict_savings = cold_conflicts.saturating_sub(warm_conflicts);
    if widening_factor > 1.0 && (decision_savings > 0 || conflict_savings > 0) {
        format!(
            "Derivation-support widening pays in this round: it expands exclusion coverage by {}x and the perturbed pressure re-solves save {decision_savings} decisions and {}. The effect is not uniform by kind, so the useful design signal is the per-kind table rather than a single global rescue story.",
            decimal(widening_factor),
            plural(conflict_savings, "conflict", "conflicts")
        )
    } else {
        format!(
            "Derivation-support widening does not pay clearly in this round: coverage is only {}x and warm pressure re-solves save {decision_savings} decisions and {}. That is a negative result for dynamic value, even if individual shapes still widen.",
            decimal(widening_factor),
            plural(conflict_savings, "conflict", "conflicts")
        )
    }
}

fn plural(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("{count} {singular}")
    } else {
        format!("{count} {plural}")
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn propagation_handles_weak_feature_guard() -> Result<()> {
        let mut index = CompiledIndex::empty();
        index.add_package_version(
            "addon",
            v(1, 0, 0),
            Vec::new(),
            BTreeMap::from([("extra".to_owned(), Vec::new())]),
        );
        index.add_package_version(
            "root",
            v(1, 0, 0),
            vec![SyntheticDep::normal("addon", "=1.0.0").optional(true)],
            BTreeMap::from([("weak".to_owned(), vec!["addon?/extra".to_owned()])]),
        );
        index.add_package_version(
            "activator",
            v(1, 0, 0),
            vec![SyntheticDep::normal("addon", "=1.0.0")],
            BTreeMap::new(),
        );
        index.finish_synthetic()?;

        let result = solve(
            &index,
            Problem::new(vec![
                RootReq::new("root", "*").feature("weak"),
                RootReq::new("activator", "*"),
            ]),
        )
        .map_err(|err| eyre::eyre!("{err:?}"))?;

        assert!(result.enabled_features["addon@1"].contains("extra"));
        Ok(())
    }

    #[test]
    fn backjump_learns_and_continues_after_hand_built_conflict() -> Result<()> {
        let scenario = downgrade_cascade()?;
        let result =
            solve(&scenario.index, scenario.problem).map_err(|err| eyre::eyre!("{err:?}"))?;
        let learned = result.learned;
        let conflicts = result.conflicts;
        assert!(conflicts > 0);
        assert!(!learned.is_empty());
        assert!(
            learned[0]
                .support
                .iter()
                .any(|edge| edge.parent == "web@1" && edge.dep == "tokio-like@1")
        );
        Ok(())
    }

    #[test]
    fn learned_region_mentions_only_refuting_support_packages() -> Result<()> {
        let scenario = downgrade_cascade()?;
        let result =
            solve(&scenario.index, scenario.problem).map_err(|err| eyre::eyre!("{err:?}"))?;
        let fact = result.learned.first().expect("first learned fact");
        let support_packages = scenario.index.support_packages(&fact.support);

        assert_eq!(
            fact.support,
            BTreeSet::from([EdgeKey {
                parent: "web@1".to_owned(),
                dep: "tokio-like@1".to_owned(),
            }])
        );
        assert_eq!(
            fact.region
                .versions
                .keys()
                .copied()
                .collect::<BTreeSet<_>>(),
            support_packages
        );
        assert_eq!(
            fact.region
                .versions
                .keys()
                .filter_map(|pkg| scenario.index.name(*pkg))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["tokio-like@1", "web@1"])
        );

        result
            .derivations
            .replay(fact.derivation)
            .map_err(|err| eyre::eyre!("learned derivation did not replay: {err:?}"))?;
        Ok(())
    }

    #[test]
    fn every_learned_no_good_derivation_replays() -> Result<()> {
        for scenario in pressure_scenarios()? {
            let Ok(result) = solve(&scenario.index, scenario.problem) else {
                continue;
            };
            for fact in &result.learned {
                result.derivations.replay(fact.derivation).map_err(|err| {
                    eyre::eyre!(
                        "{} learned derivation {:?} did not replay: {err:?}",
                        scenario.name,
                        fact.derivation
                    )
                })?;
            }
        }
        Ok(())
    }

    #[test]
    fn warm_counterfactuals_verify_inline_proofs() -> Result<()> {
        let mut cold_decisions = 0;
        let mut warm_decisions = 0;
        let mut cold_conflicts = 0;
        let mut warm_conflicts = 0;
        for scenario in pressure_scenarios()? {
            let cold = run_one(
                &scenario.index,
                &scenario.name,
                &scenario.shape,
                scenario.problem.clone(),
                None,
            );
            let bundle =
                warm_bundle_from_learned(&scenario.index, &cold.learned, &cold.derivations)?;
            let verified =
                WarmFactVerifier::for_index(&scenario.index).verify(&scenario.index, &bundle);
            assert!(
                verified.rejected.is_empty(),
                "{} rejected verified warm facts: {:?}",
                scenario.name,
                verified.rejected
            );
            let warm = run_one(
                &scenario.index,
                &format!("{} perturbed", scenario.name),
                &scenario.shape,
                Problem {
                    warm_facts: verified,
                    ..scenario.perturbed
                },
                Some(&cold),
            );
            cold_decisions += cold.cold_decisions;
            warm_decisions += warm.cold_decisions;
            cold_conflicts += cold.cold_conflicts;
            warm_conflicts += warm.cold_conflicts;
        }

        assert_eq!(cold_decisions.saturating_sub(warm_decisions), 7);
        assert_eq!(cold_conflicts.saturating_sub(warm_conflicts), 5);
        Ok(())
    }

    #[test]
    fn tampered_proof_bundle_is_rejected_and_solve_continues_cold() -> Result<()> {
        let scenario = downgrade_cascade()?;
        let cold = run_one(
            &scenario.index,
            &scenario.name,
            &scenario.shape,
            scenario.problem,
            None,
        );
        let mut bundle =
            warm_bundle_from_learned(&scenario.index, &cold.learned, &cold.derivations)?;
        tamper_first_clause_witness(&mut bundle);

        let verified =
            WarmFactVerifier::for_index(&scenario.index).verify(&scenario.index, &bundle);
        assert!(verified.facts.is_empty());
        assert!(!verified.rejected.is_empty());

        let warm = solve(
            &scenario.index,
            Problem {
                warm_facts: verified,
                ..scenario.perturbed.clone()
            },
        )
        .map_err(|err| eyre::eyre!("{err:?}"))?;
        let cold_perturbed =
            solve(&scenario.index, scenario.perturbed).map_err(|err| eyre::eyre!("{err:?}"))?;
        assert_eq!(warm.decisions, cold_perturbed.decisions);
        assert_eq!(warm.conflicts, cold_perturbed.conflicts);
        Ok(())
    }

    #[test]
    fn invalid_widen_bundle_shrinks_to_point_fact() -> Result<()> {
        let mut selected = None;
        for scenario in pressure_scenarios()? {
            let cold = run_one(
                &scenario.index,
                &scenario.name,
                &scenario.shape,
                scenario.problem.clone(),
                None,
            );
            let bundle =
                warm_bundle_from_learned(&scenario.index, &cold.learned, &cold.derivations)?;
            if bundle_has_widen_root(&bundle) {
                selected = Some((scenario, cold, bundle));
                break;
            }
        }
        let Some((scenario, cold, mut bundle)) = selected else {
            return Err(eyre::eyre!(
                "no pressure scenario produced a widened proof root"
            ));
        };
        let tampered_ix = mutate_first_widen_witness_with_fresh_digest(&mut bundle)?;
        let original_digest = cold.learned[tampered_ix].proof_digest.clone();

        let verified =
            WarmFactVerifier::for_index(&scenario.index).verify(&scenario.index, &bundle);
        let shrunk = &verified
            .facts
            .iter()
            .find(|fact| fact.fact.verification_status == VerificationStatus::ShrunkInvalidWiden)
            .expect("shrunk fact")
            .fact;
        assert_eq!(
            shrunk.verification_status,
            VerificationStatus::ShrunkInvalidWiden
        );
        assert_eq!(shrunk.point_versions, 1);
        assert_eq!(shrunk.widened_versions, 1);
        assert_ne!(shrunk.proof_digest, original_digest);
        let shrunk_digest = shrunk.proof_digest.0.clone();
        let shrunk_status = shrunk.verification_status.clone();

        let learned = match solve(
            &scenario.index,
            Problem {
                warm_facts: verified,
                ..scenario.perturbed
            },
        ) {
            Ok(result) => result.learned,
            Err(err) => match *err {
                SolveError::NoSolution { learned, .. } => learned,
            },
        };
        let records = record_facts(&scenario.index, &learned);
        assert!(records.iter().any(|fact| {
            fact.proof_digest == shrunk_digest && fact.verification_status == shrunk_status
        }));
        Ok(())
    }

    #[test]
    fn lazy_no_proof_bundle_is_rejected() -> Result<()> {
        let scenario = downgrade_cascade()?;
        let cold = run_one(
            &scenario.index,
            &scenario.name,
            &scenario.shape,
            scenario.problem,
            None,
        );
        let mut bundle =
            warm_bundle_from_learned(&scenario.index, &cold.learned, &cold.derivations)?;
        bundle.proofs = WarmProofSource::Lazy(ProofFetchRef {
            digest: ProofDigest("lazy-unimplemented".to_owned()),
        });

        let verified =
            WarmFactVerifier::for_index(&scenario.index).verify(&scenario.index, &bundle);
        assert!(verified.facts.is_empty());
        assert_eq!(verified.rejected.len(), bundle.facts.len());
        assert!(
            verified
                .rejected
                .iter()
                .all(|rejection| rejection.reason == WarmFactRejectionReason::LazyProofUnavailable)
        );
        Ok(())
    }

    #[test]
    fn selected_derivations_ship_kill_claims_for_successes() -> Result<()> {
        let scenario = downgrade_cascade()?;
        let result =
            solve(&scenario.index, scenario.problem).map_err(|err| eyre::eyre!("{err:?}"))?;
        let web = result
            .selected_derivations
            .iter()
            .find(|derivation| derivation.pkg == "web@1")
            .expect("web selected derivation");

        assert_eq!(web.version, "1.0.0");
        assert_eq!(web.killed_higher.len(), 2);
        assert!(
            web.killed_higher
                .iter()
                .all(|kill| matches!(kill.reason, KillReason::LearnedExclusion { .. }))
        );
        assert!(web.killed_higher.iter().all(|kill| {
            matches!(
                &kill.reason,
                KillReason::LearnedExclusion {
                    proof_root,
                    proof_digest,
                    ..
                } if !proof_root.is_empty() && proof_root == proof_digest
            )
        }));
        Ok(())
    }

    #[test]
    fn counterfactual_drop_dependency_matches_cold_modified_problem() -> Result<()> {
        let (index, problem) = counterfactual_surface_fixture()?;
        let completed = solve(&index, problem.clone()).map_err(|err| eyre::eyre!("{err:?}"))?;
        let verified = verified_warm_facts(&index, &completed)?;

        let counterfactual =
            counterfactual_drop_dependency(&index, &problem, &completed, &verified, "noise")?;

        assert_eq!(
            counterfactual.manifest_edit,
            ManifestEdit::RemoveRootDependency {
                package: "noise".to_owned()
            }
        );
        assert!(counterfactual.oracle_selected_equal);
        assert_eq!(
            counterfactual.selected_diff.removed,
            vec![RecordSelection {
                pkg: "noise@1".to_owned(),
                version: "1.0.0".to_owned()
            }]
        );
        assert!(
            counterfactual.warm_decisions < counterfactual.cold_decisions,
            "warm={} cold={}",
            counterfactual.warm_decisions,
            counterfactual.cold_decisions
        );
        assert_eq!(counterfactual.warm_decisions, 1);
        assert_eq!(counterfactual.cold_decisions, 3);
        assert_eq!(counterfactual.warm_conflicts, 0);
        assert_eq!(counterfactual.cold_conflicts, 1);
        Ok(())
    }

    #[test]
    fn counterfactual_decree_version_matches_cold_modified_problem() -> Result<()> {
        let (index, problem) = counterfactual_surface_fixture()?;
        let completed = solve(&index, problem.clone()).map_err(|err| eyre::eyre!("{err:?}"))?;
        let verified = verified_warm_facts(&index, &completed)?;

        let counterfactual = counterfactual_decree_version(
            &index, &problem, &completed, &verified, "noise", "1.0.0",
        )?;

        assert_eq!(
            counterfactual.manifest_edit,
            ManifestEdit::AddOrReplaceDecree {
                package: "noise".to_owned(),
                requirement: "=1.0.0".to_owned()
            }
        );
        assert!(counterfactual.oracle_selected_equal);
        assert!(counterfactual.selected_diff.added.is_empty());
        assert!(counterfactual.selected_diff.removed.is_empty());
        assert!(counterfactual.selected_diff.changed.is_empty());
        assert!(
            counterfactual.warm_decisions < counterfactual.cold_decisions,
            "warm={} cold={}",
            counterfactual.warm_decisions,
            counterfactual.cold_decisions
        );
        assert_eq!(counterfactual.warm_decisions, 1);
        assert_eq!(counterfactual.cold_decisions, 3);
        assert_eq!(counterfactual.warm_conflicts, 0);
        assert_eq!(counterfactual.cold_conflicts, 1);

        let json = facet_json::to_string(&counterfactual)?;
        let decoded: CounterfactualResult = facet_json::from_str(&json)?;
        assert_eq!(decoded, counterfactual);
        Ok(())
    }

    #[test]
    fn counterfactual_scope_gate_drops_touched_warm_facts() -> Result<()> {
        let (index, problem) = counterfactual_surface_fixture()?;
        let completed = solve(&index, problem.clone()).map_err(|err| eyre::eyre!("{err:?}"))?;
        let verified = verified_warm_facts(&index, &completed)?;

        let counterfactual =
            counterfactual_decree_version(&index, &problem, &completed, &verified, "a", "1.0.2")?;

        assert!(counterfactual.oracle_selected_equal);
        assert!(counterfactual.warm_facts_scoped_out > 0);
        assert_eq!(counterfactual.warm_facts_used, 0);
        Ok(())
    }

    fn tamper_first_clause_witness(bundle: &mut WarmFactBundle) {
        let WarmProofSource::Inline(graph) = &mut bundle.proofs else {
            return;
        };
        let graph = Arc::make_mut(graph);
        for node in graph.nodes.values_mut() {
            for premise in &mut node.premises {
                if let ProofPremise::Clause(witness) = premise {
                    if let Some(AtomWitness::InGraph { package }) = witness.antecedents.first_mut()
                    {
                        package.push_str("-tampered");
                        return;
                    }
                    if let Some(AtomWitness::Ver { package, .. }) = witness.antecedents.first_mut()
                    {
                        package.push_str("-tampered");
                        return;
                    }
                }
            }
        }
    }

    fn mutate_first_widen_witness_with_fresh_digest(bundle: &mut WarmFactBundle) -> Result<usize> {
        let WarmProofSource::Inline(graph) = &mut bundle.proofs else {
            return Err(eyre::eyre!("bundle is not inline"));
        };
        let graph = Arc::make_mut(graph);
        let (fact_ix, old_root) = bundle
            .facts
            .iter()
            .enumerate()
            .find_map(|(ix, fact)| {
                graph.nodes.get(&fact.proof_root).and_then(|node| {
                    matches!(node.rule, ProofRule::Widen { .. })
                        .then(|| (ix, fact.proof_root.clone()))
                })
            })
            .ok_or_else(|| eyre::eyre!("bundle has no widened proof root"))?;
        let mut node = graph
            .nodes
            .remove(&old_root)
            .ok_or_else(|| eyre::eyre!("missing root node"))?;
        let ProofRule::Widen { witness } = &mut node.rule else {
            return Err(eyre::eyre!("first proof root is not a widen node"));
        };
        let Some(edge) = witness.edge_stability.first_mut() else {
            return Err(eyre::eyre!("widen witness has no edge stability"));
        };
        edge.stable_versions.clear();
        let new_root = proof_node_id(&node);
        graph.nodes.insert(new_root.clone(), node);
        bundle.facts[fact_ix].proof_root = new_root.clone();
        bundle.facts[fact_ix].proof_digest = ProofDigest(new_root.0.clone());
        bundle.facts[fact_ix].fact.proof_root = new_root.clone();
        bundle.facts[fact_ix].fact.proof_digest = ProofDigest(new_root.0);
        Ok(fact_ix)
    }

    fn bundle_has_widen_root(bundle: &WarmFactBundle) -> bool {
        let WarmProofSource::Inline(graph) = &bundle.proofs else {
            return false;
        };
        bundle.facts.iter().any(|fact| {
            graph
                .nodes
                .get(&fact.proof_root)
                .is_some_and(|node| matches!(node.rule, ProofRule::Widen { .. }))
        })
    }

    fn verified_warm_facts(
        index: &CompiledIndex,
        completed: &SolveResult,
    ) -> Result<VerifiedWarmFacts> {
        let bundle = warm_bundle_from_learned(index, &completed.learned, &completed.derivations)?;
        let verified = WarmFactVerifier::for_index(index).verify(index, &bundle);
        assert!(
            verified.rejected.is_empty(),
            "fresh local bundle rejected warm facts: {:?}",
            verified.rejected
        );
        Ok(verified)
    }

    fn counterfactual_surface_fixture() -> Result<(CompiledIndex, Problem)> {
        let mut index = CompiledIndex::empty();
        add_leaf_versions(&mut index, "a", 1, 3);
        index.add_package_version("noise", rung(1), Vec::new(), BTreeMap::new());
        index.add_package_version("x", rung(1), Vec::new(), BTreeMap::new());
        index.add_package_version(
            "x",
            rung(2),
            vec![SyntheticDep::normal("a", exact_rung(1))],
            BTreeMap::new(),
        );
        index.finish_synthetic()?;
        let mut problem = Problem::new(vec![
            RootReq::new("a", "*"),
            RootReq::new("noise", "*"),
            RootReq::new("x", "*"),
        ]);
        problem.learned_kind = LearnedKind::Plain;
        Ok((index, problem))
    }

    #[test]
    fn widening_range_uses_identical_supported_edge_lines() -> Result<()> {
        let mut index = CompiledIndex::empty();
        add_leaf_versions(&mut index, "a", 1, 3);
        index.add_package_version(
            "x",
            rung(1),
            vec![SyntheticDep::normal("a", exact_rung(1))],
            BTreeMap::new(),
        );
        index.add_package_version(
            "x",
            rung(2),
            vec![SyntheticDep::normal("a", exact_rung(1))],
            BTreeMap::new(),
        );
        index.add_package_version(
            "x",
            rung(3),
            vec![SyntheticDep::normal("a", exact_rung(2))],
            BTreeMap::new(),
        );
        index.finish_synthetic()?;
        let x = index.package("x").expect("x package");
        let a = index.package("a").expect("a package");
        let selected = BTreeMap::from([(x, rung(2)), (a, rung(3))]);
        let support = BTreeSet::from([EdgeKey {
            parent: "x@1".to_owned(),
            dep: "a@1".to_owned(),
        }]);
        let region = Region::new().with_versions(a, VersionSet::exact(rung(3)));
        let (widened, _, widened_count) = index.widen_region(&selected, &support, &region);

        assert_eq!(widened_count, 2);
        assert!(widened.versions[&x].contains(&rung(1)));
        assert!(widened.versions[&x].contains(&rung(2)));
        assert!(!widened.versions[&x].contains(&rung(3)));
        Ok(())
    }

    #[test]
    fn solve_record_round_trips_through_facet_json() -> Result<()> {
        let record = solve_round3_record("downgrade-cascade")?;
        let json = facet_json::to_string(&record)?;
        let decoded: SolveRecord = facet_json::from_str(&json)?;

        assert_eq!(decoded, record);
        assert!(
            record
                .graph
                .edges
                .iter()
                .any(|edge| edge.id == "edge:crates.io:web@1->crates.io:codec@1")
        );
        assert!(
            record
                .facts
                .iter()
                .any(|fact| !fact.support_edges.is_empty())
        );
        assert!(record.steps.iter().any(|step| {
            matches!(
                step,
                SolveStep::Decree {
                    origin: DecreeOrigin::User,
                    pkg,
                    ..
                } if pkg == "tokio-like@1"
            )
        }));
        Ok(())
    }

    #[test]
    fn solved_pressure_records_explain_every_higher_ladder_rung() -> Result<()> {
        let mut solved = 0;
        let mut unsat = Vec::new();
        let mut unexplained = Vec::new();
        for scenario in pressure_scenarios()? {
            let name = scenario.name.clone();
            let shape = scenario.shape.clone();
            let record = match solve_record(&scenario.index, name.clone(), shape, scenario.problem)
            {
                Ok(record) => record,
                Err(err) => {
                    tracing::debug!(scenario = name, error = %err, "pressure scenario has no solve record");
                    unsat.push(name);
                    continue;
                }
            };
            solved += 1;
            for node in &record.graph.nodes {
                for rung in &node.ladder {
                    if matches!(rung.verdict, RungVerdict::Unexplained { .. }) {
                        unexplained
                            .push(format!("{} {} {}", record.scenario, node.pkg, rung.version));
                    }
                }
            }
        }

        assert_eq!(solved, 5);
        assert_eq!(unsat, ["exclusion-collision", "lockstep-trap"]);
        assert!(
            unexplained.is_empty(),
            "unexplained higher ladder rungs: {unexplained:?}"
        );
        Ok(())
    }
}
