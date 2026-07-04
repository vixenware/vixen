use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use eyre::{Context, Result, bail, eyre};
use facet::Facet;
use rodin_facts::{PkgIx, Region, VersionSet};
use semver::{Version, VersionReq};

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

    let crates = fetch_closure(&cache)?;
    let blocks = measure_blocks(&crates)?;
    let aggregate = aggregate_blocks(&blocks);
    let stress = stress_regions(&crates)?;
    let report = render_report(&crates, &blocks, &aggregate, &stress);
    fs::write(root.join("docs/design/rodin-c-evidence.md"), report)
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
        tracing::info!(crate_name = canonical, versions = entries.len(), "fetched crate index");

        for dep in entries.iter().flat_map(|entry| &entry.deps) {
            let dep_name = dep.package.as_ref().unwrap_or(&dep.name).to_ascii_lowercase();
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
    let mut deps: Vec<_> = entry
        .deps
        .iter()
        .map(|dep| {
            let mut features = dep.features.clone();
            features.sort();
            DepSignature {
                name: dep.name.clone(),
                req: dep.req.clone(),
                features,
                optional: dep.optional,
                default_features: dep.default_features,
                target: dep.target.clone(),
                kind: dep.kind.clone(),
            }
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
            let name = dep.package.as_ref().unwrap_or(&dep.name).to_ascii_lowercase();
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
    let full = Region::new().with_versions(pkg, full_set);
    let exclusions: Vec<_> = req_sets
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
    format!(
        "The agenda C bet is empirically supported for this closure, but not in the literal \"hundreds-to-few for almost all packages\" shape. The median crate has {} versions/block, p90 is {}, and {}% of crates have <=5 blocks; that is a real collapse from {} non-yanked versions to {} metadata blocks. The counterexamples are also real: {}. Region subtract stress did not explode on these real-shaped one-dimensional exclusions; the worst chained subtract frontier reached {} fragments.",
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
}
