use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use eyre::{Context, Result};
use facet::Facet;
use rodin_facts::{
    Assignment, Atom, Clause, ClauseId, ClauseStore, Derivation, DerivationId, DerivationStore,
    Exclusion, ExclusionId, ExclusionStore, FeatIx, FeatureInterner, PackageInterner, PinRef,
    PkgId, PkgIx, Premise, Region, RuleKind, SourceInterner, SourceIx, VersionSet,
};
use semver::{Version, VersionReq};

const CRATES_IO: &str = "crates.io";

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EdgeKey {
    pub parent: String,
    pub dep: String,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Problem {
    pub roots: Vec<RootReq>,
    pub decrees: Vec<Decree>,
    pub exclusions: Vec<FeatureExclusion>,
    pub warm_facts: Vec<LearnedNoGood>,
    pub learned_kind: LearnedKind,
}

impl Problem {
    #[must_use]
    pub fn new(roots: Vec<RootReq>) -> Self {
        Self {
            roots,
            decrees: Vec::new(),
            exclusions: Vec::new(),
            warm_facts: Vec::new(),
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
    pub trace: Vec<String>,
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
    pub why: Vec<String>,
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
    Selected,
    ExcludedByRequirement { edges: Vec<String> },
    KilledByFact { fact_id: String },
    Unexplained,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct RecordFact {
    pub id: String,
    pub kind: String,
    pub region: Vec<RecordRegionTerm>,
    pub support_edges: Vec<String>,
    pub widened_from: usize,
    pub widened_to: usize,
    pub summary: String,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct RecordRegionTerm {
    pub pkg: String,
    pub versions: String,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct RecordSelection {
    pub pkg: String,
    pub version: String,
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
        reason: String,
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
    names: BTreeMap<PkgIx, String>,
    package_by_name: BTreeMap<String, PkgIx>,
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
            names: BTreeMap::new(),
            package_by_name: BTreeMap::new(),
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
            let pkg = out.intern_package(&crate_data.name);
            let mut versions = Vec::new();
            for (version, entry) in rodin_survey::sorted_non_yanked_entries(crate_data)? {
                versions.push(version.clone());
                let mut deps = Vec::new();
                for dep in &entry.deps {
                    if dep.kind != "normal" {
                        continue;
                    }
                    let actual = rodin_survey::actual_dep_name(dep);
                    out.intern_package(&actual);
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
                    });
                }
                deps.sort_by(|left, right| {
                    left.actual
                        .cmp(&right.actual)
                        .then_with(|| left.key.cmp(&right.key))
                        .then_with(|| left.req.cmp(&right.req))
                });
                out.manifests.insert(
                    (pkg, version),
                    Manifest {
                        deps,
                        features: merged_features(entry),
                        yanked: entry.yanked,
                    },
                );
            }
            versions.sort();
            out.versions.insert(pkg, versions);
        }
        out.compile()?;
        Ok(out)
    }

    #[must_use]
    pub fn package(&self, name: &str) -> Option<PkgIx> {
        self.package_by_name
            .get(&name.to_ascii_lowercase())
            .copied()
    }

    #[must_use]
    pub fn name(&self, pkg: PkgIx) -> Option<&str> {
        self.names.get(&pkg).map(String::as_str)
    }

    #[must_use]
    pub fn versions(&self, pkg: PkgIx) -> &[Version] {
        self.versions.get(&pkg).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn add_package_version(
        &mut self,
        name: impl Into<String>,
        version: Version,
        deps: Vec<SyntheticDep>,
        features: BTreeMap<String, Vec<String>>,
    ) {
        let name = name.into().to_ascii_lowercase();
        let pkg = self.intern_package(&name);
        let mut lines = Vec::new();
        for dep in deps {
            let actual = dep.package.clone().unwrap_or_else(|| dep.name.clone());
            let actual = actual.to_ascii_lowercase();
            self.intern_package(&actual);
            let mut features = dep.features;
            features.sort();
            lines.push(DepLine {
                key: dep.name,
                actual,
                req: dep.req,
                features,
                optional: dep.optional,
                default_features: dep.default_features,
                target: None,
                kind: "normal".to_owned(),
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

    fn intern_package(&mut self, name: &str) -> PkgIx {
        let key = name.to_ascii_lowercase();
        if let Some(pkg) = self.package_by_name.get(&key) {
            return *pkg;
        }
        let pkg = self.packages.intern(PkgId::new(self.source, key.clone()));
        self.package_by_name.insert(key.clone(), pkg);
        self.names.insert(pkg, key);
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
            for dep in &manifest.deps {
                let Some(dep_pkg) = self.package(&dep.actual) else {
                    continue;
                };
                let req = parse_req(&dep.req)?;
                let support = EdgeKey {
                    parent: self.name(pkg).unwrap_or_default().to_owned(),
                    dep: dep.actual.clone(),
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
            }
            for (feature, enables) in &manifest.features {
                let feat = self.intern_feature(feature);
                for enable in enables {
                    self.push_feature_enable_clause(pkg, &version, feat, enable)
                        .wrap_err_with(|| {
                            format!(
                                "compiling feature enable {} {version} {feature} -> {enable}",
                                self.name(pkg).unwrap_or_default()
                            )
                        })?;
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
        if dep.default_features {
            let default = self.intern_feature("default");
            self.push_clause(
                vec![Guard::InGraph(pkg), Guard::Selected(pkg, version.clone())],
                Consequent::Feature(dep_pkg, default),
                Some(support.clone()),
            );
        }
        for feature in &dep.features {
            let feat = self.intern_feature(feature);
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
        feat: FeatIx,
        enable: &str,
    ) -> Result<()> {
        if let Some(dep_name) = enable.strip_prefix("dep:") {
            let Some(dep) = self.dependency_line(pkg, version, dep_name) else {
                return Ok(());
            };
            let Some(dep_pkg) = self.package(&dep.actual) else {
                return Ok(());
            };
            let support = EdgeKey {
                parent: self.name(pkg).unwrap_or_default().to_owned(),
                dep: dep.actual.clone(),
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
                Consequent::VersionSet(dep_pkg, parse_req(&dep.req)?),
                Some(support.clone()),
            );
            self.push_dependency_feature_clauses(pkg, version, &dep, dep_pkg, &support);
        } else if let Some((dep_name, dep_feature)) = enable.split_once("?/") {
            let Some(dep) = self.dependency_line(pkg, version, dep_name) else {
                return Ok(());
            };
            let Some(dep_pkg) = self.package(&dep.actual) else {
                return Ok(());
            };
            let dep_feat = self.intern_feature(dep_feature);
            let support = EdgeKey {
                parent: self.name(pkg).unwrap_or_default().to_owned(),
                dep: dep.actual.clone(),
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
            let Some(dep) = self.dependency_line(pkg, version, dep_name) else {
                return Ok(());
            };
            let Some(dep_pkg) = self.package(&dep.actual) else {
                return Ok(());
            };
            let dep_feat = self.intern_feature(dep_feature);
            let support = EdgeKey {
                parent: self.name(pkg).unwrap_or_default().to_owned(),
                dep: dep.actual.clone(),
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
                Consequent::VersionSet(dep_pkg, parse_req(&dep.req)?),
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
            let local_feat = self.intern_feature(enable);
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

    fn dependency_line(&self, pkg: PkgIx, version: &Version, key: &str) -> Option<DepLine> {
        self.manifests
            .get(&(pkg, version.clone()))
            .and_then(|manifest| manifest.deps.iter().find(|dep| dep.key == key).cloned())
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
        let mut out: Vec<_> = manifest
            .deps
            .iter()
            .filter(|line| line.actual == dep)
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntheticDep {
    pub name: String,
    pub req: String,
    pub features: Vec<String>,
    pub optional: bool,
    pub default_features: bool,
    pub package: Option<String>,
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
}

pub fn solve(
    index: &CompiledIndex,
    problem: Problem,
) -> std::result::Result<SolveResult, Box<SolveError>> {
    let mut solver = Solver::new(index, problem.learned_kind);
    for fact in &problem.warm_facts {
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
        }
    }

    fn install_warm_fact(&mut self, fact: LearnedNoGood) {
        let id = self.exclusions.append(Exclusion {
            region: fact.region.clone(),
            derivation: fact.derivation,
        });
        self.derivations.note_exclusion(id, fact.derivation);
        self.learned.push(fact);
    }

    fn seed_problem(&mut self, problem: &Problem, state: &mut State) -> Result<(), Conflict> {
        for root in &problem.roots {
            let Some(pkg) = self.index.package(&root.name) else {
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
            self.activate(
                state,
                pkg,
                Premise::Pin(PinRef(format!("root:{}", root.name))),
            );
            self.steps.push(SolveStep::Decree {
                origin: DecreeOrigin::Root,
                pkg: root.name.clone(),
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
            let Some(pkg) = self.index.package(&decree.name) else {
                continue;
            };
            let requirement = display_req(&decree.req);
            self.trace
                .push(format!("decree {} {requirement}", decree.name));
            self.steps.push(SolveStep::Decree {
                origin: DecreeOrigin::User,
                pkg: decree.name.clone(),
                requirement,
            });
            self.activate(
                state,
                pkg,
                Premise::Pin(PinRef(format!("decree:{}", decree.name))),
            );
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
        for fact in &learned {
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
                    return Err(Conflict {
                        premises: vec![Premise::Exclusion(ExclusionId(0))],
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
                    self.intersect_domain(state, pkg, next, Premise::Exclusion(ExclusionId(0)))?;
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
                            reason: "only candidate".to_owned(),
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
        let mut premises = conflict.premises;
        if let Some(hypothesis) = &conflict.decision {
            premises.push(Premise::Hypothesis(hypothesis.id));
        }
        let premises = dedup_premises(premises);
        let derivation = self.derivations.append(Derivation {
            premises: premises.clone(),
            rule: RuleKind::Conflict,
        });
        let mut region = Region::new();
        if let Some(hypothesis) = conflict.decision.clone() {
            region = region.with_versions(hypothesis.pkg, VersionSet::exact(hypothesis.version));
        } else {
            for (pkg, version) in &conflict.selected {
                region = region.with_versions(*pkg, VersionSet::exact(version.clone()));
            }
        }
        for (pkg, version) in &conflict.selected {
            if !region.versions.contains_key(pkg) {
                region = region.with_versions(*pkg, VersionSet::exact(version.clone()));
            }
        }
        let support = support_from_premises(self.index, &premises);
        let (region, point_versions, widened_versions) =
            self.index
                .widen_region(&conflict.selected, &support, &region);
        self.trace.push(format!(
            "learn: {} dead; support: {}; widened {}->{} versions",
            display_region(self.index, &region),
            display_support(&support),
            point_versions,
            widened_versions
        ));
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
        self.learned.push(LearnedNoGood {
            region,
            derivation,
            support,
            point_versions,
            widened_versions,
            kind: self.problem_kind,
        });
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

fn state_fingerprint(state: &State) -> String {
    let domains: Vec<_> = state
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
    format!("{domains:?}:{:?}", state.features)
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
            Premise::Exclusion(_) | Premise::Pin(_) | Premise::Hypothesis(_) => None,
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
        let warm_facts = cold.learned.clone();
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
        },
        Err(err) => match *err {
            SolveError::NoSolution {
                decisions,
                conflicts,
                manifests_read,
                candidates_existing,
                trace,
                learned,
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
            },
        },
    }
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
        trace: result.trace,
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
        .names
        .iter()
        .map(|(pkg, name)| {
            let source = index
                .packages
                .resolve(*pkg)
                .and_then(|pkg_id| index.sources.resolve_str(pkg_id.source))
                .unwrap_or(CRATES_IO)
                .to_owned();
            RecordPackage {
                id: package_id(&source, name),
                source: source.clone(),
                name: name.clone(),
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

fn record_graph(index: &CompiledIndex, problem: &Problem, result: &SolveResult) -> RecordGraph {
    let mut why: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for root in &problem.roots {
        why.entry(root.name.clone())
            .or_default()
            .push(format!("root manifest demands {}", display_req(&root.req)));
    }
    for decree in &problem.decrees {
        why.entry(decree.name.clone())
            .or_default()
            .push(format!("user decree requires {}", display_req(&decree.req)));
    }

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
            if !result.selected.contains_key(&dep.actual) {
                continue;
            }
            let id = edge_id(index, parent, &dep.actual);
            edges.entry(id.clone()).or_insert_with(|| RecordEdge {
                id,
                from: package_id_for_name(index, parent),
                to: package_id_for_name(index, &dep.actual),
                requirement: dep.req.clone(),
            });
            why.entry(dep.actual.clone()).or_default().push(format!(
                "{parent}@{version} requires {} {}",
                dep.actual, dep.req
            ));
        }
    }

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
                why: why.remove(pkg).unwrap_or_default(),
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
    for version in index.versions(pkg).iter().rev() {
        if version < selected {
            continue;
        }
        let verdict = if version == selected {
            RungVerdict::Selected
        } else if result
            .final_domains
            .get(pkg_name)
            .is_some_and(|domain| !domain.allowed.contains(version))
        {
            let edges = result
                .final_domains
                .get(pkg_name)
                .map(|domain| {
                    support_edge_ids(index, &support_from_premises(index, &domain.reasons))
                })
                .unwrap_or_default();
            RungVerdict::ExcludedByRequirement { edges }
        } else {
            let assignment = assignment_with_rebound(index, result, pkg, version);
            result
                .learned
                .iter()
                .enumerate()
                .find(|(_, fact)| fact.region.contains_assignment(&assignment))
                .map_or(RungVerdict::Unexplained, |(ix, _)| {
                    RungVerdict::KilledByFact {
                        fact_id: format!("fact-{ix}"),
                    }
                })
        };
        out.push(RecordRung {
            version: version.to_string(),
            verdict,
        });
    }
    out
}

fn assignment_with_rebound(
    index: &CompiledIndex,
    result: &SolveResult,
    rebound_pkg: PkgIx,
    rebound_version: &Version,
) -> Assignment {
    let mut versions = BTreeMap::new();
    for (name, version) in &result.selected {
        if let Some(pkg) = index.package(name) {
            let version = if pkg == rebound_pkg {
                rebound_version.clone()
            } else {
                version.clone()
            };
            versions.insert(pkg, version);
        }
    }

    let mut features = BTreeSet::new();
    for (name, enabled) in &result.enabled_features {
        let Some(pkg) = index.package(name) else {
            continue;
        };
        for feature in enabled {
            if let Some(feat) = index.features.get_str(feature) {
                features.insert((pkg, feat));
            }
        }
    }

    add_forced_dependency_closure(index, &mut versions);

    Assignment { versions, features }
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
                let Some(dep_pkg) = index.package(&dep.actual) else {
                    continue;
                };
                if versions.contains_key(&dep_pkg) {
                    continue;
                }
                let Ok(req) = parse_req(&dep.req) else {
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
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

fn record_facts(index: &CompiledIndex, learned: &[LearnedNoGood]) -> Vec<RecordFact> {
    learned
        .iter()
        .enumerate()
        .map(|(ix, fact)| {
            let region = record_region(index, &fact.region);
            let region_text = region
                .iter()
                .map(|term| format!("{} {}", term.pkg, term.versions))
                .collect::<Vec<_>>()
                .join("; ");
            RecordFact {
                id: format!("fact-{ix}"),
                kind: fact.kind.as_str().to_owned(),
                support_edges: support_edge_ids(index, &fact.support),
                widened_from: fact.point_versions,
                widened_to: fact.widened_versions,
                summary: format!("{region_text} is impossible"),
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
        index.add_package_version("x", v(1, 0, 0), Vec::new(), BTreeMap::new());
        index.add_package_version(
            "x",
            v(2, 0, 0),
            vec![SyntheticDep::normal("mid", "=1.0.0")],
            BTreeMap::new(),
        );
        index.add_package_version(
            "mid",
            v(1, 0, 0),
            vec![SyntheticDep::normal("a", "=1.0.0")],
            BTreeMap::new(),
        );
    } else {
        index.add_package_version("x", v(1, 0, 0), Vec::new(), BTreeMap::new());
        index.add_package_version(
            "x",
            v(2, 0, 0),
            vec![SyntheticDep::normal("a", "=1.0.0")],
            BTreeMap::new(),
        );
    }
    index.finish_synthetic()?;
    let mut problem = Problem::new(vec![RootReq::new("a", "*"), RootReq::new("x", "*")]);
    problem.learned_kind = LearnedKind::Plain;
    let mut perturbed = Problem::new(vec![
        RootReq::new("a", ">=1.0.0, <4.0.0"),
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
    index.add_package_version("x", v(1, 0, 0), Vec::new(), BTreeMap::new());
    index.add_package_version(
        "x",
        v(2, 0, 0),
        vec![SyntheticDep::normal("a", "=1.0.0").optional(true)],
        BTreeMap::from([("pin".to_owned(), vec!["dep:a".to_owned()])]),
    );
    index.add_package_version("trigger", v(0, 1, 0), Vec::new(), BTreeMap::new());
    index.add_package_version(
        "trigger",
        v(1, 0, 0),
        vec![SyntheticDep::normal("x", "=2.0.0").feature("pin")],
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
        RootReq::new("a", ">=1.0.0, <4.0.0"),
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
        v(3, 0, 0),
        vec![
            SyntheticDep::normal("tokio-like", "^3"),
            SyntheticDep::normal("codec", "^3"),
        ],
        BTreeMap::new(),
    );
    index.add_package_version(
        "web",
        v(2, 0, 0),
        vec![
            SyntheticDep::normal("tokio-like", "^1"),
            SyntheticDep::normal("codec", "=1.0.0"),
        ],
        BTreeMap::new(),
    );
    index.add_package_version(
        "web",
        v(1, 0, 0),
        vec![
            SyntheticDep::normal("tokio-like", "^1"),
            SyntheticDep::normal("codec", "^3"),
        ],
        BTreeMap::new(),
    );
    index.add_package_version(
        "client",
        v(1, 0, 0),
        vec![
            SyntheticDep::normal("web", "*"),
            SyntheticDep::normal("codec", "^3"),
        ],
        BTreeMap::new(),
    );
    index.finish_synthetic()?;
    let mut problem = Problem::new(vec![RootReq::new("client", "*")]);
    problem.decrees.push(Decree::new("tokio-like", "^1"));
    problem.learned_kind = LearnedKind::DecreeDerived;
    let mut perturbed = Problem::new(vec![RootReq::new("client", "*")]);
    perturbed
        .decrees
        .push(Decree::new("tokio-like", ">=1.0.0, <3.0.0"));
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
        v(1, 0, 0),
        vec![
            SyntheticDep::normal("a", "^3"),
            SyntheticDep::normal("b", "^3"),
        ],
        BTreeMap::new(),
    );
    index.add_package_version(
        "fallback",
        v(1, 0, 1),
        vec![
            SyntheticDep::normal("a", "^3"),
            SyntheticDep::normal("b", "=1.0.0"),
        ],
        BTreeMap::new(),
    );
    index.add_package_version(
        "fallback",
        v(1, 0, 2),
        vec![
            SyntheticDep::normal("a", "^3"),
            SyntheticDep::normal("b", "^3"),
        ],
        BTreeMap::new(),
    );
    index.add_package_version(
        "other",
        v(1, 0, 0),
        vec![SyntheticDep::normal("b", "^3")],
        BTreeMap::new(),
    );
    index.finish_synthetic()?;
    let mut problem = Problem::new(vec![
        RootReq::new("b", "*"),
        RootReq::new("fallback", "<1.0.2"),
    ]);
    problem.learned_kind = LearnedKind::HoleAware;
    let mut perturbed = Problem::new(vec![
        RootReq::new("b", ">=1.0.0, <4.0.0"),
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
        v(1, 0, 0),
        vec![SyntheticDep::normal("mode", "=1.0.0").feature("fast")],
        BTreeMap::new(),
    );
    index.add_package_version(
        "left",
        v(2, 0, 0),
        vec![SyntheticDep::normal("mode", "=1.0.0").feature("fast")],
        BTreeMap::new(),
    );
    index.add_package_version(
        "right",
        v(1, 0, 0),
        vec![SyntheticDep::normal("mode", "=1.0.0").feature("small")],
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
        v(1, 0, 0),
        vec![SyntheticDep::normal("core", "=1.0.0")],
        BTreeMap::new(),
    );
    index.add_package_version(
        "derive",
        v(2, 0, 0),
        vec![SyntheticDep::normal("core", "=2.0.0")],
        BTreeMap::new(),
    );
    index.add_package_version(
        "floor",
        v(1, 0, 0),
        vec![SyntheticDep::normal("core", "^2")],
        BTreeMap::new(),
    );
    index.finish_synthetic()?;
    let mut problem = Problem::new(vec![
        RootReq::new("core", "*"),
        RootReq::new("derive", "=1.0.0"),
        RootReq::new("floor", "*"),
    ]);
    problem.learned_kind = LearnedKind::Family;
    let mut perturbed = Problem::new(vec![
        RootReq::new("core", ">=1.0.0, <4.0.0"),
        RootReq::new("derive", ">=1.0.0, <3.0.0"),
        RootReq::new("floor", "*"),
    ]);
    perturbed.decrees.push(Decree::new("derive", "=1.0.0"));
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
    for major in first..=last {
        index.add_package_version(name, v(major, 0, 0), Vec::new(), BTreeMap::new());
    }
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

        assert!(result.enabled_features["addon"].contains("extra"));
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
                .any(|edge| edge.parent == "web" && edge.dep == "tokio-like")
        );
        Ok(())
    }

    #[test]
    fn widening_range_uses_identical_supported_edge_lines() -> Result<()> {
        let mut index = CompiledIndex::empty();
        add_leaf_versions(&mut index, "a", 1, 3);
        index.add_package_version(
            "x",
            v(1, 0, 0),
            vec![SyntheticDep::normal("a", "=1.0.0")],
            BTreeMap::new(),
        );
        index.add_package_version(
            "x",
            v(1, 0, 1),
            vec![SyntheticDep::normal("a", "=1.0.0")],
            BTreeMap::new(),
        );
        index.add_package_version(
            "x",
            v(1, 0, 2),
            vec![SyntheticDep::normal("a", "=2.0.0")],
            BTreeMap::new(),
        );
        index.finish_synthetic()?;
        let x = index.package("x").expect("x package");
        let a = index.package("a").expect("a package");
        let selected = BTreeMap::from([(x, v(1, 0, 1)), (a, v(3, 0, 0))]);
        let support = BTreeSet::from([EdgeKey {
            parent: "x".to_owned(),
            dep: "a".to_owned(),
        }]);
        let region = Region::new().with_versions(a, VersionSet::exact(v(3, 0, 0)));
        let (widened, _, widened_count) = index.widen_region(&selected, &support, &region);

        assert_eq!(widened_count, 2);
        assert!(widened.versions[&x].contains(&v(1, 0, 0)));
        assert!(widened.versions[&x].contains(&v(1, 0, 1)));
        assert!(!widened.versions[&x].contains(&v(1, 0, 2)));
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
                .any(|edge| edge.id == "edge:crates.io:web->crates.io:codec")
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
                } if pkg == "tokio-like"
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
                    if matches!(rung.verdict, RungVerdict::Unexplained) {
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
