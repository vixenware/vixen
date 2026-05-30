//! The oracle: loading the authored `cc.vx` must reproduce the engine's in-code
//! reference grammars exactly — proving the lowering is faithful and killing the
//! `cc.vx`-vs-`examples()` duplication (any drift fails here). It must also
//! *drive the engine*: a `-Wl,` command canonicalizes through the by-name `ld`
//! passthrough resolved within the loaded set.

use command_grammar_vx::load_str;

const CC_VX: &str = include_str!("../../../packages/@vixen/grammar-cc/main.vx");

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[test]
fn cc_vx_loads_to_the_reference_grammars() {
    let loaded = load_str(CC_VX).expect("cc.vx loads");
    let reference = command_model::examples();

    for name in ["cc", "ld"] {
        assert_eq!(
            loaded.get(name),
            reference.get(name),
            "loaded `{name}` must equal the in-code reference grammar"
        );
    }
}

#[test]
fn loaded_grammar_drives_the_engine_through_passthrough() {
    let set = load_str(CC_VX).expect("cc.vx loads");
    // `-Wl,` resolves the loaded `ld` grammar *by name* and canonicalizes its
    // inner command regardless of order.
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
        "passthrough canonicalized via the loaded ld grammar; got {:?}",
        c.args
    );
}
