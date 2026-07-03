//! Mock sparse registry fixtures backed by Cargo as the resolution oracle.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Cursor, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use eyre::{Context, Result, bail, eyre};
use facet::Facet;
use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tiny_http::{Header, Method, Request, Response, Server};

#[derive(Clone, Debug, Eq, Facet, PartialEq)]
struct IndexConfig {
    dl: String,
    api: String,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Package {
    name: String,
    version: String,
    deps: Vec<Dependency>,
    features: BTreeMap<String, Vec<String>>,
    yanked: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dependency {
    name: String,
    req: String,
    features: Vec<String>,
    optional: bool,
    default_features: bool,
    target: Option<String>,
    kind: DependencyKind,
    package: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum DependencyKind {
    #[default]
    Normal,
    Dev,
    Build,
}

#[derive(Debug)]
pub struct RegistryBuilder {
    packages: Vec<Package>,
}

#[derive(Debug)]
pub struct RegistryFixture {
    root: TempDir,
}

#[derive(Debug)]
pub struct MockRegistryServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    _fixture: RegistryFixture,
}

#[derive(Debug)]
pub struct CargoOracle {
    registry_url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectDependency {
    name: String,
    req: String,
    features: Vec<String>,
    default_features: bool,
    package: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OracleProject {
    dependencies: Vec<ProjectDependency>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
    pub source: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CargoLock {
    pub packages: BTreeSet<LockedPackage>,
    pub raw: String,
}

impl Package {
    #[must_use]
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            deps: Vec::new(),
            features: BTreeMap::new(),
            yanked: false,
        }
    }

    #[must_use]
    pub fn dep(mut self, dep: Dependency) -> Self {
        self.deps.push(dep);
        self
    }

    #[must_use]
    pub fn feature(
        mut self,
        name: impl Into<String>,
        enables: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.features
            .insert(name.into(), enables.into_iter().map(Into::into).collect());
        self
    }

    #[must_use]
    pub fn yanked(mut self, yanked: bool) -> Self {
        self.yanked = yanked;
        self
    }

    fn index_entry(&self, checksum: String) -> IndexEntry {
        IndexEntry {
            name: self.name.clone(),
            vers: self.version.clone(),
            deps: self.deps.iter().map(Dependency::index_dependency).collect(),
            cksum: checksum,
            features: self.features.clone(),
            yanked: self.yanked,
            links: None,
            v: 2,
        }
    }
}

impl Dependency {
    #[must_use]
    pub fn normal(name: impl Into<String>, req: impl Into<String>) -> Self {
        Self::new(name, req, DependencyKind::Normal)
    }

    #[must_use]
    pub fn build(name: impl Into<String>, req: impl Into<String>) -> Self {
        Self::new(name, req, DependencyKind::Build)
    }

    #[must_use]
    pub fn dev(name: impl Into<String>, req: impl Into<String>) -> Self {
        Self::new(name, req, DependencyKind::Dev)
    }

    #[must_use]
    pub fn new(name: impl Into<String>, req: impl Into<String>, kind: DependencyKind) -> Self {
        Self {
            name: name.into(),
            req: req.into(),
            features: Vec::new(),
            optional: false,
            default_features: true,
            target: None,
            kind,
            package: None,
        }
    }

    #[must_use]
    pub fn feature(mut self, feature: impl Into<String>) -> Self {
        self.features.push(feature.into());
        self
    }

    #[must_use]
    pub fn optional(mut self, optional: bool) -> Self {
        self.optional = optional;
        self
    }

    #[must_use]
    pub fn default_features(mut self, default_features: bool) -> Self {
        self.default_features = default_features;
        self
    }

    #[must_use]
    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    #[must_use]
    pub fn package(mut self, package: impl Into<String>) -> Self {
        self.package = Some(package.into());
        self
    }

    fn index_dependency(&self) -> IndexDependency {
        IndexDependency {
            name: self.name.clone(),
            req: self.req.clone(),
            features: self.features.clone(),
            optional: self.optional,
            default_features: self.default_features,
            target: self.target.clone(),
            kind: self.kind.as_index_str().to_owned(),
            registry: None,
            package: self.package.clone(),
        }
    }
}

impl DependencyKind {
    fn as_index_str(&self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Dev => "dev",
            Self::Build => "build",
        }
    }
}

impl RegistryBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            packages: Vec::new(),
        }
    }

    #[must_use]
    pub fn package(mut self, package: Package) -> Self {
        self.packages.push(package);
        self
    }

    pub fn write(self) -> Result<RegistryFixture> {
        let root = tempfile::tempdir().wrap_err("creating registry fixture tempdir")?;
        let index_dir = root.path().join("index");
        let crate_dir = root.path().join("crates");
        fs::create_dir_all(&index_dir).wrap_err("creating sparse index dir")?;
        fs::create_dir_all(&crate_dir).wrap_err("creating crate download dir")?;

        let mut entries_by_path: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
        for package in &self.packages {
            let crate_bytes = build_crate_archive(package)?;
            let checksum = sha256_hex(&crate_bytes);
            let crate_path = crate_dir
                .join(&package.name)
                .join(&package.version)
                .join("download");
            if let Some(parent) = crate_path.parent() {
                fs::create_dir_all(parent).wrap_err_with(|| {
                    format!("creating crate download parent for {}", package.name)
                })?;
            }
            fs::write(&crate_path, crate_bytes)
                .wrap_err_with(|| format!("writing crate archive {}", crate_path.display()))?;

            let entry = package.index_entry(checksum);
            let line = facet_json::to_string(&entry).wrap_err_with(|| {
                format!(
                    "serializing index entry {} {}",
                    package.name, package.version
                )
            })?;
            entries_by_path
                .entry(index_path_for(&package.name))
                .or_default()
                .push(line);
        }

        for (relative, lines) in entries_by_path {
            let path = index_dir.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .wrap_err_with(|| format!("creating index parent {}", parent.display()))?;
            }
            fs::write(&path, format!("{}\n", lines.join("\n")))
                .wrap_err_with(|| format!("writing index file {}", path.display()))?;
        }

        let config = IndexConfig {
            dl: "http://127.0.0.1:0/crates".to_owned(),
            api: "http://127.0.0.1:0".to_owned(),
        };
        let config_json =
            facet_json::to_string(&config).wrap_err("serializing sparse index config")?;
        fs::write(index_dir.join("config.json"), config_json)
            .wrap_err("writing sparse index config")?;

        Ok(RegistryFixture { root })
    }
}

