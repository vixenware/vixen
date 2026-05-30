//! "Why did it rebuild?" — diffing two effective commands pinpoints exactly
//! what changed in the cache identity.

use command_model::{Change, diff, examples};
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

#[test]
fn identical_commands_have_no_diff() {
    let cmd = argv(&["cc", "-c", "-O2", "-I", "src", "x.c", "-o", "x.o"]);
    let a = examples().effective_command("cc", &cmd, &BTreeMap::new());
    let b = examples().effective_command("cc", &cmd, &BTreeMap::new());
    assert!(
        diff(&a, &b).is_empty(),
        "same command -> one cache key -> no diff"
    );
}

#[test]
fn diff_pinpoints_the_one_added_flag() {
    let a = examples().effective_command(
        "cc",
        &argv(&["cc", "-c", "-O2", "-I", "src", "x.c", "-o", "x.o"]),
        &BTreeMap::new(),
    );
    let b = examples().effective_command(
        "cc",
        &argv(&["cc", "-c", "-O2", "-I", "src", "-DFOO", "x.c", "-o", "x.o"]),
        &BTreeMap::new(),
    );
    assert_eq!(
        diff(&a, &b),
        vec![Change::ArgAdded("-DFOO".to_string())],
        "the rebuild reason is exactly: a define was added"
    );
}

#[test]
fn diff_catches_an_input_that_moved_in_via_env() {
    let a = examples().effective_command(
        "cc",
        &argv(&["cc", "-c", "x.c", "-o", "x.o"]),
        &BTreeMap::new(),
    );
    let b = examples().effective_command(
        "cc",
        &argv(&["cc", "-c", "x.c", "-o", "x.o"]),
        &env(&[("CPATH", "vendor")]),
    );
    let changes = diff(&a, &b);

    assert!(
        changes
            .iter()
            .any(|c| matches!(c, Change::EnvChanged { name, .. } if name == "CPATH")),
        "the new env channel is reported; got {:?}",
        changes
    );
    assert!(
        changes
            .iter()
            .any(|c| matches!(c, Change::InputAdded(i) if i.path == "vendor")),
        "and the include dir it pulls in becomes a new input; got {:?}",
        changes
    );
}
