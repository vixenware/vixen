//! `rustc.vx` loads through the parser and drives the engine's Tier-1 compat
//! layer end-to-end: invocations rustc treats as equivalent fold to one cache
//! identity, while the literal canonical form is preserved. Zero evaluation.

use command_grammar_vx::load_str;
use std::collections::BTreeMap;

const RUSTC_VX: &str = include_str!("../../command-model/grammars/rustc.vx");

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// The cache identity (compat-normalized) for a rustc invocation.
fn identity(args: &[&str]) -> Vec<String> {
    let set = load_str(RUSTC_VX).expect("rustc.vx loads");
    set.effective_command("rustc", &argv(args), &BTreeMap::new())
        .args
}

#[test]
fn rustc_vx_loads() {
    let set = load_str(RUSTC_VX).expect("rustc.vx loads");
    assert!(set.get("rustc").is_some(), "the rustc grammar is present");
}

#[test]
fn default_and_ignored_knobs_drop_from_cache() {
    // opt-level=0 (rustc default) and codegen-units (ignored) vanish from the id.
    let id = identity(&[
        "rustc",
        "--edition=2021",
        "-Copt-level=0",
        "-Ccodegen-units=16",
        "src/lib.rs",
    ]);
    assert_eq!(id, argv(&["--edition=2021", "src/lib.rs"]));
}

#[test]
fn equivalent_invocations_share_one_cache_key() {
    let a = identity(&["rustc", "-Cdebuginfo=full", "-Clto=fat", "src/lib.rs"]);
    let b = identity(&["rustc", "-Cdebuginfo=2", "-Clto=yes", "src/lib.rs"]);
    assert_eq!(
        a, b,
        "debuginfo full≡2 and lto fat≡yes are cache-equivalent"
    );
    assert_eq!(a, argv(&["-Cdebuginfo=2", "-Clto=fat", "src/lib.rs"]));
}

#[test]
fn lto_off_is_the_default_and_drops() {
    assert_eq!(
        identity(&["rustc", "-Clto=off", "src/lib.rs"]),
        argv(&["src/lib.rs"]),
        "lto=off is the default; it drops"
    );
}

#[test]
fn literal_canonical_keeps_everything() {
    // The display form is faithful — explain/diff still see the real flags.
    let set = load_str(RUSTC_VX).unwrap();
    let c = set.canonicalize(
        "rustc",
        &argv(&["rustc", "-Copt-level=0", "-Ccodegen-units=16", "src/lib.rs"]),
    );
    assert_eq!(
        c.args,
        argv(&["-Copt-level=0", "-Ccodegen-units=16", "src/lib.rs"])
    );
}
