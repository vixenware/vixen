use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use eyre::{Context, Result, bail, eyre};
use facet::Facet;
use rodin_facts::{PkgIx, Region, VersionSet};
use semver::{Comparator, Op, Version, VersionReq};

const INDEX_BASE: &str = "https://index.crates.io";
const USER_AGENT: &str = "rodin-survey/0.1.0 (https://github.com/bearcove/vx)";
const CLOSURE_CAP: usize = 400;
const SEEDS: &[&str] = &["axum", "tokio", "serde", "clap", "reqwest", "syn", "rand"];

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
struct IndexDependency {
    name: String,
    req: String,
    features: Vec<String>,
    optional: bool,
    default_features: bool,
    target: Option<String>,
    kind: String,
    registry: Option<String>,
    package: Option<String>,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
struct IndexEntry {
    name: String,
    vers: String,
    deps: Vec<IndexDependency>,
    features: BTreeMap<String, Vec<String>>,
    features2: Option<BTreeMap<String, Vec<String>>>,
    yanked: bool,
}

#[derive(Clone, Debug)]
struct CrateData {
    name: String,
    entries: Vec<IndexEntry>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DepSignature {
    name: String,
    req: String,
    features: Vec<String>,
    optional: bool,
    default_features: bool,
    target: Option<String>,
    kind: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct EdgeKey {
    parent: String,
    dep: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct EdgeSignature {
    req: String,
    features: Vec<String>,
    optional: bool,
    default_features: bool,
    target: Option<String>,
    kind: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct MetadataSignature {
    deps: Vec<DepSignature>,
    features: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug)]
struct CrateBlocks {
    name: String,
    non_yanked_versions: usize,
    yanked_versions: usize,
    blocks: usize,
    versions_per_block: f64,
}

#[derive(Clone, Debug)]
struct Aggregate {
    crates: usize,
    versions: usize,
    yanked_versions: usize,
    blocks: usize,
    median_versions_per_block: f64,
    p90_versions_per_block: f64,
    max_versions_per_block: f64,
    share_at_most_five_blocks: f64,
}

#[derive(Clone, Debug)]
struct BlockAggregate {
    items: usize,
    versions: usize,
    blocks: usize,
    median_versions_per_block: f64,
    p90_versions_per_block: f64,
    max_versions_per_block: f64,
    share_at_most_five_blocks: f64,
}

#[derive(Clone, Debug)]
struct EdgeBlocks {
    parent: String,
    dep: String,
    parent_versions: usize,
    present_versions: usize,
    blocks: usize,
    versions_per_block: f64,
}

#[derive(Clone, Debug)]
struct EdgeStats {
    parent: String,
    dep: String,
    parent_versions: usize,
    states: Vec<Vec<EdgeSignature>>,
    rows: usize,
    lockstep_rows: usize,
}

#[derive(Clone, Debug)]
struct LockstepEdge {
    parent: String,
    dep: String,
    rows: usize,
    present_versions: usize,
}

#[derive(Clone, Debug)]
struct LockstepFamily {
    nodes: Vec<String>,
    edges: usize,
    rows: usize,
}

#[derive(Clone, Debug)]
struct LockstepReport {
    total_edges: usize,
    lockstep_edges: usize,
    total_rows: usize,
    lockstep_rows: usize,
    families: Vec<LockstepFamily>,
    edges: Vec<LockstepEdge>,
}

#[derive(Clone, Debug)]
struct Round2 {
    lockstep: LockstepReport,
    per_edge_blocks: Vec<EdgeBlocks>,
    per_edge_aggregate: BlockAggregate,
    fused_blocks: Vec<CrateBlocks>,
    fused_aggregate: Aggregate,
}

impl EdgeStats {
    fn present_versions(&self) -> usize {
        self.states.iter().filter(|state| !state.is_empty()).count()
    }
}

impl LockstepReport {
    fn edge_share(&self) -> f64 {
        ratio(self.lockstep_edges, self.total_edges)
    }

    fn row_share(&self) -> f64 {
        ratio(self.lockstep_rows, self.total_rows)
    }
}

impl BlockAggregate {
    fn overall_versions_per_block(&self) -> f64 {
        ratio(self.versions, self.blocks)
    }
}

#[derive(Clone, Debug)]
struct StressSample {
    step: usize,
    fragments: usize,
    intervals: usize,
}

#[derive(Clone, Debug)]
struct StressResult {
    name: String,
    versions: usize,
    unique_reqs: usize,
    subtract_steps: usize,
    subtract_fragments: usize,
    subtract_intervals: usize,
    max_fragments: usize,
    subtract_wall_us: u128,
    intersect_steps: usize,
    intersect_final_intervals: usize,
    intersect_wall_us: u128,
    samples: Vec<StressSample>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "rodin_survey=info".to_owned()),
        )
        .init();

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| eyre!("cannot find workspace root from manifest dir"))?
        .to_path_buf();
    let cache = root.join("target/rodin-survey-cache");
    let report_path = root.join("docs/design/rodin-c-evidence.md");

    let crates = fetch_closure(&cache)?;
    let blocks = measure_blocks(&crates)?;
    let aggregate = aggregate_blocks(&blocks);
    let round2 = measure_round2(&crates)?;
    update_report(&report_path, &crates, &blocks, &aggregate, &round2)
        .wrap_err("writing docs/design/rodin-c-evidence.md")?;

    println!("surveyed crates: {}", aggregate.crates);
    println!("non-yanked versions: {}", aggregate.versions);
    println!("metadata blocks: {}", aggregate.blocks);
    println!(
        "median versions/block: {}",
        decimal(aggregate.median_versions_per_block)
    );
    println!(
        "p90 versions/block: {}",
        decimal(aggregate.p90_versions_per_block)
    );
    println!(
        "max versions/block: {}",
        decimal(aggregate.max_versions_per_block)
    );
    println!(
        "share crates <=5 blocks: {}%",
        decimal(aggregate.share_at_most_five_blocks * 100.0)
    );
    println!(
        "round2 lockstep edges: {}/{} ({}%)",
        round2.lockstep.lockstep_edges,
        round2.lockstep.total_edges,
        decimal(round2.lockstep.edge_share() * 100.0)
    );
    println!(
        "round2 per-edge median/p90/max versions/block: {}/{}/{}",
        decimal(round2.per_edge_aggregate.median_versions_per_block),
        decimal(round2.per_edge_aggregate.p90_versions_per_block),
        decimal(round2.per_edge_aggregate.max_versions_per_block)
    );
    println!(
        "round2 fused median/p90/max versions/block: {}/{}/{}",
        decimal(round2.fused_aggregate.median_versions_per_block),
        decimal(round2.fused_aggregate.p90_versions_per_block),
        decimal(round2.fused_aggregate.max_versions_per_block)
    );