impl Default for RegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl RegistryFixture {
    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    pub fn serve(self) -> Result<MockRegistryServer> {
        MockRegistryServer::start(self)
    }
}

impl MockRegistryServer {
    pub fn start(fixture: RegistryFixture) -> Result<Self> {
        let server =
            Server::http("127.0.0.1:0").map_err(|err| eyre!("starting tiny_http server: {err}"))?;
        let addr = server
            .server_addr()
            .to_ip()
            .ok_or_else(|| eyre!("tiny_http returned a non-IP listen address"))?;
        patch_config(fixture.root(), addr)?;

        let root = fixture.root().to_path_buf();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("reg-mock-http".to_owned())
            .spawn(move || serve_until_stopped(server, root, thread_stop))
            .wrap_err("spawning registry HTTP thread")?;

        Ok(Self {
            addr,
            stop,
            handle: Some(handle),
            _fixture: fixture,
        })
    }

    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    #[must_use]
    pub fn index_url(&self) -> String {
        format!("sparse+http://{}/index/", self.addr)
    }

    #[must_use]
    pub fn oracle(&self) -> CargoOracle {
        CargoOracle {
            registry_url: self.index_url(),
        }
    }
}

impl Drop for MockRegistryServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = std::net::TcpStream::connect_timeout(&self.addr, Duration::from_millis(50));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl CargoOracle {
    pub fn generate_lockfile(&self, project: &OracleProject) -> Result<CargoLock> {
        let temp = tempfile::tempdir().wrap_err("creating oracle tempdir")?;
        let project_dir = temp.path().join("project");
        let cargo_home = temp.path().join("cargo-home");
        fs::create_dir_all(project_dir.join(".cargo")).wrap_err("creating oracle .cargo dir")?;
        fs::create_dir_all(&cargo_home).wrap_err("creating isolated CARGO_HOME")?;

        fs::write(project_dir.join("Cargo.toml"), project.manifest())
            .wrap_err("writing oracle project Cargo.toml")?;
        fs::create_dir_all(project_dir.join("src")).wrap_err("creating oracle project src dir")?;
        fs::write(
            project_dir.join("src/lib.rs"),
            "pub fn oracle_project() {}\n",
        )
        .wrap_err("writing oracle project lib.rs")?;
        fs::write(project_dir.join(".cargo/config.toml"), self.cargo_config())
            .wrap_err("writing oracle cargo config")?;

        let output = Command::new("cargo")
            .arg("generate-lockfile")
            .current_dir(&project_dir)
            .env("CARGO_HOME", &cargo_home)
            .env("CARGO_NET_RETRY", "0")
            .env("CARGO_REGISTRIES_CRATES_IO_PROTOCOL", "sparse")
            .output()
            .wrap_err("running cargo generate-lockfile")?;
        if !output.status.success() {
            bail!(
                "cargo generate-lockfile failed with status {}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let raw = fs::read_to_string(project_dir.join("Cargo.lock"))
            .wrap_err("reading generated Cargo.lock")?;
        parse_lockfile(raw)
    }

    fn cargo_config(&self) -> String {
        format!(
            "[source.crates-io]\nreplace-with = \"reg-mock\"\n\n[source.reg-mock]\nregistry = \"{}\"\n",
            self.registry_url
        )
    }
}

impl OracleProject {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn dependency(mut self, dep: ProjectDependency) -> Self {
        self.dependencies.push(dep);
        self
    }

    fn manifest(&self) -> String {
        let mut manifest = String::from(
            "[package]\nname = \"oracle-project\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\n",
        );
        for dep in &self.dependencies {
            manifest.push_str(&dep.manifest_line());
        }
        manifest
    }
}

impl ProjectDependency {
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

    fn manifest_line(&self) -> String {
        let mut fields = vec![format!("version = \"{}\"", self.req)];
        if !self.default_features {
            fields.push("default-features = false".to_owned());
        }
        if !self.features.is_empty() {
            fields.push(format!("features = [{}]", quoted_list(&self.features)));
        }
        if let Some(package) = &self.package {
            fields.push(format!("package = \"{package}\""));
        }
        format!("{} = {{ {} }}\n", self.name, fields.join(", "))
    }
}

fn build_crate_archive(package: &Package) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut archive = tar::Builder::new(&mut encoder);
        let prefix = format!("{}-{}", package.name, package.version);
        append_file(
            &mut archive,
            &format!("{prefix}/Cargo.toml"),
            package_manifest(package).as_bytes(),
        )?;
        append_file(
            &mut archive,
            &format!("{prefix}/src/lib.rs"),
            format!(
                "#![allow(dead_code)]\npub const PACKAGE_NAME: &str = {:?};\n",
                package.name
            )
            .as_bytes(),
        )?;
        archive.finish().wrap_err("finishing tar archive")?;
    }
    encoder.finish().wrap_err("finishing gzip crate archive")
}

