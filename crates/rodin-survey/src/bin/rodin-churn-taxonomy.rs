//! Field-by-field classification of REAL block-breaks (see
//! `docs/design/rodin-block-forensics.md` for the ARTIFACT/REAL/READ-SET split this builds on).
//!
//! For every REAL break in the investigated crates, this binary asks *what kind* of
//! change caused it: a requirement widening/narrowing/incompatible bump, a dependency
//! add/remove, a feature-map-only edit, or some mix — and how many distinct dependency
//! edges the break actually touches.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use eyre::{Context, Result};
use rodin_survey::{
    IndexEntry, actual_dep_name, index_path_for, parse_entries, read_cached_index_file,
    sorted_non_yanked_entries, workspace_cache_dir,
};
use semver::{Version, VersionReq};

const ROUND_TARGET_TRIPLE: &str = "aarch64-apple-darwin";
const INVESTIGATED: &[&str] = &[
    "async-graphql",
    "clap",
    "openssl",
    "clippy",
    "hyper",
    "serde",
];

/// A version requirement change, classified by comparing the accepted-version sets of
/// `from` and `to` over a probe set of versions built from every version-like token that
/// appears in either requirement string (plus a few sentinels for open-ended reqs).
///
/// This is a **sampling approximation**, not a proof: two requirements could theoretically
/// agree on every probe and still differ somewhere the probes miss. The probe set is dense
/// enough (every literal number in both strings, +-1 at each semver component, plus 0.0.0
/// and a far sentinel) that in practice it separates every case seen in real crates.io data.
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
enum ReqChangeKind {
    Widened,
    Narrowed,
    IncompatibleBump,
    Reshaped,
}

impl ReqChangeKind {
    fn label(self) -> &'static str {
        match self {
            Self::Widened => "req-widened",
            Self::Narrowed => "req-narrowed",
            Self::IncompatibleBump => "req-bumped-incompatibly",
            Self::Reshaped => "req-reshaped (same accepted set)",
        }
    }
}

#[derive(Clone, Debug)]
struct ReqChange {
    dep: String,
    from: String,
    to: String,
    kind: ReqChangeKind,
}

#[derive(Clone, Debug)]
struct DepAdded {
    dep: String,
    optional: bool,
}

#[derive(Clone, Debug)]
struct DepRemoved {
    dep: String,
    optional: bool,
}

#[derive(Clone, Debug)]
struct TaxonomyBreak {
    crate_name: String,
    from: Version,
    to: Version,
    req_changes: Vec<ReqChange>,
    deps_added: Vec<DepAdded>,
    deps_removed: Vec<DepRemoved>,
    feature_map_changed: bool,
    other_field_changed: bool,
}

impl TaxonomyBreak {
    fn touched_edges(&self) -> BTreeSet<String> {
        let mut edges = BTreeSet::new();
        for change in &self.req_changes {
            edges.insert(change.dep.clone());
        }
        for added in &self.deps_added {
            edges.insert(added.dep.clone());
        }
        for removed in &self.deps_removed {
            edges.insert(removed.dep.clone());
        }
        edges
    }

    fn is_pure_feature_map_only(&self) -> bool {
        self.feature_map_changed
            && self.req_changes.is_empty()
            && self.deps_added.is_empty()
            && self.deps_removed.is_empty()
            && !self.other_field_changed
    }

    fn quote(&self) -> String {
        let mut parts = Vec::new();
        for change in &self.req_changes {
            parts.push(format!(
                "deps[{}].req: {} -> {}",
                change.dep, change.from, change.to
            ));
        }
        for added in &self.deps_added {
            parts.push(format!(
                "deps[{}]: added{}",
                added.dep,
                if added.optional { " (optional)" } else { "" }
            ));
        }
        for removed in &self.deps_removed {
            parts.push(format!(
                "deps[{}]: removed{}",
                removed.dep,
                if removed.optional { " (optional)" } else { "" }
            ));
        }
        if self.feature_map_changed {
            parts.push("features/features2 map changed".to_owned());
        }
        format!(
            "{} {} -> {}: {}",
            self.crate_name,
            self.from,
            self.to,
            parts.join("; ")
        )
    }
}