    Ok(())
}

fn fetch_closure(cache: &Path) -> Result<BTreeMap<String, CrateData>> {
    let mut queue: VecDeque<String> = SEEDS.iter().map(|name| (*name).to_owned()).collect();
    let mut queued: BTreeSet<String> = queue.iter().cloned().collect();
    let mut crates = BTreeMap::new();

    while let Some(name) = queue.pop_front() {
        if crates.len() >= CLOSURE_CAP {
            break;
        }
        let canonical = name.to_ascii_lowercase();
        if crates.contains_key(&canonical) {
            continue;
        }

        let content = fetch_index_file(cache, &canonical)
            .wrap_err_with(|| format!("fetching sparse index file for {canonical}"))?;
        let entries = parse_entries(&content, &canonical)?;
        tracing::info!(
            crate_name = canonical,
            versions = entries.len(),
            "fetched crate index"
        );

        for dep in entries.iter().flat_map(|entry| &entry.deps) {
            let dep_name = dep
                .package
                .as_ref()
                .unwrap_or(&dep.name)
                .to_ascii_lowercase();
            if crates.len() + queue.len() >= CLOSURE_CAP {
                continue;
            }
            if !crates.contains_key(&dep_name) && queued.insert(dep_name.clone()) {
                queue.push_back(dep_name);
            }
        }

        crates.insert(
            canonical.clone(),
            CrateData {
                name: canonical,
                entries,
            },
        );
    }

    Ok(crates)
}

fn fetch_index_file(cache: &Path, name: &str) -> Result<String> {
    let relative = index_path_for(name);
    let path = cache.join(&relative);
    if path.exists() {
        return fs::read_to_string(&path)
            .wrap_err_with(|| format!("reading cached index file {}", path.display()));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("creating cache parent {}", parent.display()))?;
    }

