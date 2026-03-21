use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use eyre::{Context, Result, bail};
use facet::Facet;

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
struct SparseIndexDep {
    name: String,
    req: String,
    #[facet(default)]
    package: Option<String>,
    #[facet(default)]
    kind: Option<String>,
    #[facet(default)]
    optional: bool,
    #[facet(rename = "default_features", default)]
    default_features: bool,
    #[facet(default)]
    features: Vec<String>,
    #[facet(default)]
    target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
struct SparseIndexEntry {
    name: String,
    #[facet(rename = "vers")]
    version: String,
    #[facet(default)]
    deps: Vec<SparseIndexDep>,
    #[facet(default)]
    features: BTreeMap<String, Vec<String>>,
    #[facet(default)]
    features2: BTreeMap<String, Vec<String>>,
    #[facet(default)]
    yanked: bool,
    #[facet(default)]
    rust_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegistryDepFact {
    name: String,
    package: String,
    has_package: bool,
    req: String,
    kind: String,
    optional: bool,
    default_features: bool,
    features: Vec<String>,
    target: String,
    has_target: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegistryReleaseFact {
    version: String,
    deps: Vec<RegistryDepFact>,
    features: BTreeMap<String, Vec<String>>,
    yanked: bool,
    rust_version: String,
    has_rust_version: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegistryPackageFacts {
    name: String,
    releases: Vec<RegistryReleaseFact>,
}

pub(crate) fn run(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!(
            "Usage: cargo xtask registry-vx <snapshot-dir> <output.vx> [package ...]\n\
             Emits a generated Vixen module from a Cargo sparse-index snapshot."
        );
    }

    let snapshot_dir = Path::new(&args[0]);
    let output_path = Path::new(&args[1]);
    let package_names = &args[2..];

    let packages = if package_names.is_empty() {
        load_all_packages(snapshot_dir)?
    } else {
        load_selected_packages(snapshot_dir, package_names)?
    };
    if packages.is_empty() {
        bail!(
            "no Cargo registry package files found under {}",
            snapshot_dir.display()
        );
    }

    let output = emit_registry_vx(&packages);
    fs::write(output_path, output)
        .with_context(|| format!("failed to write {}", output_path.display()))?;
    println!("Wrote {}", output_path.display());
    Ok(())
}

fn load_selected_packages(
    snapshot_dir: &Path,
    package_names: &[String],
) -> Result<Vec<RegistryPackageFacts>> {
    let data_root = sparse_index_data_root(snapshot_dir);
    let mut packages = package_names
        .iter()
        .map(|package_name| {
            let path = data_root.join(sparse_index_rel_path(package_name));
            load_package_file(&path).with_context(|| {
                format!(
                    "failed to load sparse-index package {package_name:?} from {}",
                    path.display()
                )
            })
        })
        .collect::<Result<Vec<_>>>()?;
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(packages)
}

fn load_all_packages(snapshot_dir: &Path) -> Result<Vec<RegistryPackageFacts>> {
    let mut files = Vec::new();
    collect_regular_files(&sparse_index_data_root(snapshot_dir), &mut files)?;

    let mut packages = files
        .iter()
        .map(|path| {
            load_package_file(path)
                .with_context(|| format!("failed to load sparse-index file {}", path.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(packages)
}

fn sparse_index_data_root(snapshot_dir: &Path) -> PathBuf {
    let cache_root = snapshot_dir.join(".cache");
    if cache_root.is_dir() {
        cache_root
    } else {
        snapshot_dir.to_path_buf()
    }
}

fn collect_regular_files(dir: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries = fs::read_dir(dir)
        .with_context(|| format!("failed to read directory {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("failed to enumerate directory {}", dir.display()))?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to read file type for {}", path.display()))?;
        if file_type.is_dir() {
            collect_regular_files(&path, output)?;
        } else if file_type.is_file() {
            output.push(path);
        }
    }

    Ok(())
}

fn load_package_file(path: &Path) -> Result<RegistryPackageFacts> {
    let contents = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let entries = parse_sparse_index_entries(&contents, path)?;

    let mut releases = Vec::new();
    let mut package_name = None::<String>;
    for entry in entries {
        match &package_name {
            Some(existing) if existing != &entry.name => bail!(
                "sparse-index file {} mixes package names {:?} and {:?}",
                path.display(),
                existing,
                entry.name
            ),
            Some(_) => {}
            None => package_name = Some(entry.name.clone()),
        }

        let mut features = entry.features;
        for (feature_name, members) in entry.features2 {
            if features.insert(feature_name.clone(), members).is_some() {
                bail!(
                    "sparse-index file {} repeated feature {:?} across features/features2",
                    path.display(),
                    feature_name
                );
            }
        }

        releases.push(RegistryReleaseFact {
            version: entry.version,
            deps: entry
                .deps
                .into_iter()
                .map(|dep| RegistryDepFact {
                    name: dep.name,
                    package: dep.package.clone().unwrap_or_default(),
                    has_package: dep.package.is_some(),
                    req: dep.req,
                    kind: dep.kind.unwrap_or_else(|| "normal".to_string()),
                    optional: dep.optional,
                    default_features: dep.default_features,
                    features: dep.features,
                    target: dep.target.clone().unwrap_or_default(),
                    has_target: dep.target.is_some(),
                })
                .collect(),
            features,
            yanked: entry.yanked,
            rust_version: entry.rust_version.clone().unwrap_or_default(),
            has_rust_version: entry.rust_version.is_some(),
        });
    }

    let name = package_name
        .ok_or_else(|| eyre::eyre!("sparse-index file {} was empty", path.display()))?;
    Ok(RegistryPackageFacts { name, releases })
}

fn parse_sparse_index_entries(contents: &[u8], path: &Path) -> Result<Vec<SparseIndexEntry>> {
    if contents.contains(&0) {
        parse_sparse_index_cache_entries(contents, path)
    } else {
        parse_sparse_index_json_lines(contents, path)
    }
}

fn parse_sparse_index_json_lines(contents: &[u8], path: &Path) -> Result<Vec<SparseIndexEntry>> {
    let contents = std::str::from_utf8(contents)
        .with_context(|| format!("sparse-index file {} was not valid UTF-8", path.display()))?;

    let mut entries = Vec::new();
    for (line_index, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry = facet_json::from_str::<SparseIndexEntry>(trimmed).with_context(|| {
            format!(
                "failed to parse JSON line {} in {}",
                line_index + 1,
                path.display()
            )
        })?;
        entries.push(entry);
    }
    Ok(entries)
}

fn parse_sparse_index_cache_entries(contents: &[u8], path: &Path) -> Result<Vec<SparseIndexEntry>> {
    const SPARSE_CACHE_PREFIX_LEN: usize = 5;

    if contents.len() < SPARSE_CACHE_PREFIX_LEN {
        bail!(
            "sparse cache file {} was too short to contain a cache header",
            path.display()
        );
    }

    let header_end = contents[SPARSE_CACHE_PREFIX_LEN..]
        .iter()
        .position(|byte| *byte == 0)
        .map(|offset| SPARSE_CACHE_PREFIX_LEN + offset)
        .ok_or_else(|| {
            eyre::eyre!(
                "sparse cache file {} was missing its header terminator",
                path.display()
            )
        })?;
    let payload = &contents[header_end + 1..];

    let mut entries = Vec::new();
    let mut fields = payload.split(|byte| *byte == 0);
    while let Some(version_bytes) = fields.next() {
        if version_bytes.is_empty() {
            continue;
        }
        let json_bytes = fields.next().ok_or_else(|| {
            eyre::eyre!(
                "sparse cache file {} ended with a version but no JSON payload",
                path.display()
            )
        })?;
        let version = std::str::from_utf8(version_bytes).with_context(|| {
            format!(
                "sparse cache version tag in {} was not valid UTF-8",
                path.display()
            )
        })?;
        let json = std::str::from_utf8(json_bytes).with_context(|| {
            format!(
                "sparse cache JSON payload in {} was not valid UTF-8",
                path.display()
            )
        })?;
        let entry = facet_json::from_str::<SparseIndexEntry>(json)
            .with_context(|| format!("failed to parse sparse cache entry in {}", path.display()))?;
        if entry.version != version {
            bail!(
                "sparse cache file {} mismatched version tag {:?} vs JSON {:?}",
                path.display(),
                version,
                entry.version
            );
        }
        entries.push(entry);
    }

    Ok(entries)
}

fn sparse_index_rel_path(package_name: &str) -> PathBuf {
    let normalized = package_name.to_ascii_lowercase();
    match normalized.len() {
        0 => PathBuf::new(),
        1 => PathBuf::from("1").join(&normalized),
        2 => PathBuf::from("2").join(&normalized),
        3 => PathBuf::from("3").join(&normalized[..1]).join(&normalized),
        _ => PathBuf::from(&normalized[..2])
            .join(&normalized[2..4])
            .join(&normalized),
    }
}

fn emit_registry_vx(packages: &[RegistryPackageFacts]) -> String {
    let mut out = String::new();
    out.push_str("// @generated by `cargo xtask registry-vx`\n");
    out.push_str("// Source: Cargo sparse-index snapshot\n\n");
    out.push_str("type PackageName = String\n");
    out.push_str("type Version = String\n");
    out.push_str("type VersionReq = String\n");
    out.push_str("type FeatureName = String\n\n");
    out.push_str("struct RegistryDep {\n");
    out.push_str("  name: PackageName,\n");
    out.push_str("  package: PackageName,\n");
    out.push_str("  has_package: Bool,\n");
    out.push_str("  req: VersionReq,\n");
    out.push_str("  kind: String,\n");
    out.push_str("  optional: Bool,\n");
    out.push_str("  default_features: Bool,\n");
    out.push_str("  features: Array[FeatureName],\n");
    out.push_str("  target: String,\n");
    out.push_str("  has_target: Bool,\n");
    out.push_str("}\n\n");
    out.push_str("struct RegistryRelease {\n");
    out.push_str("  version: Version,\n");
    out.push_str("  deps: Array[RegistryDep],\n");
    out.push_str("  features: Map[FeatureName, Array[String]],\n");
    out.push_str("  yanked: Bool,\n");
    out.push_str("  rust_version: String,\n");
    out.push_str("  has_rust_version: Bool,\n");
    out.push_str("}\n\n");
    out.push_str("type RegistryPackage = Array[RegistryRelease]\n\n");
    out.push_str("struct RegistryMiss {\n");
    out.push_str("  name: PackageName,\n");
    out.push_str("}\n\n");

    for package in packages {
        let helper_name = package_helper_name(&package.name);
        out.push_str(&format!("fn {helper_name}() -> RegistryPackage {{\n"));
        out.push_str("  [\n");
        for release in &package.releases {
            out.push_str("    RegistryRelease {\n");
            out.push_str(&format!(
                "      version: {},\n",
                vx_string(&release.version)
            ));
            out.push_str("      deps: [\n");
            for dep in &release.deps {
                out.push_str("        RegistryDep {\n");
                out.push_str(&format!("          name: {},\n", vx_string(&dep.name)));
                out.push_str(&format!(
                    "          package: {},\n",
                    vx_string(&dep.package)
                ));
                out.push_str(&format!("          has_package: {},\n", dep.has_package));
                out.push_str(&format!("          req: {},\n", vx_string(&dep.req)));
                out.push_str(&format!("          kind: {},\n", vx_string(&dep.kind)));
                out.push_str(&format!("          optional: {},\n", dep.optional));
                out.push_str(&format!(
                    "          default_features: {},\n",
                    dep.default_features
                ));
                out.push_str(&format!(
                    "          features: {},\n",
                    vx_string_array(&dep.features)
                ));
                out.push_str(&format!("          target: {},\n", vx_string(&dep.target)));
                out.push_str(&format!("          has_target: {},\n", dep.has_target));
                out.push_str("        },\n");
            }
            out.push_str("      ],\n");
            out.push_str(&format!(
                "      features: {},\n",
                vx_string_array_map(&release.features)
            ));
            out.push_str(&format!("      yanked: {},\n", release.yanked));
            out.push_str(&format!(
                "      rust_version: {},\n",
                vx_string(&release.rust_version)
            ));
            out.push_str(&format!(
                "      has_rust_version: {},\n",
                release.has_rust_version
            ));
            out.push_str("    },\n");
        }
        out.push_str("  ]\n");
        out.push_str("}\n\n");
    }

    out.push_str("fn registry_data_lookup(name: PackageName) -> RegistryPackage {\n");
    for (index, package) in packages.iter().enumerate() {
        let helper_name = package_helper_name(&package.name);
        if index == 0 {
            out.push_str(&format!("  if name == {} (\n", vx_string(&package.name)));
        } else {
            out.push_str(&format!(
                "  ) else if name == {} (\n",
                vx_string(&package.name)
            ));
        }
        out.push_str(&format!("    {helper_name}()\n"));
    }
    if !packages.is_empty() {
        out.push_str("  ) else (\n");
    }
    out.push_str("    fail RegistryMiss {\n");
    out.push_str("      name: name,\n");
    out.push_str("    }\n");
    out.push_str("  )\n");
    out.push_str("}\n");

    out
}

fn package_helper_name(package_name: &str) -> String {
    let mut ident = String::from("registry_package_");
    let mut chars = package_name.chars();
    if let Some(first) = chars.next() {
        push_ident_char(&mut ident, first, true);
    }
    for ch in chars {
        push_ident_char(&mut ident, ch, false);
    }
    ident
}

fn push_ident_char(output: &mut String, ch: char, first: bool) {
    if ch.is_ascii_alphanumeric() || ch == '_' {
        if first && ch.is_ascii_digit() {
            output.push('_');
        }
        output.push(ch.to_ascii_lowercase());
    } else {
        output.push('_');
    }
}

fn vx_string(value: &str) -> String {
    format!("{value:?}")
}

fn vx_string_array(values: &[String]) -> String {
    let mut out = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&vx_string(value));
    }
    out.push(']');
    out
}

fn vx_string_array_map(values: &BTreeMap<String, Vec<String>>) -> String {
    if values.is_empty() {
        return "%{}".to_string();
    }

    let mut out = String::from("%{");
    for (index, (key, members)) in values.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        out.push_str(&vx_string(key));
        out.push_str(" => ");
        out.push_str(&vx_string_array(members));
    }
    out.push('}');
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::tempdir;

    use super::{
        RegistryDepFact, RegistryPackageFacts, RegistryReleaseFact, emit_registry_vx,
        load_all_packages, load_selected_packages, sparse_index_rel_path,
    };

    #[test]
    fn sparse_index_rel_path_matches_crates_io_layout() {
        assert_eq!(sparse_index_rel_path("a"), PathBuf::from("1/a"));
        assert_eq!(sparse_index_rel_path("ab"), PathBuf::from("2/ab"));
        assert_eq!(sparse_index_rel_path("abc"), PathBuf::from("3/a/abc"));
        assert_eq!(sparse_index_rel_path("serde"), PathBuf::from("se/rd/serde"));
        assert_eq!(sparse_index_rel_path("tokio"), PathBuf::from("to/ki/tokio"));
        assert_eq!(
            sparse_index_rel_path("HashBrown"),
            PathBuf::from("ha/sh/hashbrown")
        );
    }

    #[test]
    fn load_selected_packages_parses_sparse_index_files() {
        let dir = tempdir().expect("temp dir");
        let root = dir.path();

        let codec_path = root.join(sparse_index_rel_path("codec"));
        let logger_path = root.join(sparse_index_rel_path("logger"));
        fs::create_dir_all(codec_path.parent().expect("codec parent")).expect("mkdir codec");
        fs::create_dir_all(logger_path.parent().expect("logger parent")).expect("mkdir logger");
        fs::write(
            &codec_path,
            r#"{"name":"codec","vers":"1.1.0","deps":[{"name":"logger","req":"1.1.0","package":"logger-core","kind":"build","optional":true,"default_features":false,"features":["std"],"target":"cfg(unix)"}],"features":{"derive":["dep:logger"]},"features2":{"unstable":["derive"]},"yanked":false,"rust_version":"1.80"}"#
                .to_string()
                + "\n"
                + r#"{"name":"codec","vers":"1.0.0","deps":[{"name":"logger","req":"1.0.0"}],"yanked":false}"#,
        )
        .expect("write codec");
        fs::write(
            &logger_path,
            r#"{"name":"logger","vers":"1.1.0","deps":[],"yanked":false}"#.to_string() + "\n",
        )
        .expect("write logger");

        let packages = load_selected_packages(root, &["logger".to_string(), "codec".to_string()])
            .expect("load selected");

        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name, "codec");
        assert_eq!(packages[0].releases.len(), 2);
        assert_eq!(packages[0].releases[0].deps[0].name, "logger");
        assert_eq!(packages[0].releases[0].deps[0].package, "logger-core");
        assert!(packages[0].releases[0].deps[0].has_package);
        assert_eq!(packages[0].releases[0].deps[0].kind, "build");
        assert!(packages[0].releases[0].deps[0].optional);
        assert!(!packages[0].releases[0].deps[0].default_features);
        assert_eq!(packages[0].releases[0].deps[0].features, vec!["std"]);
        assert_eq!(packages[0].releases[0].deps[0].target, "cfg(unix)");
        assert!(packages[0].releases[0].deps[0].has_target);
        assert_eq!(
            packages[0].releases[0].features.get("derive"),
            Some(&vec!["dep:logger".to_string()])
        );
        assert_eq!(
            packages[0].releases[0].features.get("unstable"),
            Some(&vec!["derive".to_string()])
        );
        assert_eq!(packages[0].releases[0].rust_version, "1.80");
        assert!(packages[0].releases[0].has_rust_version);
        assert_eq!(packages[1].name, "logger");
        assert_eq!(packages[1].releases[0].version, "1.1.0");
    }

    #[test]
    fn load_selected_packages_parses_sparse_cache_files() {
        let dir = tempdir().expect("temp dir");
        let root = dir.path().join(".cache");

        let serde_path = root.join(sparse_index_rel_path("serde"));
        fs::create_dir_all(serde_path.parent().expect("serde parent")).expect("mkdir serde");
        fs::write(
            &serde_path,
            sparse_cache_bytes(
                "etag: \"demo\"",
                &[(
                    "1.0.0",
                    r#"{"name":"serde","vers":"1.0.0","deps":[{"name":"serde_derive","req":"^1","features":[],"optional":true,"default_features":true,"target":null,"kind":"normal"}],"features":{"derive":["dep:serde_derive"]},"yanked":false,"rust_version":"1.56"}"#,
                )],
            ),
        )
        .expect("write serde");

        let packages =
            load_selected_packages(dir.path(), &["serde".to_string()]).expect("load cache");
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "serde");
        assert_eq!(packages[0].releases[0].version, "1.0.0");
        assert_eq!(packages[0].releases[0].deps[0].kind, "normal");
        assert!(packages[0].releases[0].deps[0].optional);
        assert_eq!(packages[0].releases[0].rust_version, "1.56");
        assert!(packages[0].releases[0].has_rust_version);
    }

