//! The dream-bigger consumers on the structured model: "explain this command"
//! and cross-dialect translation (gcc → MSVC).

use command_model::{examples, explain, translate};

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[test]
fn explain_describes_flags_and_positionals() {
    let ex = explain(
        examples().get("cc").unwrap(),
        &argv(&["cc", "-c", "-I", "src", "x.c", "-o", "out.o"]),
    );
    let summary = |sp: &str| {
        ex.items
            .iter()
            .find(|i| i.spelling == sp)
            .map(|i| i.summary.clone())
            .unwrap_or_default()
    };

    assert!(
        summary("-o").contains("output path"),
        "got: {}",
        summary("-o")
    );
    assert!(summary("-I").contains("include"), "got: {}", summary("-I"));
    assert!(
        summary("x.c").contains("input"),
        "the positional knows it's an input: {}",
        summary("x.c")
    );
    assert!(
        summary("-c").contains("-c"),
        "every token is explained: {}",
        summary("-c")
    );
}

#[test]
fn translate_gcc_to_msvc() {
    // A gcc compile, re-emitted in the MSVC dialect, mechanically — note
    // `-I inc` (separated) becomes `/Iinc` (glued), the target's spelling, and
    // `-o` in *compile* mode (because `-c` is present) becomes `/Fo`, not `/Fe`.
    let out = translate(
        examples().get("cc").unwrap(),
        &argv(&["cc", "-c", "-O2", "-DFOO", "-I", "inc", "x.c", "-o", "x.o"]),
        examples().get("cl").unwrap(),
    );
    assert!(
        out.untranslatable.is_empty(),
        "fully translatable; got {:?}",
        out.untranslatable
    );
    assert_eq!(
        out.args,
        argv(&["/c", "/O2", "/DFOO", "/Iinc", "x.c", "/Fox.o"])
    );
    assert!(out.env.is_empty(), "nothing went to env; got {:?}", out.env);
}

#[test]
fn translate_mode_splits_output_flag() {
    // The same `-o`, but in *link* mode (no `-c`), must reach the other side of
    // MSVC's output split: `/Fe`, not `/Fo`. Same role + operation, different
    // mode — only the mode disambiguates.
    let out = translate(
        examples().get("cc").unwrap(),
        &argv(&["cc", "x.o", "-o", "app"]),
        examples().get("cl").unwrap(),
    );
    assert_eq!(out.args, argv(&["x.o", "/Feapp"]));
}

#[test]
fn translate_moves_a_flag_to_an_env_channel() {
    // The holistic payoff: MSVC has no library-search *flag* in this grammar,
    // but it has the `LIB` env channel (same role + operation). So `-L libs`
    // can't become a flag — it becomes `LIB=libs`. argv and env, together.
    let out = translate(
        examples().get("cc").unwrap(),
        &argv(&["cc", "-c", "-L", "libs", "x.c", "-o", "x.o"]),
        examples().get("cl").unwrap(),
    );
    assert!(
        out.untranslatable.is_empty(),
        "everything lands somewhere; got {:?}",
        out.untranslatable
    );
    assert_eq!(out.args, argv(&["/c", "x.c", "/Fox.o"]));
    assert_eq!(out.env, vec![("LIB".to_string(), "libs".to_string())]);
}

#[test]
fn translate_reports_untranslatable_flags() {
    // -shared has no MSVC correspondence — no flag and no env channel.
    let out = translate(
        examples().get("cc").unwrap(),
        &argv(&["cc", "-shared", "x.o", "-o", "lib"]),
        examples().get("cl").unwrap(),
    );
    assert!(
        out.untranslatable.iter().any(|u| u.token == "-shared"),
        "the untranslatable flag is named; got {:?}",
        out.untranslatable
    );
}
