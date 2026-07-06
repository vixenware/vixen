//! An isolated differential-fixture corpus for the rodin resolver.
//!
//! Each fixture is a tiny **path-dependency Cargo workspace** describing exactly
//! one resolver behavior (a `cfg(any())` never-dep, a weak `dep?/feat`, a
//! feature-activated target-conditional optional dep, build/normal feature
//! isolation, …). Because every dependency is a local path crate, cargo resolves
//! it offline and instantly, with no registry and no network. The same fixture
//! definition drives two consumers:
//!
//! * **cargo** — the oracle. `cargo tree -e normal,build --target <triple>` gives
//!   the per-target dependency graph cargo would build.
//! * **rodin** — the system under test. The identical crate/feature/target
//!   structure is fed to a [`CompiledIndex`] and solved with a per-target
//!   [`ResolveContext`].
//!
//! A fixture check asserts rodin's per-target selection equals cargo's. Fast,
//! isolated, and each fixture pinpoints one failure mode — the opposite of one
//! giant whole-workspace oracle where every behavior is tangled together.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use eyre::{Result, bail};
use rodin_core::{CompiledIndex, Problem, ResolveContext, RootReq, SyntheticDep};
use rodin_survey::TargetCfg;
use semver::Version;

/// Which dependency table a dependency lives in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DepKind {
    Normal,
    Build,
    Dev,
}

impl DepKind {
    fn cargo_table(self) -> &'static str {
        match self {
            Self::Normal => "dependencies",
            Self::Build => "build-dependencies",
            Self::Dev => "dev-dependencies",
        }
    }

    fn rodin_kind(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Build => "build",
            Self::Dev => "dev",
        }
    }
}

/// A single dependency edge in a fixture crate.
#[derive(Clone, Debug)]
pub struct FixtureDep {
    pub name: String,
    pub kind: DepKind,
    pub optional: bool,
    pub default_features: bool,
    pub features: Vec<String>,
    /// `cfg(...)` expression or an explicit target triple; `None` = unconditional.
    pub target: Option<String>,
}

impl FixtureDep {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: DepKind::Normal,
            optional: false,
            default_features: true,
            features: Vec::new(),
            target: None,
        }
    }

    #[must_use]
    pub fn kind(mut self, kind: DepKind) -> Self {
        self.kind = kind;
        self
    }

    #[must_use]
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    #[must_use]
    pub fn no_default_features(mut self) -> Self {
        self.default_features = false;
        self
    }

    #[must_use]
    pub fn feature(mut self, feature: impl Into<String>) -> Self {
        self.features.push(feature.into());
        self
    }

    #[must_use]
    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }
}

/// A crate in a fixture workspace.
#[derive(Clone, Debug)]
pub struct FixtureCrate {
    pub name: String,
    pub features: BTreeMap<String, Vec<String>>,
    pub deps: Vec<FixtureDep>,
}

impl FixtureCrate {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            features: BTreeMap::new(),
            deps: Vec::new(),
        }
    }

    #[must_use]
    pub fn feature(mut self, name: impl Into<String>, enables: &[&str]) -> Self {
        self.features
            .insert(name.into(), enables.iter().map(|s| (*s).to_owned()).collect());
        self
    }

    #[must_use]
    pub fn dep(mut self, dep: FixtureDep) -> Self {
        self.deps.push(dep);
        self
    }
}

/// A whole fixture: a set of crates, the root to build, its requested features,
/// and the targets to check.
#[derive(Clone, Debug)]
pub struct Fixture {
    pub name: String,
    pub crates: Vec<FixtureCrate>,
    pub root: String,
    pub root_default_features: bool,
    pub root_features: Vec<String>,
}

