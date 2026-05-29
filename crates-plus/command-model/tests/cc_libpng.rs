//! Integration tests against the `cc` flags `old-zoo/libpng.vx` actually emits.
//! These exercise the canonicalizer the way a real build would drive it.

use command_model::{CC, Lint, canonicalize};

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[test]
fn messy_compile_canonicalizes_and_lints() {
    // A deliberately messy compile: out-of-order flags, an overridden -O, a
    // duplicated define, includes in a particular search order.
    let input = argv(&[
        "cc",
        "-I",
        "src",
        "-O0",
        "-DNEON=2",
        "-c",
        "-I",
        "gen",
        "-O2",
        "-DNEON=2",
        "-fPIC",
        "-o",
        "out/png.o",
        "src/png.c",
    ]);
    let c = canonicalize(&CC, &input);

    assert_eq!(c.program, "cc");
    assert_eq!(
        c.args,
        argv(&[
            "-c",
            "-fPIC",
            "-O2",
            "-DNEON=2",
            "-I",
            "src",
            "-I",
            "gen",
            "src/png.c",
            "-o",
            "out/png.o",
        ]),
        "flags fold to a fixed layout; -O0 dropped, duplicate define collapsed, include order kept"
    );

    assert!(
        c.lints
            .iter()
            .any(|l| matches!(l, Lint::Overridden { kept, .. } if kept == "-O2")),
        "must flag the overridden optimization level; got {:?}",
        c.lints
    );
    assert!(
        c.lints
            .iter()
            .any(|l| matches!(l, Lint::DuplicateDefine { name } if name == "NEON")),
        "must flag the duplicate define; got {:?}",
        c.lints
    );
}

#[test]
fn permutations_share_one_cache_key() {
    // The whole point: two spellings of the same compile produce the same
    // canonical args, so they hit one cache entry.
    let a = canonicalize(
        &CC,
        &argv(&[
            "cc", "-c", "-O2", "-DFOO", "-I", "inc", "src/x.c", "-o", "x.o",
        ]),
    );
    let b = canonicalize(
        &CC,
        &argv(&[
            "cc", "-DFOO", "-I", "inc", "-O2", "src/x.c", "-c", "-o", "x.o",
        ]),
    );

    assert_eq!(a.args, b.args, "reorderings must canonicalize identically");
    assert!(
        a.lints.is_empty(),
        "clean command should not lint: {:?}",
        a.lints
    );
    assert!(
        b.lints.is_empty(),
        "clean command should not lint: {:?}",
        b.lints
    );
}

#[test]
fn conflicting_define_is_flagged() {
    let c = canonicalize(
        &CC,
        &argv(&["cc", "-c", "-DLEVEL=1", "-DLEVEL=2", "src/x.c", "-o", "x.o"]),
    );
    assert!(
        c.lints
            .iter()
            .any(|l| matches!(l, Lint::ConflictingDefine { name, .. } if name == "LEVEL")),
        "conflicting define values must be flagged; got {:?}",
        c.lints
    );
    // last value wins in the canonical form
    assert!(c.args.contains(&"-DLEVEL=2".to_string()));
}
