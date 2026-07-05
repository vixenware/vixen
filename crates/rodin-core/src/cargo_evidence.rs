use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use eyre::{Context, Result, bail};
use facet::Facet;
use flate2::read::GzDecoder;
use rodin_facts::CompatClass;
use semver::{Version, VersionReq};
use tar::Archive;

use crate::{
    CompiledIndex, CounterfactualResult, LearnedKind, PackageLockMetadata, Problem,
    RecordSelection, RootReq, SyntheticDep, WarmFactVerifier, counterfactual_drop_dependency,
    domain_label, solve, warm_bundle_from_learned,
};

const DEFAULT_FACET_CC: &str = "/Users/amos/oss/facet-cc";
const DEFAULT_REGISTRY_SRC: &str =
    "/Users/amos/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f";
const DEFAULT_REGISTRY_CACHE: &str =
    "/Users/amos/.cargo/registry/cache/index.crates.io-1949cf8c6b5b557f";
const ROUND_TARGET_TRIPLE: &str = "aarch64-apple-darwin";

#[derive(Clone, Debug, Facet, PartialEq)]
pub struct CargoEvidenceReport {
    pub workspace: String,
    pub fresh_lock_oracle: CargoFreshLockOracle,
    pub projections: Vec<CargoProjectionReport>,
}

#[derive(Clone, Debug, Facet, PartialEq)]
pub struct CargoProjectionReport {
    pub projection: CargoProjectionMode,
    pub lock_packages: usize,
    pub lock_unique_names: usize,
    pub workspace_roots: usize,
    pub indexed_package_names: usize,
    pub indexed_versions: usize,
    pub indexed_edges: usize,
    pub extraction_gaps: Vec<CargoExtractionGap>,
    pub solve: CargoSolveStats,
    pub historical_oracle: CargoOracleReport,
    pub fresh_oracle: Option<CargoOracleReport>,
    pub warm_counterfactual: Option<CargoWarmCounterfactual>,
}

#[derive(Clone, Copy, Debug, Eq, Facet, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub enum CargoProjectionMode {
    FixedTargetRuntime,
    LockUnionDev,
}

impl CargoProjectionMode {
    fn label(self) -> &'static str {
        match self {
            Self::FixedTargetRuntime => "fixed-target runtime",
            Self::LockUnionDev => "lock-union dev-included",
        }
    }

    fn includes_target(self, target: Option<&str>) -> bool {
        match self {
            Self::FixedTargetRuntime => target.is_none_or(target_matches_round),
            Self::LockUnionDev => true,
        }
    }

    fn includes_dev_dependency(
        self,
        package_name: &str,
        workspace_members: &BTreeSet<String>,
    ) -> bool {
        match self {
            Self::FixedTargetRuntime => workspace_members.contains(package_name),
            Self::LockUnionDev => true,
        }
    }
}

#[derive(Clone, Debug, Facet, PartialEq)]
pub struct CargoFreshLockOracle {
    pub attempted: bool,
    pub solved: bool,
    pub lock_packages: usize,
    pub lock_unique_names: usize,
    pub wall_ms: u64,
    pub message: String,
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
    WorkspaceDependencyUnresolved,
}

#[derive(Clone, Debug, Facet, PartialEq)]
pub struct CargoSolveStats {
    pub solved: bool,
    pub decisions: usize,
    pub conflicts: usize,
    pub restarts: usize,
    pub learned_facts: usize,
    pub learned_scans: usize,
    pub learned_prunes: usize,
    pub learned_conflicts: usize,
    pub learned_subsumed: usize,
    pub learned_replaced: usize,
    pub point_versions: usize,
    pub widened_versions: usize,
    pub widening_factor: f64,
    pub wall_ms: u64,
    pub trace_tail: Vec<String>,
}

#[derive(Clone, Debug, Facet, PartialEq)]
pub struct CargoOracleReport {
    pub oracle: CargoOracleKind,
    pub exact_matches: usize,
    pub compared_names: usize,
    pub match_rate: f64,
    pub divergences: Vec<CargoDivergence>,
}

#[derive(Clone, Copy, Debug, Eq, Facet, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub enum CargoOracleKind {
    HistoricalLock,
    FreshLock,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct CargoDivergence {
    pub kind: CargoDivergenceKind,
    pub package: String,
    pub cargo_lock_versions: Vec<String>,
    pub rodin_version: Option<String>,
    pub classification: String,
    pub verdict: CargoDivergenceVerdict,
    pub accepted_policy: Option<String>,
}

#[derive(Clone, Debug, Eq, Facet, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub enum CargoDivergenceKind {
    MultipleLockedVersionsUnsupported,
    MissingInRodin,
    ExtraInRodin,
    VersionMismatch,
}

#[derive(Clone, Debug, Eq, Facet, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub enum CargoDivergenceVerdict {
    ExtractionBug,
    SemverModelInterpretationDifference,
    LegitimateTieBreakDifference,
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
    lock_versions: Vec<LockedDomain>,
    gaps: Vec<CargoExtractionGap>,
    indexed_edges: usize,
}

struct ExtractionModel<'a> {
    projection: CargoProjectionMode,
    target_names: &'a BTreeSet<String>,
    workspace_members: &'a BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LockedDomain {
    name: String,
    class: CompatClass,
    version: Version,
}