fn append_file<W: std::io::Write>(
    archive: &mut tar::Builder<W>,
    path: &str,
    bytes: &[u8],
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header
        .set_path(path)
        .wrap_err_with(|| format!("setting tar path {path}"))?;
    header.set_size(
        bytes
            .len()
            .try_into()
            .wrap_err("converting tar entry length")?,
    );
    header.set_mode(0o644);
    header.set_cksum();
    archive
        .append(&header, Cursor::new(bytes))
        .wrap_err_with(|| format!("appending tar file {path}"))
}

fn package_manifest(package: &Package) -> String {
    let mut manifest = format!(
        "[package]\nname = \"{}\"\nversion = \"{}\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n",
        package.name, package.version
    );
    if !package.deps.is_empty() {
        manifest.push_str("\n[dependencies]\n");
        for dep in package
            .deps
            .iter()
            .filter(|dep| dep.kind == DependencyKind::Normal)
        {
            manifest.push_str(&dependency_manifest_line(dep));
        }
    }
    if !package.features.is_empty() {
        manifest.push_str("\n[features]\n");
        for (name, enables) in &package.features {
            manifest.push_str(&format!("{name} = [{}]\n", quoted_list(enables)));
        }
    }
    manifest
}

fn dependency_manifest_line(dep: &Dependency) -> String {
    let mut fields = vec![format!("version = \"{}\"", dep.req)];
    if !dep.default_features {
        fields.push("default-features = false".to_owned());
    }
    if dep.optional {
        fields.push("optional = true".to_owned());
    }
    if !dep.features.is_empty() {
        fields.push(format!("features = [{}]", quoted_list(&dep.features)));
    }
    if let Some(package) = &dep.package {
        fields.push(format!("package = \"{package}\""));
    }
    format!("{} = {{ {} }}\n", dep.name, fields.join(", "))
}

