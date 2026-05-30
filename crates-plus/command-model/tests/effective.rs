//! The effective-command closure: declared env (explicit only), the input set
//! the cache must hash (argv paths, env paths, bubbled-up passthrough paths),
//! and impurity marks.

use command_model::{CC, Impurity, Input, effective_command};
use std::collections::BTreeMap;

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn has(ec: &command_model::EffectiveCommand, role: &str, path: &str) -> bool {
    ec.inputs.contains(&Input {
        role: role.to_string(),
        path: path.to_string(),
    })
}

#[test]
fn declared_env_enters_identity_ambient_does_not() {
    let e = env(&[
        ("CPATH", "vendor/include:gen"),
        ("HOME", "/home/amos"),
        ("RANDOM_THING", "x"),
    ]);
    let ec = effective_command(
        &CC,
        &argv(&["cc", "-c", "-I", "src", "src/x.c", "-o", "x.o"]),
        &e,
    );

    // Only the declared channel is consulted; HOME / RANDOM_THING are ignored.
    assert_eq!(
        ec.env,
        vec![("CPATH".to_string(), "vendor/include:gen".to_string())],
        "undeclared env must not enter the identity"
    );

    // CPATH's dirs become include inputs alongside the explicit -I.
    assert!(has(&ec, "include", "vendor/include"));
    assert!(has(&ec, "include", "gen"));
    assert!(has(&ec, "include", "src"));
    // The source file is an input.
    assert!(has(&ec, "source", "src/x.c"));
}

#[test]
fn passthrough_path_inputs_bubble_up() {
    // --version-script names a file inside the -Wl, passthrough; it must surface
    // as an input of the outer command.
    let ec = effective_command(
        &CC,
        &argv(&[
            "cc",
            "-shared",
            "-Wl,--version-script=link.map",
            "-o",
            "lib.so",
            "x.o",
        ]),
        &BTreeMap::new(),
    );
    assert!(has(&ec, "version-script", "link.map"), "{:?}", ec.inputs);
    assert!(has(&ec, "source", "x.o"));
}

#[test]
fn unknown_flag_marks_impure() {
    let ec = effective_command(
        &CC,
        &argv(&["cc", "-c", "--frobnicate", "src/x.c", "-o", "x.o"]),
        &BTreeMap::new(),
    );
    assert!(
        ec.impurities
            .iter()
            .any(|i| matches!(i, Impurity::UnknownFlag { token } if token == "--frobnicate")),
        "an undescribed flag must mark the command impure; got {:?}",
        ec.impurities
    );
}

#[test]
fn inputs_are_deduped() {
    // The same dir reached via -I and via CPATH collapses to one input.
    let e = env(&[("CPATH", "shared")]);
    let ec = effective_command(
        &CC,
        &argv(&["cc", "-c", "-I", "shared", "src/x.c", "-o", "x.o"]),
        &e,
    );
    let shared = ec
        .inputs
        .iter()
        .filter(|i| i.path == "shared" && i.role == "include")
        .count();
    assert_eq!(
        shared, 1,
        "duplicate input from argv + env must dedup; got {:?}",
        ec.inputs
    );
}