impl Fixture {
    pub fn new(name: impl Into<String>, root: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            crates: Vec::new(),
            root: root.into(),
            root_default_features: true,
            root_features: Vec::new(),
        }
    }

    #[must_use]
    pub fn krate(mut self, krate: FixtureCrate) -> Self {
        self.crates.push(krate);
        self
    }

    #[must_use]
    pub fn root_feature(mut self, feature: impl Into<String>) -> Self {
        self.root_features.push(feature.into());
        self
    }

    /// Assert rodin's per-target selection equals cargo's. Returns the shared
    /// selection on success; on mismatch, an error naming the symmetric diff.
    pub fn assert_selection_matches(&self, triple: &str) -> Result<BTreeSet<String>> {
        let workspace = self.materialize()?;
        let cargo = cargo_selected(&workspace.root, &self.root, triple)?;
        let rodin = self.rodin_selected(triple)?;
        if cargo != rodin {
            let only_cargo: Vec<_> = cargo.difference(&rodin).cloned().collect();
            let only_rodin: Vec<_> = rodin.difference(&cargo).cloned().collect();
            bail!(
                "fixture `{}` selection diverged for {triple}:\n  cargo-only (rodin missed): {only_cargo:?}\n  rodin-only (rodin over-selected): {only_rodin:?}",
                self.name
            );
        }
        Ok(cargo)
    }

    /// Write the fixture as a real path-dependency Cargo workspace in a fresh
    /// temp directory.
    fn materialize(&self) -> Result<MaterializedWorkspace> {
        let root = unique_temp_dir(&self.name);
        std::fs::create_dir_all(&root)?;
        let mut members = String::new();
        for krate in &self.crates {
            writeln!(members, "    \"{}\",", krate.name).ok();
        }
        let workspace_toml =
            format!("[workspace]\nresolver = \"2\"\nmembers = [\n{members}]\n");
        std::fs::write(root.join("Cargo.toml"), workspace_toml)?;

        for krate in &self.crates {
            let dir = root.join(&krate.name);
            std::fs::create_dir_all(dir.join("src"))?;
            std::fs::write(dir.join("src").join("lib.rs"), "")?;
            std::fs::write(dir.join("Cargo.toml"), self.crate_manifest(krate))?;
        }
        Ok(MaterializedWorkspace { root })
    }

    fn crate_manifest(&self, krate: &FixtureCrate) -> String {
        let mut toml = String::new();
        writeln!(toml, "[package]").ok();
        writeln!(toml, "name = \"{}\"", krate.name).ok();
        writeln!(toml, "version = \"0.1.0\"").ok();
        writeln!(toml, "edition = \"2021\"").ok();

        if !krate.features.is_empty() {
            writeln!(toml, "\n[features]").ok();
            for (feature, enables) in &krate.features {
                let list = enables
                    .iter()
                    .map(|e| format!("\"{e}\""))
                    .collect::<Vec<_>>()
                    .join(", ");
                writeln!(toml, "{feature} = [{list}]").ok();
            }
        }

        // Group deps by (target, kind) into the correct cargo table.
        let mut tables: BTreeMap<(Option<String>, &'static str), Vec<&FixtureDep>> = BTreeMap::new();
        for dep in &krate.deps {
            tables
                .entry((dep.target.clone(), dep.kind.cargo_table()))
                .or_default()
                .push(dep);
        }
        for ((target, table), deps) in &tables {
            let header = match target {
                None => format!("[{table}]"),
                Some(cfg) => format!("[target.'{cfg}'.{table}]"),
            };
            writeln!(toml, "\n{header}").ok();
            for dep in deps {
                writeln!(toml, "{}", dep_line(dep)).ok();
            }
        }
        toml
    }

    /// Build a [`CompiledIndex`] from the same fixture definition and solve it
    /// for `triple`, returning the selected crate names.
    fn rodin_selected(&self, triple: &str) -> Result<BTreeSet<String>> {
        let mut index = CompiledIndex::empty();
        let version = Version::new(0, 1, 0);
        for krate in &self.crates {
            let deps = krate.deps.iter().map(synthetic_dep).collect();
            index.add_package_version(
                krate.name.clone(),
                version.clone(),
                deps,
                krate.features.clone(),
            );
        }
        index.finish_synthetic()?;

        let mut root = RootReq::new(self.root.clone(), "*");
        for feature in &self.root_features {
            root = root.feature(feature.clone());
        }
        // NB: default features on the root are on unless a fixture opts out; the
        // resolver models a build of the root member.
        let target = TargetCfg::from_rustc(triple)?;
        let problem = Problem::new(vec![root]).with_context(ResolveContext::for_target(target));
        let result =
            rodin_core::solve(&index, problem).map_err(|err| eyre::eyre!("{err:?}"))?;
        Ok(result
            .selected
            .keys()
            .filter_map(|label| label.rsplit_once('@').map(|(name, _)| name.to_owned()))
            .collect())
    }
}

struct MaterializedWorkspace {
    root: PathBuf,
}

fn dep_line(dep: &FixtureDep) -> String {
    let mut attrs = vec![
        format!("path = \"../{}\"", dep.name),
        "version = \"0.1.0\"".to_owned(),
    ];
    if dep.optional {
        attrs.push("optional = true".to_owned());
    }
    if !dep.default_features {
        attrs.push("default-features = false".to_owned());
    }
    if !dep.features.is_empty() {
        let list = dep
            .features
            .iter()
            .map(|f| format!("\"{f}\""))
            .collect::<Vec<_>>()
            .join(", ");
        attrs.push(format!("features = [{list}]"));
    }
    format!("{} = {{ {} }}", dep.name, attrs.join(", "))
}

fn synthetic_dep(dep: &FixtureDep) -> SyntheticDep {
    let mut synthetic = SyntheticDep::normal(dep.name.clone(), "=0.1.0")
        .optional(dep.optional)
        .kind(dep.kind.rodin_kind());
    synthetic.default_features = dep.default_features;
    synthetic.features = dep.features.clone();
    if let Some(target) = &dep.target {
        synthetic = synthetic.target(target.clone());
    }
    synthetic
}

/// The set of crate names cargo includes in the graph for `triple`, offline.
pub fn cargo_selected(workspace: &Path, root: &str, triple: &str) -> Result<BTreeSet<String>> {
    let output = Command::new("cargo")
        .args([
            "tree",
            "-e",
            "normal,build",
            "--target",
            triple,
            "--prefix",
            "none",
            "--offline",
            "-p",
            root,
        ])
        .current_dir(workspace)
        .output()?;
    if !output.status.success() {
        bail!(
            "cargo tree failed for {root} on {triple}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let text = String::from_utf8(output.stdout)?;
    Ok(text
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect())
}

fn unique_temp_dir(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nonce = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "rodin-fixture-{name}-{}-{nonce}",
        std::process::id()
    ))
}
