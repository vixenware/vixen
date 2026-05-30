//! Guarded productions, first cut: the legal flag set depends on accumulated
//! mode state. A link-only flag after a mode selector (`-c`) is an impurity.

use command_model::{CC, Impurity, canonicalize, effective_command};
use std::collections::BTreeMap;

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[test]
fn link_flag_in_compile_mode_is_impure() {
    // -c selects compile mode; -l (link-only) is then illegal.
    let ec = effective_command(
        &CC,
        &argv(&["cc", "-c", "-lm", "src/x.c", "-o", "x.o"]),
        &BTreeMap::new(),
    );
    assert!(
        ec.impurities
            .iter()
            .any(|i| matches!(i, Impurity::IllegalInMode { flag, mode } if flag == "-l" && mode == "compile")),
        "a link flag in compile mode must mark the command impure; got {:?}",
        ec.impurities
    );
}

#[test]
fn link_flags_in_link_mode_are_fine() {
    // No selector -> default (link) mode; -shared and -Wl, are legal.
    let ec = effective_command(
        &CC,
        &argv(&[
            "cc",
            "-shared",
            "-Wl,--gc-sections",
            "-lm",
            "x.o",
            "-o",
            "lib.so",
        ]),
        &BTreeMap::new(),
    );
    assert!(
        ec.impurities.is_empty(),
        "link flags in link mode are legal; got {:?}",
        ec.impurities
    );
}

#[test]
fn compile_mode_flag_set_is_clean() {
    // An ordinary compile (no link flags) stays clean.
    let c = canonicalize(
        &CC,
        &argv(&["cc", "-c", "-O2", "-I", "src", "src/x.c", "-o", "x.o"]),
    );
    assert!(
        !c.lints
            .iter()
            .any(|l| matches!(l, command_model::Lint::IllegalInMode { .. })),
        "a normal compile must not trip the mode guard; got {:?}",
        c.lints
    );
}