fn quoted_list(items: &[String]) -> String {
    items
        .iter()
        .map(|item| format!("\"{item}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
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

fn patch_config(root: &Path, addr: SocketAddr) -> Result<()> {
    let config = IndexConfig {
        dl: format!("http://{addr}/crates"),
        api: format!("http://{addr}"),
    };
    let config_json =
        facet_json::to_string(&config).wrap_err("serializing served sparse index config")?;
    fs::write(root.join("index/config.json"), config_json)
        .wrap_err("patching sparse index config for server")
}

fn serve_until_stopped(server: Server, root: PathBuf, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Acquire) {
        match server.recv_timeout(Duration::from_millis(50)) {
            Ok(Some(request)) => {
                if let Err(err) = handle_request(request, &root) {
                    tracing::warn!(error = %err, "failed to serve registry request");
                }
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(error = %err, "registry HTTP server receive error");
            }
        }
    }
}

fn handle_request(request: Request, root: &Path) -> Result<()> {
    let method = request.method().clone();
    let url = request.url().to_owned();
    tracing::debug!(%method, %url, "registry HTTP request");

    if method != Method::Get && method != Method::Head {
        return respond(request, Response::empty(405));
    }

    let path = request_path(root, &url)?;
    if path.is_file() {
        let bytes =
            fs::read(&path).wrap_err_with(|| format!("reading served file {}", path.display()))?;
        let response = if method == Method::Head {
            Response::from_data(Vec::new())
        } else {
            Response::from_data(bytes)
        }
        .with_header(
            Header::from_bytes("Cache-Control", "no-store")
                .map_err(|_| eyre!("creating Cache-Control header"))?,
        );
        respond(request, response)
    } else {
        respond(request, Response::empty(404))
    }
}

fn request_path(root: &Path, url: &str) -> Result<PathBuf> {
    let url_path = url.split('?').next().unwrap_or(url);
    let relative = url_path
        .strip_prefix('/')
        .ok_or_else(|| eyre!("request path did not start with slash: {url_path}"))?;
    if relative
        .split('/')
        .any(|part| part == ".." || part.is_empty())
    {
        bail!("rejecting invalid registry request path {url_path}");
    }
    Ok(root.join(relative))
}

fn respond(request: Request, response: Response<impl Read>) -> Result<()> {
    request
        .respond(response)
        .map_err(|err| eyre!("responding to registry HTTP request: {err}"))
}

fn parse_lockfile(raw: String) -> Result<CargoLock> {
    let mut packages = BTreeSet::new();
    let mut current: Option<LockedPackageBuilder> = None;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            if let Some(package) = current.take().and_then(LockedPackageBuilder::finish) {
                packages.insert(package);
            }
            current = Some(LockedPackageBuilder::default());
            continue;
        }
        let Some(builder) = current.as_mut() else {
            continue;
        };
        if let Some(value) = parse_string_field(trimmed, "name") {
            builder.name = Some(value);
        } else if let Some(value) = parse_string_field(trimmed, "version") {
            builder.version = Some(value);
        } else if let Some(value) = parse_string_field(trimmed, "source") {
            builder.source = Some(value);
        }
    }

    if let Some(package) = current.and_then(LockedPackageBuilder::finish) {
        packages.insert(package);
    }

    Ok(CargoLock { packages, raw })
}

#[derive(Default)]
struct LockedPackageBuilder {
    name: Option<String>,
    version: Option<String>,
    source: Option<String>,
}

impl LockedPackageBuilder {
    fn finish(self) -> Option<LockedPackage> {
        Some(LockedPackage {
            name: self.name?,
            version: self.version?,
            source: self.source,
        })
    }
}

fn parse_string_field(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.trim_start();
    let value = rest.strip_prefix('=')?.trim_start();
    parse_toml_basic_string(value)
}

