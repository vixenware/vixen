//! Manifest parsing + resolution against the real first-party grammar packages.

use vx_package::{Manifest, PackageResolver, ResolveError, parse_manifest};

const CC_MAIN: &str = include_str!("../../../crates-plus/command-model/grammars/cc.vx");

#[test]
fn manifest_parses_from_grammar_frontmatter() {
    let manifest = parse_manifest(CC_MAIN)
        .expect("frontmatter parses")
        .expect("cc.vx has frontmatter");
    assert_eq!(
        manifest,
        Manifest {
            id: "@vixen/grammar-cc".to_string(),
            version: "0.1.0".to_string(),
            dependencies: vec![],
        }
    );
}

#[test]
fn no_frontmatter_is_no_manifest() {
    let manifest = parse_manifest("let x = 1\n").expect("parses");
    assert_eq!(manifest, None);
}

#[test]
fn bundled_resolver_finds_the_grammar_vocabulary() {
    let r = vx_package::BundledResolver;
    let pkg = r
        .resolve("vixen:command-grammar")
        .expect("command-grammar is bundled");
    assert_eq!(pkg.id, "vixen:command-grammar");
    assert!(
        pkg.main.contains("struct Grammar"),
        "it's the authoring vocab"
    );
    assert!(matches!(
        r.resolve("vixen:nope"),
        Err(ResolveError::NotFound(_))
    ));
}

#[test]
fn fs_resolver_reads_a_package_main() {
    // Lay out a throwaway package tree and resolve it.
    let dir = std::env::temp_dir().join("vx-package-test-pkgs");
    let pkg_dir = dir.join("@acme/widget");
    std::fs::create_dir_all(&pkg_dir).unwrap();
    std::fs::write(pkg_dir.join("main.vx"), "let answer = 42\n").unwrap();

    let r = vx_package::FsResolver::new(&dir);
    let pkg = r.resolve("@acme/widget").expect("resolves");
    assert_eq!(pkg.id, "@acme/widget");
    assert!(pkg.main.contains("answer"));

    assert!(matches!(
        r.resolve("@acme/missing"),
        Err(ResolveError::NotFound(_))
    ));
    assert!(matches!(r.resolve("bare"), Err(ResolveError::BadSpec(_))));

    let _ = std::fs::remove_dir_all(&dir);
}
