//! Cargo-shaped version resolution over ecosystem-neutral package facts.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::fmt::{self, Display};
use std::fs;
use std::path::{Path, PathBuf};

use eyre::{Context, Result, bail, eyre};
use facet::Facet;
use pubgrub::{
    DefaultStringReporter, Dependencies, DependencyConstraints, DependencyProvider, PubGrubError,
    Reporter as _, resolve,
};
use semver::{Version, VersionReq};
use thiserror::Error;
use version_ranges::Ranges;

type VersionSet = Ranges<Version>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyFact {
    pub name: String,
    pub req: String,
    pub features: Vec<String>,
    pub optional: bool,
    pub default_features: bool,
    pub kind: DependencyKind,
    pub package: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DependencyKind {
    Normal,
    Dev,
    Build,
    Other(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VersionMetadata {
    pub deps: Vec<DependencyFact>,
    pub features: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FeatureExclusion {
    pub package: String,
    pub features: BTreeSet<String>,
}

pub trait Facts {
    fn versions_of(&self, name: &str) -> Result<Vec<Version>>;
    fn deps_of(&self, name: &str, version: &Version) -> Result<VersionMetadata>;
    fn feature_exclusions(&self) -> Vec<FeatureExclusion> {
        Vec::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootDependency {
    pub name: String,
    pub req: String,
    pub features: Vec<String>,
    pub default_features: bool,
    pub package: Option<String>,
}

impl RootDependency {
    #[must_use]
    pub fn new(name: impl Into<String>, req: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            req: req.into(),
            features: Vec::new(),
            default_features: true,
            package: None,
        }
    }

    #[must_use]
    pub fn feature(mut self, feature: impl Into<String>) -> Self {
        self.features.push(feature.into());
        self
    }

    #[must_use]
    pub fn default_features(mut self, default_features: bool) -> Self {
        self.default_features = default_features;
        self
    }

    #[must_use]
    pub fn package(mut self, package: impl Into<String>) -> Self {
        self.package = Some(package.into());
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPackage {
    pub name: String,
    pub version: Version,
    pub features: BTreeSet<String>,
    pub deps: Vec<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SolveResult {
    pub graph: Vec<ResolvedPackage>,
    pub deferred_weak_dependencies: Vec<DeferredWeakDependency>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeferredWeakDependency {
    pub package: String,
    pub version: Version,
    pub feature: String,
    pub target: String,
    pub target_feature: String,
}

#[derive(Debug, Error)]
pub enum SolveError {
    #[error("{report}")]
    NoSolution {
        report: String,
        deferred_weak_dependencies: Vec<DeferredWeakDependency>,
    },
    #[error(transparent)]
    Facts(#[from] eyre::Report),
}

pub fn solve(
    facts: &impl Facts,
    root_deps: Vec<RootDependency>,
) -> Result<SolveResult, SolveError> {
    let provider = PubgrubProvider::new(facts, root_deps)?;
    let solution = match resolve(&provider, PackageId::Root, root_version()) {
        Ok(solution) => solution,
        Err(PubGrubError::NoSolution(mut tree)) => {
            tree.collapse_no_versions();
            return Err(SolveError::NoSolution {
                report: provider.decorate_report(DefaultStringReporter::report(&tree)),
                deferred_weak_dependencies: provider.deferred_weak_dependencies,
            });
        }
        Err(err) => return Err(SolveError::Facts(eyre!("{err:?}"))),
    };
    provider.collapse(solution)
}

#[derive(Clone, Debug)]
pub struct FixtureFacts {
    packages: BTreeMap<String, BTreeMap<Version, VersionMetadata>>,
    exclusions: Vec<FeatureExclusion>,
}

impl FixtureFacts {
    pub fn from_registry_root(root: impl AsRef<Path>) -> Result<Self> {
        let index_root = root.as_ref().join("index");
        let mut packages: BTreeMap<String, BTreeMap<Version, VersionMetadata>> = BTreeMap::new();
        for path in index_files(&index_root)? {
            if path.file_name().and_then(|name| name.to_str()) == Some("config.json") {
                continue;
            }
            let content = fs::read_to_string(&path)
                .wrap_err_with(|| format!("reading fixture index file {}", path.display()))?;
            for line in content.lines().filter(|line| !line.trim().is_empty()) {
                let entry: IndexEntry = facet_json::from_str(line).wrap_err_with(|| {
                    format!("parsing fixture index line from {}", path.display())
                })?;
                if entry.yanked {
                    continue;
                }
                let version = Version::parse(&entry.vers).wrap_err_with(|| {
                    format!("parsing version {} for {}", entry.vers, entry.name)
                })?;
                let deps = entry.deps.into_iter().map(DependencyFact::from).collect();
                packages.entry(entry.name).or_default().insert(
                    version,
                    VersionMetadata {
                        deps,
                        features: entry.features,
                    },
                );
            }
        }
        Ok(Self {
            packages,
            exclusions: Vec::new(),
        })
    }

    #[must_use]
    pub fn with_feature_exclusion(
        mut self,
        package: impl Into<String>,
        features: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.exclusions.push(FeatureExclusion {
            package: package.into(),
            features: features.into_iter().map(Into::into).collect(),
        });
        self
    }
}

impl Facts for FixtureFacts {
    fn versions_of(&self, name: &str) -> Result<Vec<Version>> {
        Ok(self
            .packages
            .get(name)
            .map(|versions| versions.keys().cloned().collect())
            .unwrap_or_default())
    }

    fn deps_of(&self, name: &str, version: &Version) -> Result<VersionMetadata> {
        self.packages
            .get(name)
            .and_then(|versions| versions.get(version))
            .cloned()
            .ok_or_else(|| eyre!("missing fixture metadata for {name} {version}"))
    }

    fn feature_exclusions(&self) -> Vec<FeatureExclusion> {
        self.exclusions.clone()
    }
}

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
    cksum: String,
    features: BTreeMap<String, Vec<String>>,
    yanked: bool,
    links: Option<String>,
    v: u32,
}

impl From<IndexDependency> for DependencyFact {
    fn from(dep: IndexDependency) -> Self {
        let kind = match dep.kind.as_str() {
            "normal" => DependencyKind::Normal,
            "dev" => DependencyKind::Dev,
            "build" => DependencyKind::Build,
            other => DependencyKind::Other(other.to_owned()),
        };
        Self {
            name: dep.name,
            req: dep.req,
            features: dep.features,
            optional: dep.optional,
            default_features: dep.default_features,
            kind,
            package: dep.package,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum PackageId {
    Root,
    Crate(String),
    Feature {
        package: String,
        feature: String,
    },
    Conflict {
        package: String,
        left: String,
        right: String,
    },
}

impl Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root => f.write_str("root"),
            Self::Crate(name) => f.write_str(name),
            Self::Feature { package, feature } => write!(f, "{package}/{feature}"),
            Self::Conflict {
                package,
                left,
                right,
            } => {
                write!(f, "{package}/!conflict({left},{right})")
            }
        }
    }
}

#[derive(Clone, Debug)]
struct PubgrubProvider<'a, F> {
    facts: &'a F,
    root_deps: Vec<RootDependency>,
    exclusions: Vec<EncodedExclusion>,
    deferred_weak_dependencies: Vec<DeferredWeakDependency>,
}

#[derive(Clone, Debug)]
struct EncodedExclusion {
    package: String,
    left: String,
    right: String,
}

impl<'a, F: Facts> PubgrubProvider<'a, F> {
    fn new(facts: &'a F, root_deps: Vec<RootDependency>) -> Result<Self, SolveError> {
        let mut exclusions = Vec::new();
        for exclusion in facts.feature_exclusions() {
            let features: Vec<_> = exclusion.features.into_iter().collect();
            for (index, left) in features.iter().enumerate() {
                for right in features.iter().skip(index + 1) {
                    exclusions.push(EncodedExclusion {
                        package: exclusion.package.clone(),
                        left: left.clone(),
                        right: right.clone(),
                    });
                }
            }
        }
        Ok(Self {
            facts,
            root_deps,
            exclusions,
            deferred_weak_dependencies: Vec::new(),
        })
    }

    fn collapse(
        self,
        solution: pubgrub::SelectedDependencies<PackageId, Version>,
    ) -> Result<SolveResult, SolveError> {
        let selected: BTreeMap<_, _> = solution.into_iter().collect();
        let mut graph = Vec::new();
        let mut indices = BTreeMap::new();
        for (package, version) in &selected {
            let PackageId::Crate(name) = package else {
                continue;
            };
            let index = graph.len();
            indices.insert(name.clone(), index);
            graph.push(ResolvedPackage {
                name: name.clone(),
                version: version.clone(),
                features: BTreeSet::new(),
                deps: Vec::new(),
            });
        }

        let mut active_optional_deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for package in selected.keys() {
            let PackageId::Feature { package, feature } = package else {
                continue;
            };
            if let Some(index) = indices.get(package).copied() {
                graph[index].features.insert(feature.clone());
                if let Some(version) = selected.get(&PackageId::Crate(package.clone())) {
                    let metadata = self.facts.deps_of(package, version)?;
                    collect_optional_deps_from_feature(
                        package,
                        version,
                        feature,
                        &metadata,
                        &mut active_optional_deps,
                    );
                }
            }
        }

        for package in &mut graph {
            let metadata = self.facts.deps_of(&package.name, &package.version)?;
            let mut dep_indices = BTreeSet::new();
            for dep in &metadata.deps {
                if dep.kind != DependencyKind::Normal {
                    continue;
                }
                if dep.optional
                    && !active_optional_deps
                        .get(&package.name)
                        .is_some_and(|deps| deps.contains(&dep.name))
                {
                    continue;
                }
                let actual = dep.package.as_ref().unwrap_or(&dep.name);
                if let Some(index) = indices.get(actual) {
                    dep_indices.insert(*index);
                }
            }
            package.deps = dep_indices.into_iter().collect();
        }

        Ok(SolveResult {
            graph,
            deferred_weak_dependencies: self.deferred_weak_dependencies,
        })
    }

    fn decorate_report(&self, mut report: String) -> String {
        for exclusion in &self.exclusions {
            let left = PackageId::Feature {
                package: exclusion.package.clone(),
                feature: exclusion.left.clone(),
            };
            let right = PackageId::Feature {
                package: exclusion.package.clone(),
                feature: exclusion.right.clone(),
            };
            if report.contains(&left.to_string()) && report.contains(&right.to_string()) {
                report.push_str(&format!(
                    "\nfeature exclusion conflict: {} features {} and {} cannot both be active",
                    exclusion.package, exclusion.left, exclusion.right
                ));
            }
        }
        report
    }
}

impl<F: Facts> DependencyProvider for PubgrubProvider<'_, F> {
    type P = PackageId;
    type V = Version;
    type VS = VersionSet;
    type Priority = (u32, Reverse<usize>, Reverse<PackageId>);
    type M = String;
    type Err = Infallible;

    fn prioritize(
        &self,
        package: &Self::P,
        range: &Self::VS,
        package_statistics: &pubgrub::PackageResolutionStatistics,
    ) -> Self::Priority {
        let version_count = self
            .available_versions(package)
            .map(|versions| {
                versions
                    .into_iter()
                    .filter(|version| range.contains(version))
                    .count()
            })
            .unwrap_or(0);
        (
            package_statistics.conflict_count(),
            Reverse(version_count),
            Reverse(package.clone()),
        )
    }

    fn choose_version(
        &self,
        package: &Self::P,
        range: &Self::VS,
    ) -> Result<Option<Self::V>, Self::Err> {
        Ok(self.available_versions(package).and_then(|mut versions| {
            versions.sort();
            versions
                .into_iter()
                .rev()
                .find(|version| range.contains(version))
        }))
    }

    fn get_dependencies(
        &self,
        package: &Self::P,
        version: &Self::V,
    ) -> Result<Dependencies<Self::P, Self::VS, Self::M>, Self::Err> {
        let result = match self.dependencies_for(package, version) {
            Ok(deps) => Dependencies::Available(deps.into_iter().collect()),
            Err(err) => Dependencies::Unavailable(err.to_string()),
        };
        Ok(result)
    }
}

impl<F: Facts> PubgrubProvider<'_, F> {
    fn available_versions(&self, package: &PackageId) -> Option<Vec<Version>> {
        match package {
            PackageId::Root => Some(vec![root_version()]),
            PackageId::Crate(name) | PackageId::Feature { package: name, .. } => {
                self.facts.versions_of(name).ok()
            }
            PackageId::Conflict { .. } => {
                Some(vec![conflict_version(false), conflict_version(true)])
            }
        }
    }

    fn dependencies_for(
        &self,
        package: &PackageId,
        version: &Version,
    ) -> Result<Vec<(PackageId, VersionSet)>> {
        match package {
            PackageId::Root => root_dependencies(&self.root_deps),
            PackageId::Crate(name) => {
                let metadata = self.facts.deps_of(name, version)?;
                package_dependencies(&metadata)
            }
            PackageId::Feature { package, feature } => {
                let metadata = self.facts.deps_of(package, version)?;
                feature_dependencies(package, version, feature, &metadata, &self.exclusions)
            }
            PackageId::Conflict { .. } => Ok(Vec::new()),
        }
    }
}

fn root_dependencies(root_deps: &[RootDependency]) -> Result<Vec<(PackageId, VersionSet)>> {
    let mut deps = Vec::new();
    for dep in root_deps {
        let actual = dep.package.as_ref().unwrap_or(&dep.name);
        let req = version_req(&dep.req)?;
        deps.push((PackageId::Crate(actual.clone()), req.clone()));
        if dep.default_features {
            deps.push((
                PackageId::Feature {
                    package: actual.clone(),
                    feature: "default".to_owned(),
                },
                req.clone(),
            ));
        }
        for feature in &dep.features {
            deps.push((
                PackageId::Feature {
                    package: actual.clone(),
                    feature: feature.clone(),
                },
                req.clone(),
            ));
        }
    }
    Ok(deps)
}

fn package_dependencies(metadata: &VersionMetadata) -> Result<Vec<(PackageId, VersionSet)>> {
    let mut deps = Vec::new();
    for dep in &metadata.deps {
        if dep.kind != DependencyKind::Normal || dep.optional {
            continue;
        }
        add_dependency_activation(&mut deps, dep)?;
    }
    Ok(deps)
}

fn feature_dependencies(
    package: &str,
    version: &Version,
    feature: &str,
    metadata: &VersionMetadata,
    exclusions: &[EncodedExclusion],
) -> Result<Vec<(PackageId, VersionSet)>> {
    let mut deps = vec![(
        PackageId::Crate(package.to_owned()),
        VersionSet::singleton(version.clone()),
    )];

    for exclusion in exclusions
        .iter()
        .filter(|exclusion| exclusion.package == package)
    {
        if feature == exclusion.left {
            deps.push((
                PackageId::Conflict {
                    package: package.to_owned(),
                    left: exclusion.left.clone(),
                    right: exclusion.right.clone(),
                },
                VersionSet::singleton(conflict_version(false)),
            ));
        } else if feature == exclusion.right {
            deps.push((
                PackageId::Conflict {
                    package: package.to_owned(),
                    left: exclusion.left.clone(),
                    right: exclusion.right.clone(),
                },
                VersionSet::singleton(conflict_version(true)),
            ));
        }
    }

    let Some(enables) = metadata.features.get(feature) else {
        if feature == "default" {
            return Ok(deps);
        }
        bail!("feature {package}/{feature} is not declared");
    };

    for enable in enables {
        if let Some(dep_name) = enable.strip_prefix("dep:") {
            let dep = dependency_by_key(metadata, dep_name)?;
            add_dependency_activation(&mut deps, dep)?;
        } else if let Some((dep_name, dep_feature)) = enable.split_once("?/") {
            tracing::debug!(
                package,
                version = %version,
                feature,
                dependency = dep_name,
                dependency_feature = dep_feature,
                "deferring weak feature dependency; pubgrub dependency constraints have no guard for already-active packages"
            );
        } else if let Some((dep_name, dep_feature)) = enable.split_once('/') {
            let dep = dependency_by_key(metadata, dep_name)?;
            add_dependency_activation(&mut deps, dep)?;
            let actual = dep.package.as_ref().unwrap_or(&dep.name);
            deps.push((
                PackageId::Feature {
                    package: actual.clone(),
                    feature: dep_feature.to_owned(),
                },
                version_req(&dep.req)?,
            ));
        } else {
            deps.push((
                PackageId::Feature {
                    package: package.to_owned(),
                    feature: enable.clone(),
                },
                VersionSet::singleton(version.clone()),
            ));
        }
    }
    Ok(deps)
}

fn collect_optional_deps_from_feature(
    package: &str,
    version: &Version,
    feature: &str,
    metadata: &VersionMetadata,
    active_optional_deps: &mut BTreeMap<String, BTreeSet<String>>,
) {
    let Some(enables) = metadata.features.get(feature) else {
        return;
    };
    for enable in enables {
        let dep_name = enable
            .strip_prefix("dep:")
            .or_else(|| enable.split_once('/').map(|(dep, _)| dep));
        let Some(dep_name) = dep_name else {
            continue;
        };
        if dependency_by_key(metadata, dep_name).is_ok_and(|dep| dep.optional) {
            active_optional_deps
                .entry(package.to_owned())
                .or_default()
                .insert(dep_name.to_owned());
        }
    }
    tracing::trace!(package, version = %version, feature, "collected feature dependency edges");
}

fn add_dependency_activation(
    deps: &mut Vec<(PackageId, VersionSet)>,
    dep: &DependencyFact,
) -> Result<()> {
    let actual = dep.package.as_ref().unwrap_or(&dep.name);
    let req = version_req(&dep.req)?;
    deps.push((PackageId::Crate(actual.clone()), req.clone()));
    if dep.default_features {
        deps.push((
            PackageId::Feature {
                package: actual.clone(),
                feature: "default".to_owned(),
            },
            req.clone(),
        ));
    }
    for feature in &dep.features {
        deps.push((
            PackageId::Feature {
                package: actual.clone(),
                feature: feature.clone(),
            },
            req.clone(),
        ));
    }
    Ok(())
}

fn dependency_by_key<'a>(metadata: &'a VersionMetadata, name: &str) -> Result<&'a DependencyFact> {
    metadata
        .deps
        .iter()
        .find(|dep| dep.name == name)
        .ok_or_else(|| eyre!("feature references dependency {name}, but no such dependency exists"))
}

fn version_req(req: &str) -> Result<VersionSet> {
    let parsed = VersionReq::parse(req).wrap_err_with(|| format!("parsing version req {req}"))?;
    Ok(VersionSet::from_req(parsed))
}

fn root_version() -> Version {
    Version::new(0, 0, 0)
}

fn conflict_version(right: bool) -> Version {
    Version::new(u64::from(right), 0, 0)
}

fn index_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(&path).wrap_err_with(|| format!("reading {}", path.display()))? {
            let entry =
                entry.wrap_err_with(|| format!("reading entry under {}", path.display()))?;
            let entry_path = entry.path();
            if entry_path.is_dir() {
                stack.push(entry_path);
            } else if entry_path.is_file() {
                files.push(entry_path);
            }
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use pretty_assertions::assert_eq;
    use reg_mock::{
        CargoLock, Dependency, OracleProject, Package, ProjectDependency, RegistryBuilder,
    };

    use super::*;

    #[test]
    fn unique_solution_matches_cargo_lockfile_package_set() -> Result<()> {
        let fixture = RegistryBuilder::new()
            .package(Package::new("root", "1.0.0").dep(Dependency::normal("leaf", "=1.0.0")))
            .package(Package::new("leaf", "1.0.0"))
            .package(Package::new("leaf", "2.0.0"))
            .write()?;
        let facts = FixtureFacts::from_registry_root(fixture.root())?;
        let server = fixture.serve()?;
        let cargo = server.oracle().generate_lockfile(
            &OracleProject::new().dependency(ProjectDependency::new("root", "=1.0.0")),
        )?;

        let solved = solve(&facts, vec![RootDependency::new("root", "=1.0.0")])?;

        assert_eq!(package_set(&solved), lock_package_set(&cargo));
        Ok(())
    }

    #[test]
    fn diamond_features_and_optional_deps_match_cargo_lockfile_package_set() -> Result<()> {
        let fixture = RegistryBuilder::new()
            .package(
                Package::new("left", "1.0.0")
                    .dep(Dependency::normal("shared", "=1.0.0"))
                    .dep(Dependency::normal("addon", "=1.0.0").optional(true))
                    .feature("default", Vec::<String>::new())
                    .feature("with-addon", ["dep:addon", "addon/extra"]),
            )
            .package(Package::new("right", "1.0.0").dep(Dependency::normal("shared", "=1.0.0")))
            .package(Package::new("shared", "1.0.0").feature("default", Vec::<String>::new()))
            .package(
                Package::new("addon", "1.0.0")
                    .feature("default", Vec::<String>::new())
                    .feature("extra", Vec::<String>::new()),
            )
            .write()?;
        let facts = FixtureFacts::from_registry_root(fixture.root())?;
        let server = fixture.serve()?;
        let cargo = server.oracle().generate_lockfile(
            &OracleProject::new()
                .dependency(ProjectDependency::new("left", "=1.0.0").feature("with-addon"))
                .dependency(ProjectDependency::new("right", "=1.0.0")),
        )?;

        let solved = solve(
            &facts,
            vec![
                RootDependency::new("left", "=1.0.0").feature("with-addon"),
                RootDependency::new("right", "=1.0.0"),
            ],
        )?;

        assert_eq!(package_set(&solved), lock_package_set(&cargo));
        assert!(features_of(&solved, "left").contains("with-addon"));
        assert!(features_of(&solved, "addon").contains("extra"));
        Ok(())
    }

    #[test]
    fn multiple_solution_is_valid_and_deterministic() -> Result<()> {
        let fixture = RegistryBuilder::new()
            .package(
                Package::new("alpha", "1.0.0").dep(Dependency::normal("choice", ">=1.0.0, <3.0.0")),
            )
            .package(
                Package::new("beta", "1.0.0").dep(Dependency::normal("choice", ">=1.0.0, <3.0.0")),
            )
            .package(Package::new("choice", "1.0.0"))
            .package(Package::new("choice", "2.0.0"))
            .write()?;
        let facts = FixtureFacts::from_registry_root(fixture.root())?;

        let root = vec![
            RootDependency::new("alpha", "=1.0.0"),
            RootDependency::new("beta", "=1.0.0"),
        ];
        let first = solve(&facts, root.clone())?;
        let second = solve(&facts, root)?;

        assert_eq!(first, second);
        assert_solution_valid(&facts, &first)?;
        // Cargo currently also picks choice 2.0.0 for this fixture; do not require
        // equality here because this resolver's package priority is intentionally
        // only deterministic, not Cargo's full heuristic.
        assert_eq!(version_of(&first, "choice"), Some(Version::new(2, 0, 0)));
        Ok(())
    }

    #[test]
    fn feature_exclusion_conflict_names_both_demanding_packages() -> Result<()> {
        let fixture = RegistryBuilder::new()
            .package(
                Package::new("left", "1.0.0")
                    .dep(Dependency::normal("mode", "=1.0.0").feature("fast")),
            )
            .package(
                Package::new("right", "1.0.0")
                    .dep(Dependency::normal("mode", "=1.0.0").feature("small")),
            )
            .package(
                Package::new("mode", "1.0.0")
                    .feature("default", Vec::<String>::new())
                    .feature("fast", Vec::<String>::new())
                    .feature("small", Vec::<String>::new()),
            )
            .write()?;
        let facts = FixtureFacts::from_registry_root(fixture.root())?
            .with_feature_exclusion("mode", ["fast", "small"]);

        let error = solve(
            &facts,
            vec![
                RootDependency::new("left", "=1.0.0"),
                RootDependency::new("right", "=1.0.0"),
            ],
        )
        .expect_err("conflicting features should fail");
        let message = error.to_string();

        assert!(message.contains("left"), "{message}");
        assert!(message.contains("right"), "{message}");
        assert!(message.contains("mode/fast"), "{message}");
        assert!(message.contains("mode/small"), "{message}");
        Ok(())
    }

    fn package_set(solved: &SolveResult) -> BTreeSet<(String, String)> {
        solved
            .graph
            .iter()
            .map(|package| (package.name.clone(), package.version.to_string()))
            .collect()
    }

    fn lock_package_set(lock: &CargoLock) -> BTreeSet<(String, String)> {
        lock.packages
            .iter()
            .filter(|package| package.name != "oracle-project")
            .map(|package| (package.name.clone(), package.version.clone()))
            .collect()
    }

    fn features_of(solved: &SolveResult, name: &str) -> BTreeSet<String> {
        solved
            .graph
            .iter()
            .find(|package| package.name == name)
            .map(|package| package.features.clone())
            .unwrap_or_default()
    }

    fn version_of(solved: &SolveResult, name: &str) -> Option<Version> {
        solved
            .graph
            .iter()
            .find(|package| package.name == name)
            .map(|package| package.version.clone())
    }

    fn assert_solution_valid(facts: &FixtureFacts, solved: &SolveResult) -> Result<()> {
        let versions: BTreeMap<_, _> = solved
            .graph
            .iter()
            .map(|package| (package.name.as_str(), &package.version))
            .collect();
        for package in &solved.graph {
            let metadata = facts.deps_of(&package.name, &package.version)?;
            for dep in metadata.deps.iter().filter(|dep| !dep.optional) {
                let actual = dep.package.as_ref().unwrap_or(&dep.name);
                let version = versions
                    .get(actual.as_str())
                    .ok_or_else(|| eyre!("missing dependency {actual}"))?;
                let req = VersionReq::parse(&dep.req)?;
                if !req.matches(version) {
                    bail!(
                        "{} {} violates {} {}",
                        actual,
                        version,
                        package.name,
                        dep.req
                    );
                }
            }
        }
        Ok(())
    }
}
