//! Manifest parsing + resolution against the real first-party grammar packages.

use vx_package::{
    Manifest, MapResolver, PackageResolver, ResolveError, import_specs, parse_manifest,
    resolve_closure,
};

const CC_MAIN: &str = include_str!("../../../packages/@vixen/grammar-cc/main.vx");

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

#[test]
fn import_specs_are_extracted() {
    let src = "let { foo } = import \"@acme/lib\"\nlet core = import \"vixen:core\"\n";
    assert_eq!(import_specs(src), vec!["@acme/lib", "vixen:core"]);
}

#[test]
fn closure_is_transitive_and_deduped() {
    let mut r = MapResolver::default();
    r.insert("@acme/app", "let { mid } = import \"@acme/mid\"\n");
    r.insert(
        "@acme/mid",
        "let { leaf } = import \"@acme/leaf\"\nlet x = import \"@acme/app\"\n",
    );
    r.insert("@acme/leaf", "let leaf = 1\n");

    let root = "let { app } = import \"@acme/app\"\n";
    let closure = resolve_closure(root, &r);

    assert!(
        closure.errors.is_empty(),
        "all resolve; got {:?}",
        closure.errors
    );
    let ids: Vec<&str> = closure.packages.iter().map(|p| p.id.as_str()).collect();
    // discovery order: app, then its dep mid, then mid's dep leaf; the back-edge
    // mid -> app is deduped away.
    assert_eq!(ids, ["@acme/app", "@acme/mid", "@acme/leaf"]);
}

#[test]
fn closure_collects_unresolved_imports() {
    let r = MapResolver::default();
    let closure = resolve_closure("let { x } = import \"@acme/missing\"\n", &r);
    assert!(closure.packages.is_empty());
    assert_eq!(closure.errors.len(), 1);
    assert!(matches!(closure.errors[0].1, ResolveError::NotFound(_)));
}
