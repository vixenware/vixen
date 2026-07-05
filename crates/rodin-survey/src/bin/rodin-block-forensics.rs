use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use eyre::{Context, Result};
use rodin_survey::{
    CrateData, IndexDependency, IndexEntry, index_path_for, parse_entries, read_cached_index_file,
    sorted_non_yanked_entries, workspace_cache_dir,
};
use semver::Version;

const ROUND_TARGET_TRIPLE: &str = "aarch64-apple-darwin";
const INVESTIGATED: &[&str] = &[
    "async-graphql",
    "clap",
    "openssl",
    "clippy",
    "hyper",
    "serde",
];

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DeclaredDep {
    name: String,
    req: String,
    features: Vec<String>,
    optional: bool,
    default_features: bool,
    target: Option<String>,
    kind: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RuntimeDep {
    actual: String,
    req: String,
    features: Vec<String>,
    optional: bool,
    default_features: bool,
    target: Option<String>,
    kind: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DeclaredSignature {
    deps: Vec<DeclaredDep>,
    features: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeSignature {
    deps: Vec<RuntimeDep>,
    features: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EdgeReadSignature {
    deps: Vec<EdgeReadDep>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct EdgeReadDep {
    actual: String,
    req: String,
    optional: bool,
    kind: String,
}

#[derive(Clone, Debug)]
struct CrateLedger {
    name: String,
    unavailable: Option<String>,
    non_yanked_versions: usize,
    current_blocks: usize,
    corrected_blocks: usize,
    edge_read_blocks: usize,
    real_breaks: usize,
    projection_artifact_breaks: usize,
    read_set_candidate_breaks: usize,
    breaks: Vec<BreakLedger>,
}

#[derive(Clone, Debug)]
struct BreakLedger {
    from: Version,
    to: Version,
    classification: BreakClass,
    fields: Vec<String>,
    rule: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BreakClass {
    Real,
    ProjectionArtifact,
    ReadSetCandidate,
}

impl BreakClass {
    fn label(self) -> &'static str {
        match self {
            Self::Real => "REAL CHURN",
            Self::ProjectionArtifact => "ARTIFACT",
            Self::ReadSetCandidate => "READ-SET CANDIDATE",
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "rodin_survey=info".to_owned()),
        )
        .init();

    let cache = workspace_cache_dir()?;
    let ledgers = INVESTIGATED
        .iter()
        .map(|name| investigate_crate(&cache, name))
        .collect::<Result<Vec<_>>>()?;
    let report = render_report(&ledgers);
    let path = workspace_root()?.join("docs/design/rodin-block-forensics.md");
    fs::write(&path, report).wrap_err_with(|| format!("writing {}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

fn workspace_root() -> Result<std::path::PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| eyre::eyre!("cannot find workspace root from manifest dir"))
}

fn investigate_crate(cache: &Path, name: &str) -> Result<CrateLedger> {
    let relative = index_path_for(name);
    let Some(content) = read_cached_index_file(cache, &relative)? else {
        return Ok(CrateLedger {
            name: name.to_owned(),
            unavailable: Some(format!(
                "missing cached sparse-index row {}",
                relative.display()
            )),
            non_yanked_versions: 0,
            current_blocks: 0,
            corrected_blocks: 0,
            edge_read_blocks: 0,
            real_breaks: 0,
            projection_artifact_breaks: 0,
            read_set_candidate_breaks: 0,
            breaks: Vec::new(),
        });
    };
    let entries = parse_entries(&content, name)?;
    let crate_data = CrateData {
        name: name.to_owned(),
        entries,
    };
    let versions = sorted_non_yanked_entries(&crate_data)?;
    let declared = versions
        .iter()
        .map(|(_, entry)| declared_signature(entry))
        .collect::<Vec<_>>();
    let corrected = versions
        .iter()
        .map(|(_, entry)| runtime_signature(entry))
        .collect::<Vec<_>>();
    let edge_read = versions
        .iter()
        .map(|(_, entry)| edge_read_signature(entry))
        .collect::<Vec<_>>();

    let mut breaks = Vec::new();
    for ix in 1..versions.len() {
        if declared[ix - 1] == declared[ix] {
            continue;
        }
        let classification = if corrected[ix - 1] == corrected[ix] {
            BreakClass::ProjectionArtifact
        } else if edge_read[ix - 1] == edge_read[ix] {
            BreakClass::ReadSetCandidate
        } else {
            BreakClass::Real
        };
        let fields = match classification {
            BreakClass::ProjectionArtifact => {
                projection_artifact_fields(versions[ix - 1].1, versions[ix].1)
            }
            BreakClass::ReadSetCandidate => {
                read_set_candidate_fields(versions[ix - 1].1, versions[ix].1)
            }
            BreakClass::Real => real_fields(&corrected[ix - 1], &corrected[ix]),
        };
        breaks.push(BreakLedger {
            from: versions[ix - 1].0.clone(),
            to: versions[ix].0.clone(),
            classification,
            fields,
            rule: rule_for(classification).to_owned(),
        });
    }

    Ok(CrateLedger {
        name: name.to_owned(),
        unavailable: None,
        non_yanked_versions: versions.len(),
        current_blocks: count_blocks(&declared),
        corrected_blocks: count_blocks(&corrected),
        edge_read_blocks: count_blocks(&edge_read),
        real_breaks: breaks
            .iter()
            .filter(|entry| entry.classification == BreakClass::Real)
            .count(),
        projection_artifact_breaks: breaks
            .iter()
            .filter(|entry| entry.classification == BreakClass::ProjectionArtifact)
            .count(),
        read_set_candidate_breaks: breaks
            .iter()
            .filter(|entry| entry.classification == BreakClass::ReadSetCandidate)
            .count(),
        breaks,
    })
}

fn declared_signature(entry: &IndexEntry) -> DeclaredSignature {
    let mut deps = entry
        .deps
        .iter()
        .map(|dep| {
            let mut features = dep.features.clone();
            features.sort();
            DeclaredDep {
                name: dep.name.clone(),
                req: dep.req.clone(),
                features,
                optional: dep.optional,
                default_features: dep.default_features,
                target: dep.target.clone(),
                kind: dep.kind.clone(),
            }
        })
        .collect::<Vec<_>>();
    deps.sort();
    DeclaredSignature {
        deps,
        features: feature_signature(entry),
    }
}

fn runtime_signature(entry: &IndexEntry) -> RuntimeSignature {
    let mut deps = entry
        .deps
        .iter()
        .filter(|dep| dep.kind != "dev")
        .filter(|dep| dep.target.as_deref().is_none_or(target_matches_round))
        .map(|dep| {
            let mut features = dep.features.clone();
            features.sort();
            RuntimeDep {
                actual: actual_dep_name(dep),
                req: dep.req.clone(),
                features,
                optional: dep.optional,
                default_features: dep.default_features,
                target: dep.target.clone(),
                kind: dep.kind.clone(),
            }
        })
        .collect::<Vec<_>>();
    deps.sort();
    RuntimeSignature {
        deps,
        features: feature_signature(entry),
    }
}

fn edge_read_signature(entry: &IndexEntry) -> EdgeReadSignature {
    let mut deps = entry
        .deps
        .iter()
        .filter(|dep| dep.kind != "dev")
        .filter(|dep| dep.target.as_deref().is_none_or(target_matches_round))
        .map(|dep| EdgeReadDep {
            actual: actual_dep_name(dep),
            req: dep.req.clone(),
            optional: dep.optional,
            kind: dep.kind.clone(),
        })
        .collect::<Vec<_>>();
    deps.sort();
    EdgeReadSignature { deps }
}

fn feature_signature(entry: &IndexEntry) -> BTreeMap<String, Vec<String>> {
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

fn count_blocks<T: Eq>(signatures: &[T]) -> usize {
    let mut blocks = 0;
    let mut previous = None;
    for signature in signatures {
        if previous != Some(signature) {
            blocks += 1;
            previous = Some(signature);
        }
    }
    blocks
}

fn projection_artifact_fields(left: &IndexEntry, right: &IndexEntry) -> Vec<String> {
    let mut fields = BTreeSet::new();
    for dep in removed_deps(left, right)
        .into_iter()
        .chain(removed_deps(right, left))
    {
        if dep.kind == "dev" {
            fields.insert(format!("deps[{}, kind=dev]", actual_dep_name(dep)));
        } else if dep
            .target
            .as_deref()
            .is_some_and(|target| !target_matches_round(target))
        {
            fields.insert(format!(
                "deps[{}, target={}]",
                actual_dep_name(dep),
                dep.target.as_deref().unwrap_or("<none>")
            ));
        }
    }
    if fields.is_empty() {
        fields.insert("projection-only metadata difference".to_owned());
    }
    fields.into_iter().collect()
}

fn read_set_candidate_fields(left: &IndexEntry, right: &IndexEntry) -> Vec<String> {
    let mut fields = BTreeSet::new();
    let left_runtime = runtime_signature(left);
    let right_runtime = runtime_signature(right);
    if left_runtime.deps != right_runtime.deps {
        fields.insert(
            "dep features/default_features/target changed without edge req change".to_owned(),
        );
    }
    if left_runtime.features != right_runtime.features {
        fields.insert("features/features2 map changed outside observed edge read-set".to_owned());
    }
    if fields.is_empty() {
        fields.insert("unconsumed declared metadata changed".to_owned());
    }
    fields.into_iter().collect()
}

fn real_fields(left: &RuntimeSignature, right: &RuntimeSignature) -> Vec<String> {
    let mut fields = BTreeSet::new();
    let left_deps = dep_map(&left.deps);
    let right_deps = dep_map(&right.deps);
    for key in left_deps.keys().chain(right_deps.keys()) {
        match (left_deps.get(key), right_deps.get(key)) {
            (Some(left), Some(right)) if left != right => {
                if left.req != right.req {
                    fields.insert(format!("deps[{key}].req: {} -> {}", left.req, right.req));
                }
                if left.optional != right.optional {
                    fields.insert(format!(
                        "deps[{key}].optional: {} -> {}",
                        left.optional, right.optional
                    ));
                }
                if left.kind != right.kind {
                    fields.insert(format!("deps[{key}].kind: {} -> {}", left.kind, right.kind));
                }
                if left.features != right.features {
                    fields.insert(format!("deps[{key}].features changed"));
                }
                if left.default_features != right.default_features {
                    fields.insert(format!("deps[{key}].default_features changed"));
                }
                if left.target != right.target {
                    fields.insert(format!("deps[{key}].target changed"));
                }
            }
            (Some(_), None) => {
                fields.insert(format!("deps[{key}]: removed"));
            }
            (None, Some(_)) => {
                fields.insert(format!("deps[{key}]: added"));
            }
            _ => {}
        }
    }
    if left.features != right.features {
        fields.insert("features/features2 map changed".to_owned());
    }
    if fields.is_empty() {
        fields.insert("runtime metadata changed".to_owned());
    }
    fields.into_iter().take(4).collect()
}

fn dep_map(deps: &[RuntimeDep]) -> BTreeMap<String, &RuntimeDep> {
    let mut out = BTreeMap::new();
    for dep in deps {
        let mut key = dep.actual.clone();
        let mut ix = 2;
        while out.contains_key(&key) {
            key = format!("{}#{ix}", dep.actual);
            ix += 1;
        }
        out.insert(key, dep);
    }
    out
}

fn removed_deps<'a>(left: &'a IndexEntry, right: &'a IndexEntry) -> Vec<&'a IndexDependency> {
    let right_keys = left_right_keys(right);
    left.deps
        .iter()
        .filter(|dep| !right_keys.contains(&dep_key(dep)))
        .collect()
}

fn left_right_keys(entry: &IndexEntry) -> BTreeSet<String> {
    entry.deps.iter().map(dep_key).collect()
}

fn dep_key(dep: &IndexDependency) -> String {
    format!(
        "{}|{}|{:?}|{}|{}|{:?}|{}",
        actual_dep_name(dep),
        dep.req,
        sorted_values(&dep.features),
        dep.optional,
        dep.default_features,
        dep.target,
        dep.kind
    )
}

fn sorted_values(values: &[String]) -> Vec<String> {
    let mut out = values.to_vec();
    out.sort();
    out
}

fn actual_dep_name(dep: &IndexDependency) -> String {
    dep.package
        .as_ref()
        .unwrap_or(&dep.name)
        .to_ascii_lowercase()
}

fn rule_for(classification: BreakClass) -> &'static str {
    match classification {
        BreakClass::Real => "resolver-relevant runtime metadata changed",
        BreakClass::ProjectionArtifact => {
            "fixed-target runtime projection ignores non-workspace dev deps and target-false deps"
        }
        BreakClass::ReadSetCandidate => {
            "observed edge read-set would not consume this field; not a global normalization"
        }
    }
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

fn render_report(ledgers: &[CrateLedger]) -> String {
    let mut report = String::new();
    report.push_str("# Rodin record-level block forensics\n\n");
    report.push_str("Generated by `cargo run -p rodin-survey --bin rodin-block-forensics`. Rows are read from `target/rodin-survey-cache` with an offline fallback to Cargo's local sparse-index cache. No network is required or used by this binary.\n\n");
    report.push_str("## Method\n\n");
    report.push_str("- Current blocks reproduce the agenda C declared whole-manifest signature: dependency rows by name/req/features/optional/default_features/target/kind plus merged features/features2.\n");
    report.push_str("- Corrected fixed-target blocks remove only unambiguous projection artifacts for the product runtime target `aarch64-apple-darwin`: non-workspace dev dependencies and target predicates that evaluate false.\n");
    report.push_str("- Read-set candidates are breaks that would collapse if only the observed edge fields from Round 10 were consumed (`actual`, `kind`, `optional`, `req`). They are reported separately, not applied as a global normalization.\n\n");

    render_summary(&mut report, ledgers);
    for ledger in ledgers {
        render_crate(&mut report, ledger);
    }
    render_verdict(&mut report, ledgers);
    report
}

fn render_summary(report: &mut String, ledgers: &[CrateLedger]) {
    report.push_str("## Summary\n\n");
    report.push_str("| crate | status | non-yanked | current blocks | corrected blocks | current versions/block | corrected versions/block | real | projection artifact | read-set candidate |\n");
    report.push_str("|---|---|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    for ledger in ledgers {
        report.push_str("| ");
        report.push_str(&ledger.name);
        report.push_str(" | ");
        if let Some(reason) = &ledger.unavailable {
            report.push_str(reason);
            report.push_str(" | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |\n");
            continue;
        }
        report.push_str("available | ");
        report.push_str(&ledger.non_yanked_versions.to_string());
        report.push_str(" | ");
        report.push_str(&ledger.current_blocks.to_string());
        report.push_str(" | ");
        report.push_str(&ledger.corrected_blocks.to_string());
        report.push_str(" | ");
        report.push_str(&decimal(ratio(
            ledger.non_yanked_versions,
            ledger.current_blocks,
        )));
        report.push_str(" | ");
        report.push_str(&decimal(ratio(
            ledger.non_yanked_versions,
            ledger.corrected_blocks,
        )));
        report.push_str(" | ");
        report.push_str(&ledger.real_breaks.to_string());
        report.push_str(" | ");
        report.push_str(&ledger.projection_artifact_breaks.to_string());
        report.push_str(" | ");
        report.push_str(&ledger.read_set_candidate_breaks.to_string());
        report.push_str(" |\n");
    }
    report.push('\n');
}

fn render_crate(report: &mut String, ledger: &CrateLedger) {
    let _ = writeln!(report, "## {}\n", ledger.name);
    if let Some(reason) = &ledger.unavailable {
        let _ = writeln!(
            report,
            "STOP: {reason}. This crate cannot be classified record-by-record from the offline rows present in this checkout.\n"
        );
        return;
    }
    report.push_str("| from | to | classification | fields | normalization rule |\n");
    report.push_str("|---|---|---|---|---|\n");
    for entry in &ledger.breaks {
        let _ = writeln!(
            report,
            "| {} | {} | {} | {} | {} |",
            entry.from,
            entry.to,
            entry.classification.label(),
            escape_table(&entry.fields.join("<br>")),
            entry.rule
        );
    }
    report.push('\n');
}

fn render_verdict(report: &mut String, ledgers: &[CrateLedger]) {
    let available = ledgers
        .iter()
        .filter(|ledger| ledger.unavailable.is_none())
        .collect::<Vec<_>>();
    let versions: usize = available
        .iter()
        .map(|ledger| ledger.non_yanked_versions)
        .sum();
    let current_blocks: usize = available.iter().map(|ledger| ledger.current_blocks).sum();
    let corrected_blocks: usize = available.iter().map(|ledger| ledger.corrected_blocks).sum();
    let edge_read_blocks: usize = available.iter().map(|ledger| ledger.edge_read_blocks).sum();
    let real: usize = available.iter().map(|ledger| ledger.real_breaks).sum();
    let artifacts: usize = available
        .iter()
        .map(|ledger| ledger.projection_artifact_breaks)
        .sum();
    let candidates: usize = available
        .iter()
        .map(|ledger| ledger.read_set_candidate_breaks)
        .sum();
    report.push_str("## Verdict\n\n");
    let _ = writeln!(
        report,
        "Across the {} investigated crates with local rows, {} non-yanked versions currently form {} declared blocks ({} versions/block). The fixed-target runtime normalization forms {} blocks ({} versions/block). The projection artifact split is {} breaks versus {} real breaks; another {} breaks are observed-read-set candidates but are not globally normalized.",
        available.len(),
        versions,
        current_blocks,
        decimal(ratio(versions, current_blocks)),
        corrected_blocks,
        decimal(ratio(versions, corrected_blocks)),
        artifacts,
        real,
        candidates
    );
    let _ = writeln!(
        report,
        "\nIf the Round 10 edge-read-set rule were applied as a diagnostic only, these crates would form {} edge-read blocks ({} versions/block). That is an upper-bound diagnostic for selected edges, not a safe whole-manifest policy.",
        edge_read_blocks,
        decimal(ratio(versions, edge_read_blocks))
    );
    report.push_str("\nNo honest overall 400-crate corrected number is reported here because the fresh worktree does not contain the full `target/rodin-survey-cache` used to generate `rodin-c-evidence.md`, and `async-graphql` is absent from both the survey cache and Cargo's local sparse-index cache.\n");
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
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

fn escape_table(value: &str) -> String {
    value.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_target_runtime_signature_ignores_dev_and_false_target_deps() {
        let base = entry(
            "1.0.0",
            vec![
                dep("core", "^1", "normal", None),
                dep("try", "^1", "dev", None),
            ],
        );
        let changed = entry(
            "1.0.1",
            vec![
                dep("core", "^1", "normal", None),
                dep("try", "^2", "dev", None),
                dep("winapi", "^0.3", "normal", Some("cfg(windows)")),
            ],
        );

        assert_ne!(declared_signature(&base), declared_signature(&changed));
        assert_eq!(runtime_signature(&base), runtime_signature(&changed));
    }

    fn entry(vers: &str, deps: Vec<IndexDependency>) -> IndexEntry {
        IndexEntry {
            name: "demo".to_owned(),
            vers: vers.to_owned(),
            deps,
            features: BTreeMap::new(),
            features2: None,
            yanked: false,
        }
    }

    fn dep(name: &str, req: &str, kind: &str, target: Option<&str>) -> IndexDependency {
        IndexDependency {
            name: name.to_owned(),
            req: req.to_owned(),
            features: Vec::new(),
            optional: false,
            default_features: true,
            target: target.map(str::to_owned),
            kind: kind.to_owned(),
            registry: None,
            package: None,
        }
    }
}
