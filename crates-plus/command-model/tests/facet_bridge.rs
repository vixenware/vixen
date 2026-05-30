//! Half-A of the grammar bridge: grammars load as **facet JSON** with zero
//! evaluation, and the deserialized [`GrammarSet`] drives the engine identically
//! to the in-code reference set.
//!
//! `fixtures/grammars.json` is the on-disk contract the daemon's `cc.vx`
//! evaluation must eventually produce (half B); for now it is generated from the
//! reference `examples()` set. Regenerate it with:
//!
//! ```sh
//! cargo test -p command-model regen_fixture -- --ignored
//! ```

use command_model::{GrammarSet, examples, translate};

const FIXTURE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/grammars.json");

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn load() -> GrammarSet {
    let json = std::fs::read_to_string(FIXTURE_PATH)
        .expect("run `cargo test -p command-model regen_fixture -- --ignored` to generate it");
    facet_json::from_str(&json).expect("fixture is valid facet JSON for a GrammarSet")
}

/// Not a test — the generator for the committed fixture. Ignored so a normal run
/// never rewrites it; run explicitly to regenerate after changing the grammars.
#[test]
#[ignore = "regenerates the committed fixture from examples()"]
fn regen_fixture() {
    let json = facet_json::to_string(&examples()).expect("serialize the reference set");
    std::fs::create_dir_all(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures")).unwrap();
    std::fs::write(FIXTURE_PATH, json).unwrap();
}

#[test]
fn fixture_is_exactly_the_reference_set() {
    assert_eq!(
        load(),
        examples(),
        "the committed JSON round-trips to the in-code reference set; \
         if grammars changed, regenerate the fixture"
    );
}

#[test]
fn deserialized_grammars_drive_the_engine() {
    // The whole point: parse JSON -> GrammarSet, then run the engine, with ZERO
    // evaluation anywhere in this path.
    let set = load();

    // Passthrough resolves "ld" *by name* within the deserialized set, and the
    // inner ld command canonicalizes regardless of order.
    let c = set.canonicalize(
        "cc",
        &argv(&[
            "cc",
            "-shared",
            "-Wl,--as-needed,--gc-sections",
            "-o",
            "lib.so",
            "x.o",
        ]),
    );
    assert!(
        c.args.iter().any(|t| t == "-Wl,--gc-sections,--as-needed"),
        "passthrough canonicalized via the by-name ld grammar; got {:?}",
        c.args
    );

    // Mode-aware translation cc -> cl, both grammars loaded entirely from JSON:
    // `-o` in compile mode reaches `/Fo`, and `-I inc` re-spells to `/Iinc`.
    let out = translate(
        set.get("cc").unwrap(),
        &argv(&["cc", "-c", "-I", "inc", "x.c", "-o", "x.o"]),
        set.get("cl").unwrap(),
    );
    assert_eq!(out.args, argv(&["/c", "/Iinc", "x.c", "/Fox.o"]));
    assert!(out.untranslatable.is_empty());
}