fn parse_toml_basic_string(value: &str) -> Option<String> {
    let value = value.strip_prefix('"')?;
    let mut output = String::new();
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            output.push(match ch {
                '"' => '"',
                '\\' => '\\',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                other => other,
            });
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(output);
        } else {
            output.push(ch);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn fixture_with_one_valid_solution_matches_expected_package_set() -> Result<()> {
        let server = RegistryBuilder::new()
            .package(Package::new("root", "1.0.0").dep(Dependency::normal("leaf", "=1.0.0")))
            .package(Package::new("leaf", "1.0.0"))
            .package(Package::new("leaf", "2.0.0"))
            .write()?
            .serve()?;

        let lock = server.oracle().generate_lockfile(
            &OracleProject::new().dependency(ProjectDependency::new("root", "=1.0.0")),
        )?;

        assert_eq!(
            package_set(&lock),
            names_versions([("leaf", "1.0.0"), ("root", "1.0.0")])
        );
        Ok(())
    }

    #[test]
    fn optional_dependency_activates_only_when_feature_demands_it() -> Result<()> {
        let server = RegistryBuilder::new()
            .package(
                Package::new("left", "1.0.0")
                    .dep(Dependency::normal("shared", "=1.0.0"))
                    .dep(Dependency::normal("addon", "=1.0.0").optional(true))
                    .feature("default", Vec::<String>::new())
                    .feature("with-addon", ["dep:addon"]),
            )
            .package(Package::new("right", "1.0.0").dep(Dependency::normal("shared", "=1.0.0")))
            .package(Package::new("shared", "1.0.0"))
            .package(Package::new("addon", "1.0.0"))
            .write()?
            .serve()?;

        let without_feature = server.oracle().generate_lockfile(
            &OracleProject::new()
                .dependency(ProjectDependency::new("left", "=1.0.0"))
                .dependency(ProjectDependency::new("right", "=1.0.0")),
        )?;
        assert_eq!(
            package_set(&without_feature),
            names_versions([("left", "1.0.0"), ("right", "1.0.0"), ("shared", "1.0.0")])
        );

        let with_feature = server.oracle().generate_lockfile(
            &OracleProject::new()
                .dependency(ProjectDependency::new("left", "=1.0.0").feature("with-addon"))
                .dependency(ProjectDependency::new("right", "=1.0.0")),
        )?;
        assert_eq!(
            package_set(&with_feature),
            names_versions([
                ("addon", "1.0.0"),
                ("left", "1.0.0"),
                ("right", "1.0.0"),
                ("shared", "1.0.0"),
            ])
        );
        Ok(())
    }

    #[test]
    #[ignore = "hypothesis landed negative: Cargo produced identical lockfiles for reversed dependency order on this multiple-solution fixture"]
    fn dependency_order_changes_lockfile_when_multiple_valid_solutions_exist() -> Result<()> {
        let server = RegistryBuilder::new()
            .package(
                Package::new("alpha", "1.0.0").dep(Dependency::normal("choice", ">=1.0.0, <3.0.0")),
            )
            .package(
                Package::new("beta", "1.0.0").dep(Dependency::normal("choice", ">=1.0.0, <3.0.0")),
            )
            .package(Package::new("choice", "1.0.0"))
            .package(Package::new("choice", "2.0.0"))
            .write()?
            .serve()?;

        let alpha_then_beta = server.oracle().generate_lockfile(
            &OracleProject::new()
                .dependency(ProjectDependency::new("alpha", "=1.0.0"))
                .dependency(ProjectDependency::new("beta", "=1.0.0")),
        )?;
        let beta_then_alpha = server.oracle().generate_lockfile(
            &OracleProject::new()
                .dependency(ProjectDependency::new("beta", "=1.0.0"))
                .dependency(ProjectDependency::new("alpha", "=1.0.0")),
        )?;

        assert_ne!(alpha_then_beta.raw, beta_then_alpha.raw);
        Ok(())
    }

    fn package_set(lock: &CargoLock) -> BTreeSet<(String, String)> {
        lock.packages
            .iter()
            .filter(|package| package.name != "oracle-project")
            .map(|package| (package.name.clone(), package.version.clone()))
            .collect()
    }

    fn names_versions<const N: usize>(items: [(&str, &str); N]) -> BTreeSet<(String, String)> {
        items
            .into_iter()
            .map(|(name, version)| (name.to_owned(), version.to_owned()))
            .collect()
    }
}