#[derive(Clone, Debug, Default)]
struct CrateTaxonomy {
    non_yanked_versions: usize,
    real_breaks: usize,
    corrected_blocks: usize,
    edge_read_blocks: usize,
    edge_present_versions: usize,
    edge_present_blocks: usize,
    single_edge_breaks: usize,
    edges_touched_total: usize,
    req_widened: usize,
    req_narrowed: usize,
    req_incompatible: usize,
    req_reshaped: usize,
    dep_added_required: usize,
    dep_added_optional: usize,
    dep_removed_required: usize,
    dep_removed_optional: usize,
    feature_map_cooccurring: usize,
    pure_feature_map_only: usize,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "rodin_survey=info".to_owned()),
        )
        .init();

    let cache = workspace_cache_dir()?;
    let mut breaks = Vec::new();
    let mut per_crate = BTreeMap::new();

    for name in INVESTIGATED {
        let Some((taxonomy, crate_breaks)) = investigate_crate(&cache, name)? else {
            continue;
        };
        per_crate.insert((*name).to_owned(), taxonomy);
        breaks.extend(crate_breaks);
    }

    let report = render_report(&per_crate, &breaks);
    let path = workspace_root()?.join("docs/design/rodin-churn-taxonomy.md");
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

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RuntimeDep {
    actual: String,
    req: String,
    features: Vec<String>,
    optional: bool,
    default_features: bool,
    target: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeSignature {
    deps: BTreeMap<String, RuntimeDep>,
    features: BTreeMap<String, Vec<String>>,
}

fn runtime_signature(entry: &IndexEntry) -> RuntimeSignature {
    let mut deps = BTreeMap::new();
    for dep in &entry.deps {
        if dep.kind == "dev" {
            continue;
        }
        if dep
            .target
            .as_deref()
            .is_some_and(|target| !target_matches_round(target))
        {
            continue;
        }
        let mut features = dep.features.clone();
        features.sort();
        let key = dedupe_key(&deps, &actual_dep_name(dep));
        deps.insert(
            key,
            RuntimeDep {
                actual: actual_dep_name(dep),
                req: dep.req.clone(),
                features,
                optional: dep.optional,
                default_features: dep.default_features,
                target: dep.target.clone(),
            },
        );
    }
    RuntimeSignature {
        deps,
        features: feature_signature(entry),
    }
}

fn dedupe_key(deps: &BTreeMap<String, RuntimeDep>, name: &str) -> String {
    let mut key = name.to_owned();
    let mut ix = 2;
    while deps.contains_key(&key) {
        key = format!("{name}#{ix}");
        ix += 1;
    }
    key
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

/// Per-edge stability: for every dependency name that ever appears (in the fixed-target
/// runtime signature) across a crate's version history, count how many consecutive
/// versions its own (req, features, optional, default_features, target) state holds
/// steady before that ONE edge changes — as opposed to the whole-row signature.
///
/// Returns `(versions covered by some edge's stable run, number of stable runs)`. The
/// ratio `versions / runs` is the average per-edge stability length, directly comparable
/// to the whole-row `non_yanked_versions / corrected_blocks` ratio.
fn per_edge_stability(runtime: &[RuntimeSignature]) -> (usize, usize) {
    let mut edge_names: BTreeSet<&str> = BTreeSet::new();
    for signature in runtime {
        edge_names.extend(signature.deps.keys().map(String::as_str));
    }

    let mut total_versions = 0usize;
    let mut total_blocks = 0usize;
    for edge in edge_names {
        let mut previous: Option<&RuntimeDep> = None;
        for signature in runtime {
            let current = signature.deps.get(edge);
            if let Some(dep) = current {
                total_versions += 1;
                if previous != current {
                    total_blocks += 1;
                }
                previous = Some(dep);
            } else {
                previous = None;
            }
        }
    }
    (total_versions, total_blocks)
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

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct EdgeReadDep {
    actual: String,
    req: String,
    optional: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EdgeReadSignature {
    deps: Vec<EdgeReadDep>,
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
        })
        .collect::<Vec<_>>();
    deps.sort();
    EdgeReadSignature { deps }
}

fn investigate_crate(
    cache: &Path,
    name: &str,
) -> Result<Option<(CrateTaxonomy, Vec<TaxonomyBreak>)>> {
    let relative = index_path_for(name);
    let Some(content) = read_cached_index_file(cache, &relative)? else {
        tracing::warn!(%name, "no cached sparse-index row; skipping");
        return Ok(None);
    };
    let entries = parse_entries(&content, name)?;
    let crate_data = rodin_survey::CrateData {
        name: name.to_owned(),
        entries,
    };
    let versions = sorted_non_yanked_entries(&crate_data)?;
    let runtime: Vec<_> = versions
        .iter()
        .map(|(_, entry)| runtime_signature(entry))
        .collect();
    let edge_read: Vec<_> = versions
        .iter()
        .map(|(_, entry)| edge_read_signature(entry))
        .collect();

    let (edge_present_versions, edge_present_blocks) = per_edge_stability(&runtime);

    let mut taxonomy = CrateTaxonomy {
        non_yanked_versions: versions.len(),
        corrected_blocks: count_blocks(&runtime),
        edge_read_blocks: count_blocks(&edge_read),
        edge_present_versions,
        edge_present_blocks,
        ..Default::default()
    };
    let mut breaks = Vec::new();

    for ix in 1..versions.len() {
        if runtime[ix - 1] == runtime[ix] {
            continue;
        }
        // Mirror rodin-block-forensics: this is only a REAL break if the observed
        // edge-read-set also disagrees. If the edge-read-set agrees, it's a READ-SET
        // CANDIDATE (feature-map-only or dep-shape-only), not counted here.
        if edge_read[ix - 1] == edge_read[ix] {
            continue;
        }

        let left = &runtime[ix - 1];
        let right = &runtime[ix];
        let mut req_changes = Vec::new();
        let mut deps_added = Vec::new();
        let mut deps_removed = Vec::new();
        let mut other_field_changed = false;

        for key in left
            .deps
            .keys()
            .chain(right.deps.keys())
            .collect::<BTreeSet<_>>()
        {
            match (left.deps.get(key), right.deps.get(key)) {
                (Some(before), Some(after)) if before != after => {
                    if before.req != after.req {
                        req_changes.push(ReqChange {
                            dep: before.actual.clone(),
                            from: before.req.clone(),
                            to: after.req.clone(),
                            kind: classify_req_change(&before.req, &after.req),
                        });
                    }
                    if before.optional != after.optional
                        || before.default_features != after.default_features
                        || before.target != after.target
                        || before.features != after.features
                    {
                        other_field_changed = true;
                    }
                }
                (Some(before), None) => {
                    deps_removed.push(DepRemoved {
                        dep: before.actual.clone(),
                        optional: before.optional,
                    });
                }
                (None, Some(after)) => {
                    deps_added.push(DepAdded {
                        dep: after.actual.clone(),
                        optional: after.optional,
                    });
                }
                _ => {}
            }
        }

        let feature_map_changed = left.features != right.features;

        let entry = TaxonomyBreak {
            crate_name: name.to_owned(),
            from: versions[ix - 1].0.clone(),
            to: versions[ix].0.clone(),
            req_changes,
            deps_added,
            deps_removed,
            feature_map_changed,
            other_field_changed,
        };

        taxonomy.real_breaks += 1;
        let edges = entry.touched_edges();
        taxonomy.edges_touched_total += edges.len().max(1);
        if edges.len() == 1 {
            taxonomy.single_edge_breaks += 1;
        }
        for change in &entry.req_changes {
            match change.kind {
                ReqChangeKind::Widened => taxonomy.req_widened += 1,
                ReqChangeKind::Narrowed => taxonomy.req_narrowed += 1,
                ReqChangeKind::IncompatibleBump => taxonomy.req_incompatible += 1,
                ReqChangeKind::Reshaped => taxonomy.req_reshaped += 1,
            }
        }
        for added in &entry.deps_added {
            if added.optional {
                taxonomy.dep_added_optional += 1;
            } else {
                taxonomy.dep_added_required += 1;
            }
        }
        for removed in &entry.deps_removed {
            if removed.optional {
                taxonomy.dep_removed_optional += 1;
            } else {
                taxonomy.dep_removed_required += 1;
            }
        }
        if entry.feature_map_changed {
            taxonomy.feature_map_cooccurring += 1;
        }
        if entry.is_pure_feature_map_only() {
            taxonomy.pure_feature_map_only += 1;
        }

        breaks.push(entry);
    }

    Ok(Some((taxonomy, breaks)))
}

/// Classify a requirement change by sampling a probe set of versions built from every
/// version-like token in `from`/`to`, plus perturbations and sentinels, then comparing
/// which probes each requirement accepts.
fn classify_req_change(from: &str, to: &str) -> ReqChangeKind {
    let probes = probe_set(from, to);
    let Ok(from_req) = VersionReq::parse(from) else {
        return ReqChangeKind::IncompatibleBump;
    };
    let Ok(to_req) = VersionReq::parse(to) else {
        return ReqChangeKind::IncompatibleBump;
    };

    let from_accepts: BTreeSet<&Version> = probes.iter().filter(|v| from_req.matches(v)).collect();
    let to_accepts: BTreeSet<&Version> = probes.iter().filter(|v| to_req.matches(v)).collect();

    if from_accepts == to_accepts {
        ReqChangeKind::Reshaped
    } else if from_accepts.is_subset(&to_accepts) {
        ReqChangeKind::Widened
    } else if to_accepts.is_subset(&from_accepts) {
        ReqChangeKind::Narrowed
    } else {
        ReqChangeKind::IncompatibleBump
    }
}

fn probe_set(from: &str, to: &str) -> Vec<Version> {
    let mut probes = BTreeSet::new();
    probes.insert(Version::new(0, 0, 0));
    probes.insert(Version::new(0, 0, 1));
    probes.insert(Version::new(9999, 0, 0));

    for token in version_tokens(from).into_iter().chain(version_tokens(to)) {
        let (major, minor, patch) = token;
        for (dm, dn, dp) in [
            (0i64, 0i64, 0i64),
            (0, 0, 1),
            (0, 0, -1),
            (0, 1, 0),
            (0, -1, 0),
            (1, 0, 0),
            (-1, 0, 0),
        ] {
            let m = major as i64 + dm;
            let n = minor as i64 + dn;
            let p = patch as i64 + dp;
            if m < 0 || n < 0 || p < 0 {
                continue;
            }
            probes.insert(Version::new(m as u64, n as u64, p as u64));
        }
    }
    probes.into_iter().collect()
}

/// Every `major[.minor[.patch]]` token in a requirement string, normalized to a full triple
/// (missing components default to 0). Ignores pre-release/build metadata for probe purposes.
fn version_tokens(req: &str) -> Vec<(u64, u64, u64)> {
    let mut tokens = Vec::new();
    let bytes = req.as_bytes();
    let mut ix = 0;
    while ix < bytes.len() {
        if bytes[ix].is_ascii_digit() {
            let start = ix;
            while ix < bytes.len() && (bytes[ix].is_ascii_digit() || bytes[ix] == b'.') {
                ix += 1;
            }
            let token = &req[start..ix];
            let mut parts = token.split('.');
            let major = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
            let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
            let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
            tokens.push((major, minor, patch));
        } else {
            ix += 1;
        }
    }
    tokens
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

fn render_report(per_crate: &BTreeMap<String, CrateTaxonomy>, breaks: &[TaxonomyBreak]) -> String {
    let mut report = String::new();
    report.push_str("# Rodin churn taxonomy\n\n");
    report.push_str("Generated by `cargo run -p rodin-survey --bin rodin-churn-taxonomy`. Classifies every REAL block-break identified by `rodin-block-forensics` (see `docs/design/rodin-block-forensics.md`) by change kind: requirement widening/narrowing/incompatible-bump, dependency add/remove (optional or not), feature-map co-occurrence, and how many distinct dependency edges a break actually touches.\n\n");
    report.push_str("## Method\n\n");
    report.push_str("- A REAL break is one where the fixed-target runtime signature (deps minus dev/false-target, plus merged features/features2) differs AND the narrower Round-10 edge-read-set (actual/req/optional per non-dev/true-target dep) also differs — i.e. exactly the REAL CHURN rows from the forensics pass.\n");
    report.push_str("- Requirement changes are classified by sampling a probe set of versions (every version-like token in either requirement string, perturbed by +-1 at each semver component, plus 0.0.0/9999.0.0 sentinels) and comparing which probes each requirement string accepts via `semver::VersionReq::matches`. This is a **sampling approximation**: dense enough to separate every case observed in this dataset, but not a proof of interval containment.\n");
    report.push_str("  - **req-widened**: every probe the old requirement accepted, the new one also accepts, plus at least one more.\n");
    report.push_str(
        "  - **req-narrowed**: the reverse — new accepts a strict subset of what old accepted.\n",
    );
    report.push_str("  - **req-bumped-incompatibly**: neither accepts a subset of the other (some probes flip both ways) — the classic `^1 -> ^2` shape.\n");
    report.push_str("  - **req-reshaped**: the two requirement strings accept the exact same probe set (e.g. `^0.6.3 -> ~0.6.3` for a 0.x version, or `^1.4.0 -> >=1.4.0, <2.0.0`) — syntax changed, resolver-visible set did not.\n");
    report.push_str("- A break's *touched edges* is the set of dependency names it adds, removes, or req-changes. Feature-map-only co-occurrence is tracked separately and does not count as touching an edge.\n\n");

    render_summary(&mut report, per_crate);
    render_locality(&mut report, per_crate);
    render_examples(&mut report, breaks);
    render_discussion(&mut report, per_crate);
    report
}

fn render_discussion(report: &mut String, per_crate: &BTreeMap<String, CrateTaxonomy>) {
    report.push_str("## Discussion\n\n");

    report.push_str("### async-graphql: what is it actually doing\n\n");
    if let Some(tax) = per_crate.get("async-graphql") {
        let _ = writeln!(
            report,
            "async-graphql was the block-forensics STOP (missing cached row); with its sparse-index row fetched and cached (see `crates/rodin-survey/fixtures/sparse-index/async-graphql.jsonl`), it classifies as {} real breaks over {} non-yanked versions — confirming the \"350 blocks, worst case\" figure from the forensics pass (349 corrected blocks here). Two things drive it, both visible in the per-crate table above:\n\n",
            tax.real_breaks, tax.non_yanked_versions,
        );
    }
    report.push_str("1. **Workspace-split lockstep pins.** async-graphql is split into sibling crates (`async-graphql-derive`, `async-graphql-parser`, `async-graphql-value`) that are version-pinned from the main crate, usually with an exact or tight caret req that gets bumped on *every* release of the main crate, whether or not anything relevant to the dependent changed. This is why async-graphql's avg-edges/break (2.75) and single-edge ratio (0.375) are the outliers in the dataset — most other investigated crates (serde: 1.00 single-edge every time; openssl: 1.25 avg edges) have at most one or two such external pins (e.g. openssl/openssl-sys, clap/clap_derive-clap_builder). async-graphql simply has more of them, plus more that changed simultaneously.\n");
    report.push_str("2. **Continuous optional-integration churn.** async-graphql accumulates and drops *optional*, feature-gated integrations with the wider ecosystem (chrono, chrono-tz, bson, uuid, url, im, parking_lot, tempfile, and others) as it adds/removes support for external types across its lifetime. These show up as dep-added/dep-removed pairs, frequently marked optional, often co-occurring with a features/features2 map change (the crate is wiring a new dep into a new optional feature in the same release). A `graphql-parser` -> `async-graphql-parser` swap (1.10.12 -> 1.11.0, `async-graphql-parser: added` + `graphql-parser: removed` in the same version) is a rename disguised as add+remove — a real break, not an artifact, since any fact keyed to the old edge name is genuinely gone.\n\n");
    report.push_str("Both are genuine, unavoidable churn under any read-set narrowing that still tracks whole-row (or even whole-row-with-narrowed-fields) state: as long as blocking is done per row, one lockstep-pinned sibling or one new optional integration anywhere in the manifest re-shatters the block for every other, unrelated edge too.\n\n");

    report.push_str(
        "### Monotone-compatible changes: think carefully, not just \"real churn happened\"\n\n",
    );
    report.push_str("A CDCL-style solver over version regions learns facts of the shape *\"these regions are jointly UNSAT\"* — a conflict clause. The question this taxonomy exists to answer is: which of the REAL-CHURN field changes above are guaranteed to leave a previously-learned conflict clause valid for the OLD version, and which can silently invalidate it?\n\n");
    report.push_str("Treat a manifest as a constraint set over the resolver's boolean variables (one clause per dependency edge: \"if this crate-version is chosen, this dependency's requirement must hold\"). UNSAT-ness is **monotone under adding constraints**: if a constraint set `S` is unsatisfiable, any superset `S' ⊇ S` is unsatisfiable too — new constraints can only narrow the solution space, never widen it back open. That gives a real, sound asymmetry:\n\n");
    report.push_str("- **dep-added** adds a brand-new constraint edge. It can never invalidate a conflict clause that was already provable from the OLD, smaller constraint set — that clause used a subset of what's now available and remains sound. It can, and often does, introduce *new* conflicts the old version never had (this dataset counts 194 dep-added instances), but those are additive, not retroactive.\n");
    report.push_str("- **req-narrowed** shrinks the accepted-version interval for an existing edge — also constraint-tightening (fewer satisfying assignments). Same argument: a conflict clause proved when the requirement was looser remains provable when it's stricter. This dataset's dominant req-change kind (708 of ~1450 req-change instances, mostly ordinary patch/point releases: `^0.8.0 -> ^0.8.1` is a narrowing under caret semantics, not a \"bump\", because it raises the floor without moving the ceiling) is therefore monotone-safe far more often than the raw REAL-CHURN count suggests.\n");
    report.push_str("- **dep-removed** deletes a constraint edge. If a conflict clause depended on that edge (e.g. \"crate X requires Y, and Y<2 conflicts with Z\"), removing the edge can make the clause's premise vanish — the clause is no longer *derivable* from the new manifest, even though nothing else proves it false either. This is not the same as \"the fact becomes wrong\"; it means the fact's derivation is no longer licensed and must be re-checked. 94 required + 49 optional dep-removed instances in this dataset are candidates for this asymmetry.\n");
    report.push_str("- **req-widened** and **req-bumped-incompatibly** relax or shift the accepted interval — the dangerous direction. A version combination that was UNSAT under the old (tighter or differently-shaped) requirement can become SAT under the new one. Any conflict clause resting on that edge must be treated as invalidated, full stop; the sampling method above found only 28 unambiguous widenings but 709 incompatible-bump instances (interval *shifts*, e.g. `^0.5.0 -> ^0.6.0`), which are widenings-with-narrowing-elsewhere and must be treated as unsafe on the SAT side even though they also drop old floor versions.\n\n");
    report.push_str("Net: roughly the constraint-*tightening* moves (dep-added, req-narrowed) are safe to treat as extending a fact's validity range without re-derivation; constraint-*loosening* moves (dep-removed, req-widened, req-bumped-incompatibly) are not. Today's implementation draws the validity boundary at \"the read-set changed at all\", which is strictly more conservative than this — it also invalidates on every dep-added and req-narrowed instance, i.e. on the majority-safe direction. That gap, quantified per-crate above as the edge/whole-row fineness ratio (aggregate 7.29x), is the target for Part 2's research question.\n\n");
}

fn render_summary(report: &mut String, per_crate: &BTreeMap<String, CrateTaxonomy>) {
    report.push_str("## Per-crate counts\n\n");
    report.push_str("| crate | non-yanked versions | real breaks | req-widened | req-narrowed | req-incompatible-bump | req-reshaped | dep-added (required) | dep-added (optional) | dep-removed (required) | dep-removed (optional) | feature-map co-occurring |\n");
    report.push_str("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    let mut totals = CrateTaxonomy::default();
    for (name, tax) in per_crate {
        let _ = writeln!(
            report,
            "| {name} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            tax.non_yanked_versions,
            tax.real_breaks,
            tax.req_widened,
            tax.req_narrowed,
            tax.req_incompatible,
            tax.req_reshaped,
            tax.dep_added_required,
            tax.dep_added_optional,
            tax.dep_removed_required,
            tax.dep_removed_optional,
            tax.feature_map_cooccurring,
        );
        totals.real_breaks += tax.real_breaks;
        totals.req_widened += tax.req_widened;
        totals.req_narrowed += tax.req_narrowed;
        totals.req_incompatible += tax.req_incompatible;
        totals.req_reshaped += tax.req_reshaped;
        totals.dep_added_required += tax.dep_added_required;
        totals.dep_added_optional += tax.dep_added_optional;
        totals.dep_removed_required += tax.dep_removed_required;
        totals.dep_removed_optional += tax.dep_removed_optional;
        totals.feature_map_cooccurring += tax.feature_map_cooccurring;
        totals.pure_feature_map_only += tax.pure_feature_map_only;
        totals.single_edge_breaks += tax.single_edge_breaks;
        totals.edges_touched_total += tax.edges_touched_total;
    }
    let _ = writeln!(
        report,
        "| **total** | **{}** | **{}** | **{}** | **{}** | **{}** | **{}** | **{}** | **{}** | **{}** | **{}** | **{}** |",
        per_crate
            .values()
            .map(|t| t.non_yanked_versions)
            .sum::<usize>(),
        totals.real_breaks,
        totals.req_widened,
        totals.req_narrowed,
        totals.req_incompatible,
        totals.req_reshaped,
        totals.dep_added_required,
        totals.dep_added_optional,
        totals.dep_removed_required,
        totals.dep_removed_optional,
        totals.feature_map_cooccurring,
    );
    report.push('\n');
}

fn render_locality(report: &mut String, per_crate: &BTreeMap<String, CrateTaxonomy>) {
    report.push_str("## Edge locality\n\n");
    report.push_str("`single-edge` = fraction of REAL breaks that touch exactly one dependency name (add/remove/req-change). `avg edges/break` = mean distinct edges touched. `corrected blocks` and `edge-read blocks` reproduce the forensics-pass counts (whole fixed-target row vs Round-10 edge-read-set); `blocks ratio` = corrected/edge-read, i.e. how many whole-row blocks collapse into one edge-read-set block on average — this stays close to 1.0 because narrowing the *field set* barely helps when almost every version touches *some* edge's req.\n\n");
    report.push_str("| crate | single-edge breaks | total real breaks | single-edge ratio | avg edges/break | corrected blocks | edge-read blocks | blocks ratio |\n");
    report.push_str("|---|---:|---:|---:|---:|---:|---:|---:|\n");
    let mut total_single = 0usize;
    let mut total_real = 0usize;
    let mut total_edges = 0usize;
    let mut total_corrected_blocks = 0usize;
    let mut total_edge_read_blocks = 0usize;
    for (name, tax) in per_crate {
        let ratio = if tax.real_breaks == 0 {
            0.0
        } else {
            tax.single_edge_breaks as f64 / tax.real_breaks as f64
        };
        let avg_edges = if tax.real_breaks == 0 {
            0.0
        } else {
            tax.edges_touched_total as f64 / tax.real_breaks as f64
        };
        let blocks_ratio = if tax.edge_read_blocks == 0 {
            0.0
        } else {
            tax.corrected_blocks as f64 / tax.edge_read_blocks as f64
        };
        let _ = writeln!(
            report,
            "| {name} | {} | {} | {:.3} | {:.2} | {} | {} | {:.3} |",
            tax.single_edge_breaks,
            tax.real_breaks,
            ratio,
            avg_edges,
            tax.corrected_blocks,
            tax.edge_read_blocks,
            blocks_ratio,
        );
        total_single += tax.single_edge_breaks;
        total_real += tax.real_breaks;
        total_edges += tax.edges_touched_total;
        total_corrected_blocks += tax.corrected_blocks;
        total_edge_read_blocks += tax.edge_read_blocks;
    }
    let total_ratio = if total_real == 0 {
        0.0
    } else {
        total_single as f64 / total_real as f64
    };
    let total_avg_edges = if total_real == 0 {
        0.0
    } else {
        total_edges as f64 / total_real as f64
    };
    let total_blocks_ratio = if total_edge_read_blocks == 0 {
        0.0
    } else {
        total_corrected_blocks as f64 / total_edge_read_blocks as f64
    };
    let _ = writeln!(
        report,
        "| **total** | **{total_single}** | **{total_real}** | **{total_ratio:.3}** | **{total_avg_edges:.2}** | **{total_corrected_blocks}** | **{total_edge_read_blocks}** | **{total_blocks_ratio:.3}** |"
    );
    report.push('\n');

    report.push_str("### Per-edge stability vs whole-row stability — the key ratio\n\n");
    report.push_str("For every dependency name that ever appears, `edge versions/block` counts how many consecutive versions THAT ONE edge's own (req, features, optional, default_features, target) state holds steady before it changes, averaged over every edge. `whole-row versions/block` is `non-yanked versions / corrected blocks` from the forensics pass — how long the *entire manifest* stays stable. `fineness` = edge/whole-row, i.e. how much longer a single edge's fact would remain valid under per-edge tracking than it does today under whole-row blocking.\n\n");
    report.push_str("| crate | edge versions/block | whole-row versions/block | fineness (edge / whole-row) |\n");
    report.push_str("|---|---:|---:|---:|\n");
    let mut total_edge_versions = 0usize;
    let mut total_edge_blocks = 0usize;
    let mut total_whole_versions = 0usize;
    for (name, tax) in per_crate {
        let edge_vpb = if tax.edge_present_blocks == 0 {
            0.0
        } else {
            tax.edge_present_versions as f64 / tax.edge_present_blocks as f64
        };
        let whole_vpb = if tax.corrected_blocks == 0 {
            0.0
        } else {
            tax.non_yanked_versions as f64 / tax.corrected_blocks as f64
        };
        let fineness = if whole_vpb == 0.0 {
            0.0
        } else {
            edge_vpb / whole_vpb
        };
        let _ = writeln!(
            report,
            "| {name} | {edge_vpb:.2} | {whole_vpb:.2} | {fineness:.2} |"
        );
        total_edge_versions += tax.edge_present_versions;
        total_edge_blocks += tax.edge_present_blocks;
        total_whole_versions += tax.non_yanked_versions;
    }
    let total_edge_vpb = if total_edge_blocks == 0 {
        0.0
    } else {
        total_edge_versions as f64 / total_edge_blocks as f64
    };
    let total_whole_vpb = if total_corrected_blocks == 0 {
        0.0
    } else {
        total_whole_versions as f64 / total_corrected_blocks as f64
    };
    let total_fineness = if total_whole_vpb == 0.0 {
        0.0
    } else {
        total_edge_vpb / total_whole_vpb
    };
    let _ = writeln!(
        report,
        "| **total** | **{total_edge_vpb:.2}** | **{total_whole_vpb:.2}** | **{total_fineness:.2}** |"
    );
    report.push('\n');
}

fn render_examples(report: &mut String, breaks: &[TaxonomyBreak]) {
    report.push_str("## Examples\n\n");
    for kind in [
        ReqChangeKind::Widened,
        ReqChangeKind::Narrowed,
        ReqChangeKind::IncompatibleBump,
        ReqChangeKind::Reshaped,
    ] {
        render_kind_examples(report, kind.label(), breaks, |b| {
            b.req_changes.iter().any(|c| c.kind == kind)
        });
    }
    render_kind_examples(report, "dep-added", breaks, |b| !b.deps_added.is_empty());
    render_kind_examples(report, "dep-removed", breaks, |b| {
        !b.deps_removed.is_empty()
    });
    render_kind_examples(
        report,
        "feature-map co-occurring with other real changes",
        breaks,
        |b| b.feature_map_changed && !b.is_pure_feature_map_only(),
    );
    render_kind_examples(report, "single-edge breaks", breaks, |b| {
        b.touched_edges().len() == 1
    });
    render_kind_examples(report, "multi-edge breaks", breaks, |b| {
        b.touched_edges().len() > 1
    });
}

fn render_kind_examples(
    report: &mut String,
    title: &str,
    breaks: &[TaxonomyBreak],
    predicate: impl Fn(&TaxonomyBreak) -> bool,
) {
    let _ = writeln!(report, "### {title}\n");
    let matches: Vec<_> = breaks.iter().filter(|b| predicate(b)).collect();
    if matches.is_empty() {
        report.push_str("(none observed)\n\n");
        return;
    }
    for entry in matches.iter().take(5) {
        let _ = writeln!(report, "- `{}`", entry.quote());
    }
    let _ = writeln!(report, "\n{} occurrences total.\n", matches.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_patch_bump_on_zero_x_is_a_narrowing() {
        // ^0.8.0 == >=0.8.0,<0.9.0; ^0.8.1 == >=0.8.1,<0.9.0 — strictly smaller.
        assert_eq!(
            classify_req_change("^0.8.0", "^0.8.1"),
            ReqChangeKind::Narrowed
        );
    }

    #[test]
    fn caret_minor_bump_on_zero_x_is_an_incompatible_shift() {
        // ^0.5.0 == >=0.5.0,<0.6.0; ^0.6.0 == >=0.6.0,<0.7.0 — disjoint ranges.
        assert_eq!(
            classify_req_change("^0.5.0", "^0.6.0"),
            ReqChangeKind::IncompatibleBump
        );
    }

    #[test]
    fn wildcard_to_caret_is_a_narrowing() {
        assert_eq!(classify_req_change("*", "^0.6.3"), ReqChangeKind::Narrowed);
    }

    #[test]
    fn explicit_bounds_widening_matches_caret_shrink() {
        // real openssl example: bitflags ^0.2 (>=0.2.0,<0.3.0) -> >= 0.2, < 0.4 (superset).
        assert_eq!(
            classify_req_change("^0.2", ">= 0.2, < 0.4"),
            ReqChangeKind::Widened
        );
    }

    #[test]
    fn caret_rewritten_as_explicit_bounds_is_reshaped() {
        // real async-graphql example: ^1.4.0 and >=1.4.0,<2.0.0 accept the same set.
        assert_eq!(
            classify_req_change("^1.4.0", ">=1.4.0, <2.0.0"),
            ReqChangeKind::Reshaped
        );
    }

    #[test]
    fn touched_edges_counts_distinct_dependency_names_only() {
        let entry = TaxonomyBreak {
            crate_name: "demo".to_owned(),
            from: Version::new(1, 0, 0),
            to: Version::new(1, 1, 0),
            req_changes: vec![ReqChange {
                dep: "foo".to_owned(),
                from: "^1".to_owned(),
                to: "^1.1".to_owned(),
                kind: ReqChangeKind::Narrowed,
            }],
            deps_added: vec![DepAdded {
                dep: "bar".to_owned(),
                optional: false,
            }],
            deps_removed: Vec::new(),
            feature_map_changed: true,
            other_field_changed: false,
        };
        assert_eq!(entry.touched_edges().len(), 2);
        assert!(!entry.is_pure_feature_map_only());
    }
}