    #[test]
    fn load_all_packages_sorts_by_package_name() {
        let dir = tempdir().expect("temp dir");
        let root = dir.path();

        let zeta_path = root.join(sparse_index_rel_path("zeta"));
        let alpha_path = root.join(sparse_index_rel_path("alpha"));
        fs::create_dir_all(zeta_path.parent().expect("zeta parent")).expect("mkdir zeta");
        fs::create_dir_all(alpha_path.parent().expect("alpha parent")).expect("mkdir alpha");
        fs::write(
            &zeta_path,
            r#"{"name":"zeta","vers":"1.0.0","deps":[],"yanked":false}"#,
        )
        .expect("write zeta");
        fs::write(
            &alpha_path,
            r#"{"name":"alpha","vers":"1.0.0","deps":[],"yanked":false}"#,
        )
        .expect("write alpha");

        let packages = load_all_packages(root).expect("load all");
        assert_eq!(
            packages
                .iter()
                .map(|pkg| pkg.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
    }

    #[test]
    fn emitter_writes_generated_lookup_module() {
        let mut feature_map = BTreeMap::new();
        feature_map.insert("derive".to_string(), vec!["dep:serde_derive".to_string()]);

        let output = emit_registry_vx(&[
            RegistryPackageFacts {
                name: "codec".to_string(),
                releases: vec![
                    RegistryReleaseFact {
                        version: "1.1.0".to_string(),
                        deps: vec![RegistryDepFact {
                            name: "logger".to_string(),
                            package: "logger-core".to_string(),
                            has_package: true,
                            req: "1.1.0".to_string(),
                            kind: "build".to_string(),
                            optional: true,
                            default_features: false,
                            features: vec!["std".to_string()],
                            target: "cfg(unix)".to_string(),
                            has_target: true,
                        }],
                        features: feature_map.clone(),
                        yanked: false,
                        rust_version: "1.80".to_string(),
                        has_rust_version: true,
                    },
                    RegistryReleaseFact {
                        version: "1.0.0".to_string(),
                        deps: vec![RegistryDepFact {
                            name: "logger".to_string(),
                            package: String::new(),
                            has_package: false,
                            req: "1.0.0".to_string(),
                            kind: "normal".to_string(),
                            optional: false,
                            default_features: false,
                            features: Vec::new(),
                            target: String::new(),
                            has_target: false,
                        }],
                        features: BTreeMap::new(),
                        yanked: false,
                        rust_version: String::new(),
                        has_rust_version: false,
                    },
                ],
            },
            RegistryPackageFacts {
                name: "logger".to_string(),
                releases: vec![RegistryReleaseFact {
                    version: "1.1.0".to_string(),
                    deps: Vec::new(),
                    features: BTreeMap::new(),
                    yanked: false,
                    rust_version: String::new(),
                    has_rust_version: false,
                }],
            },
        ]);

        assert!(output.contains("struct RegistryDep"));
        assert!(output.contains("package: PackageName"));
        assert!(output.contains("has_package: Bool"));
        assert!(output.contains("kind: String"));
        assert!(output.contains("features: Map[FeatureName, Array[String]]"));
        assert!(output.contains("rust_version: String"));
        assert!(output.contains("has_rust_version: Bool"));
        assert!(output.contains("fn registry_package_codec() -> RegistryPackage"));
        assert!(output.contains("package: \"logger-core\""));
        assert!(output.contains("has_package: true"));
        assert!(output.contains("kind: \"build\""));
        assert!(output.contains("optional: true"));
        assert!(output.contains("default_features: false"));
        assert!(output.contains("features: %{"));
        assert!(output.contains("rust_version: \"1.80\""));
        assert!(output.contains("has_rust_version: true"));
        assert!(output.contains("fn registry_data_lookup(name: PackageName) -> RegistryPackage"));
        assert!(output.contains("fail RegistryMiss"));
    }

    #[test]
    fn checked_in_sparse_snapshot_matches_generated_registry_module() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask should live under workspace root");
        let snapshot_root = workspace_root.join("specs/eval/run/registry_snapshot/index");
        let generated_path =
            workspace_root.join("specs/eval/run/registry_snapshot/registry_data.vx");

        let packages = load_all_packages(&snapshot_root).expect("load checked-in sparse snapshot");
        let actual = emit_registry_vx(&packages);
        let expected =
            fs::read_to_string(&generated_path).expect("read checked-in generated registry module");
        assert_eq!(actual, expected);
    }

    fn sparse_cache_bytes(etag: &str, entries: &[(&str, &str)]) -> Vec<u8> {
        let mut bytes = vec![0x03, 0x02, 0x00, 0x00, 0x00];
        bytes.extend_from_slice(etag.as_bytes());
        bytes.push(0);
        for (version, json) in entries {
            bytes.extend_from_slice(version.as_bytes());
            bytes.push(0);
            bytes.extend_from_slice(json.as_bytes());
            bytes.push(0);
        }
        bytes
    }
}
