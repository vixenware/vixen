use std::collections::BTreeSet;
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
pub const ROUND_TARGET_TRIPLE: &str = "aarch64-apple-darwin";

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
    if let Some(content) = read_cargo_sparse_cache(relative)? {
        return Ok(Some(content));
    }
    read_committed_fixture(relative)
}

/// Falls back to a repo-committed snapshot under `crates/rodin-survey/fixtures/sparse-index/`
/// for crates whose sparse-index row is neither in the workspace cache nor Cargo's local
/// sparse-index cache (e.g. `async-graphql`, fetched once and pinned so forensics/taxonomy
/// generators reproduce offline).
fn read_committed_fixture(relative: &Path) -> Result<Option<String>> {
    let Some(name) = relative.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/sparse-index")
        .join(format!("{name}.jsonl"));
    if !path.exists() {
        return Ok(None);
    }
    fs::read_to_string(&path)
        .wrap_err_with(|| format!("reading committed fixture {}", path.display()))
        .map(Some)
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

#[must_use]
pub fn fixed_target_runtime_dependency_is_consumed(
    package_name: &str,
    kind: &str,
    target: Option<&str>,
    workspace_members: &BTreeSet<String>,
) -> bool {
    if kind == "dev" && !workspace_members.contains(package_name) {
        return false;
    }
    target.is_none_or(target_matches_round)
}

#[must_use]
pub fn target_matches_round(expr: &str) -> bool {
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
/// The set of `cfg` predicates active for a target triple.
///
/// rustc is the oracle for which cfgs a target has — `rustc --print cfg
/// --target <triple>` emits the exact set, so we never hand-maintain a table
/// that drifts from the compiler. A dependency's `[target.'…'.dependencies]`
/// expression is evaluated against this set to decide whether the dependency is
/// consumed on that target. This is the per-context resolver-v2 replacement for
/// the fixed-target [`target_matches_round`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetCfg {
    triple: String,
    flags: BTreeSet<String>,
    key_values: BTreeSet<(String, String)>,
}

impl TargetCfg {
    /// Build the cfg set for `triple` by asking rustc (the oracle).
    ///
    /// Requires the target to be known to the active toolchain (no target std
    /// needed — `--print cfg` only reads the target spec).
    pub fn from_rustc(triple: &str) -> Result<Self> {
        let output = Command::new("rustc")
            .args(["--print", "cfg", "--target", triple])
            .output()
            .wrap_err("running `rustc --print cfg`")?;
        if !output.status.success() {
            bail!(
                "rustc --print cfg --target {triple} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let text = String::from_utf8(output.stdout).wrap_err("decoding rustc cfg output")?;
        Ok(Self::from_rustc_output(triple, &text))
    }

    /// Parse the `key="value"` / bare-flag lines rustc emits.
    #[must_use]
    pub fn from_rustc_output(triple: &str, text: &str) -> Self {
        let mut flags = BTreeSet::new();
        let mut key_values = BTreeSet::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let value = value.trim().trim_matches('"').to_owned();
                key_values.insert((key.trim().to_owned(), value));
            } else {
                flags.insert(line.to_owned());
            }
        }
        Self {
            triple: triple.to_owned(),
            flags,
            key_values,
        }
    }

    /// The fixed calibration target ([`ROUND_TARGET_TRIPLE`],
    /// aarch64-apple-darwin) as a `TargetCfg`, matching the cfg values the
    /// legacy [`target_matches_round`] hardcoded. Deterministic, no rustc.
    #[must_use]
    pub fn round_target() -> Self {
        Self::from_parts(
            ROUND_TARGET_TRIPLE,
            ["unix"],
            [
                ("target_arch", "aarch64"),
                ("target_endian", "little"),
                ("target_env", ""),
                ("target_family", "unix"),
                ("target_os", "macos"),
                ("target_pointer_width", "64"),
                ("target_vendor", "apple"),
            ],
        )
    }

    /// Construct explicitly (deterministic tests, no toolchain dependency).
    #[must_use]
    pub fn from_parts(
        triple: impl Into<String>,
        flags: impl IntoIterator<Item = &'static str>,
        key_values: impl IntoIterator<Item = (&'static str, &'static str)>,
    ) -> Self {
        Self {
            triple: triple.into(),
            flags: flags.into_iter().map(str::to_owned).collect(),
            key_values: key_values
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
        }
    }

    #[must_use]
    pub fn triple(&self) -> &str {
        &self.triple
    }

    /// Does a dependency's `target` field match this target?
    ///
    /// The field is either a literal triple (`x86_64-pc-windows-msvc`) or a
    /// `cfg(...)` expression.
    #[must_use]
    pub fn matches_target_field(&self, target: &str) -> bool {
        let target = target.trim().trim_matches('"').trim_matches('\'');
        if let Some(cfg) = target.strip_prefix("cfg(").and_then(|t| t.strip_suffix(')')) {
            self.eval_cfg(cfg)
        } else {
            target == self.triple
        }
    }

    fn eval_cfg(&self, expr: &str) -> bool {
        let expr = expr.trim();
        if let Some(inner) = expr.strip_prefix("all(").and_then(|t| t.strip_suffix(')')) {
            return split_cfg_args(inner).iter().all(|arg| self.eval_cfg(arg));
        }
        if let Some(inner) = expr.strip_prefix("any(").and_then(|t| t.strip_suffix(')')) {
            return split_cfg_args(inner).iter().any(|arg| self.eval_cfg(arg));
        }
        if let Some(inner) = expr.strip_prefix("not(").and_then(|t| t.strip_suffix(')')) {
            return !self.eval_cfg(inner);
        }
        if let Some((key, value)) = expr.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"').trim_matches('\'');
            return self.key_values.contains(&(key.to_owned(), value.to_owned()));
        }
        self.flags.contains(expr.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn darwin_cfg() -> TargetCfg {
        // A representative slice of `rustc --print cfg --target aarch64-apple-darwin`.
        TargetCfg::from_parts(
            "aarch64-apple-darwin",
            ["unix", "debug_assertions"],
            [
                ("target_arch", "aarch64"),
                ("target_os", "macos"),
                ("target_family", "unix"),
                ("target_endian", "little"),
                ("target_pointer_width", "64"),
                ("target_vendor", "apple"),
                ("target_env", ""),
            ],
        )
    }

    #[test]
    fn target_cfg_parses_rustc_output_and_evaluates_expressions() {
        let text = "debug_assertions\ntarget_arch=\"aarch64\"\ntarget_os=\"macos\"\nunix\n";
        let cfg = TargetCfg::from_rustc_output("aarch64-apple-darwin", text);
        assert!(cfg.matches_target_field("cfg(unix)"));
        assert!(cfg.matches_target_field("cfg(target_arch = \"aarch64\")"));
        assert!(!cfg.matches_target_field("cfg(target_os = \"linux\")"));
        assert!(cfg.matches_target_field("aarch64-apple-darwin"));
        assert!(!cfg.matches_target_field("x86_64-pc-windows-msvc"));
    }

    #[test]
    fn target_cfg_evaluates_nested_boolean_expressions() {
        let cfg = darwin_cfg();
        assert!(cfg.matches_target_field("cfg(any(windows, unix))"));
        assert!(!cfg.matches_target_field("cfg(all(unix, windows))"));
        assert!(cfg.matches_target_field("cfg(not(windows))"));
        assert!(cfg.matches_target_field(
            "cfg(all(target_os = \"macos\", target_arch = \"aarch64\"))"
        ));
        assert!(!cfg.matches_target_field("cfg(windows)"));
    }

    #[test]
    fn target_cfg_from_rustc_matches_the_compiler_for_a_builtin_triple() {
        // `--print cfg --target` reads only the (builtin) target spec, so this is
        // host-independent: linux cfgs regardless of where the test runs.
        let cfg = TargetCfg::from_rustc("x86_64-unknown-linux-gnu")
            .expect("rustc knows the linux triple");
        assert!(cfg.matches_target_field("cfg(unix)"));
        assert!(cfg.matches_target_field("cfg(target_os = \"linux\")"));
        assert!(cfg.matches_target_field("cfg(target_arch = \"x86_64\")"));
        assert!(!cfg.matches_target_field("cfg(windows)"));
        assert!(!cfg.matches_target_field("cfg(target_os = \"macos\")"));
    }

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

    #[test]
    fn fixed_target_projection_keeps_workspace_dev_and_drops_false_targets() {
        let empty = BTreeSet::new();
        let workspace = BTreeSet::from(["member".to_owned()]);

        assert!(!fixed_target_runtime_dependency_is_consumed(
            "external", "dev", None, &empty
        ));
        assert!(fixed_target_runtime_dependency_is_consumed(
            "member", "dev", None, &workspace
        ));
        assert!(!fixed_target_runtime_dependency_is_consumed(
            "member",
            "normal",
            Some("cfg(windows)"),
            &workspace
        ));
        assert!(fixed_target_runtime_dependency_is_consumed(
            "member",
            "normal",
            Some("cfg(unix)"),
            &workspace
        ));
    }
}
