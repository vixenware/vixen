//! MSVC `cl.exe` coverage: DualLead (`/c` ≡ `-c`), ColonAttached (`/std:`),
//! GluedNoDelim (`/Foout.obj`), and that declared env (`INCLUDE`) enters the
//! identity while undeclared env stays out.

use command_model::examples;
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
fn dual_lead_canonicalizes_to_one_form() {
    // clang-cl accepts `-`-led flags; they must canonicalize to the `/` form.
    let slash = examples().canonicalize(
        "cl",
        &argv(&["cl", "/c", "/O2", "/DFOO", "/Iinc", "x.c", "/Fox.obj"]),
    );
    let dash = examples().canonicalize(
        "cl",
        &argv(&["cl", "-c", "-O2", "-DFOO", "-Iinc", "x.c", "-Fox.obj"]),
    );

    assert_eq!(
        slash.args, dash.args,
        "/-lead and --lead canonicalize identically"
    );
    assert_eq!(
        slash.args,
        argv(&["/c", "/O2", "/DFOO", "/Iinc", "x.c", "/Fox.obj"]),
        "canonical form uses the / spelling and the fixed layout"
    );
}

#[test]
fn colon_attached_std() {
    let c = examples().canonicalize("cl", &argv(&["cl", "/c", "/std:c++20", "x.c", "/Fox.obj"]));
    assert!(
        c.args.contains(&"/std:c++20".to_string()),
        "got {:?}",
        c.args
    );
    // dash spelling lands on the same canonical token
    let d = examples().canonicalize("cl", &argv(&["cl", "-c", "-std:c++20", "x.c", "-Fox.obj"]));
    assert_eq!(c.args, d.args);
}

#[test]
fn glued_no_delim_output_last() {
    // `/Fo` glues its path with no delimiter; it's an output, emitted last.
    let c = examples().canonicalize("cl", &argv(&["cl", "/Foobj/x.obj", "/c", "x.c"]));
    assert_eq!(c.args, argv(&["/c", "x.c", "/Foobj/x.obj"]));
}

#[test]
fn last_opt_level_wins() {
    let c = examples().canonicalize("cl", &argv(&["cl", "/c", "/Od", "/O2", "x.c", "/Fox.obj"]));
    assert!(c.args.contains(&"/O2".to_string()) && !c.args.contains(&"/Od".to_string()));
}

#[test]
fn include_env_enters_identity_path_stays_out() {
    let e = env(&[("INCLUDE", "/sdk/include"), ("PATH", "whatever")]);
    let ec = examples().effective_command("cl", &argv(&["cl", "/c", "x.c", "/Fox.obj"]), &e);
    assert!(
        ec.env.iter().any(|(k, _)| k == "INCLUDE"),
        "declared INCLUDE is consumed"
    );
    assert!(
        !ec.env.iter().any(|(k, _)| k == "PATH"),
        "undeclared PATH stays out"
    );
}
