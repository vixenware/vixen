use std::fs;
use std::path::{Path, PathBuf};

use eyre::{Context, Result, bail};
use facet::Facet;

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
struct SparseIndexDep {
    name: String,
    req: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
struct SparseIndexEntry {
    name: String,
    #[facet(rename = "vers")]
    version: String,
    #[facet(default)]
    deps: Vec<SparseIndexDep>,
    #[facet(default)]
    yanked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegistryDepFact {
    name: String,
    req: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegistryReleaseFact {
    version: String,
    deps: Vec<RegistryDepFact>,
    yanked: bool,
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
             Emits a generated Vixen module from a curated Cargo sparse-index snapshot."
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
    let mut packages = package_names
        .iter()
        .map(|package_name| {
            let path = snapshot_dir.join(sparse_index_rel_path(package_name));
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
    collect_regular_files(snapshot_dir, &mut files)?;

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
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;

    let mut releases = Vec::new();
    let mut package_name = None::<String>;
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
        releases.push(RegistryReleaseFact {
            version: entry.version,
            deps: entry
                .deps
                .into_iter()
                .map(|dep| RegistryDepFact {
                    name: dep.name,
                    req: dep.req,
                })
                .collect(),
            yanked: entry.yanked,
        });
    }

    let name = package_name
        .ok_or_else(|| eyre::eyre!("sparse-index file {} was empty", path.display()))?;
    Ok(RegistryPackageFacts { name, releases })
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
    out.push_str("// Source: curated Cargo sparse-index snapshot\n\n");
    out.push_str("type PackageName = String\n");
    out.push_str("type Version = String\n");
    out.push_str("type VersionReq = String\n\n");
    out.push_str("struct RegistryDep {\n");
    out.push_str("  name: PackageName,\n");
    out.push_str("  req: VersionReq,\n");
    out.push_str("}\n\n");
    out.push_str("struct RegistryRelease {\n");
    out.push_str("  version: Version,\n");
    out.push_str("  deps: Array[RegistryDep],\n");
    out.push_str("  yanked: Bool,\n");
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
                out.push_str(&format!("          req: {},\n", vx_string(&dep.req)));
                out.push_str("        },\n");
            }
            out.push_str("      ],\n");
            out.push_str(&format!("      yanked: {},\n", release.yanked));
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

#[cfg(test)]
mod tests {
    use super::{
        emit_registry_vx, load_all_packages, load_selected_packages, sparse_index_rel_path,
    };
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn sparse_index_rel_path_matches_crates_io_layout() {
        assert_eq!(sparse_index_rel_path("a"), std::path::PathBuf::from("1/a"));
        assert_eq!(
            sparse_index_rel_path("ab"),
            std::path::PathBuf::from("2/ab")
        );
        assert_eq!(
            sparse_index_rel_path("abc"),
            std::path::PathBuf::from("3/a/abc")
        );
        assert_eq!(
            sparse_index_rel_path("serde"),
            std::path::PathBuf::from("se/rd/serde")
        );
        assert_eq!(
            sparse_index_rel_path("tokio"),
            std::path::PathBuf::from("to/ki/tokio")
        );
        assert_eq!(
            sparse_index_rel_path("HashBrown"),
            std::path::PathBuf::from("ha/sh/hashbrown")
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
            r#"{"name":"codec","vers":"1.1.0","deps":[{"name":"logger","req":"1.1.0"}],"yanked":false}"#
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
        assert_eq!(packages[1].name, "logger");
        assert_eq!(packages[1].releases[0].version, "1.1.0");
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
        let output = emit_registry_vx(&[
            super::RegistryPackageFacts {
                name: "codec".to_string(),
                releases: vec![
                    super::RegistryReleaseFact {
                        version: "1.1.0".to_string(),
                        deps: vec![super::RegistryDepFact {
                            name: "logger".to_string(),
                            req: "1.1.0".to_string(),
                        }],
                        yanked: false,
                    },
                    super::RegistryReleaseFact {
                        version: "1.0.0".to_string(),
                        deps: vec![super::RegistryDepFact {
                            name: "logger".to_string(),
                            req: "1.0.0".to_string(),
                        }],
                        yanked: false,
                    },
                ],
            },
            super::RegistryPackageFacts {
                name: "logger".to_string(),
                releases: vec![super::RegistryReleaseFact {
                    version: "1.1.0".to_string(),
                    deps: Vec::new(),
                    yanked: false,
                }],
            },
        ]);

        assert!(output.contains("struct RegistryDep"));
        assert!(output.contains("struct RegistryRelease"));
        assert!(output.contains("type RegistryPackage = Array[RegistryRelease]"));
        assert!(output.contains("fn registry_package_codec() -> RegistryPackage"));
        assert!(output.contains("version: \"1.1.0\""));
        assert!(output.contains("name: \"logger\""));
        assert!(output.contains("fn registry_data_lookup(name: PackageName) -> RegistryPackage"));
        assert!(output.contains("fail RegistryMiss"));
    }
}
