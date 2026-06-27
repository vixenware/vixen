//! The Tier-1 compatibility layer: the cache identity (`effective_command`)
//! folds values the tool treats as equivalent, while the literal canonical form
//! keeps them verbatim. A minimal rustc-shaped grammar exercises the three rule
//! kinds: default-value elision, value classes, and ignored flags.

use command_model::{Compat, Flag, Grammar, GrammarSet, Policy, Spelling};
use std::collections::BTreeMap;

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn cflag(canonical: &str, role: &str) -> Flag {
    Flag {
        canonical: canonical.into(),
        spelling: Spelling::Suffixed { sep: "=".into() },
        policy: Policy::Once,
        role: role.into(),
        ..Flag::DEFAULT
    }
}

fn compat(flag: &str, ignored: bool, classes: Vec<Vec<String>>, default_value: &str) -> Compat {
    Compat {
        flag: flag.into(),
        ignored,
        classes,
        default_value: default_value.into(),
    }
}

/// `-Copt-level` (default 0), `-Cdebuginfo` (classes, default 0),
/// `-Ccodegen-units` (ignored).
fn rc() -> GrammarSet {
    GrammarSet {
        grammars: vec![Grammar {
            name: "rc".into(),
            flags: vec![
                cflag("-Copt-level", "opt-level"),
                cflag("-Cdebuginfo", "debuginfo"),
                cflag("-Ccodegen-units", "codegen-units"),
            ],
            env: vec![],
            positionals: vec![],
            modes: vec![],
            compat: vec![
                compat("-Copt-level", false, vec![], "0"),
                compat(
                    "-Cdebuginfo",
                    false,
                    vec![
                        vec!["0".into(), "none".into()],
                        vec!["1".into(), "limited".into()],
                        vec!["2".into(), "full".into()],
                    ],
                    "0",
                ),
                compat("-Ccodegen-units", true, vec![], ""),
            ],
        }],
    }
}

#[test]
fn default_value_drops_from_cache_but_not_canonical() {
    let gs = rc();
    let ec = gs.effective_command("rc", &argv(&["rustc", "-Copt-level=0"]), &BTreeMap::new());
    assert!(
        ec.args.is_empty(),
        "opt-level=0 is the default; it drops from the cache identity: {:?}",
        ec.args
    );
    // …but the literal canonical form keeps it (explain/diff see the real flags).
    let c = gs.canonicalize("rc", &argv(&["rustc", "-Copt-level=0"]));
    assert_eq!(c.args, argv(&["-Copt-level=0"]));
}

#[test]
fn value_classes_collapse_in_cache() {
    let gs = rc();
    let a = gs.effective_command(
        "rc",
        &argv(&["rustc", "-Cdebuginfo=full"]),
        &BTreeMap::new(),
    );
    let b = gs.effective_command("rc", &argv(&["rustc", "-Cdebuginfo=2"]), &BTreeMap::new());
    assert_eq!(
        a.args,
        argv(&["-Cdebuginfo=2"]),
        "canonicalized to the class rep"
    );
    assert_eq!(a.args, b.args, "`full` and `2` are cache-equivalent");
}

#[test]
fn ignored_flag_drops_from_cache() {
    let gs = rc();
    let ec = gs.effective_command(
        "rc",
        &argv(&["rustc", "-Ccodegen-units=16"]),
        &BTreeMap::new(),
    );
    assert!(
        ec.args.is_empty(),
        "codegen-units never affects the cache identity: {:?}",
        ec.args
    );
}

#[test]
fn equivalent_commands_share_one_cache_key() {
    let gs = rc();
    let verbose = gs.effective_command(
        "rc",
        &argv(&[
            "rustc",
            "-Copt-level=0",
            "-Cdebuginfo=full",
            "-Ccodegen-units=16",
        ]),
        &BTreeMap::new(),
    );
    let terse = gs.effective_command("rc", &argv(&["rustc", "-Cdebuginfo=2"]), &BTreeMap::new());
    assert_eq!(
        verbose.args, terse.args,
        "compat folds both invocations to the same identity"
    );
    assert!(
        command_model::diff(&verbose, &terse).is_empty(),
        "so they share a cache key — no rebuild"
    );
}
