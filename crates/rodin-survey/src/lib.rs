use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use eyre::{Context, Result, bail, eyre};
use facet::Facet;
use semver::Version;

pub const INDEX_BASE: &str = "https://index.crates.io";
pub const USER_AGENT: &str = "rodin-survey/0.1.0 (https://github.com/bearcove/vx)";
pub const CLOSURE_CAP: usize = 400;
pub const SEEDS: &[&str] = &["axum", "tokio", "serde", "clap", "reqwest", "syn", "rand"];

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct IndexDependency {
    pub name: String,
    pub req: String,
    pub features: Vec<String>,
    pub optional: bool,
    pub default_features: bool,
    pub target: Option<String>,
    pub kind: String,
    pub registry: Option<String>,
    pub package: Option<String>,
}

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
pub struct IndexEntry {
    pub name: String,
    pub vers: String,
    pub deps: Vec<IndexDependency>,
    pub features: BTreeMap<String, Vec<String>>,
    pub features2: Option<BTreeMap<String, Vec<String>>>,
    pub yanked: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrateData {
    pub name: String,
    pub entries: Vec<IndexEntry>,
}

pub fn workspace_cache_dir() -> Result<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| eyre!("cannot find workspace root from manifest dir"))?
        .to_path_buf();
    Ok(root.join("target/rodin-survey-cache"))
}

pub fn fetch_default_closure() -> Result<BTreeMap<String, CrateData>> {
    fetch_closure(&workspace_cache_dir()?, SEEDS, CLOSURE_CAP)
}

pub fn fetch_closure(
    cache: &Path,
    seeds: &[&str],
    closure_cap: usize,
) -> Result<BTreeMap<String, CrateData>> {
    let mut queue: VecDeque<String> = seeds.iter().map(|name| (*name).to_owned()).collect();
    let mut queued: std::collections::BTreeSet<String> = queue.iter().cloned().collect();
    let mut crates = BTreeMap::new();

    while let Some(name) = queue.pop_front() {
        if crates.len() >= closure_cap {
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
            let dep_name = actual_dep_name(dep);
            if crates.len() + queue.len() >= closure_cap {
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

pub fn fetch_index_file(cache: &Path, name: &str) -> Result<String> {
    let relative = index_path_for(name);
    if let Some(content) = read_cached_index_file(cache, &relative)? {
        return Ok(content);
    }

    if let Some(parent) = cache.join(&relative).parent() {
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

    fs::write(cache.join(&relative), &output.stdout).wrap_err_with(|| {
        format!(
            "writing cached index file {}",
            cache.join(&relative).display()
        )
    })?;
    String::from_utf8(output.stdout).wrap_err_with(|| format!("index file {url} was not UTF-8"))
}

pub fn read_cached_index_file(cache: &Path, relative: &Path) -> Result<Option<String>> {
    let path = cache.join(relative);
    if path.exists() {
        return fs::read_to_string(&path)
            .wrap_err_with(|| format!("reading cached index file {}", path.display()))
            .map(Some);
    }
    read_cargo_sparse_cache(relative)
}

fn read_cargo_sparse_cache(relative: &Path) -> Result<Option<String>> {
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(None);
    };
    let path = PathBuf::from(home)
        .join(".cargo/registry/index/index.crates.io-1949cf8c6b5b557f/.cache")
        .join(relative);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path)
        .wrap_err_with(|| format!("reading Cargo sparse cache file {}", path.display()))?;
    Ok(Some(decode_cargo_sparse_cache(&bytes)?))
}

fn decode_cargo_sparse_cache(bytes: &[u8]) -> Result<String> {
    let mut rows = Vec::new();
    for part in bytes.split(|byte| *byte == 0) {
        if part.first() != Some(&b'{') {
            continue;
        }
        rows.push(
            std::str::from_utf8(part)
                .wrap_err("Cargo sparse cache row was not UTF-8")?
                .to_owned(),
        );
    }
    Ok(rows.join("\n"))
}

pub fn parse_entries(content: &str, expected_name: &str) -> Result<Vec<IndexEntry>> {
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

#[must_use]
pub fn index_path_for(name: &str) -> PathBuf {
    let lower = name.to_ascii_lowercase();
    match lower.len() {
        1 => PathBuf::from("1").join(lower),
        2 => PathBuf::from("2").join(lower),
        3 => PathBuf::from("3").join(&lower[0..1]).join(lower),
        _ => PathBuf::from(&lower[0..2]).join(&lower[2..4]).join(lower),
    }
}

#[must_use]
pub fn actual_dep_name(dep: &IndexDependency) -> String {
    dep.package
        .as_ref()
        .unwrap_or(&dep.name)
        .to_ascii_lowercase()
}

pub fn sorted_non_yanked_entries(crate_data: &CrateData) -> Result<Vec<(Version, &IndexEntry)>> {
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

pub fn sorted_non_yanked_versions(crate_data: &CrateData) -> Result<Vec<Version>> {
    sorted_non_yanked_entries(crate_data)
        .map(|entries| entries.into_iter().map(|(version, _)| version).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_sparse_cache_decoder_keeps_only_json_rows() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x03\x02\0etag: \"demo\"\0");
        bytes.extend_from_slice(b"1.0.0\0{\"name\":\"demo\",\"vers\":\"1.0.0\"}\0");
        bytes.extend_from_slice(b"1.0.1\0{\"name\":\"demo\",\"vers\":\"1.0.1\"}");

        let decoded = decode_cargo_sparse_cache(&bytes).unwrap();

        assert_eq!(
            decoded,
            "{\"name\":\"demo\",\"vers\":\"1.0.0\"}\n{\"name\":\"demo\",\"vers\":\"1.0.1\"}"
        );
    }
}