    let url = format!("{INDEX_BASE}/{}", relative.display());
    tracing::info!(%url, "cache miss; fetching sparse index file");
    let output = Command::new("curl")
        .args(["-fsSL", "-A", USER_AGENT, &url])
        .output()
        .wrap_err("running curl")?;
    if !output.status.success() {
        bail!(
            "curl failed for {url} with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fs::write(&path, &output.stdout)
        .wrap_err_with(|| format!("writing cached index file {}", path.display()))?;
    String::from_utf8(output.stdout).wrap_err_with(|| format!("index file {url} was not UTF-8"))
}

fn parse_entries(content: &str, expected_name: &str) -> Result<Vec<IndexEntry>> {
    let mut entries = Vec::new();
    for (line_ix, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: IndexEntry = facet_json::from_str(line)
            .wrap_err_with(|| format!("parsing {expected_name} index line {}", line_ix + 1))?;
        entries.push(entry);
    }
    Ok(entries)
}

fn index_path_for(name: &str) -> PathBuf {
    let lower = name.to_ascii_lowercase();
    match lower.len() {
        1 => PathBuf::from("1").join(lower),
        2 => PathBuf::from("2").join(lower),
        3 => PathBuf::from("3").join(&lower[0..1]).join(lower),
        _ => PathBuf::from(&lower[0..2]).join(&lower[2..4]).join(lower),
    }
}

fn measure_blocks(crates: &BTreeMap<String, CrateData>) -> Result<Vec<CrateBlocks>> {
    let mut out = Vec::new();
    for crate_data in crates.values() {
        let mut versions = Vec::new();
        let mut yanked_versions = 0;
        for entry in &crate_data.entries {
            if entry.yanked {
                yanked_versions += 1;
                continue;
            }
            let version = Version::parse(&entry.vers)
                .wrap_err_with(|| format!("parsing {} {}", crate_data.name, entry.vers))?;
            versions.push((version, metadata_signature(entry)));
        }

        versions.sort_by(|left, right| left.0.cmp(&right.0));
        let mut blocks = 0;
        let mut previous: Option<&MetadataSignature> = None;
        for (_, signature) in &versions {
            if previous != Some(signature) {
                blocks += 1;
                previous = Some(signature);
            }
        }

        let non_yanked_versions = versions.len();
        let versions_per_block = if blocks == 0 {
            0.0
        } else {
            non_yanked_versions as f64 / blocks as f64
        };
        out.push(CrateBlocks {
            name: crate_data.name.clone(),
            non_yanked_versions,
            yanked_versions,
            blocks,
            versions_per_block,
        });
    }

    out.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(out)
}

fn metadata_signature(entry: &IndexEntry) -> MetadataSignature {
    metadata_signature_with_skips(&entry.name, entry, &BTreeSet::new())
}

fn metadata_signature_with_skips(
    parent: &str,
    entry: &IndexEntry,
    skipped_edges: &BTreeSet<EdgeKey>,
) -> MetadataSignature {
    let mut deps: Vec<_> = entry
        .deps
        .iter()
        .filter_map(|dep| {
            let edge = EdgeKey {
                parent: parent.to_ascii_lowercase(),
                dep: actual_dep_name(dep),
            };
            if skipped_edges.contains(&edge) {
                return None;
            }
            let mut features = dep.features.clone();
            features.sort();
            Some(DepSignature {
                name: dep.name.clone(),
                req: dep.req.clone(),
                features,
                optional: dep.optional,
                default_features: dep.default_features,
                target: dep.target.clone(),
                kind: dep.kind.clone(),
            })
        })
        .collect();
    deps.sort();

    let mut features = entry.features.clone();
    if let Some(features2) = &entry.features2 {
        for (name, values) in features2 {
            features.insert(name.clone(), values.clone());
        }
    }
    for values in features.values_mut() {
        values.sort();
    }

    MetadataSignature { deps, features }
}

fn actual_dep_name(dep: &IndexDependency) -> String {
    dep.package
        .as_ref()
        .unwrap_or(&dep.name)
        .to_ascii_lowercase()
}

fn edge_signature(dep: &IndexDependency) -> EdgeSignature {
    let mut features = dep.features.clone();
    features.sort();
    EdgeSignature {
        req: dep.req.clone(),
        features,
        optional: dep.optional,
        default_features: dep.default_features,
        target: dep.target.clone(),
        kind: dep.kind.clone(),
    }
}

fn measure_round2(crates: &BTreeMap<String, CrateData>) -> Result<Round2> {
    let edge_stats = edge_stats(crates)?;
    let lockstep = lockstep_report(&edge_stats);
    let lockstep_keys: BTreeSet<_> = lockstep
        .edges
        .iter()
        .map(|edge| EdgeKey {
            parent: edge.parent.clone(),
            dep: edge.dep.clone(),
        })
        .collect();
    let per_edge_blocks = measure_per_edge_blocks(&edge_stats, &lockstep_keys);
    let per_edge_aggregate = aggregate_edge_blocks(&per_edge_blocks);
    let fused_blocks = measure_fused_blocks(crates, &lockstep_keys)?;
    let fused_aggregate = aggregate_blocks(&fused_blocks);

    Ok(Round2 {
        lockstep,
        per_edge_blocks,
        per_edge_aggregate,
        fused_blocks,
        fused_aggregate,
    })
}

fn edge_stats(crates: &BTreeMap<String, CrateData>) -> Result<Vec<EdgeStats>> {
    let mut stats: BTreeMap<EdgeKey, EdgeStats> = BTreeMap::new();

    for crate_data in crates.values() {
        let versions = sorted_non_yanked_entries(crate_data)?;
        let parent_versions = versions.len();
        for (sequence_ix, (version, entry)) in versions.iter().enumerate() {
            let states = edge_states(entry);
            for (dep, signatures) in states {
                let key = EdgeKey {
                    parent: crate_data.name.clone(),
                    dep: dep.clone(),
                };
                let stat = stats.entry(key).or_insert_with(|| EdgeStats {
                    parent: crate_data.name.clone(),
                    dep,
                    parent_versions,
                    states: vec![Vec::new(); parent_versions],
                    rows: 0,
                    lockstep_rows: 0,
                });
                for signature in signatures {
                    stat.rows += 1;
                    if is_lockstep_req(&signature.req, version) {
                        stat.lockstep_rows += 1;
                    }
                    stat.states[sequence_ix].push(signature);
                }
            }
        }
    }

    let mut out: Vec<_> = stats.into_values().collect();
    for stat in &mut out {
        for state in &mut stat.states {
            state.sort();
        }
    }
    out.sort_by(|left, right| {
        left.parent
            .cmp(&right.parent)
            .then_with(|| left.dep.cmp(&right.dep))
    });
    Ok(out)
}

fn sorted_non_yanked_entries(crate_data: &CrateData) -> Result<Vec<(Version, &IndexEntry)>> {
    let mut entries = Vec::new();
    for entry in &crate_data.entries {
        if entry.yanked {
            continue;
        }
        let version = Version::parse(&entry.vers)
            .wrap_err_with(|| format!("parsing {} {}", crate_data.name, entry.vers))?;
        entries.push((version, entry));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(entries)
}

fn edge_states(entry: &IndexEntry) -> BTreeMap<String, Vec<EdgeSignature>> {
    let mut states: BTreeMap<String, Vec<EdgeSignature>> = BTreeMap::new();
    for dep in &entry.deps {
        states
            .entry(actual_dep_name(dep))
            .or_default()
            .push(edge_signature(dep));
    }
    for signatures in states.values_mut() {
        signatures.sort();
    }
    states
}

fn is_lockstep_req(req: &str, parent_version: &Version) -> bool {
    let Ok(req) = VersionReq::parse(req) else {
        return false;
    };
    let [cmp] = req.comparators.as_slice() else {
        return false;
    };
    if !matches!(cmp.op, Op::Exact | Op::Caret) {
        return false;
    }
    comparator_has_full_triple(cmp) && comparator_version(cmp) == *parent_version
}

fn comparator_has_full_triple(cmp: &Comparator) -> bool {
    cmp.minor.is_some() && cmp.patch.is_some()
}

fn comparator_version(cmp: &Comparator) -> Version {
    Version {
        major: cmp.major,
        minor: cmp.minor.unwrap_or(0),
        patch: cmp.patch.unwrap_or(0),
        pre: cmp.pre.clone(),
        build: semver::BuildMetadata::EMPTY,
    }
}

fn lockstep_report(edge_stats: &[EdgeStats]) -> LockstepReport {
    let mut edges = Vec::new();
    let total_rows = edge_stats.iter().map(|edge| edge.rows).sum();
    let lockstep_rows = edge_stats.iter().map(|edge| edge.lockstep_rows).sum();

    for edge in edge_stats {
        if edge.rows > 0 && edge.rows == edge.lockstep_rows && edge.present_versions() >= 2 {
            edges.push(LockstepEdge {
                parent: edge.parent.clone(),
                dep: edge.dep.clone(),
                rows: edge.rows,
                present_versions: edge.present_versions(),
            });
        }
    }

    edges.sort_by(|left, right| {
        right
            .rows
            .cmp(&left.rows)
            .then_with(|| left.parent.cmp(&right.parent))
            .then_with(|| left.dep.cmp(&right.dep))
    });
    let families = lockstep_families(&edges);

    LockstepReport {
        total_edges: edge_stats.len(),
        lockstep_edges: edges.len(),
        total_rows,
        lockstep_rows,
        families,
        edges,
    }
}

fn lockstep_families(edges: &[LockstepEdge]) -> Vec<LockstepFamily> {
    let mut graph: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for edge in edges {
        graph
            .entry(edge.parent.clone())
            .or_default()
            .insert(edge.dep.clone());
        graph
            .entry(edge.dep.clone())
            .or_default()
            .insert(edge.parent.clone());
    }

    let mut seen = BTreeSet::new();
    let mut families = Vec::new();
    for node in graph.keys() {
        if !seen.insert(node.clone()) {
            continue;
        }
        let mut stack = vec![node.clone()];
        let mut nodes = Vec::new();
        while let Some(current) = stack.pop() {
            nodes.push(current.clone());
            if let Some(neighbors) = graph.get(&current) {
                for neighbor in neighbors {
                    if seen.insert(neighbor.clone()) {
                        stack.push(neighbor.clone());
                    }
                }
            }
        }
        nodes.sort();
        let mut component_edges = 0;
        let mut rows = 0;
        for edge in edges {
            if nodes.binary_search(&edge.parent).is_ok() && nodes.binary_search(&edge.dep).is_ok() {
                component_edges += 1;
                rows += edge.rows;
            }
        }
        families.push(LockstepFamily {
            nodes,
            edges: component_edges,
            rows,
        });
    }

    families.sort_by(|left, right| {
        right
            .rows
            .cmp(&left.rows)
            .then_with(|| right.edges.cmp(&left.edges))
            .then_with(|| left.nodes.cmp(&right.nodes))
    });
    families
}

fn measure_per_edge_blocks(
    edge_stats: &[EdgeStats],
    lockstep_edges: &BTreeSet<EdgeKey>,
) -> Vec<EdgeBlocks> {
    let mut out = Vec::new();
    for edge in edge_stats {
        let key = EdgeKey {
            parent: edge.parent.clone(),
            dep: edge.dep.clone(),
        };
        if lockstep_edges.contains(&key) {
            continue;
        }
        let blocks = count_edge_blocks(&edge.states);
        let versions_per_block = if blocks == 0 {
            0.0
        } else {
            edge.parent_versions as f64 / blocks as f64
        };
        out.push(EdgeBlocks {
            parent: edge.parent.clone(),
            dep: edge.dep.clone(),
            parent_versions: edge.parent_versions,
            present_versions: edge.present_versions(),
            blocks,
            versions_per_block,
        });
    }
    out.sort_by(|left, right| {
        left.parent
            .cmp(&right.parent)
            .then_with(|| left.dep.cmp(&right.dep))
    });
    out
}

fn count_edge_blocks(states: &[Vec<EdgeSignature>]) -> usize {
    let mut blocks = 0;
    let mut previous: Option<&Vec<EdgeSignature>> = None;
    for state in states {
        if previous != Some(state) {
            blocks += 1;
            previous = Some(state);
        }
    }
    blocks
}

fn aggregate_edge_blocks(blocks: &[EdgeBlocks]) -> BlockAggregate {
    let mut ratios: Vec<_> = blocks
        .iter()
        .filter(|entry| entry.blocks > 0)
        .map(|entry| entry.versions_per_block)
        .collect();
    ratios.sort_by(|left, right| left.total_cmp(right));

    BlockAggregate {
        items: blocks.len(),
        versions: blocks.iter().map(|entry| entry.parent_versions).sum(),
        blocks: blocks.iter().map(|entry| entry.blocks).sum(),
        median_versions_per_block: percentile(&ratios, 0.5),
        p90_versions_per_block: percentile(&ratios, 0.9),
        max_versions_per_block: ratios.last().copied().unwrap_or(0.0),
        share_at_most_five_blocks: if blocks.is_empty() {
            0.0
        } else {
            blocks.iter().filter(|entry| entry.blocks <= 5).count() as f64 / blocks.len() as f64
        },
    }
}

fn measure_fused_blocks(
    crates: &BTreeMap<String, CrateData>,
    lockstep_edges: &BTreeSet<EdgeKey>,
) -> Result<Vec<CrateBlocks>> {
    let mut out = Vec::new();
    for crate_data in crates.values() {
        let mut versions = Vec::new();
        let mut yanked_versions = 0;
        for entry in &crate_data.entries {
            if entry.yanked {
                yanked_versions += 1;
                continue;
            }
            let version = Version::parse(&entry.vers)
                .wrap_err_with(|| format!("parsing {} {}", crate_data.name, entry.vers))?;
            versions.push((
                version,
                metadata_signature_with_skips(&crate_data.name, entry, lockstep_edges),
            ));
        }

        versions.sort_by(|left, right| left.0.cmp(&right.0));
        let mut blocks = 0;
        let mut previous: Option<&MetadataSignature> = None;
        for (_, signature) in &versions {
            if previous != Some(signature) {
                blocks += 1;
                previous = Some(signature);
            }
        }
        let non_yanked_versions = versions.len();
        let versions_per_block = if blocks == 0 {
            0.0
        } else {
            non_yanked_versions as f64 / blocks as f64
        };
        out.push(CrateBlocks {
            name: crate_data.name.clone(),
            non_yanked_versions,
            yanked_versions,
            blocks,
            versions_per_block,
        });
    }

    out.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(out)
}

fn aggregate_blocks(blocks: &[CrateBlocks]) -> Aggregate {
    let mut ratios: Vec<_> = blocks
        .iter()
        .filter(|entry| entry.blocks > 0)
        .map(|entry| entry.versions_per_block)
        .collect();
    ratios.sort_by(|left, right| left.total_cmp(right));

    Aggregate {
        crates: blocks.len(),
        versions: blocks.iter().map(|entry| entry.non_yanked_versions).sum(),
        yanked_versions: blocks.iter().map(|entry| entry.yanked_versions).sum(),
        blocks: blocks.iter().map(|entry| entry.blocks).sum(),
        median_versions_per_block: percentile(&ratios, 0.5),
        p90_versions_per_block: percentile(&ratios, 0.9),
        max_versions_per_block: ratios.last().copied().unwrap_or(0.0),
        share_at_most_five_blocks: if blocks.is_empty() {
            0.0
        } else {
            blocks.iter().filter(|entry| entry.blocks <= 5).count() as f64 / blocks.len() as f64
        },
    }
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((sorted.len() - 1) as f64 * q).ceil() as usize;
    sorted[rank]
}

fn stress_regions(crates: &BTreeMap<String, CrateData>) -> Result<Vec<StressResult>> {
    let reqs_by_package = reqs_by_package(crates);
    let mut largest: Vec<_> = crates.values().collect();
    largest.sort_by(|left, right| {
        non_yanked_version_count(right)
            .cmp(&non_yanked_version_count(left))
            .then_with(|| left.name.cmp(&right.name))
    });

    let mut out = Vec::new();
    for (pkg_ix, crate_data) in largest.into_iter().take(20).enumerate() {
        let versions = sorted_non_yanked_versions(crate_data)?;
        if versions.is_empty() {
            continue;
        }
        let Some(reqs) = reqs_by_package.get(&crate_data.name) else {
            continue;
        };
        let req_sets = requirement_sets(reqs);
        if req_sets.is_empty() {
            continue;
        }
        out.push(stress_one(
            crate_data.name.clone(),
            PkgIx(pkg_ix as u32),
            &versions,
            &req_sets,
        ));
    }
    Ok(out)
}

fn reqs_by_package(crates: &BTreeMap<String, CrateData>) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for entry in crates.values().flat_map(|crate_data| &crate_data.entries) {
        for dep in &entry.deps {
            let name = dep
                .package
                .as_ref()
                .unwrap_or(&dep.name)
                .to_ascii_lowercase();
            out.entry(name).or_default().insert(dep.req.clone());
        }
    }
    out
}

fn requirement_sets(reqs: &BTreeSet<String>) -> Vec<VersionSet> {
    reqs.iter()
        .filter_map(|req| VersionReq::parse(req).ok())
        .map(|req| VersionSet::from_req(&req))
        .filter(|set| !set.is_empty())
        .collect()
}

fn stress_one(
    name: String,
    pkg: PkgIx,
    versions: &[Version],
    req_sets: &[VersionSet],
) -> StressResult {
    let lower = versions.first().cloned();
    let upper = versions.last().map(next_patch);
    let full_set = VersionSet::from_interval(lower, upper);
    let partial_sets: Vec<_> = req_sets
        .iter()
        .map(|set| set.intersect(&full_set))
        .filter(|set| !set.is_empty() && !full_set.is_subset_of(set))
        .collect();
    let exclusion_sets = if partial_sets.is_empty() {
        req_sets.to_vec()
    } else {
        partial_sets
    };
    let full = Region::new().with_versions(pkg, full_set);
    let exclusions: Vec<_> = exclusion_sets
        .iter()
        .cycle()
        .take(100)
        .map(|set| Region::new().with_versions(pkg, set.clone()))
        .collect();

    let mut frontier = vec![full.clone()];
    let mut samples = Vec::new();
    let mut max_fragments = frontier.len();
    let subtract_start = Instant::now();
    for (step, exclusion) in exclusions.iter().enumerate() {
        let mut next = Vec::new();
        for fragment in &frontier {
            next.extend(fragment.subtract(exclusion));
        }
        frontier = next;
        max_fragments = max_fragments.max(frontier.len());

        let step = step + 1;
        if matches!(step, 1 | 2 | 5 | 10 | 25 | 50 | 75 | 100) {
            samples.push(StressSample {
                step,
                fragments: frontier.len(),
                intervals: region_interval_count(&frontier),
            });
        }
    }
    let subtract_wall_us = subtract_start.elapsed().as_micros();

    let intersect_start = Instant::now();
    let mut intersect_steps = 0;
    let mut intersection = Some(full);
    for exclusion in &exclusions {
        let Some(current) = &intersection else {
            break;
        };
        intersection = current.intersect(exclusion);
        intersect_steps += 1;
    }
    let intersect_wall_us = intersect_start.elapsed().as_micros();
    let intersect_final_intervals = intersection
        .as_ref()
        .map(|region| region_interval_count(std::slice::from_ref(region)))
        .unwrap_or(0);

    StressResult {
        name,
        versions: versions.len(),
        unique_reqs: req_sets.len(),
        subtract_steps: exclusions.len(),
        subtract_fragments: frontier.len(),
        subtract_intervals: region_interval_count(&frontier),
        max_fragments,
        subtract_wall_us,
        intersect_steps,
        intersect_final_intervals,
        intersect_wall_us,
        samples,
    }
}

fn region_interval_count(regions: &[Region]) -> usize {
    regions
        .iter()
        .map(|region| {
            region
                .versions
                .values()
                .map(|set| set.intervals().len())
                .sum::<usize>()
        })
        .sum()
}

fn non_yanked_version_count(crate_data: &CrateData) -> usize {
    crate_data
        .entries
        .iter()
        .filter(|entry| !entry.yanked)
        .count()
}

fn sorted_non_yanked_versions(crate_data: &CrateData) -> Result<Vec<Version>> {
    let mut versions = Vec::new();
    for entry in &crate_data.entries {
        if entry.yanked {
            continue;
        }
        versions.push(
            Version::parse(&entry.vers)
                .wrap_err_with(|| format!("parsing {} {}", crate_data.name, entry.vers))?,
        );
    }
    versions.sort();
    Ok(versions)
}

fn next_patch(version: &Version) -> Version {
    Version {
        major: version.major,
        minor: version.minor,
        patch: version.patch + 1,
        pre: semver::Prerelease::EMPTY,
        build: semver::BuildMetadata::EMPTY,
    }
}

fn render_report(
    crates: &BTreeMap<String, CrateData>,
    blocks: &[CrateBlocks],
    aggregate: &Aggregate,
    stress: &[StressResult],
) -> String {
    let mut report = String::new();
    report.push_str("# Rodin agenda C evidence\n\n");
    report.push_str("Generated by `cargo run -p rodin-survey`. The survey fetches crates.io sparse-index files sequentially, with User-Agent `");
    report.push_str(USER_AGENT);
    report.push_str("`, and caches them under `target/rodin-survey-cache/` so repeat runs are offline. Seed crates: ");
    report.push_str(&SEEDS.join(", "));
    report.push_str(". Closure cap: ");
    report.push_str(&CLOSURE_CAP.to_string());
    report.push_str(" crates. Actual closure size: ");
    report.push_str(&crates.len().to_string());
    report.push_str(" crates.\n\n");

    report.push_str("## Method\n\n");
    report.push_str("- Walk: start from the seed crate names, fetch each sparse-index file, parse every JSONL version row with `facet-json`, enqueue dependency names from `package` when present or `name` otherwise, and stop at the closure cap.\n");
    report.push_str("- Measurement A: ignore yanked versions for block construction, sort remaining versions by semver order, normalize each row to resolution metadata (`deps` by name/req/features/optional/default_features/target/kind plus merged `features`/`features2` maps), and count contiguous runs of identical metadata. Yanked counts are reported separately.\n");
    report.push_str("- Measurement B: choose the 20 closure members with the most non-yanked versions, collect real requirement strings targeting those packages from the fetched index rows, convert them through `rodin_facts::VersionSet::from_req`, and run 100 chained `Region::subtract` exclusions from a full version box plus a companion `Region::intersect` chain.\n\n");

    report.push_str("## Aggregate\n\n");
    report.push_str("| metric | value |\n|---|---:|\n");
    push_row(&mut report, "crates", aggregate.crates);
    push_row(&mut report, "non-yanked versions", aggregate.versions);
    push_row(&mut report, "yanked versions", aggregate.yanked_versions);
    push_row(&mut report, "metadata blocks", aggregate.blocks);
    push_row(
        &mut report,
        "overall versions/block",
        decimal(aggregate.versions as f64 / aggregate.blocks as f64),
    );
    push_row(
        &mut report,
        "median versions/block",
        decimal(aggregate.median_versions_per_block),
    );
    push_row(
        &mut report,
        "p90 versions/block",
        decimal(aggregate.p90_versions_per_block),
    );
    push_row(
        &mut report,
        "max versions/block",
        decimal(aggregate.max_versions_per_block),
    );
    push_row(
        &mut report,
        "share of crates with <=5 blocks",
        format!("{}%", decimal(aggregate.share_at_most_five_blocks * 100.0)),
    );
    report.push('\n');

    report.push_str("## Per-crate blocks\n\n");
    report.push_str("| crate | non-yanked versions | yanked versions | blocks | versions/block |\n|---|---:|---:|---:|---:|\n");
    for entry in blocks {
        report.push_str("| ");
        report.push_str(&entry.name);
        report.push_str(" | ");
        report.push_str(&entry.non_yanked_versions.to_string());
        report.push_str(" | ");
        report.push_str(&entry.yanked_versions.to_string());
        report.push_str(" | ");
        report.push_str(&entry.blocks.to_string());
        report.push_str(" | ");
        report.push_str(&decimal(entry.versions_per_block));
        report.push_str(" |\n");
    }
    report.push('\n');

    let mut worst = blocks.to_vec();
    worst.sort_by(|left, right| {
        right
            .blocks
            .cmp(&left.blocks)
            .then_with(|| compare_f64(left.versions_per_block, right.versions_per_block))
            .then_with(|| left.name.cmp(&right.name))
    });
    report.push_str("## Ten worst crates by block count\n\n");
    report.push_str("| rank | crate | non-yanked versions | blocks | versions/block | yanked versions |\n|---:|---|---:|---:|---:|---:|\n");
    for (ix, entry) in worst.iter().take(10).enumerate() {
        report.push_str("| ");
        report.push_str(&(ix + 1).to_string());
        report.push_str(" | ");
        report.push_str(&entry.name);
        report.push_str(" | ");
        report.push_str(&entry.non_yanked_versions.to_string());
        report.push_str(" | ");
        report.push_str(&entry.blocks.to_string());
        report.push_str(" | ");
        report.push_str(&decimal(entry.versions_per_block));
        report.push_str(" | ");
        report.push_str(&entry.yanked_versions.to_string());
        report.push_str(" |\n");
    }
    report.push('\n');

    report.push_str("## Region algebra stress\n\n");
    report.push_str("| crate | versions | unique real reqs | subtract steps | fragments after 100 | intervals after 100 | max fragments | subtract wall us | intersect steps | intersect final intervals | intersect wall us |\n");
    report.push_str("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    for entry in stress {
        report.push_str("| ");
        report.push_str(&entry.name);
        report.push_str(" | ");
        report.push_str(&entry.versions.to_string());
        report.push_str(" | ");
        report.push_str(&entry.unique_reqs.to_string());
        report.push_str(" | ");
        report.push_str(&entry.subtract_steps.to_string());
        report.push_str(" | ");
        report.push_str(&entry.subtract_fragments.to_string());
        report.push_str(" | ");
        report.push_str(&entry.subtract_intervals.to_string());
        report.push_str(" | ");
        report.push_str(&entry.max_fragments.to_string());
        report.push_str(" | ");
        report.push_str(&entry.subtract_wall_us.to_string());
        report.push_str(" | ");
        report.push_str(&entry.intersect_steps.to_string());
        report.push_str(" | ");
        report.push_str(&entry.intersect_final_intervals.to_string());
        report.push_str(" | ");
        report.push_str(&entry.intersect_wall_us.to_string());
        report.push_str(" |\n");
    }
    report.push('\n');

    report.push_str("### Subtract growth samples\n\n");
    report.push_str("| crate | step | fragments | intervals |\n|---|---:|---:|---:|\n");
    for entry in stress {
        for sample in &entry.samples {
            report.push_str("| ");
            report.push_str(&entry.name);
            report.push_str(" | ");
            report.push_str(&sample.step.to_string());
            report.push_str(" | ");
            report.push_str(&sample.fragments.to_string());
            report.push_str(" | ");
            report.push_str(&sample.intervals.to_string());
            report.push_str(" |\n");
        }
    }
    report.push('\n');

    report.push_str("## Verdict\n\n");
    report.push_str(&verdict(aggregate, &worst, stress));
    report.push('\n');

    report
}

fn update_report(
    path: &Path,
    crates: &BTreeMap<String, CrateData>,
    blocks: &[CrateBlocks],
    aggregate: &Aggregate,
    round2: &Round2,
) -> Result<()> {
    let existing = if path.exists() {
        fs::read_to_string(path).wrap_err_with(|| format!("reading {}", path.display()))?
    } else {
        let stress = stress_regions(crates)?;
        render_report(crates, blocks, aggregate, &stress)
    };
    let base = existing
        .find("\n## Round 2\n")
        .map_or(existing.as_str(), |ix| &existing[..ix])
        .trim_end();
    let round2 = render_round2(aggregate, round2);
    fs::write(path, format!("{base}\n\n{round2}"))
        .wrap_err_with(|| format!("writing {}", path.display()))
}

fn render_round2(round1: &Aggregate, round2: &Round2) -> String {
    let mut report = String::new();
    report.push_str("## Round 2\n\n");
    report.push_str("Round 2 keeps the Round 1 closure and sparse-index cache fixed, then asks whether the counterexamples are mostly lockstep family edges or whether the useful granularity is per-edge rather than whole-manifest.\n\n");

    report.push_str("### Method\n\n");
    report.push_str("- Lockstep detection: for each unique `parent -> actual-dep` edge, scan the parent's non-yanked version sequence. A row is lockstep when its requirement parses as exactly one comparator, either exact `=x.y.z` or caret/full-triple `^x.y.z`/`x.y.z`, and that comparator version equals the parent version. An edge is classified lockstep only when every present row is lockstep and it appears in at least two parent versions.\n");
    report.push_str("- Per-edge blocks: for each non-lockstep `parent -> actual-dep` edge, count contiguous runs across the full non-yanked parent version sequence using only that edge's metadata (`req/features/optional/default_features/target/kind`). Absence is a state, so add/remove transitions count.\n");
    report.push_str("- Post-fusion whole-manifest blocks: repeat Round 1's whole-manifest block count, but drop lockstep dependency rows from the metadata identity, leaving the feature maps unchanged.\n\n");

    render_lockstep(&mut report, &round2.lockstep);
    render_per_edge_blocks(
        &mut report,
        &round2.per_edge_blocks,
        &round2.per_edge_aggregate,
    );
    render_fused_blocks(&mut report, &round2.fused_blocks, &round2.fused_aggregate);

    report.push_str("### Round 2 verdict\n\n");
    report.push_str(&round2_verdict(round1, round2));
    report.push('\n');
    report
}

fn render_lockstep(report: &mut String, lockstep: &LockstepReport) {
    report.push_str("### Lockstep detection\n\n");
    report.push_str("| metric | value |\n|---|---:|\n");
    push_row(report, "unique dep edges", lockstep.total_edges);
    push_row(report, "lockstep dep edges", lockstep.lockstep_edges);
    push_row(
        report,
        "lockstep edge share",
        format!("{}%", decimal(lockstep.edge_share() * 100.0)),
    );
    push_row(report, "dep rows", lockstep.total_rows);
    push_row(report, "lockstep dep rows", lockstep.lockstep_rows);
    push_row(
        report,
        "lockstep row share",
        format!("{}%", decimal(lockstep.row_share() * 100.0)),
    );
    report.push('\n');

    report.push_str(
        "| rank | family nodes | lockstep edges | lockstep rows |\n|---:|---|---:|---:|\n",
    );
    for (ix, family) in lockstep.families.iter().take(30).enumerate() {
        report.push_str("| ");
        report.push_str(&(ix + 1).to_string());
        report.push_str(" | ");
        report.push_str(&family.nodes.join(", "));
        report.push_str(" | ");
        report.push_str(&family.edges.to_string());
        report.push_str(" | ");
        report.push_str(&family.rows.to_string());
        report.push_str(" |\n");
    }
    report.push('\n');

    report.push_str(
        "| rank | edge | present parent versions | lockstep rows |\n|---:|---|---:|---:|\n",
    );
    for (ix, edge) in lockstep.edges.iter().take(40).enumerate() {
        report.push_str("| ");
        report.push_str(&(ix + 1).to_string());
        report.push_str(" | ");
        report.push_str(&edge.parent);
        report.push_str(" -> ");
        report.push_str(&edge.dep);
        report.push_str(" | ");
        report.push_str(&edge.present_versions.to_string());
        report.push_str(" | ");
        report.push_str(&edge.rows.to_string());
        report.push_str(" |\n");
    }
    report.push('\n');
}

fn render_per_edge_blocks(report: &mut String, blocks: &[EdgeBlocks], aggregate: &BlockAggregate) {
    report.push_str("### Per-edge blocks, excluding lockstep edges\n\n");
    report.push_str("| metric | value |\n|---|---:|\n");
    push_row(report, "non-lockstep edges", aggregate.items);
    push_row(report, "parent-version observations", aggregate.versions);
    push_row(report, "edge blocks", aggregate.blocks);
    push_row(
        report,
        "overall versions/block",
        decimal(aggregate.overall_versions_per_block()),
    );
    push_row(
        report,
        "median versions/block",
        decimal(aggregate.median_versions_per_block),
    );
    push_row(
        report,
        "p90 versions/block",
        decimal(aggregate.p90_versions_per_block),
    );
    push_row(
        report,
        "max versions/block",
        decimal(aggregate.max_versions_per_block),
    );
    push_row(
        report,
        "share of edges with <=5 blocks",
        format!("{}%", decimal(aggregate.share_at_most_five_blocks * 100.0)),
    );
    report.push('\n');

    let mut worst = blocks.to_vec();
    worst.sort_by(|left, right| {
        right
            .blocks
            .cmp(&left.blocks)
            .then_with(|| compare_f64(left.versions_per_block, right.versions_per_block))
            .then_with(|| left.parent.cmp(&right.parent))
            .then_with(|| left.dep.cmp(&right.dep))
    });
    report.push_str("| rank | edge | parent versions | present versions | blocks | versions/block |\n|---:|---|---:|---:|---:|---:|\n");
    for (ix, edge) in worst.iter().take(10).enumerate() {
        report.push_str("| ");
        report.push_str(&(ix + 1).to_string());
        report.push_str(" | ");
        report.push_str(&edge.parent);
        report.push_str(" -> ");
        report.push_str(&edge.dep);
        report.push_str(" | ");
        report.push_str(&edge.parent_versions.to_string());
        report.push_str(" | ");
        report.push_str(&edge.present_versions.to_string());
        report.push_str(" | ");
        report.push_str(&edge.blocks.to_string());
        report.push_str(" | ");
        report.push_str(&decimal(edge.versions_per_block));
        report.push_str(" |\n");
    }
    report.push('\n');
}

fn render_fused_blocks(report: &mut String, blocks: &[CrateBlocks], aggregate: &Aggregate) {
    report.push_str("### Post-fusion whole-manifest blocks\n\n");
    report.push_str("| metric | value |\n|---|---:|\n");
    push_row(report, "crates", aggregate.crates);
    push_row(report, "non-yanked versions", aggregate.versions);
    push_row(report, "yanked versions", aggregate.yanked_versions);
    push_row(report, "metadata blocks", aggregate.blocks);
    push_row(
        report,
        "overall versions/block",
        decimal(aggregate.versions as f64 / aggregate.blocks as f64),
    );
    push_row(
        report,
        "median versions/block",
        decimal(aggregate.median_versions_per_block),
    );
    push_row(
        report,
        "p90 versions/block",
        decimal(aggregate.p90_versions_per_block),
    );
    push_row(
        report,
        "max versions/block",
        decimal(aggregate.max_versions_per_block),
    );
    push_row(
        report,
        "share of crates with <=5 blocks",
        format!("{}%", decimal(aggregate.share_at_most_five_blocks * 100.0)),
    );
    report.push('\n');

    let mut worst = blocks.to_vec();
    worst.sort_by(|left, right| {
        right
            .blocks
            .cmp(&left.blocks)
            .then_with(|| compare_f64(left.versions_per_block, right.versions_per_block))
            .then_with(|| left.name.cmp(&right.name))
    });
    report.push_str("| rank | crate | non-yanked versions | blocks | versions/block | yanked versions |\n|---:|---|---:|---:|---:|---:|\n");
    for (ix, entry) in worst.iter().take(10).enumerate() {
        report.push_str("| ");
        report.push_str(&(ix + 1).to_string());
        report.push_str(" | ");
        report.push_str(&entry.name);
        report.push_str(" | ");
        report.push_str(&entry.non_yanked_versions.to_string());
        report.push_str(" | ");
        report.push_str(&entry.blocks.to_string());
        report.push_str(" | ");
        report.push_str(&decimal(entry.versions_per_block));
        report.push_str(" | ");
        report.push_str(&entry.yanked_versions.to_string());
        report.push_str(" |\n");
    }
    report.push('\n');
}

fn round2_verdict(round1: &Aggregate, round2: &Round2) -> String {
    let round1_overall = round1.versions as f64 / round1.blocks as f64;
    let fused_overall =
        round2.fused_aggregate.versions as f64 / round2.fused_aggregate.blocks as f64;
    format!(
        "Family fusion finds real structure: {} of {} unique edges are lockstep ({}% by edge count, {}% by dep rows). It improves whole-manifest blocks from {} to {}, and overall versions/block from {} to {}, but that is still not a big collapse: fused median is {}, p90 is {}, and only {}% of crates have <=5 blocks. Per-edge granularity is much stronger: {} non-lockstep edges collapse to {} edge blocks over {} parent-version observations, with median {}, p90 {}, max {}, and {}% of edges at <=5 blocks. So family fusion alone does not rescue the whole-manifest frontier bet; per-edge granularity is the only round-2 hypothesis that looks empirically useful, and it points away from whole-manifest metadata blocks as the primary volume unit.",
        round2.lockstep.lockstep_edges,
        round2.lockstep.total_edges,
        decimal(round2.lockstep.edge_share() * 100.0),
        decimal(round2.lockstep.row_share() * 100.0),
        round1.blocks,
        round2.fused_aggregate.blocks,
        decimal(round1_overall),
        decimal(fused_overall),
        decimal(round2.fused_aggregate.median_versions_per_block),
        decimal(round2.fused_aggregate.p90_versions_per_block),
        decimal(round2.fused_aggregate.share_at_most_five_blocks * 100.0),
        round2.per_edge_aggregate.items,
        round2.per_edge_aggregate.blocks,
        round2.per_edge_aggregate.versions,
        decimal(round2.per_edge_aggregate.median_versions_per_block),
        decimal(round2.per_edge_aggregate.p90_versions_per_block),
        decimal(round2.per_edge_aggregate.max_versions_per_block),
        decimal(round2.per_edge_aggregate.share_at_most_five_blocks * 100.0)
    )
}

fn verdict(aggregate: &Aggregate, worst: &[CrateBlocks], stress: &[StressResult]) -> String {
    let worst_names = worst
        .iter()
        .take(3)
        .map(|entry| format!("{} ({} blocks)", entry.name, entry.blocks))
        .collect::<Vec<_>>()
        .join(", ");
    let max_fragments = stress
        .iter()
        .map(|entry| entry.max_fragments)
        .max()
        .unwrap_or(0);
    let overall = aggregate.versions as f64 / aggregate.blocks as f64;
    format!(
        "The data does not support the strong agenda C falsifier target, \"block-ratio brutal, hundreds-to-few, for almost all packages.\" The measured collapse is only {} versions/block overall: median {}, p90 {}, and just {}% of crates have <=5 blocks, with {} non-yanked versions becoming {} metadata blocks. The counterexamples are not edge noise: {}. What does survive is the narrower claim that metadata blocks remove some raw-version churn, and the Region subtract stress did not explode on these real-shaped one-dimensional exclusions; the worst chained subtract frontier reached {} fragments.",
        decimal(overall),
        decimal(aggregate.median_versions_per_block),
        decimal(aggregate.p90_versions_per_block),
        decimal(aggregate.share_at_most_five_blocks * 100.0),
        aggregate.versions,
        aggregate.blocks,
        worst_names,
        max_fragments
    )
}

fn push_row(value: &mut String, name: &str, entry: impl ToString) {
    value.push_str("| ");
    value.push_str(name);
    value.push_str(" | ");
    value.push_str(&entry.to_string());
    value.push_str(" |\n");
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

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn compare_f64(left: f64, right: f64) -> Ordering {
    left.total_cmp(&right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_index_paths_match_cargo_layout() {
        assert_eq!(index_path_for("a"), PathBuf::from("1/a"));
        assert_eq!(index_path_for("ab"), PathBuf::from("2/ab"));
        assert_eq!(index_path_for("abc"), PathBuf::from("3/a/abc"));
        assert_eq!(index_path_for("abcd"), PathBuf::from("ab/cd/abcd"));
    }

    #[test]
    fn block_count_uses_contiguous_runs_not_global_buckets() {
        let dep = IndexDependency {
            name: "dep".to_owned(),
            req: "^1".to_owned(),
            features: Vec::new(),
            optional: false,
            default_features: true,
            target: None,
            kind: "normal".to_owned(),
            registry: None,
            package: None,
        };
        let entry = |vers: &str, deps: Vec<IndexDependency>| IndexEntry {
            name: "demo".to_owned(),
            vers: vers.to_owned(),
            deps,
            features: BTreeMap::new(),
            features2: None,
            yanked: false,
        };
        let crates = BTreeMap::from([(
            "demo".to_owned(),
            CrateData {
                name: "demo".to_owned(),
                entries: vec![
                    entry("1.0.0", Vec::new()),
                    entry("1.0.1", vec![dep.clone()]),
                    entry("1.0.2", Vec::new()),
                ],
            },
        )]);

        let blocks = measure_blocks(&crates).unwrap();
        assert_eq!(blocks[0].blocks, 3);
    }

    #[test]
    fn lockstep_req_accepts_exact_and_full_triple_caret_only() {
        let parent = Version::parse("1.2.3").unwrap();

        assert!(is_lockstep_req("=1.2.3", &parent));
        assert!(is_lockstep_req("^1.2.3", &parent));
        assert!(is_lockstep_req("1.2.3", &parent));

        assert!(!is_lockstep_req("^1.2", &parent));
        assert!(!is_lockstep_req(">=1.2.3", &parent));
        assert!(!is_lockstep_req("=1.2.4", &parent));
    }

    #[test]
    fn per_edge_blocks_count_absence_as_state() {
        let dep = IndexDependency {
            name: "dep".to_owned(),
            req: "^1".to_owned(),
            features: Vec::new(),
            optional: false,
            default_features: true,
            target: None,
            kind: "normal".to_owned(),
            registry: None,
            package: None,
        };
        let entry = |vers: &str, deps: Vec<IndexDependency>| IndexEntry {
            name: "demo".to_owned(),
            vers: vers.to_owned(),
            deps,
            features: BTreeMap::new(),
            features2: None,
            yanked: false,
        };
        let crates = BTreeMap::from([(
            "demo".to_owned(),
            CrateData {
                name: "demo".to_owned(),
                entries: vec![
                    entry("1.0.0", Vec::new()),
                    entry("1.0.1", vec![dep.clone()]),
                    entry("1.0.2", vec![dep]),
                    entry("1.0.3", Vec::new()),
                ],
            },
        )]);

        let stats = edge_stats(&crates).unwrap();
        let blocks = measure_per_edge_blocks(&stats, &BTreeSet::new());
        assert_eq!(blocks[0].blocks, 3);
        assert_eq!(blocks[0].present_versions, 2);
    }
}