#[derive(Clone, Debug, Default)]
struct ManifestDeps {
    deps_by_name: BTreeMap<String, Vec<ManifestDep>>,
    features: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ManifestDep {
    key: String,
    actual: String,
    req: Option<String>,
    features: Vec<String>,
    optional: bool,
    default_features: bool,
    default_features_explicit: bool,
    workspace: bool,
    kind: DepKind,
    target: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DepKind {
    Normal,
    Build,
    Dev,
}

impl DepKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Build => "build",
            Self::Dev => "dev",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LockPackage {
    name: String,
    version: Version,
    source: Option<String>,
    checksum: Option<String>,
    dependencies: Vec<LockDep>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LockDep {
    name: String,
    version: Option<Version>,
    entry: String,
}

#[must_use]
pub fn render_cargo_evidence_markdown(report: &CargoEvidenceReport) -> String {
    let mut out = String::new();
    out.push_str("## Round 9: Per-row Cargo oracle verdict table\n\n");
    let _ = writeln!(
        out,
        "Source snapshot: `{}` Cargo.lock plus local Cargo registry source/cache artifacts. The production extractor is offline-only, reads local Cargo.toml files and `.crate` archives directly, indexes cached registry alternatives for locked package names, and models Cargo-compatible coexisting domains keyed by package name and compatibility class. The fresh-lock oracle is test-only: it runs Cargo offline in a throwaway workspace copy and never feeds Rodin's solve path. Every divergence row below carries exactly one verdict and, for accepted divergences, exactly one policy rule.\n",
        report.workspace
    );

    out.push_str("### Fresh-lock oracle\n\n");
    let _ = writeln!(
        out,
        "| attempted | solved | lock packages | unique names | wall ms | message |\n|---|---|---:|---:|---:|---|\n| {} | {} | {} | {} | {} | {} |\n",
        report.fresh_lock_oracle.attempted,
        report.fresh_lock_oracle.solved,
        report.fresh_lock_oracle.lock_packages,
        report.fresh_lock_oracle.lock_unique_names,
        report.fresh_lock_oracle.wall_ms,
        markdown_cell(&report.fresh_lock_oracle.message)
    );

    out.push_str("### Oracle matrix\n\n");
    out.push_str("| projection | oracle | exact matches | compared | match rate | divergences | decisions | conflicts | wall ms |\n");
    out.push_str("|---|---|---:|---:|---:|---:|---:|---:|---:|\n");
    for projection in &report.projections {
        for oracle in projection.oracles() {
            let _ = writeln!(
                out,
                "| {} | {:?} | {} | {} | {:.2}% | {} | {} | {} | {} |",
                projection.projection.label(),
                oracle.oracle,
                oracle.exact_matches,
                oracle.compared_names,
                oracle.match_rate * 100.0,
                oracle.divergences.len(),
                projection.solve.decisions,
                projection.solve.conflicts,
                projection.solve.wall_ms
            );
        }
    }
    out.push('\n');

    render_accepted_divergence_policies(&mut out, report);

    for projection in &report.projections {
        render_projection_markdown(&mut out, projection);
    }
    out
}

impl CargoProjectionReport {
    fn oracles(&self) -> Vec<&CargoOracleReport> {
        let mut out = vec![&self.historical_oracle];
        if let Some(fresh) = &self.fresh_oracle {
            out.push(fresh);
        }
        out
    }
}

fn render_projection_markdown(out: &mut String, report: &CargoProjectionReport) {
    let _ = writeln!(out, "### Projection: {}\n", report.projection.label());
    match report.projection {
        CargoProjectionMode::FixedTargetRuntime => {
            let _ = writeln!(
                out,
                "Product-default projection: target-conditionals are evaluated against `{ROUND_TARGET_TRIPLE}`, build dependencies are active for the host, and dev dependencies are active only for workspace members.\n"
            );
        }
        CargoProjectionMode::LockUnionDev => {
            out.push_str(
                "Lock-comparison projection: target-conditional dependency edges are unioned and dev dependencies are included so Rodin is compared against Cargo.lock's target-independent, dev-inclusive snapshot shape.\n\n",
            );
        }
    }
    out.push_str("#### Extraction\n\n");
    let _ = writeln!(
        out,
        "- historical lock package entries: {}\n- historical lock unique package names: {}\n- workspace roots: {}\n- indexed package names: {}\n- indexed package versions: {}\n- indexed dependency edges: {}\n- extraction/modeling gaps: {}\n",
        report.lock_packages,
        report.lock_unique_names,
        report.workspace_roots,
        report.indexed_package_names,
        report.indexed_versions,
        report.indexed_edges,
        report.extraction_gaps.len()
    );
    out.push_str("#### Solve numbers\n\n");
    let _ = writeln!(
        out,
        "| solved | decisions | conflicts | restarts | learned facts | learned scans | learned prunes | subsumed | replaced | point versions | widened versions | widening factor | wall ms |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.3} | {} |\n",
        report.solve.solved,
        report.solve.decisions,
        report.solve.conflicts,
        report.solve.restarts,
        report.solve.learned_facts,
        report.solve.learned_scans,
        report.solve.learned_prunes,
        report.solve.learned_subsumed,
        report.solve.learned_replaced,
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
    for oracle in report.oracles() {
        render_oracle_markdown(out, oracle);
    }
    if let Some(warm) = &report.warm_counterfactual {
        out.push_str("\n#### Warm counterfactual\n\n");
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
    out.push_str("\n#### Extraction and modeling gap samples\n\n");
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
    out.push('\n');
}

fn render_oracle_markdown(out: &mut String, report: &CargoOracleReport) {
    let _ = writeln!(out, "#### {:?} oracle\n", report.oracle);
    let _ = writeln!(
        out,
        "- exact locked-domain matches: {}/{} ({:.2}%)\n- divergences: {}\n",
        report.exact_matches,
        report.compared_names,
        report.match_rate * 100.0,
        report.divergences.len()
    );
    out.push_str("| divergence kind | count |\n|---|---:|\n");
    for (kind, count) in divergence_counts(&report.divergences) {
        let _ = writeln!(out, "| {kind:?} | {count} |");
    }
    out.push('\n');
    out.push_str("| classification | count |\n|---|---:|\n");
    for (classification, count) in divergence_classification_counts(&report.divergences) {
        let _ = writeln!(out, "| {} | {} |", markdown_cell(&classification), count);
    }
    out.push('\n');
    out.push_str("| verdict | count |\n|---|---:|\n");
    for (verdict, count) in divergence_verdict_counts(&report.divergences) {
        let _ = writeln!(out, "| {verdict:?} | {count} |");
    }
    out.push('\n');
    out.push_str(
        "| kind | package | Cargo.lock | Rodin | verdict | classification | accepted policy |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|\n");
    for divergence in &report.divergences {
        let _ = writeln!(
            out,
            "| {:?} | `{}` | `{}` | `{}` | {:?} | {} | {} |",
            divergence.kind,
            divergence.package,
            divergence.cargo_lock_versions.join(", "),
            divergence
                .rodin_version
                .clone()
                .unwrap_or_else(|| "-".to_owned()),
            divergence.verdict,
            divergence.classification,
            divergence
                .accepted_policy
                .as_deref()
                .map(markdown_cell)
                .unwrap_or_else(|| "-".to_owned())
        );
    }
    out.push('\n');
}

fn render_accepted_divergence_policies(out: &mut String, report: &CargoEvidenceReport) {
    let mut policies = BTreeSet::new();
    for projection in &report.projections {
        for oracle in projection.oracles() {
            for divergence in &oracle.divergences {
                if let Some(policy) = &divergence.accepted_policy {
                    policies.insert(policy.clone());
                }
            }
        }
    }
    out.push_str("### Accepted divergences\n\n");
    if policies.is_empty() {
        out.push_str("No accepted divergence policies were needed after extraction fixes.\n\n");
        return;
    }
    out.push_str("| policy | Rodin rule |\n|---|---|\n");
    for policy in policies {
        let rule = accepted_policy_rule(&policy);
        let _ = writeln!(
            out,
            "| {} | {} |",
            markdown_cell(&policy),
            markdown_cell(rule)
        );
    }
    out.push('\n');
}

fn accepted_policy_rule(policy: &str) -> &'static str {
    match policy {
        "lockfile-free version choice" => {
            "Rodin does not preserve a historical or fresh Cargo.lock version when multiple cached versions satisfy the active ranges; it selects the highest version in the package domain under the canonical order."
        }
        "offline snapshot boundary" => {
            "Rodin only resolves packages present in the offline lock/cache snapshot and does not synthesize missing registry artifacts or unversioned lock edges."
        }
        "fixed-target inactive target edge" => {
            "The fixed-target runtime projection evaluates target-conditional edges for aarch64-apple-darwin and does not select packages reachable only through inactive target predicates."
        }
        "fixed-target inactive dev edge" => {
            "The fixed-target runtime projection keeps dev-dependencies active only for workspace members; packages reachable only through non-workspace dev edges are outside that projection."
        }
        "lockfile-free graph membership" => {
            "Rodin treats Cargo.lock as an oracle, not as a root set; a locked package class is absent when the extracted manifest closure does not require that domain."
        }
        "manifest-closure extra selection" => {
            "Rodin solves the extracted manifest closure over the offline cache, so it may select an internally justified cached package domain that Cargo's lockfile did not retain."
        }
        "historical-lock superseded by fresh oracle" => {
            "A historical Cargo.lock row is accepted when the fresh Cargo oracle selects the same package domain and version as Rodin."
        }
        _ => "Policy text is carried in the divergence row.",
    }
}

fn markdown_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', "<br>")
}

fn divergence_counts(divergences: &[CargoDivergence]) -> BTreeMap<CargoDivergenceKind, usize> {
    let mut counts = BTreeMap::new();
    for divergence in divergences {
        *counts.entry(divergence.kind.clone()).or_default() += 1;
    }
    counts
}

fn divergence_classification_counts(divergences: &[CargoDivergence]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for divergence in divergences {
        *counts.entry(divergence.classification.clone()).or_default() += 1;
    }
    counts
}

fn divergence_verdict_counts(
    divergences: &[CargoDivergence],
) -> BTreeMap<CargoDivergenceVerdict, usize> {
    let mut counts = BTreeMap::new();
    for divergence in divergences {
        *counts.entry(divergence.verdict.clone()).or_default() += 1;
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

fn default_registry_cache_for_src(registry_src: &Path) -> PathBuf {
    registry_src
        .file_name()
        .map(|index| {
            registry_src
                .parent()
                .and_then(Path::parent)
                .map(|registry| registry.join("cache").join(index))
                .unwrap_or_else(|| PathBuf::from(DEFAULT_REGISTRY_CACHE))
        })
        .unwrap_or_else(|| PathBuf::from(DEFAULT_REGISTRY_CACHE))
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
    let registry_cache = default_registry_cache_for_src(registry_src);
    let fresh_lock = run_fresh_lock_oracle(workspace)?;
    let projection_modes = [
        CargoProjectionMode::FixedTargetRuntime,
        CargoProjectionMode::LockUnionDev,
    ];
    let mut projections = Vec::new();
    for projection in projection_modes {
        projections.push(run_projection_evidence(
            workspace,
            registry_src,
            &registry_cache,
            projection,
            fresh_lock.lock.as_deref(),
        )?);
    }
    Ok(CargoEvidenceReport {
        workspace: workspace.display().to_string(),
        fresh_lock_oracle: fresh_lock.report,
        projections,
    })
}

struct FreshLockRun {
    report: CargoFreshLockOracle,
    lock: Option<Vec<LockPackage>>,
    #[cfg(test)]
    lock_text: Option<String>,
}

fn run_fresh_lock_oracle(workspace: &Path) -> Result<FreshLockRun> {
    let start = Instant::now();
    let temp_root = std::env::temp_dir().join(format!(
        "rodin-fresh-lock-oracle-{}-{}",
        std::process::id(),
        fresh_lock_nonce()
    ));
    let copy = temp_root.join("workspace");
    let mut cleanup = true;
    let result = (|| -> Result<FreshLockRun> {
        copy_workspace_for_oracle(workspace, &copy)?;
        let output = Command::new("cargo")
            .arg("generate-lockfile")
            .arg("--offline")
            .env("CARGO_NET_OFFLINE", "true")
            .current_dir(&copy)
            .output()
            .wrap_err("running fresh-lock Cargo oracle")?;
        let wall_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        if !output.status.success() {
            return Ok(FreshLockRun {
                report: CargoFreshLockOracle {
                    attempted: true,
                    solved: false,
                    lock_packages: 0,
                    lock_unique_names: 0,
                    wall_ms,
                    message: command_tail(&output.stderr),
                },
                lock: None,
                #[cfg(test)]
                lock_text: None,
            });
        }
        let lock_text = fs::read_to_string(copy.join("Cargo.lock"))
            .wrap_err("reading fresh-lock oracle Cargo.lock")?;
        let lock = parse_lock(&lock_text)?;
        let lock_unique_names = lock
            .iter()
            .map(|package| package.name.clone())
            .collect::<BTreeSet<_>>()
            .len();
        Ok(FreshLockRun {
            report: CargoFreshLockOracle {
                attempted: true,
                solved: true,
                lock_packages: lock.len(),
                lock_unique_names,
                wall_ms,
                message: "cargo generate-lockfile --offline succeeded in throwaway copy".to_owned(),
            },
            lock: Some(lock),
            #[cfg(test)]
            lock_text: Some(lock_text),
        })
    })();
    if let Err(err) = &result {
        cleanup = temp_root.exists();
        tracing::warn!(error = %err, "fresh-lock Cargo oracle failed before reporting");
    }
    if cleanup {
        let _ = fs::remove_dir_all(&temp_root);
    }
    match result {
        Ok(run) => Ok(run),
        Err(err) => Ok(FreshLockRun {
            report: CargoFreshLockOracle {
                attempted: true,
                solved: false,
                lock_packages: 0,
                lock_unique_names: 0,
                wall_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                message: err.to_string(),
            },
            lock: None,
            #[cfg(test)]
            lock_text: None,
        }),
    }
}

fn fresh_lock_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

fn command_tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut lines = text.lines().rev().take(8).collect::<Vec<_>>();
    lines.reverse();
    let message = lines.join("\n");
    if message.is_empty() {
        "cargo generate-lockfile --offline failed without stderr".to_owned()
    } else {
        message
    }
}

fn copy_workspace_for_oracle(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).wrap_err_with(|| format!("creating {}", dst.display()))?;
    for entry in fs::read_dir(src).wrap_err_with(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        if matches!(
            name_text.as_ref(),
            ".git" | "target" | ".direnv" | ".jj" | "node_modules"
        ) {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_workspace_for_oracle(&from, &to)?;
        } else if ty.is_file() {
            fs::copy(&from, &to)
                .wrap_err_with(|| format!("copying {} to {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn run_projection_evidence(
    workspace: &Path,
    registry_src: &Path,
    registry_cache: &Path,
    projection: CargoProjectionMode,
    fresh_lock: Option<&[LockPackage]>,
) -> Result<CargoProjectionReport> {
    let extract_start = Instant::now();
    let extracted = extract_cargo_universe(
        workspace,
        registry_src,
        registry_cache,
        projection,
        fresh_lock,
    )?;
    tracing::info!(
        projection = ?projection,
        elapsed_ms = extract_start.elapsed().as_millis(),
        indexed_edges = extracted.indexed_edges,
        gaps = extracted.gaps.len(),
        "rodin cargo evidence extraction complete"
    );
    let roots = extracted.problem.roots.len();
    let lock_packages = extracted.lock_versions.len();
    let lock_unique_names = extracted
        .lock_versions
        .iter()
        .map(|locked| locked.name.clone())
        .collect::<BTreeSet<_>>()
        .len();
    let indexed_package_names = extracted.index.package_count();
    let indexed_versions = extracted.index.version_count();

    let start = Instant::now();
    tracing::info!(projection = ?projection, "rodin cargo evidence solve starting");
    let solved = solve(&extracted.index, extracted.problem.clone());
    let wall_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    tracing::info!(projection = ?projection, wall_ms, "rodin cargo evidence solve finished");
    let (selected, solve_stats, learned, derivations) = match solved {
        Ok(result) => {
            let stats = solve_stats(SolveStatsInput {
                solved: true,
                decisions: result.decisions,
                conflicts: result.conflicts,
                restarts: result.restarts,
                wall_ms,
                learned_scans: result.learned_scans,
                learned_prunes: result.learned_prunes,
                learned_conflicts: result.learned_conflicts,
                learned_subsumed: result.learned_subsumed,
                learned_replaced: result.learned_replaced,
                learned: &result.learned,
                trace: &result.trace,
            });
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
                restarts,
                learned_scans,
                learned_prunes,
                learned_conflicts,
                learned_subsumed,
                learned_replaced,
                learned,
                derivations,
                trace,
                ..
            } => {
                let stats = solve_stats(SolveStatsInput {
                    solved: false,
                    decisions,
                    conflicts,
                    restarts,
                    wall_ms,
                    learned_scans,
                    learned_prunes,
                    learned_conflicts,
                    learned_subsumed,
                    learned_replaced,
                    learned: &learned,
                    trace: &trace,
                });
                (BTreeMap::new(), stats, learned, derivations)
            }
        },
    };
    let fresh_domains = fresh_lock.map(locked_domains);
    let historical_oracle = compare_to_lock(
        CargoOracleKind::HistoricalLock,
        &extracted.lock_versions,
        &selected,
        &extracted.gaps,
        fresh_domains.as_deref(),
    );
    let fresh_oracle = fresh_domains.as_ref().map(|fresh| {
        compare_to_lock(
            CargoOracleKind::FreshLock,
            fresh,
            &selected,
            &extracted.gaps,
            None,
        )
    });
    let warm_counterfactual =
        if solve_stats.solved && projection == CargoProjectionMode::FixedTargetRuntime {
            warm_counterfactual(&extracted, &selected, &learned, &derivations).ok()
        } else {
            None
        };

    Ok(CargoProjectionReport {
        projection,
        lock_packages,
        lock_unique_names,
        workspace_roots: roots,
        indexed_package_names,
        indexed_versions,
        indexed_edges: extracted.indexed_edges,
        extraction_gaps: extracted.gaps,
        solve: solve_stats,
        historical_oracle,
        fresh_oracle,
        warm_counterfactual,
    })
}

struct SolveStatsInput<'a> {
    solved: bool,
    decisions: usize,
    conflicts: usize,
    restarts: usize,
    wall_ms: u64,
    learned_scans: usize,
    learned_prunes: usize,
    learned_conflicts: usize,
    learned_subsumed: usize,
    learned_replaced: usize,
    learned: &'a [crate::LearnedNoGood],
    trace: &'a [String],
}

fn solve_stats(input: SolveStatsInput<'_>) -> CargoSolveStats {
    let active = input
        .learned
        .iter()
        .filter(|fact| fact.active)
        .collect::<Vec<_>>();
    let point_versions = active.iter().map(|fact| fact.point_versions).sum();
    let widened_versions = active.iter().map(|fact| fact.widened_versions).sum();
    CargoSolveStats {
        solved: input.solved,
        decisions: input.decisions,
        conflicts: input.conflicts,
        restarts: input.restarts,
        learned_facts: active.len(),
        learned_scans: input.learned_scans,
        learned_prunes: input.learned_prunes,
        learned_conflicts: input.learned_conflicts,
        learned_subsumed: input.learned_subsumed,
        learned_replaced: input.learned_replaced,
        point_versions,
        widened_versions,
        widening_factor: if point_versions == 0 {
            0.0
        } else {
            widened_versions as f64 / point_versions as f64
        },
        wall_ms: input.wall_ms,
        trace_tail: input
            .trace
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
        .find(|name| selected_domain_for_base(&extracted.index, selected, name).is_some())
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

fn selected_domain_for_base(
    index: &CompiledIndex,
    selected: &BTreeMap<String, Version>,
    base: &str,
) -> Option<String> {
    selected.keys().find_map(|label| {
        index
            .package(label)
            .and_then(|pkg| index.base_name(pkg))
            .is_some_and(|name| name == base)
            .then(|| label.clone())
    })
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

fn extract_cargo_universe(
    workspace: &Path,
    registry_src: &Path,
    registry_cache: &Path,
    projection: CargoProjectionMode,
    fresh_lock: Option<&[LockPackage]>,
) -> Result<ExtractedCargoUniverse> {
    let lock_path = workspace.join("Cargo.lock");
    let lock = parse_lock(&fs::read_to_string(&lock_path).wrap_err("reading Cargo.lock")?)?;
    let lock_by_name = lock_versions_by_name(&lock);
    let locked_domains = locked_domains(&lock);
    let mut registry_sources = registry_sources_by_name(&lock);
    if let Some(fresh) = fresh_lock {
        registry_sources.extend(registry_sources_by_name(fresh));
    }
    let workspace_members = workspace_members(workspace)?;
    let workspace_member_set = workspace_members.iter().cloned().collect::<BTreeSet<_>>();
    let local_manifests = local_manifest_paths(workspace, &workspace_members)?;
    let workspace_deps = workspace_dependency_defs(workspace)?;
    let mut gaps = Vec::new();
    let mut index = CompiledIndex::empty();
    let mut indexed_edges = 0;
    let mut target_names: BTreeSet<_> = lock_by_name.keys().cloned().collect();
    if let Some(fresh) = fresh_lock {
        target_names.extend(fresh.iter().map(|package| package.name.clone()));
    }
    let model = ExtractionModel {
        projection,
        target_names: &target_names,
        workspace_members: &workspace_member_set,
    };

    for package_name in &target_names {
        let versions = candidate_versions_with_fresh(
            package_name,
            &lock,
            fresh_lock,
            registry_src,
            registry_cache,
        )?;
        for version in versions {
            let source = lock
                .iter()
                .find(|pkg| pkg.name == *package_name && pkg.version == version)
                .and_then(|pkg| pkg.source.clone())
                .or_else(|| {
                    fresh_lock.and_then(|fresh| {
                        fresh
                            .iter()
                            .find(|pkg| pkg.name == *package_name && pkg.version == version)
                            .and_then(|pkg| pkg.source.clone())
                    })
                })
                .or_else(|| registry_sources.get(package_name).cloned());
            let manifest = manifest_text(
                workspace,
                registry_src,
                registry_cache,
                &local_manifests,
                package_name,
                &version,
                source.as_deref(),
            );
            let manifest_deps = match manifest
                .as_ref()
                .map(|text| parse_manifest_deps_with_workspace(text, Some(&workspace_deps)))
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
            let fresh_package = fresh_lock.and_then(|fresh| {
                fresh
                    .iter()
                    .find(|pkg| pkg.name == *package_name && pkg.version == version)
            });
            let emit_package = fresh_package.or(locked_package);
            let deps = deps_for_version(
                package_name,
                &version,
                emit_package,
                &manifest_deps,
                &model,
                &mut gaps,
            );
            indexed_edges += deps.len();
            let lock = emit_package
                .map(|package| PackageLockMetadata {
                    source: package.source.clone(),
                    checksum: package.checksum.clone(),
                    dependency_entries: Some(
                        package
                            .dependencies
                            .iter()
                            .map(|dep| dep.entry.clone())
                            .collect(),
                    ),
                })
                .unwrap_or_else(|| PackageLockMetadata {
                    source: source.clone(),
                    checksum: None,
                    dependency_entries: None,
                });
            index.add_package_version_with_lock_metadata(
                package_name,
                version,
                deps,
                manifest_deps.features,
                lock,
            );
        }
    }
    tracing::info!(
        packages = index.package_count(),
        versions = index.version_count(),
        indexed_edges,
        gaps = gaps.len(),
        "rodin cargo evidence index compile starting"
    );
    index.finish_synthetic()?;
    tracing::info!(
        packages = index.package_count(),
        versions = index.version_count(),
        "rodin cargo evidence index compile finished"
    );

    let roots = local_manifests
        .keys()
        .filter_map(|name| {
            lock_by_name
                .get(name)
                .and_then(|versions| versions.last())
                .map(|version| (name, version))
        })
        .map(|(name, version)| RootReq::new(name.clone(), format!("={version}")))
        .collect();
    let mut problem = Problem::new(roots);
    problem.learned_kind = LearnedKind::Family;
    Ok(ExtractedCargoUniverse {
        index,
        problem,
        lock_versions: locked_domains,
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
    {
        let mut builder = ManifestDepBuilder {
            package_name,
            version,
            locked_package,
            model,
            gaps,
            seen: &mut seen,
            out: &mut out,
        };
        for deps in manifest.deps_by_name.values() {
            for dep in deps {
                builder.push(dep);
            }
        }
    }
    if let Some(locked) = locked_package {
        for dep in locked
            .dependencies
            .iter()
            .filter(|dep| dep.version.is_none() && !seen.contains(&dep.name))
        {
            gaps.push(CargoExtractionGap {
                kind: CargoExtractionGapKind::DependencyOutsideLockUniverse,
                package: format!("{package_name}@{version}"),
                detail: format!("{} has no version in Cargo.lock edge", dep.name),
            });
        }
    }
    out
}

struct ManifestDepBuilder<'a, 'b> {
    package_name: &'a str,
    version: &'a Version,
    locked_package: Option<&'a LockPackage>,
    model: &'a ExtractionModel<'a>,
    gaps: &'b mut Vec<CargoExtractionGap>,
    seen: &'b mut BTreeSet<String>,
    out: &'b mut Vec<SyntheticDep>,
}

impl ManifestDepBuilder<'_, '_> {
    fn push(&mut self, dep: &ManifestDep) {
        if dep.workspace {
            self.gaps.push(CargoExtractionGap {
                kind: CargoExtractionGapKind::WorkspaceDependencyUnresolved,
                package: self.package_version(),
                detail: dep.key.clone(),
            });
            return;
        }
        if !self.model.projection.includes_target(dep.target.as_deref()) {
            self.gaps.push(CargoExtractionGap {
                kind: CargoExtractionGapKind::TargetConditionalDependency,
                package: self.package_version(),
                detail: format!(
                    "{} gated by {}",
                    dep.actual,
                    dep.target.as_deref().unwrap_or("<none>")
                ),
            });
            return;
        }
        if dep.kind == DepKind::Dev
            && !self
                .model
                .projection
                .includes_dev_dependency(self.package_name, self.model.workspace_members)
        {
            self.gaps.push(CargoExtractionGap {
                kind: CargoExtractionGapKind::DevDependency,
                package: self.package_version(),
                detail: dep.actual.clone(),
            });
            return;
        }
        if !self.model.target_names.contains(&dep.actual) {
            self.gaps.push(CargoExtractionGap {
                kind: CargoExtractionGapKind::DependencyOutsideLockUniverse,
                package: self.package_version(),
                detail: dep.actual.clone(),
            });
            return;
        }
        let req = dep.req.clone().unwrap_or_else(|| "*".to_owned());
        if VersionReq::parse(&req).is_err() {
            self.gaps.push(CargoExtractionGap {
                kind: CargoExtractionGapKind::SemverRequirementRejected,
                package: self.package_version(),
                detail: format!("{} {}", dep.actual, req),
            });
            return;
        }
        let mut synthetic = SyntheticDep::normal(dep.key.clone(), req);
        synthetic.package = (dep.actual != dep.key).then(|| dep.actual.clone());
        synthetic.features = dep.features.clone();
        synthetic.default_features = dep.default_features;
        synthetic.optional = dep.optional;
        synthetic.kind = dep.kind.as_str().to_owned();
        synthetic.target = dep.target.clone();
        if let Some(class) = self.locked_class_hint(dep) {
            synthetic = synthetic.class_hint(class);
        }
        self.out.push(synthetic);
        self.seen.insert(dep.actual.clone());
    }

    fn package_version(&self) -> String {
        format!("{}@{}", self.package_name, self.version)
    }

    fn locked_class_hint(&self, dep: &ManifestDep) -> Option<CompatClass> {
        let locked = self.locked_package?;
        let version = locked.dependencies.iter().find_map(|line| {
            (line.name == dep.actual || line.name == dep.key)
                .then(|| line.version.clone())
                .flatten()
        })?;
        Some(CompatClass::from_version(&version))
    }
}

fn compare_to_lock(
    oracle: CargoOracleKind,
    lock_versions: &[LockedDomain],
    selected: &BTreeMap<String, Version>,
    gaps: &[CargoExtractionGap],
    fresh_reference: Option<&[LockedDomain]>,
) -> CargoOracleReport {
    let mut exact_matches = 0;
    let mut divergences = Vec::new();
    let locked_labels = lock_versions
        .iter()
        .map(|locked| domain_label(&locked.name, locked.class))
        .collect::<BTreeSet<_>>();
    for locked in lock_versions {
        let label = domain_label(&locked.name, locked.class);
        match selected.get(&label) {
            Some(selected) if selected == &locked.version => exact_matches += 1,
            Some(selected) => {
                let classification =
                    classify_version_mismatch(oracle, &label, selected, fresh_reference);
                let (verdict, accepted_policy) =
                    verdict_for_divergence(CargoDivergenceKind::VersionMismatch, &classification);
                divergences.push(CargoDivergence {
                    kind: CargoDivergenceKind::VersionMismatch,
                    package: label.clone(),
                    cargo_lock_versions: vec![locked.version.to_string()],
                    rodin_version: Some(selected.to_string()),
                    classification,
                    verdict,
                    accepted_policy,
                });
            }
            None => {
                let classification = classify_missing_locked_domain(locked, gaps);
                let (verdict, accepted_policy) =
                    verdict_for_divergence(CargoDivergenceKind::MissingInRodin, &classification);
                divergences.push(CargoDivergence {
                    kind: CargoDivergenceKind::MissingInRodin,
                    package: label.clone(),
                    cargo_lock_versions: vec![locked.version.to_string()],
                    rodin_version: None,
                    classification,
                    verdict,
                    accepted_policy,
                });
            }
        }
    }
    for (name, version) in selected {
        if !locked_labels.contains(name) {
            let classification = classify_extra_selection(oracle, name, version, fresh_reference);
            let (verdict, accepted_policy) =
                verdict_for_divergence(CargoDivergenceKind::ExtraInRodin, &classification);
            divergences.push(CargoDivergence {
                kind: CargoDivergenceKind::ExtraInRodin,
                package: name.clone(),
                cargo_lock_versions: Vec::new(),
                rodin_version: Some(version.to_string()),
                classification,
                verdict,
                accepted_policy,
            });
        }
    }
    let compared_names = lock_versions.len();
    CargoOracleReport {
        oracle,
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

fn classify_version_mismatch(
    oracle: CargoOracleKind,
    label: &str,
    selected: &Version,
    fresh_reference: Option<&[LockedDomain]>,
) -> String {
    if oracle == CargoOracleKind::HistoricalLock
        && fresh_reference
            .and_then(|fresh| fresh_version_for_label(fresh, label))
            .is_some_and(|fresh| fresh == selected)
    {
        "fresh-lock oracle selects Rodin's version; historical lock divergence vindicated"
            .to_owned()
    } else {
        "same package compatibility class selected a different version from Cargo.lock".to_owned()
    }
}

fn classify_extra_selection(
    oracle: CargoOracleKind,
    label: &str,
    selected: &Version,
    fresh_reference: Option<&[LockedDomain]>,
) -> String {
    if oracle == CargoOracleKind::HistoricalLock
        && fresh_reference
            .and_then(|fresh| fresh_version_for_label(fresh, label))
            .is_some_and(|fresh| fresh == selected)
    {
        "fresh-lock oracle includes Rodin's selected domain; historical extra-selection vindicated"
            .to_owned()
    } else {
        "selected from cached registry universe but absent from Cargo.lock".to_owned()
    }
}

fn verdict_for_divergence(
    kind: CargoDivergenceKind,
    classification: &str,
) -> (CargoDivergenceVerdict, Option<String>) {
    if classification.contains("manifest unavailable") {
        return (CargoDivergenceVerdict::ExtractionBug, None);
    }
    if classification.contains("workspace dependency inheritance could not be resolved") {
        return (CargoDivergenceVerdict::ExtractionBug, None);
    }
    if classification.contains("semver subset") {
        return (
            CargoDivergenceVerdict::SemverModelInterpretationDifference,
            None,
        );
    }
    if classification.contains("out-of-snapshot")
        || classification.contains("unversioned Cargo.lock edge")
    {
        return (
            CargoDivergenceVerdict::LegitimateTieBreakDifference,
            Some("offline snapshot boundary".to_owned()),
        );
    }
    if classification.contains("fresh-lock oracle selects Rodin's version")
        || classification.contains("fresh-lock oracle includes Rodin's selected domain")
    {
        return (
            CargoDivergenceVerdict::LegitimateTieBreakDifference,
            Some("historical-lock superseded by fresh oracle".to_owned()),
        );
    }
    if classification.contains("target-conditional dependency projection is inactive") {
        return (
            CargoDivergenceVerdict::LegitimateTieBreakDifference,
            Some("fixed-target inactive target edge".to_owned()),
        );
    }
    if classification.contains("dev-dependency projection is inactive") {
        return (
            CargoDivergenceVerdict::LegitimateTieBreakDifference,
            Some("fixed-target inactive dev edge".to_owned()),
        );
    }
    if classification.contains("locked package class was not reached")
        || classification.contains("build-dependency projection is inactive")
        || classification.contains("feature/optional dependency projection is inactive")
        || classification.contains("reached only through Cargo.lock exact-edge fallback")
    {
        return (
            CargoDivergenceVerdict::LegitimateTieBreakDifference,
            Some("lockfile-free graph membership".to_owned()),
        );
    }
    if classification.contains("selected from cached registry universe but absent from Cargo.lock")
    {
        return (
            CargoDivergenceVerdict::LegitimateTieBreakDifference,
            Some("manifest-closure extra selection".to_owned()),
        );
    }
    if classification.contains("same package compatibility class selected a different version")
        || matches!(kind, CargoDivergenceKind::VersionMismatch)
    {
        return (
            CargoDivergenceVerdict::LegitimateTieBreakDifference,
            Some("lockfile-free version choice".to_owned()),
        );
    }
    if matches!(kind, CargoDivergenceKind::ExtraInRodin) {
        return (
            CargoDivergenceVerdict::LegitimateTieBreakDifference,
            Some("manifest-closure extra selection".to_owned()),
        );
    }
    (
        CargoDivergenceVerdict::LegitimateTieBreakDifference,
        Some("lockfile-free graph membership".to_owned()),
    )
}

fn fresh_version_for_label<'a>(fresh: &'a [LockedDomain], label: &str) -> Option<&'a Version> {
    fresh
        .iter()
        .find(|locked| domain_label(&locked.name, locked.class) == label)
        .map(|locked| &locked.version)
}

fn classify_missing_locked_domain(locked: &LockedDomain, gaps: &[CargoExtractionGap]) -> String {
    let package_version = format!("{}@{}", locked.name, locked.version);
    let mut kinds = BTreeSet::new();
    for gap in gaps.iter().filter(|gap| gap.package == package_version) {
        if matches!(
            gap.kind,
            CargoExtractionGapKind::ManifestMissing
                | CargoExtractionGapKind::ManifestDependencyUnparsed
                | CargoExtractionGapKind::SemverRequirementRejected
                | CargoExtractionGapKind::WorkspaceDependencyUnresolved
        ) {
            kinds.insert(gap.kind.clone());
        }
    }
    for gap in gaps
        .iter()
        .filter(|gap| gap_detail_mentions_package(&gap.detail, &locked.name))
    {
        kinds.insert(gap.kind.clone());
    }
    if kinds.contains(&CargoExtractionGapKind::ManifestMissing) {
        "manifest unavailable for this locked package artifact".to_owned()
    } else if kinds.contains(&CargoExtractionGapKind::TargetConditionalDependency) {
        "target-conditional dependency projection is inactive in this round".to_owned()
    } else if kinds.contains(&CargoExtractionGapKind::OptionalDependency) {
        "feature/optional dependency projection is inactive in this round".to_owned()
    } else if kinds.contains(&CargoExtractionGapKind::DevDependency) {
        "dev-dependency projection is inactive in this round".to_owned()
    } else if kinds.contains(&CargoExtractionGapKind::BuildDependency) {
        "build-dependency projection is inactive in this round".to_owned()
    } else if kinds.contains(&CargoExtractionGapKind::DependencyOutsideLockUniverse) {
        "manifest dependency references an out-of-snapshot crate or an unversioned Cargo.lock edge"
            .to_owned()
    } else if kinds.contains(&CargoExtractionGapKind::SemverRequirementRejected) {
        "manifest requirement was not representable by Rodin's current semver subset".to_owned()
    } else if kinds.contains(&CargoExtractionGapKind::WorkspaceDependencyUnresolved) {
        "workspace dependency inheritance could not be resolved from the root workspace table"
            .to_owned()
    } else if kinds.contains(&CargoExtractionGapKind::LockExactFallback) {
        "reached only through Cargo.lock exact-edge fallback, not real manifest range solving"
            .to_owned()
    } else {
        "locked package class was not reached under the extracted root/range/feature projection"
            .to_owned()
    }
}

fn gap_detail_mentions_package(detail: &str, name: &str) -> bool {
    detail == name
        || detail
            .strip_prefix(name)
            .is_some_and(|rest| matches!(rest.as_bytes().first(), Some(b' ' | b'@' | b':')))
}

fn candidate_versions_with_fresh(
    name: &str,
    lock: &[LockPackage],
    fresh_lock: Option<&[LockPackage]>,
    registry_src: &Path,
    registry_cache: &Path,
) -> Result<Vec<Version>> {
    let mut combined = lock.to_vec();
    if let Some(fresh) = fresh_lock {
        combined.extend_from_slice(fresh);
    }
    candidate_versions(name, &combined, registry_src, registry_cache)
}

fn candidate_versions(
    name: &str,
    lock: &[LockPackage],
    registry_src: &Path,
    registry_cache: &Path,
) -> Result<Vec<Version>> {
    let is_registry = lock.iter().any(|pkg| {
        pkg.name == name
            && pkg
                .source
                .as_deref()
                .is_some_and(|source| source.starts_with("registry+"))
    });
    if !is_registry {
        let mut versions = lock
            .iter()
            .filter(|pkg| pkg.name == name)
            .map(|pkg| pkg.version.clone())
            .collect::<Vec<_>>();
        versions.sort();
        versions.dedup();
        return Ok(versions);
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
    if registry_cache.exists() {
        for entry in fs::read_dir(registry_cache).wrap_err("reading registry cache")? {
            let entry = entry?;
            let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(version_text) = file_name
                .strip_prefix(&format!("{name}-"))
                .and_then(|text| text.strip_suffix(".crate"))
            else {
                continue;
            };
            if let Ok(version) = Version::parse(version_text) {
                versions.push(version);
            }
        }
    }
    if versions.is_empty() {
        versions.extend(
            lock.iter()
                .filter(|pkg| pkg.name == name)
                .map(|pkg| pkg.version.clone()),
        );
    }
    versions.sort();
    versions.dedup();
    Ok(versions)
}

fn manifest_text(
    workspace: &Path,
    registry_src: &Path,
    registry_cache: &Path,
    local_manifests: &BTreeMap<String, PathBuf>,
    name: &str,
    version: &Version,
    source: Option<&str>,
) -> Option<String> {
    if source.is_some_and(|source| source.starts_with("registry+")) {
        fs::read_to_string(
            registry_src
                .join(format!("{name}-{version}"))
                .join("Cargo.toml"),
        )
        .ok()
        .or_else(|| manifest_text_from_crate(registry_cache, name, version).ok())
    } else {
        local_manifests
            .get(name)
            .cloned()
            .or_else(|| Some(workspace.join(name).join("Cargo.toml")))
            .and_then(|path| fs::read_to_string(path).ok())
    }
}

fn manifest_text_from_crate(
    registry_cache: &Path,
    name: &str,
    version: &Version,
) -> Result<String> {
    let path = registry_cache.join(format!("{name}-{version}.crate"));
    let file = File::open(&path).wrap_err_with(|| format!("opening {}", path.display()))?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    let wanted = format!("{name}-{version}/Cargo.toml");
    for entry in archive.entries().wrap_err("reading .crate archive")? {
        let mut entry = entry?;
        if entry.path()?.to_string_lossy() == wanted {
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            return Ok(text);
        }
    }
    bail!("Cargo.toml not found in {}", path.display())
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
                checksum: None,
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
        } else if let Some(checksum) = line.strip_prefix("checksum = ").and_then(parse_quoted) {
            pkg.checksum = Some(checksum);
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
    Ok(LockDep {
        name,
        version,
        entry: text.to_owned(),
    })
}

fn parse_manifest_deps(text: &str) -> Result<ManifestDeps> {
    parse_manifest_deps_with_workspace(text, None)
}

fn parse_manifest_deps_with_workspace(
    text: &str,
    workspace_deps: Option<&ManifestDeps>,
) -> Result<ManifestDeps> {
    let mut out = ManifestDeps::default();
    let mut section = ManifestSection::Other;
    let mut current_table: Option<ManifestDep> = None;
    let mut current_feature: Option<(String, String)> = None;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if let Some((feature, value)) = current_feature.as_mut() {
            value.push(' ');
            value.push_str(line);
            if line.contains(']') {
                out.features.insert(feature.clone(), quoted_values(value));
                current_feature = None;
            }
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            if let Some(dep) = current_table.take() {
                let dep = resolve_workspace_dep(dep, workspace_deps);
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
                    default_features_explicit: false,
                    workspace: false,
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
        } else if section == ManifestSection::Features {
            if let Some((feature, value)) = line.split_once('=') {
                let feature = feature.trim().to_owned();
                let value = value.trim().to_owned();
                if value.contains(']') {
                    out.features.insert(feature, quoted_values(&value));
                } else {
                    current_feature = Some((feature, value));
                }
            }
        } else if let ManifestSection::Dependencies { kind, target } = section.clone()
            && let Some((key, value)) = line.split_once('=')
        {
            let key = key.trim().to_ascii_lowercase();
            let dep = parse_inline_dep(key, value.trim(), kind, target);
            let dep = resolve_workspace_dep(dep, workspace_deps);
            out.deps_by_name
                .entry(dep.actual.clone())
                .or_default()
                .push(dep);
        }
    }
    if let Some(dep) = current_table.take() {
        let dep = resolve_workspace_dep(dep, workspace_deps);
        out.deps_by_name
            .entry(dep.actual.clone())
            .or_default()
            .push(dep);
    }
    Ok(out)
}

fn resolve_workspace_dep(
    mut dep: ManifestDep,
    workspace_deps: Option<&ManifestDeps>,
) -> ManifestDep {
    if !dep.workspace {
        return dep;
    }
    let Some(base) = workspace_deps
        .and_then(|deps| workspace_dep_by_key(deps, &dep.key))
        .cloned()
    else {
        return dep;
    };
    let local_features = dep.features;
    let local_default_features = dep.default_features;
    let local_default_features_explicit = dep.default_features_explicit;
    let local_optional = dep.optional;
    let local_kind = dep.kind;
    let local_target = dep.target;
    dep = base;
    dep.workspace = false;
    dep.kind = local_kind;
    dep.target = local_target;
    dep.optional = local_optional;
    if local_default_features_explicit {
        dep.default_features = local_default_features;
    }
    dep.features.extend(local_features);
    dep.features.sort();
    dep.features.dedup();
    dep
}

fn workspace_dep_by_key<'a>(deps: &'a ManifestDeps, key: &str) -> Option<&'a ManifestDep> {
    deps.deps_by_name
        .get(key)
        .and_then(|entries| entries.first())
        .or_else(|| {
            deps.deps_by_name
                .values()
                .flatten()
                .find(|dep| dep.key == key)
        })
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ManifestSection {
    Other,
    Features,
    Dependencies {
        kind: DepKind,
        target: Option<String>,
    },
    DependencyTable,
}

fn target_expr_from_header(header: &str) -> Option<String> {
    let target = header.strip_prefix("target.")?;
    let expr = target
        .split_once(".dependencies")
        .or_else(|| target.split_once(".build-dependencies"))
        .or_else(|| target.split_once(".dev-dependencies"))
        .map(|(expr, _)| expr)
        .unwrap_or(target)
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_owned();
    (!expr.is_empty()).then_some(expr)
}

fn target_matches_round(expr: &str) -> bool {
    let expr = expr.trim().trim_matches('"').trim_matches('\'');
    if expr == ROUND_TARGET_TRIPLE {
        return true;
    }
    if let Some(cfg) = expr
        .strip_prefix("cfg(")
        .and_then(|text| text.strip_suffix(')'))
    {
        return eval_cfg_expr(cfg);
    }
    false
}

fn eval_cfg_expr(expr: &str) -> bool {
    let expr = expr.trim();
    if let Some(inner) = expr
        .strip_prefix("all(")
        .and_then(|text| text.strip_suffix(')'))
    {
        return split_cfg_args(inner).iter().all(|arg| eval_cfg_expr(arg));
    }
    if let Some(inner) = expr
        .strip_prefix("any(")
        .and_then(|text| text.strip_suffix(')'))
    {
        return split_cfg_args(inner).iter().any(|arg| eval_cfg_expr(arg));
    }
    if let Some(inner) = expr
        .strip_prefix("not(")
        .and_then(|text| text.strip_suffix(')'))
    {
        return !eval_cfg_expr(inner);
    }
    if expr == "unix" {
        return true;
    }
    if expr == "windows" {
        return false;
    }
    if let Some((key, value)) = expr.split_once('=') {
        let key = key.trim();
        let value = value.trim().trim_matches('"').trim_matches('\'');
        return match key {
            "target_arch" => value == "aarch64",
            "target_endian" => value == "little",
            "target_env" => value.is_empty(),
            "target_family" => value == "unix",
            "target_os" => value == "macos",
            "target_pointer_width" => value == "64",
            "target_vendor" => value == "apple",
            _ => false,
        };
    }
    false
}

fn split_cfg_args(inner: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    let mut in_string = false;
    for ch in inner.chars() {
        match ch {
            '"' | '\'' => {
                in_string = !in_string;
                current.push(ch);
            }
            '(' if !in_string => {
                depth += 1;
                current.push(ch);
            }
            ')' if !in_string => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if !in_string && depth == 0 => {
                if !current.trim().is_empty() {
                    args.push(current.trim().to_owned());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_owned());
    }
    args
}

fn dependency_section(header: &str) -> Option<ManifestSection> {
    if header == "dependencies" {
        Some(ManifestSection::Dependencies {
            kind: DepKind::Normal,
            target: None,
        })
    } else if header == "build-dependencies" {
        Some(ManifestSection::Dependencies {
            kind: DepKind::Build,
            target: None,
        })
    } else if header == "dev-dependencies" {
        Some(ManifestSection::Dependencies {
            kind: DepKind::Dev,
            target: None,
        })
    } else if header == "features" {
        Some(ManifestSection::Features)
    } else if header.contains(".dependencies") {
        Some(ManifestSection::Dependencies {
            kind: DepKind::Normal,
            target: target_expr_from_header(header),
        })
    } else if header.contains(".build-dependencies") {
        Some(ManifestSection::Dependencies {
            kind: DepKind::Build,
            target: target_expr_from_header(header),
        })
    } else if header.contains(".dev-dependencies") {
        Some(ManifestSection::Dependencies {
            kind: DepKind::Dev,
            target: target_expr_from_header(header),
        })
    } else {
        None
    }
}

fn dependency_table_header(header: &str) -> Option<(DepKind, Option<String>, String)> {
    for marker in [
        (".dependencies.", DepKind::Normal),
        (".build-dependencies.", DepKind::Build),
        (".dev-dependencies.", DepKind::Dev),
    ] {
        if let Some((prefix, dep)) = header.rsplit_once(marker.0) {
            return Some((
                marker.1,
                target_expr_from_header(prefix),
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
            return Some((marker.1, None, dep.to_ascii_lowercase()));
        }
    }
    None
}

fn parse_inline_dep(
    key: String,
    value: &str,
    kind: DepKind,
    target: Option<String>,
) -> ManifestDep {
    let (key, workspace_from_dotted_key) = key
        .strip_suffix(".workspace")
        .map(|base| (base.to_owned(), value == "true"))
        .unwrap_or((key, false));
    let mut dep = ManifestDep {
        key: key.clone(),
        actual: key,
        req: None,
        features: Vec::new(),
        optional: false,
        default_features: true,
        default_features_explicit: false,
        workspace: workspace_from_dotted_key,
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
        "default-features" => {
            dep.default_features = value != "false";
            dep.default_features_explicit = true;
        }
        "features" => dep.features = quoted_values(value),
        "workspace" => dep.workspace = value == "true",
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
        let Some(name) = package_name(&text) else {
            continue;
        };
        out.insert(name, manifest);
    }
    Ok(out)
}

fn workspace_dependency_defs(workspace: &Path) -> Result<ManifestDeps> {
    let text = fs::read_to_string(workspace.join("Cargo.toml"))
        .wrap_err("reading workspace Cargo.toml dependencies")?;
    parse_manifest_deps(&text)
}

fn package_name(text: &str) -> Option<String> {
    let mut in_package = false;
    let mut name = None;
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
        }
    }
    name
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

fn locked_domains(lock: &[LockPackage]) -> Vec<LockedDomain> {
    let mut out = lock
        .iter()
        .map(|package| LockedDomain {
            name: package.name.clone(),
            class: CompatClass::from_version(&package.version),
            version: package.version.clone(),
        })
        .collect::<Vec<_>>();
    out.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.class.cmp(&right.class))
            .then_with(|| left.version.cmp(&right.version))
    });
    out.dedup();
    out
}

fn registry_sources_by_name(lock: &[LockPackage]) -> BTreeMap<String, String> {
    lock.iter()
        .filter_map(|pkg| {
            pkg.source
                .as_ref()
                .filter(|source| source.starts_with("registry+"))
                .map(|source| (pkg.name.clone(), source.clone()))
        })
        .collect()
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
    use crate::{SolverOptions, emit_lock, solve_with_options};

    type PackageBlockKey = (String, Version, Option<String>);

    #[test]
    fn parses_lock_dependency_versions() -> Result<()> {
        assert_eq!(
            parse_lock_dep("indexmap 2.14.0")?,
            LockDep {
                name: "indexmap".to_owned(),
                version: Some(Version::parse("2.14.0")?),
                entry: "indexmap 2.14.0".to_owned(),
            }
        );
        assert_eq!(
            parse_lock_dep("facet-core")?,
            LockDep {
                name: "facet-core".to_owned(),
                version: None,
                entry: "facet-core".to_owned(),
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

            [features]
            default = ["hashbrown/raw", "dep:windows-sys"]
            cli = [
              "windows-sys/Win32_System",
              "hashbrown?/serde",
            ]
            "#,
        )?;
        let hashbrown = &parsed.deps_by_name["hashbrown"][0];
        assert_eq!(hashbrown.req.as_deref(), Some("0.17"));
        assert_eq!(hashbrown.target, None);
        let windows = &parsed.deps_by_name["windows-sys"][0];
        assert_eq!(windows.target.as_deref(), Some("cfg(windows)"));
        assert!(windows.optional);
        assert_eq!(windows.features, ["Win32_Foundation"]);
        assert_eq!(
            parsed.features["default"],
            ["hashbrown/raw", "dep:windows-sys"]
        );
        assert_eq!(
            parsed.features["cli"],
            ["windows-sys/Win32_System", "hashbrown?/serde"]
        );
        assert!(!target_matches_round("cfg(windows)"));
        assert!(target_matches_round(
            "cfg(all(unix, target_os = \"macos\"))"
        ));
        Ok(())
    }

    #[test]
    fn parses_workspace_inherited_dependencies() -> Result<()> {
        let workspace = parse_manifest_deps(
            r#"
            [workspace.dependencies]
            blake3 = { version = "^1", default-features = false, features = ["std"] }
            serde-ish = { package = "serde", version = "1" }
            "#,
        )?;
        let parsed = parse_manifest_deps_with_workspace(
            r#"
            [dependencies]
            blake3.workspace = true
            serde-ish = { workspace = true, features = ["derive"] }
            "#,
            Some(&workspace),
        )?;
        let blake3 = &parsed.deps_by_name["blake3"][0];
        assert_eq!(blake3.req.as_deref(), Some("^1"));
        assert!(!blake3.default_features);
        assert_eq!(blake3.features, ["std"]);
        let serde = &parsed.deps_by_name["serde"][0];
        assert_eq!(serde.key, "serde-ish");
        assert_eq!(serde.actual, "serde");
        assert_eq!(serde.req.as_deref(), Some("1"));
        assert_eq!(serde.features, ["derive"]);
        Ok(())
    }

    #[test]
    fn package_name_does_not_require_literal_version() {
        assert_eq!(
            package_name(
                r#"
                [package]
                name = "wasm-browser-tests"
                version.workspace = true
                "#
            )
            .as_deref(),
            Some("wasm-browser-tests")
        );
    }

    #[test]
    fn divergence_verdicts_have_specific_accepted_policies() {
        let cases = [
            (
                CargoDivergenceKind::MissingInRodin,
                "target-conditional dependency projection is inactive in this round",
                Some("fixed-target inactive target edge"),
            ),
            (
                CargoDivergenceKind::MissingInRodin,
                "dev-dependency projection is inactive in this round",
                Some("fixed-target inactive dev edge"),
            ),
            (
                CargoDivergenceKind::MissingInRodin,
                "locked package class was not reached under the extracted root/range/feature projection",
                Some("lockfile-free graph membership"),
            ),
            (
                CargoDivergenceKind::ExtraInRodin,
                "selected from cached registry universe but absent from Cargo.lock",
                Some("manifest-closure extra selection"),
            ),
            (
                CargoDivergenceKind::VersionMismatch,
                "same package compatibility class selected a different version from Cargo.lock",
                Some("lockfile-free version choice"),
            ),
            (
                CargoDivergenceKind::VersionMismatch,
                "fresh-lock oracle selects Rodin's version; historical lock divergence vindicated",
                Some("historical-lock superseded by fresh oracle"),
            ),
        ];
        for (kind, classification, expected_policy) in cases {
            let (verdict, policy) = verdict_for_divergence(kind, classification);
            assert_eq!(
                verdict,
                CargoDivergenceVerdict::LegitimateTieBreakDifference
            );
            assert_eq!(policy.as_deref(), expected_policy);
        }
    }

    #[test]
    fn cargo_artifact_selection_is_search_order_invariant() -> Result<()> {
        let temp = std::env::temp_dir().join(format!(
            "rodin-cargo-selection-invariant-{}",
            std::process::id()
        ));
        if temp.exists() {
            fs::remove_dir_all(&temp)?;
        }
        let workspace = temp.join("workspace");
        let registry_src = temp.join("registry-src");
        let registry_cache = temp.join("registry-cache");
        fs::create_dir_all(workspace.join("app"))?;
        fs::create_dir_all(&registry_src)?;
        fs::create_dir_all(&registry_cache)?;
        fs::write(
            workspace.join("Cargo.toml"),
            r#"
            [workspace]
            members = ["app"]
            "#,
        )?;
        fs::write(
            workspace.join("app").join("Cargo.toml"),
            r#"
            [package]
            name = "app"
            version = "1.0.0"

            [dependencies]
            x = "*"
            y = "*"
            "#,
        )?;
        fs::write(
            workspace.join("Cargo.lock"),
            r#"
            version = 4

            [[package]]
            name = "a"
            version = "1.0.0"
            source = "registry+https://example.invalid/index"

            [[package]]
            name = "app"
            version = "1.0.0"
            dependencies = [
              "x",
              "y",
            ]

            [[package]]
            name = "x"
            version = "1.0.0"
            source = "registry+https://example.invalid/index"
            dependencies = [
              "a 1.0.0",
            ]

            [[package]]
            name = "y"
            version = "1.0.0"
            source = "registry+https://example.invalid/index"
            dependencies = [
              "a 1.0.0",
            ]
            "#,
        )?;
        for (name, version, deps) in [
            ("a", "1.0.0", ""),
            ("a", "1.0.1", ""),
            ("x", "1.0.0", "a = \"=1.0.0\""),
            ("x", "1.0.1", "a = \"=1.0.1\""),
            ("y", "1.0.0", "a = \"=1.0.0\""),
        ] {
            let dir = registry_src.join(format!("{name}-{version}"));
            fs::create_dir_all(&dir)?;
            fs::write(
                dir.join("Cargo.toml"),
                format!(
                    r#"
                    [package]
                    name = "{name}"
                    version = "{version}"

                    [dependencies]
                    {deps}
                    "#
                ),
            )?;
        }
        let fresh = vec![
            LockPackage {
                name: "a".to_owned(),
                version: Version::parse("1.0.1")?,
                source: Some("registry+https://example.invalid/index".to_owned()),
                checksum: None,
                dependencies: Vec::new(),
            },
            LockPackage {
                name: "x".to_owned(),
                version: Version::parse("1.0.1")?,
                source: Some("registry+https://example.invalid/index".to_owned()),
                checksum: None,
                dependencies: Vec::new(),
            },
        ];
        let extracted = extract_cargo_universe(
            &workspace,
            &registry_src,
            &registry_cache,
            CargoProjectionMode::FixedTargetRuntime,
            Some(&fresh),
        )?;
        let lexical = solve_with_options(
            &extracted.index,
            extracted.problem.clone(),
            SolverOptions {
                indexed_exclusions: true,
                subsume_learned: true,
                activity_ordering: false,
                restarts: false,
                read_set_widening: false,
            },
        )
        .map_err(|err| eyre::eyre!("{err:?}"))?;
        let full = solve_with_options(&extracted.index, extracted.problem, SolverOptions::FULL)
            .map_err(|err| eyre::eyre!("{err:?}"))?;
        assert_eq!(
            full.selected, lexical.selected,
            "cargo artifact selection changed under activity/restart search"
        );
        fs::remove_dir_all(temp)?;
        Ok(())
    }

    #[test]
    fn emit_lock_matches_fresh_facet_cc_lock_for_exact_oracle_rows() -> Result<()> {
        let workspace = Path::new(DEFAULT_FACET_CC);
        let registry_src = Path::new(DEFAULT_REGISTRY_SRC);
        let registry_cache = default_registry_cache_for_src(registry_src);
        let fresh = run_fresh_lock_oracle(workspace)?;
        let fresh_text = fresh
            .lock_text
            .clone()
            .ok_or_else(|| eyre::eyre!("fresh lock oracle did not produce lock text"))?;
        let fresh_lock = fresh
            .lock
            .ok_or_else(|| eyre::eyre!("fresh lock oracle did not solve"))?;
        let extracted = extract_cargo_universe(
            workspace,
            registry_src,
            &registry_cache,
            CargoProjectionMode::LockUnionDev,
            Some(&fresh_lock),
        )?;
        let result = solve_with_options(&extracted.index, extracted.problem, SolverOptions::FULL)
            .map_err(|err| eyre::eyre!("{err:?}"))?;
        let fresh_domains = locked_domains(&fresh_lock);
        let matching_keys = fresh_domains
            .iter()
            .filter(|locked| {
                let label = domain_label(&locked.name, locked.class);
                result
                    .selected
                    .get(&label)
                    .is_some_and(|version| version == &locked.version)
            })
            .filter_map(|locked| {
                fresh_lock
                    .iter()
                    .find(|package| {
                        package.name == locked.name && package.version == locked.version
                    })
                    .map(package_key)
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            matching_keys.len(),
            853,
            "Round 9 lock-union fresh exact row count drifted"
        );

        let emitted_blocks = package_blocks(&emit_lock(&result))?;
        let fresh_blocks = package_blocks(&fresh_text)?;
        for key in matching_keys {
            assert_eq!(
                emitted_blocks.get(&key),
                fresh_blocks.get(&key),
                "emitted Cargo.lock block diverged for {key:?}"
            );
        }
        Ok(())
    }

    fn package_key(package: &LockPackage) -> PackageBlockKey {
        (
            package.name.clone(),
            package.version.clone(),
            package.source.clone(),
        )
    }

    fn package_blocks(text: &str) -> Result<BTreeMap<PackageBlockKey, String>> {
        let packages = parse_lock(text)?;
        let starts = text
            .match_indices("[[package]]")
            .map(|(ix, _)| ix)
            .collect::<Vec<_>>();
        let mut out = BTreeMap::new();
        for (ix, package) in packages.iter().enumerate() {
            let Some(start) = starts.get(ix).copied() else {
                bail!("missing block start for {}", package.name);
            };
            let end = starts.get(ix + 1).copied().unwrap_or(text.len());
            out.insert(
                package_key(package),
                text[start..end].trim_end_matches('\n').to_owned(),
            );
        }
        Ok(out)
    }
}
