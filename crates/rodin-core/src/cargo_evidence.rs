use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use eyre::{Context, Result, bail};
use facet::Facet;
use semver::{Version, VersionReq};

use crate::{
    CompiledIndex, CounterfactualResult, Decree, LearnedKind, Problem, RecordSelection, RootReq,
    SyntheticDep, WarmFactVerifier, counterfactual_drop_dependency, solve,
    warm_bundle_from_learned,
};

const DEFAULT_FACET_CC: &str = "/Users/amos/oss/facet-cc";
const DEFAULT_REGISTRY_SRC: &str =
    "/Users/amos/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f";

#[derive(Clone, Debug, Facet, PartialEq)]
pub struct CargoEvidenceReport {
    pub workspace: String,
    pub lock_packages: usize,
    pub lock_unique_names: usize,
    pub workspace_roots: usize,
    pub indexed_package_names: usize,
    pub indexed_versions: usize,
    pub indexed_edges: usize,
    pub extraction_gaps: Vec<CargoExtractionGap>,
    pub solve: CargoSolveStats,
    pub oracle: CargoOracleReport,
    pub warm_counterfactual: Option<CargoWarmCounterfactual>,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct CargoExtractionGap {
    pub kind: CargoExtractionGapKind,
    pub package: String,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, Facet, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub enum CargoExtractionGapKind {
    MultipleLockedVersionsUnsupported,
    ManifestMissing,
    ManifestDependencyUnparsed,
    DependencyOutsideLockUniverse,
    DroppedDuplicateVersionEdge,
    LockExactFallback,
    TargetConditionalDependency,
    BuildDependency,
    DevDependency,
    OptionalDependency,
    SemverRequirementRejected,
}

#[derive(Clone, Debug, Facet, PartialEq)]
pub struct CargoSolveStats {
    pub solved: bool,
    pub decisions: usize,
    pub conflicts: usize,
    pub learned_facts: usize,
    pub point_versions: usize,
    pub widened_versions: usize,
    pub widening_factor: f64,
    pub wall_ms: u64,
    pub trace_tail: Vec<String>,
}

#[derive(Clone, Debug, Facet, PartialEq)]
pub struct CargoOracleReport {
    pub exact_matches: usize,
    pub compared_names: usize,
    pub match_rate: f64,
    pub divergences: Vec<CargoDivergence>,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct CargoDivergence {
    pub kind: CargoDivergenceKind,
    pub package: String,
    pub cargo_lock_versions: Vec<String>,
    pub rodin_version: Option<String>,
    pub classification: String,
}

#[derive(Clone, Debug, Eq, Facet, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub enum CargoDivergenceKind {
    MultipleLockedVersionsUnsupported,
    MissingInRodin,
    ExtraInRodin,
    VersionMismatch,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct CargoWarmCounterfactual {
    pub dropped_root: String,
    pub oracle_selected_equal: bool,
    pub warm_decisions: usize,
    pub cold_decisions: usize,
    pub warm_conflicts: usize,
    pub cold_conflicts: usize,
    pub warm_facts_used: usize,
    pub warm_facts_scoped_out: usize,
    pub diff_added: Vec<RecordSelection>,
    pub diff_removed: Vec<RecordSelection>,
    pub diff_changed: Vec<CargoChangedSelection>,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct CargoChangedSelection {
    pub pkg: String,
    pub from: String,
    pub to: String,
}

struct ExtractedCargoUniverse {
    index: CompiledIndex,
    problem: Problem,
    lock_versions: BTreeMap<String, Vec<Version>>,
    gaps: Vec<CargoExtractionGap>,
    indexed_edges: usize,
}

struct ExtractionModel<'a> {
    target_names: &'a BTreeSet<String>,
    representative: &'a BTreeMap<String, Version>,
    duplicate_names: &'a BTreeSet<String>,
}

#[derive(Clone, Debug, Default)]
struct ManifestDeps {
    deps_by_name: BTreeMap<String, Vec<ManifestDep>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ManifestDep {
    key: String,
    actual: String,
    req: Option<String>,
    features: Vec<String>,
    optional: bool,
    default_features: bool,
    kind: DepKind,
    target: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DepKind {
    Normal,
    Build,
    Dev,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LockPackage {
    name: String,
    version: Version,
    source: Option<String>,
    dependencies: Vec<LockDep>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LockDep {
    name: String,
    version: Option<Version>,
}

#[must_use]
pub fn render_cargo_evidence_markdown(report: &CargoEvidenceReport) -> String {
    let mut out = String::new();
    out.push_str("## Round 4: real-world Cargo.lock evidence\n\n");
    let _ = writeln!(
        out,
        "Source snapshot: `{}` Cargo.lock plus local Cargo registry cache. The extractor is offline-only, indexes cached registry alternatives for locked package names, and encodes the Cargo.lock snapshot as decrees to the representative locked version for each package name. Rodin models one selected version per package name; Cargo.lock entries with multiple coexisting versions are classified rather than hidden.\n",
        report.workspace
    );
    out.push_str("### Extraction\n\n");
    out.push_str(
        "Locked versions use Cargo.lock's concrete dependency list as active edges. Cached alternative versions use parseable manifest dependencies, while dev, optional, and target-conditional manifest dependencies are classified as modeling gaps and left inactive in this round.\n\n",
    );
    let _ = writeln!(
        out,
        "- lock package entries: {}\n- lock unique package names: {}\n- workspace roots: {}\n- indexed package names: {}\n- indexed package versions: {}\n- indexed dependency edges: {}\n- extraction/modeling gaps: {}\n",
        report.lock_packages,
        report.lock_unique_names,
        report.workspace_roots,
        report.indexed_package_names,
        report.indexed_versions,
        report.indexed_edges,
        report.extraction_gaps.len()
    );
    out.push_str("### Solve numbers\n\n");
    let _ = writeln!(
        out,
        "| solved | decisions | conflicts | learned facts | point versions | widened versions | widening factor | wall ms |\n|---|---:|---:|---:|---:|---:|---:|---:|\n| {} | {} | {} | {} | {} | {} | {:.3} | {} |\n",
        report.solve.solved,
        report.solve.decisions,
        report.solve.conflicts,
        report.solve.learned_facts,
        report.solve.point_versions,
        report.solve.widened_versions,
        report.solve.widening_factor,
        report.solve.wall_ms
    );
    if !report.solve.trace_tail.is_empty() {
        out.push_str("\nSolve trace tail:\n\n");
        for line in &report.solve.trace_tail {
            let _ = writeln!(out, "- `{line}`");
        }
        out.push('\n');
    }
    out.push_str("### Cargo.lock oracle\n\n");
    let _ = writeln!(
        out,
        "- exact package-name matches: {}/{} ({:.2}%)\n- divergences: {}\n",
        report.oracle.exact_matches,
        report.oracle.compared_names,
        report.oracle.match_rate * 100.0,
        report.oracle.divergences.len()
    );
    out.push_str("| divergence kind | count |\n|---|---:|\n");
    for (kind, count) in divergence_counts(&report.oracle.divergences) {
        let _ = writeln!(out, "| {kind:?} | {count} |");
    }
    out.push('\n');
    out.push_str("| kind | package | Cargo.lock | Rodin | classification |\n");
    out.push_str("|---|---|---|---|---|\n");
    for divergence in &report.oracle.divergences {
        let _ = writeln!(
            out,
            "| {:?} | `{}` | `{}` | `{}` | {} |",
            divergence.kind,
            divergence.package,
            divergence.cargo_lock_versions.join(", "),
            divergence
                .rodin_version
                .clone()
                .unwrap_or_else(|| "-".to_owned()),
            divergence.classification
        );
    }
    if let Some(warm) = &report.warm_counterfactual {
        out.push_str("\n### Warm counterfactual\n\n");
        let _ = writeln!(
            out,
            "Dropped root dependency `{}`. Warm/cold selected sets equal: `{}`.\n",
            warm.dropped_root, warm.oracle_selected_equal
        );
        let _ = writeln!(
            out,
            "| warm decisions | cold decisions | decision delta | warm conflicts | cold conflicts | conflict delta | warm facts used | scoped out |\n|---:|---:|---:|---:|---:|---:|---:|---:|\n| {} | {} | {} | {} | {} | {} | {} | {} |\n",
            warm.warm_decisions,
            warm.cold_decisions,
            warm.cold_decisions as isize - warm.warm_decisions as isize,
            warm.warm_conflicts,
            warm.cold_conflicts,
            warm.cold_conflicts as isize - warm.warm_conflicts as isize,
            warm.warm_facts_used,
            warm.warm_facts_scoped_out
        );
        let _ = writeln!(
            out,
            "| added | removed | changed |\n|---:|---:|---:|\n| {} | {} | {} |\n",
            warm.diff_added.len(),
            warm.diff_removed.len(),
            warm.diff_changed.len()
        );
    }
    out.push_str("\n### Extraction and modeling gap samples\n\n");
    out.push_str("| gap kind | count |\n|---|---:|\n");
    for (kind, count) in gap_counts(&report.extraction_gaps) {
        let _ = writeln!(out, "| {kind:?} | {count} |");
    }
    out.push('\n');
    out.push_str("| kind | package | detail |\n|---|---|---|\n");
    for gap in report.extraction_gaps.iter().take(60) {
        let _ = writeln!(
            out,
            "| {:?} | `{}` | {} |",
            gap.kind, gap.package, gap.detail
        );
    }
    if report.extraction_gaps.len() > 60 {
        let _ = writeln!(
            out,
            "\n{} additional extraction/modeling gaps omitted from this table; the JSON artifact carries the full set.\n",
            report.extraction_gaps.len() - 60
        );
    }
    out
}

fn divergence_counts(divergences: &[CargoDivergence]) -> BTreeMap<CargoDivergenceKind, usize> {
    let mut counts = BTreeMap::new();
    for divergence in divergences {
        *counts.entry(divergence.kind.clone()).or_default() += 1;
    }
    counts
}

fn gap_counts(gaps: &[CargoExtractionGap]) -> BTreeMap<CargoExtractionGapKind, usize> {
    let mut counts = BTreeMap::new();
    for gap in gaps {
        *counts.entry(gap.kind.clone()).or_default() += 1;
    }
    counts
}

pub fn append_cargo_evidence_round(path: &Path, report: &CargoEvidenceReport) -> Result<()> {
    let mut existing =
        fs::read_to_string(path).wrap_err_with(|| format!("reading {}", path.display()))?;
    if !existing.ends_with('\n') {
        existing.push('\n');
    }
    existing.push('\n');
    existing.push_str(&render_cargo_evidence_markdown(report));
    fs::write(path, existing).wrap_err_with(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn run_facet_cc_evidence() -> Result<CargoEvidenceReport> {
    run_cargo_evidence(Path::new(DEFAULT_FACET_CC), Path::new(DEFAULT_REGISTRY_SRC))
}

pub fn run_cargo_evidence(workspace: &Path, registry_src: &Path) -> Result<CargoEvidenceReport> {
    let extracted = extract_cargo_universe(workspace, registry_src)?;
    let roots = extracted.problem.roots.len();
    let lock_packages = extracted.lock_versions.values().map(Vec::len).sum();
    let lock_unique_names = extracted.lock_versions.len();
    let indexed_package_names = extracted.index.package_count();
    let indexed_versions = extracted.index.version_count();

    let start = Instant::now();
    let solved = solve(&extracted.index, extracted.problem.clone());
    let wall_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    let (selected, solve_stats, learned, derivations) = match solved {
        Ok(result) => {
            let stats = solve_stats(
                true,
                result.decisions,
                result.conflicts,
                wall_ms,
                &result.learned,
                &result.trace,
            );
            (
                result.selected.clone(),
                stats,
                result.learned,
                result.derivations,
            )
        }
        Err(err) => match *err {
            crate::SolveError::NoSolution {
                decisions,
                conflicts,
                learned,
                derivations,
                trace,
                ..
            } => {
                let stats = solve_stats(false, decisions, conflicts, wall_ms, &learned, &trace);
                (BTreeMap::new(), stats, learned, derivations)
            }
        },
    };
    let oracle = compare_to_lock(&extracted.lock_versions, &selected);
    let warm_counterfactual = if solve_stats.solved {
        warm_counterfactual(&extracted, &selected, &learned, &derivations).ok()
    } else {
        None
    };

    Ok(CargoEvidenceReport {
        workspace: workspace.display().to_string(),
        lock_packages,
        lock_unique_names,
        workspace_roots: roots,
        indexed_package_names,
        indexed_versions,
        indexed_edges: extracted.indexed_edges,
        extraction_gaps: extracted.gaps,
        solve: solve_stats,
        oracle,
        warm_counterfactual,
    })
}

fn solve_stats(
    solved: bool,
    decisions: usize,
    conflicts: usize,
    wall_ms: u64,
    learned: &[crate::LearnedNoGood],
    trace: &[String],
) -> CargoSolveStats {
    let point_versions = learned.iter().map(|fact| fact.point_versions).sum();
    let widened_versions = learned.iter().map(|fact| fact.widened_versions).sum();
    CargoSolveStats {
        solved,
        decisions,
        conflicts,
        learned_facts: learned.len(),
        point_versions,
        widened_versions,
        widening_factor: if point_versions == 0 {
            0.0
        } else {
            widened_versions as f64 / point_versions as f64
        },
        wall_ms,
        trace_tail: trace
            .iter()
            .rev()
            .take(12)
            .map(|line| summarize_trace_line(line))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
    }
}

fn summarize_trace_line(line: &str) -> String {
    const LIMIT: usize = 220;
    if line.len() <= LIMIT {
        return line.to_owned();
    }
    let mut summary = line.chars().take(LIMIT).collect::<String>();
    summary.push_str("...");
    summary
}

fn warm_counterfactual(
    extracted: &ExtractedCargoUniverse,
    selected: &BTreeMap<String, Version>,
    learned: &[crate::LearnedNoGood],
    derivations: &rodin_facts::DerivationStore,
) -> Result<CargoWarmCounterfactual> {
    let bundle = warm_bundle_from_learned(&extracted.index, learned, derivations)?;
    let verified = WarmFactVerifier::for_index(&extracted.index).verify(&extracted.index, &bundle);
    let dropped = extracted
        .problem
        .roots
        .iter()
        .map(|root| root.name.as_str())
        .find(|name| selected.contains_key(*name))
        .unwrap_or_else(|| {
            extracted
                .problem
                .roots
                .first()
                .map(|root| root.name.as_str())
                .unwrap_or("facet")
        })
        .to_owned();
    let completed = solve(&extracted.index, extracted.problem.clone())
        .map_err(|err| eyre::eyre!("cold completed solve for counterfactual: {err:?}"))?;
    let result = counterfactual_drop_dependency(
        &extracted.index,
        &extracted.problem,
        &completed,
        &verified,
        &dropped,
    )?;
    Ok(warm_from_counterfactual(dropped, result))
}

fn warm_from_counterfactual(
    dropped_root: String,
    result: CounterfactualResult,
) -> CargoWarmCounterfactual {
    CargoWarmCounterfactual {
        dropped_root,
        oracle_selected_equal: result.oracle_selected_equal,
        warm_decisions: result.warm_decisions,
        cold_decisions: result.cold_decisions,
        warm_conflicts: result.warm_conflicts,
        cold_conflicts: result.cold_conflicts,
        warm_facts_used: result.warm_facts_used,
        warm_facts_scoped_out: result.warm_facts_scoped_out,
        diff_added: result.selected_diff.added,
        diff_removed: result.selected_diff.removed,
        diff_changed: result
            .selected_diff
            .changed
            .into_iter()
            .map(|changed| CargoChangedSelection {
                pkg: changed.pkg,
                from: changed.from,
                to: changed.to,
            })
            .collect(),
    }
}

fn extract_cargo_universe(workspace: &Path, registry_src: &Path) -> Result<ExtractedCargoUniverse> {
    let lock_path = workspace.join("Cargo.lock");
    let lock = parse_lock(&fs::read_to_string(&lock_path).wrap_err("reading Cargo.lock")?)?;
    let lock_by_name = lock_versions_by_name(&lock);
    let duplicate_names = duplicate_names(&lock_by_name);
    let representative = representative_versions(&lock_by_name);
    let workspace_members = workspace_members(workspace)?;
    let local_manifests = local_manifest_paths(workspace, &workspace_members)?;
    let mut gaps = duplicate_names
        .iter()
        .filter_map(|name| {
            lock_by_name.get(name).map(|versions| CargoExtractionGap {
                kind: CargoExtractionGapKind::MultipleLockedVersionsUnsupported,
                package: name.clone(),
                detail: format!(
                    "Cargo.lock contains {}; Rodin currently has one global version domain per package name",
                    display_versions(versions)
                ),
            })
        })
        .collect::<Vec<_>>();
    let mut index = CompiledIndex::empty();
    let mut indexed_edges = 0;
    let target_names: BTreeSet<_> = lock_by_name.keys().cloned().collect();
    let model = ExtractionModel {
        target_names: &target_names,
        representative: &representative,
        duplicate_names: &duplicate_names,
    };

    for package_name in &target_names {
        let versions = candidate_versions(
            package_name,
            &lock,
            registry_src,
            &duplicate_names,
            &representative,
        )?;
        for version in versions {
            let source = lock
                .iter()
                .find(|pkg| pkg.name == *package_name && pkg.version == version)
                .and_then(|pkg| pkg.source.clone());
            let manifest = manifest_path(
                workspace,
                registry_src,
                &local_manifests,
                package_name,
                &version,
                source.as_deref(),
            );
            let manifest_deps = match manifest
                .as_ref()
                .and_then(|path| fs::read_to_string(path).ok())
                .map(|text| parse_manifest_deps(&text))
            {
                Some(Ok(deps)) => deps,
                Some(Err(err)) => {
                    gaps.push(CargoExtractionGap {
                        kind: CargoExtractionGapKind::ManifestDependencyUnparsed,
                        package: format!("{package_name}@{version}"),
                        detail: err.to_string(),
                    });
                    ManifestDeps::default()
                }
                None => ManifestDeps::default(),
            };
            if manifest.is_none() {
                gaps.push(CargoExtractionGap {
                    kind: CargoExtractionGapKind::ManifestMissing,
                    package: format!("{package_name}@{version}"),
                    detail: "no local workspace or registry Cargo.toml found".to_owned(),
                });
            }
            let locked_package = lock
                .iter()
                .find(|pkg| pkg.name == *package_name && pkg.version == version);
            let deps = deps_for_version(
                package_name,
                &version,
                locked_package,
                &manifest_deps,
                &model,
                &mut gaps,
            );
            indexed_edges += deps.len();
            index.add_package_version(package_name, version, deps, BTreeMap::new());
        }
    }
    index.finish_synthetic()?;

    let roots = local_manifests
        .keys()
        .filter_map(|name| representative.get(name).map(|version| (name, version)))
        .map(|(name, version)| RootReq::new(name.clone(), format!("={version}")))
        .collect();
    let mut problem = Problem::new(roots);
    problem.learned_kind = LearnedKind::Family;
    problem.decrees = representative
        .iter()
        .map(|(name, version)| Decree::new(name.clone(), format!("={version}")))
        .collect();
    Ok(ExtractedCargoUniverse {
        index,
        problem,
        lock_versions: lock_by_name,
        gaps,
        indexed_edges,
    })
}

fn deps_for_version(
    package_name: &str,
    version: &Version,
    locked_package: Option<&LockPackage>,
    manifest: &ManifestDeps,
    model: &ExtractionModel<'_>,
    gaps: &mut Vec<CargoExtractionGap>,
) -> Vec<SyntheticDep> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let activate_manifest_deps = locked_package.is_none();
    {
        let mut builder = ManifestDepBuilder {
            package_name,
            version,
            model,
            gaps,
            seen: &mut seen,
            out: &mut out,
            activate: activate_manifest_deps,
        };
        for deps in manifest.deps_by_name.values() {
            for dep in deps {
                builder.push(dep);
            }
        }
    }
    if let Some(locked) = locked_package {
        seen.clear();
        for dep in &locked.dependencies {
            if seen.contains(&dep.name) {
                continue;
            }
            let Some(rep) = model.representative.get(&dep.name) else {
                gaps.push(CargoExtractionGap {
                    kind: CargoExtractionGapKind::DependencyOutsideLockUniverse,
                    package: format!("{package_name}@{version}"),
                    detail: dep.name.clone(),
                });
                continue;
            };
            if model.duplicate_names.contains(&dep.name)
                && dep.version.as_ref().is_some_and(|version| version != rep)
            {
                gaps.push(CargoExtractionGap {
                    kind: CargoExtractionGapKind::DroppedDuplicateVersionEdge,
                    package: format!("{package_name}@{version}"),
                    detail: format!(
                        "{}@{} is not the representative {}",
                        dep.name,
                        dep.version.as_ref().expect("checked"),
                        rep
                    ),
                });
                continue;
            }
            out.push(SyntheticDep::normal(dep.name.clone(), format!("={rep}")));
            seen.insert(dep.name.clone());
            gaps.push(CargoExtractionGap {
                kind: CargoExtractionGapKind::LockExactFallback,
                package: format!("{package_name}@{version}"),
                detail: format!("{} -> {} ={}", package_name, dep.name, rep),
            });
        }
    }
    out
}

struct ManifestDepBuilder<'a, 'b> {
    package_name: &'a str,
    version: &'a Version,
    model: &'a ExtractionModel<'a>,
    gaps: &'b mut Vec<CargoExtractionGap>,
    seen: &'b mut BTreeSet<String>,
    out: &'b mut Vec<SyntheticDep>,
    activate: bool,
}

impl ManifestDepBuilder<'_, '_> {
    fn push(&mut self, dep: &ManifestDep) {
        if !self.model.target_names.contains(&dep.actual) {
            self.gaps.push(CargoExtractionGap {
                kind: CargoExtractionGapKind::DependencyOutsideLockUniverse,
                package: self.package_version(),
                detail: dep.actual.clone(),
            });
            return;
        }
        let Some(rep) = self.model.representative.get(&dep.actual) else {
            return;
        };
        if self.model.duplicate_names.contains(&dep.actual) {
            self.gaps.push(CargoExtractionGap {
                kind: CargoExtractionGapKind::DroppedDuplicateVersionEdge,
                package: self.package_version(),
                detail: format!(
                    "{} uses duplicate package {}; representative is {}",
                    dep.key, dep.actual, rep
                ),
            });
            return;
        }
        let req = dep.req.clone().unwrap_or_else(|| format!("={rep}"));
        if VersionReq::parse(&req).is_err() {
            self.gaps.push(CargoExtractionGap {
                kind: CargoExtractionGapKind::SemverRequirementRejected,
                package: self.package_version(),
                detail: format!("{} {}", dep.actual, req),
            });
            return;
        }
        if dep.target {
            self.gaps.push(CargoExtractionGap {
                kind: CargoExtractionGapKind::TargetConditionalDependency,
                package: self.package_version(),
                detail: dep.actual.clone(),
            });
        }
        match dep.kind {
            DepKind::Build => self.gaps.push(CargoExtractionGap {
                kind: CargoExtractionGapKind::BuildDependency,
                package: self.package_version(),
                detail: dep.actual.clone(),
            }),
            DepKind::Dev => self.gaps.push(CargoExtractionGap {
                kind: CargoExtractionGapKind::DevDependency,
                package: self.package_version(),
                detail: dep.actual.clone(),
            }),
            DepKind::Normal => {}
        }
        if dep.optional {
            self.gaps.push(CargoExtractionGap {
                kind: CargoExtractionGapKind::OptionalDependency,
                package: self.package_version(),
                detail: dep.actual.clone(),
            });
        }
        if !self.activate || dep.kind == DepKind::Dev || dep.target || dep.optional {
            return;
        }
        let mut synthetic = SyntheticDep::normal(dep.key.clone(), req);
        synthetic.package = (dep.actual != dep.key).then(|| dep.actual.clone());
        synthetic.features = dep.features.clone();
        synthetic.default_features = dep.default_features;
        synthetic.optional = dep.optional;
        self.out.push(synthetic);
        self.seen.insert(dep.actual.clone());
    }

    fn package_version(&self) -> String {
        format!("{}@{}", self.package_name, self.version)
    }
}

fn compare_to_lock(
    lock_versions: &BTreeMap<String, Vec<Version>>,
    selected: &BTreeMap<String, Version>,
) -> CargoOracleReport {
    let mut exact_matches = 0;
    let mut divergences = Vec::new();
    for (name, versions) in lock_versions {
        match versions.as_slice() {
            [single] => match selected.get(name) {
                Some(selected) if selected == single => exact_matches += 1,
                Some(selected) => divergences.push(CargoDivergence {
                    kind: CargoDivergenceKind::VersionMismatch,
                    package: name.clone(),
                    cargo_lock_versions: vec![single.to_string()],
                    rodin_version: Some(selected.to_string()),
                    classification: "Rodin solved against cached registry alternatives without Cargo.lock pinning".to_owned(),
                }),
                None => divergences.push(CargoDivergence {
                    kind: CargoDivergenceKind::MissingInRodin,
                    package: name.clone(),
                    cargo_lock_versions: vec![single.to_string()],
                    rodin_version: None,
                    classification: "not reached under current normal/build/dev/feature projection".to_owned(),
                }),
            },
            many => divergences.push(CargoDivergence {
                kind: CargoDivergenceKind::MultipleLockedVersionsUnsupported,
                package: name.clone(),
                cargo_lock_versions: many.iter().map(ToString::to_string).collect(),
                rodin_version: selected.get(name).map(ToString::to_string),
                classification: "Cargo selected multiple versions of one package name; Rodin currently has one global domain per package name".to_owned(),
            }),
        }
    }
    for (name, version) in selected {
        if !lock_versions.contains_key(name) {
            divergences.push(CargoDivergence {
                kind: CargoDivergenceKind::ExtraInRodin,
                package: name.clone(),
                cargo_lock_versions: Vec::new(),
                rodin_version: Some(version.to_string()),
                classification: "selected from cached registry universe but absent from Cargo.lock"
                    .to_owned(),
            });
        }
    }
    let compared_names = lock_versions.len();
    CargoOracleReport {
        exact_matches,
        compared_names,
        match_rate: if compared_names == 0 {
            0.0
        } else {
            exact_matches as f64 / compared_names as f64
        },
        divergences,
    }
}

fn candidate_versions(
    name: &str,
    lock: &[LockPackage],
    registry_src: &Path,
    duplicate_names: &BTreeSet<String>,
    representative: &BTreeMap<String, Version>,
) -> Result<Vec<Version>> {
    if duplicate_names.contains(name) {
        return representative
            .get(name)
            .cloned()
            .map(|version| vec![version])
            .ok_or_else(|| eyre::eyre!("missing representative for {name}"));
    }
    let is_registry = lock.iter().any(|pkg| {
        pkg.name == name
            && pkg
                .source
                .as_deref()
                .is_some_and(|source| source.starts_with("registry+"))
    });
    if !is_registry {
        return representative
            .get(name)
            .cloned()
            .map(|version| vec![version])
            .ok_or_else(|| eyre::eyre!("missing representative for {name}"));
    }
    let mut versions = Vec::new();
    for entry in fs::read_dir(registry_src).wrap_err("reading registry src")? {
        let entry = entry?;
        let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(version_text) = file_name.strip_prefix(&format!("{name}-")) else {
            continue;
        };
        if let Ok(version) = Version::parse(version_text)
            && entry.path().join("Cargo.toml").exists()
        {
            versions.push(version);
        }
    }
    if versions.is_empty()
        && let Some(version) = representative.get(name)
    {
        versions.push(version.clone());
    }
    versions.sort();
    versions.dedup();
    Ok(versions)
}

fn manifest_path(
    workspace: &Path,
    registry_src: &Path,
    local_manifests: &BTreeMap<String, PathBuf>,
    name: &str,
    version: &Version,
    source: Option<&str>,
) -> Option<PathBuf> {
    if source.is_some_and(|source| source.starts_with("registry+")) {
        Some(
            registry_src
                .join(format!("{name}-{version}"))
                .join("Cargo.toml"),
        )
    } else {
        local_manifests
            .get(name)
            .cloned()
            .or_else(|| Some(workspace.join(name).join("Cargo.toml")))
    }
    .filter(|path| path.exists())
}

fn parse_lock(text: &str) -> Result<Vec<LockPackage>> {
    let mut packages = Vec::new();
    let mut current: Option<LockPackage> = None;
    let mut in_deps = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line == "[[package]]" {
            if let Some(pkg) = current.take() {
                packages.push(pkg);
            }
            current = Some(LockPackage {
                name: String::new(),
                version: Version::new(0, 0, 0),
                source: None,
                dependencies: Vec::new(),
            });
            in_deps = false;
            continue;
        }
        let Some(pkg) = current.as_mut() else {
            continue;
        };
        if in_deps {
            if line == "]" {
                in_deps = false;
                continue;
            }
            for quoted in quoted_values(line) {
                pkg.dependencies.push(parse_lock_dep(&quoted)?);
            }
            continue;
        }
        if let Some(name) = line.strip_prefix("name = ").and_then(parse_quoted) {
            pkg.name = name.to_ascii_lowercase();
        } else if let Some(version) = line.strip_prefix("version = ").and_then(parse_quoted) {
            pkg.version = Version::parse(&version)?;
        } else if let Some(source) = line.strip_prefix("source = ").and_then(parse_quoted) {
            pkg.source = Some(source);
        } else if line.starts_with("dependencies = [") {
            in_deps = true;
            for quoted in quoted_values(line) {
                pkg.dependencies.push(parse_lock_dep(&quoted)?);
            }
            if line.ends_with(']') {
                in_deps = false;
            }
        }
    }
    if let Some(pkg) = current.take() {
        packages.push(pkg);
    }
    Ok(packages)
}

fn parse_lock_dep(text: &str) -> Result<LockDep> {
    let without_source = text.split_once(" (").map_or(text, |(name, _)| name);
    let mut parts = without_source.split_whitespace().collect::<Vec<_>>();
    if parts.is_empty() {
        bail!("empty lock dependency");
    }
    let name = parts.remove(0).to_ascii_lowercase();
    let version = parts.first().and_then(|part| Version::parse(part).ok());
    Ok(LockDep { name, version })
}

fn parse_manifest_deps(text: &str) -> Result<ManifestDeps> {
    let mut out = ManifestDeps::default();
    let mut section = ManifestSection::Other;
    let mut current_table: Option<ManifestDep> = None;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            if let Some(dep) = current_table.take() {
                out.deps_by_name
                    .entry(dep.actual.clone())
                    .or_default()
                    .push(dep);
            }
            let header = line.trim_matches(['[', ']']);
            if let Some((kind, target, dep_key)) = dependency_table_header(header) {
                section = ManifestSection::DependencyTable;
                current_table = Some(ManifestDep {
                    key: dep_key.clone(),
                    actual: dep_key,
                    req: None,
                    features: Vec::new(),
                    optional: false,
                    default_features: true,
                    kind,
                    target,
                });
            } else {
                section = dependency_section(header).unwrap_or(ManifestSection::Other);
            }
            continue;
        }
        if let Some(dep) = current_table.as_mut() {
            apply_dep_field(dep, line);
        } else if let ManifestSection::Dependencies { kind, target } = section
            && let Some((key, value)) = line.split_once('=')
        {
            let key = key.trim().to_ascii_lowercase();
            let dep = parse_inline_dep(key, value.trim(), kind, target);
            out.deps_by_name
                .entry(dep.actual.clone())
                .or_default()
                .push(dep);
        }
    }
    if let Some(dep) = current_table.take() {
        out.deps_by_name
            .entry(dep.actual.clone())
            .or_default()
            .push(dep);
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManifestSection {
    Other,
    Dependencies { kind: DepKind, target: bool },
    DependencyTable,
}

fn dependency_section(header: &str) -> Option<ManifestSection> {
    if header == "dependencies" {
        Some(ManifestSection::Dependencies {
            kind: DepKind::Normal,
            target: false,
        })
    } else if header == "build-dependencies" {
        Some(ManifestSection::Dependencies {
            kind: DepKind::Build,
            target: false,
        })
    } else if header == "dev-dependencies" {
        Some(ManifestSection::Dependencies {
            kind: DepKind::Dev,
            target: false,
        })
    } else if header.contains(".dependencies") {
        Some(ManifestSection::Dependencies {
            kind: DepKind::Normal,
            target: header.starts_with("target."),
        })
    } else if header.contains(".build-dependencies") {
        Some(ManifestSection::Dependencies {
            kind: DepKind::Build,
            target: header.starts_with("target."),
        })
    } else if header.contains(".dev-dependencies") {
        Some(ManifestSection::Dependencies {
            kind: DepKind::Dev,
            target: header.starts_with("target."),
        })
    } else {
        None
    }
}

fn dependency_table_header(header: &str) -> Option<(DepKind, bool, String)> {
    for marker in [
        (".dependencies.", DepKind::Normal),
        (".build-dependencies.", DepKind::Build),
        (".dev-dependencies.", DepKind::Dev),
    ] {
        if let Some((prefix, dep)) = header.rsplit_once(marker.0) {
            return Some((
                marker.1,
                prefix.starts_with("target"),
                dep.to_ascii_lowercase(),
            ));
        }
    }
    for marker in [
        ("dependencies.", DepKind::Normal),
        ("build-dependencies.", DepKind::Build),
        ("dev-dependencies.", DepKind::Dev),
    ] {
        if let Some(dep) = header.strip_prefix(marker.0) {
            return Some((marker.1, false, dep.to_ascii_lowercase()));
        }
    }
    None
}

fn parse_inline_dep(key: String, value: &str, kind: DepKind, target: bool) -> ManifestDep {
    let mut dep = ManifestDep {
        key: key.clone(),
        actual: key,
        req: None,
        features: Vec::new(),
        optional: false,
        default_features: true,
        kind,
        target,
    };
    if let Some(version) = parse_quoted(value) {
        dep.req = Some(version);
    } else if value.starts_with('{') {
        for field in inline_fields(value) {
            apply_dep_field(&mut dep, &field);
        }
    }
    dep
}

fn apply_dep_field(dep: &mut ManifestDep, line: &str) {
    let Some((key, value)) = line.split_once('=') else {
        return;
    };
    let key = key.trim();
    let value = value.trim().trim_end_matches(',');
    match key {
        "version" => dep.req = parse_quoted(value),
        "package" => {
            if let Some(package) = parse_quoted(value) {
                dep.actual = package.to_ascii_lowercase();
            }
        }
        "optional" => dep.optional = value == "true",
        "default-features" => dep.default_features = value != "false",
        "features" => dep.features = quoted_values(value),
        _ => {}
    }
}

fn inline_fields(value: &str) -> Vec<String> {
    let body = value
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .trim();
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut bracket_depth = 0usize;
    let mut in_string = false;
    for ch in body.chars() {
        match ch {
            '"' => {
                in_string = !in_string;
                current.push(ch);
            }
            '[' if !in_string => {
                bracket_depth += 1;
                current.push(ch);
            }
            ']' if !in_string => {
                bracket_depth = bracket_depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if !in_string && bracket_depth == 0 => {
                if !current.trim().is_empty() {
                    fields.push(current.trim().to_owned());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        fields.push(current.trim().to_owned());
    }
    fields
}

fn workspace_members(workspace: &Path) -> Result<Vec<String>> {
    let root = fs::read_to_string(workspace.join("Cargo.toml"))
        .wrap_err("reading workspace Cargo.toml")?;
    let mut in_members = false;
    let mut out = Vec::new();
    for raw in root.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        if line.starts_with("members = [") {
            in_members = true;
        }
        if in_members {
            out.extend(quoted_values(line));
            if line.ends_with(']') {
                break;
            }
        }
    }
    Ok(out)
}

fn local_manifest_paths(workspace: &Path, members: &[String]) -> Result<BTreeMap<String, PathBuf>> {
    let mut out = BTreeMap::new();
    for member in members {
        let manifest = workspace.join(member).join("Cargo.toml");
        let Ok(text) = fs::read_to_string(&manifest) else {
            continue;
        };
        let Some((name, _)) = package_name_version(&text) else {
            continue;
        };
        out.insert(name, manifest);
    }
    Ok(out)
}

fn package_name_version(text: &str) -> Option<(String, Version)> {
    let mut in_package = false;
    let mut name = None;
    let mut version = None;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(value) = line.strip_prefix("name = ").and_then(parse_quoted) {
            name = Some(value.to_ascii_lowercase());
        } else if let Some(value) = line.strip_prefix("version = ").and_then(parse_quoted)
            && let Ok(parsed) = Version::parse(&value)
        {
            version = Some(parsed);
        }
    }
    Some((name?, version?))
}

fn lock_versions_by_name(lock: &[LockPackage]) -> BTreeMap<String, Vec<Version>> {
    let mut out: BTreeMap<String, Vec<Version>> = BTreeMap::new();
    for package in lock {
        out.entry(package.name.clone())
            .or_default()
            .push(package.version.clone());
    }
    for versions in out.values_mut() {
        versions.sort();
        versions.dedup();
    }
    out
}

fn duplicate_names(lock_by_name: &BTreeMap<String, Vec<Version>>) -> BTreeSet<String> {
    lock_by_name
        .iter()
        .filter_map(|(name, versions)| (versions.len() > 1).then_some(name.clone()))
        .collect()
}

fn representative_versions(
    lock_by_name: &BTreeMap<String, Vec<Version>>,
) -> BTreeMap<String, Version> {
    lock_by_name
        .iter()
        .filter_map(|(name, versions)| {
            versions
                .last()
                .cloned()
                .map(|version| (name.clone(), version))
        })
        .collect()
}

fn display_versions(versions: &[Version]) -> String {
    versions
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_quoted(value: &str) -> Option<String> {
    let value = value.trim();
    let rest = value.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

fn quoted_values(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find('"') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('"') else {
            break;
        };
        out.push(rest[..end].to_owned());
        rest = &rest[end + 1..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lock_dependency_versions() -> Result<()> {
        assert_eq!(
            parse_lock_dep("indexmap 2.14.0")?,
            LockDep {
                name: "indexmap".to_owned(),
                version: Some(Version::parse("2.14.0")?),
            }
        );
        assert_eq!(
            parse_lock_dep("facet-core")?,
            LockDep {
                name: "facet-core".to_owned(),
                version: None,
            }
        );
        Ok(())
    }

    #[test]
    fn parses_normalized_manifest_dependency_tables() -> Result<()> {
        let parsed = parse_manifest_deps(
            r#"
            [dependencies.hashbrown]
            version = "0.17"
            default-features = false

            [target."cfg(windows)".dependencies.windows-sys]
            version = "0.61"
            optional = true
            package = "windows-sys"
            features = ["Win32_Foundation"]
            "#,
        )?;
        let hashbrown = &parsed.deps_by_name["hashbrown"][0];
        assert_eq!(hashbrown.req.as_deref(), Some("0.17"));
        assert!(!hashbrown.target);
        let windows = &parsed.deps_by_name["windows-sys"][0];
        assert!(windows.target);
        assert!(windows.optional);
        assert_eq!(windows.features, ["Win32_Foundation"]);
        Ok(())
    }
}
