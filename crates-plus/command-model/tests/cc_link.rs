//! Link-side coverage: the `-Wl,` recursive passthrough into the `ld` grammar,
//! `-l` (order-significant, no dedup), and `-L` (dedup, order kept).

use command_model::{Lint, examples};

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[test]
fn wl_passthrough_canonicalizes_recursively() {
    // The value of -Wl, is itself an ld command; reordering the ld flags inside
    // the passthrough must yield the same canonical form.
    let a = examples().canonicalize(
        "cc",
        &argv(&[
            "cc",
            "-shared",
            "-Wl,--gc-sections,--version-script=v.map",
            "-o",
            "lib.so",
            "x.o",
        ]),
    );
    let b = examples().canonicalize(
        "cc",
        &argv(&[
            "cc",
            "-shared",
            "-Wl,--version-script=v.map,--gc-sections",
            "-o",
            "lib.so",
            "x.o",
        ]),
    );

    assert_eq!(
        a.args, b.args,
        "ld flags inside -Wl, canonicalize regardless of order"
    );
    assert!(
        a.args
            .iter()
            .any(|t| t == "-Wl,--gc-sections,--version-script=v.map"),
        "the passthrough is re-emitted in the ld grammar's canonical order; got {:?}",
        a.args
    );
}

#[test]
fn repeated_wl_occurrences_merge() {
    // Two -Wl, flags fold into one canonical passthrough.
    let c = examples().canonicalize(
        "cc",
        &argv(&[
            "cc",
            "-shared",
            "-Wl,--gc-sections",
            "-Wl,--as-needed",
            "-o",
            "lib.so",
            "x.o",
        ]),
    );
    assert!(
        c.args.iter().any(|t| t == "-Wl,--gc-sections,--as-needed"),
        "repeated -Wl, occurrences merge into one; got {:?}",
        c.args
    );
}

#[test]
fn libs_keep_order_but_search_dirs_dedup() {
    let c = examples().canonicalize(
        "cc",
        &argv(&[
            "cc", "-L", "a", "-lm", "-L", "b", "-lpng", "-L", "a", "-lm", "-o", "app", "x.o",
        ]),
    );

    // -l repeats are preserved in order: a circular-archive repeat is meaningful.
    let libs: Vec<String> = c
        .args
        .iter()
        .filter(|t| t.starts_with("-l"))
        .cloned()
        .collect();
    assert_eq!(
        libs,
        argv(&["-lm", "-lpng", "-lm"]),
        "lib order + repeats preserved"
    );

    // -L search dirs dedup, order kept.
    let dirs: Vec<String> = c
        .args
        .windows(2)
        .filter(|w| w[0] == "-L")
        .map(|w| w[1].clone())
        .collect();
    assert_eq!(dirs, argv(&["a", "b"]), "search dirs dedup, order kept");
    assert!(
        c.lints
            .iter()
            .any(|l| matches!(l, Lint::Duplicate { flag, .. } if flag == "-L")),
        "the duplicate -L a should be flagged; got {:?}",
        c.lints
    );
}

#[test]
fn warning_family_dedups_and_sorts() {
    let c = examples().canonicalize(
        "cc",
        &argv(&[
            "cc", "-c", "-Wall", "-Wextra", "-Wall", "src/x.c", "-o", "x.o",
        ]),
    );
    let warns: Vec<String> = c
        .args
        .iter()
        .filter(|t| t.starts_with("-W") && !t.starts_with("-Wl,"))
        .cloned()
        .collect();
    assert_eq!(
        warns,
        argv(&["-Wall", "-Wextra"]),
        "warnings dedup and sort"
    );
}
