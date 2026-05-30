//! The dream-bigger consumers on the structured model: "explain this command"
//! and cross-dialect translation (gcc → MSVC).

use command_model::{CC, CL, explain, translate};

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[test]
fn explain_describes_flags_and_positionals() {
    let ex = explain(&CC, &argv(&["cc", "-c", "-I", "src", "x.c", "-o", "out.o"]));
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
    // `-I inc` (separated) becomes `/Iinc` (glued), the target's spelling.
    let out = translate(
        &CC,
        &argv(&["cc", "-c", "-O2", "-DFOO", "-I", "inc", "x.c", "-o", "x.o"]),
        &CL,
    )
    .expect("fully translatable");
    assert_eq!(out, argv(&["/c", "/O2", "/DFOO", "/Iinc", "x.c", "/Fox.o"]));
}

#[test]
fn translate_reports_untranslatable_flags() {
    // -shared has no MSVC correspondence in this table.
    let err = translate(&CC, &argv(&["cc", "-shared", "x.o", "-o", "lib"]), &CL)
        .expect_err("should fail on -shared");
    assert!(
        err.iter().any(|u| u.token == "-shared"),
        "the untranslatable flag is named; got {:?}",
        err
    );
}
