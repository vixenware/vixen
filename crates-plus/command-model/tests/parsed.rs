//! The role-tagged structured parse: every token knows the part it plays, and
//! every positional is resolved against the grammar's schema to its *meaning*.

use command_model::{AR, ArgRole, CC, Kind, ParsedCommand, Span, parse_command};

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn positionals(pc: &ParsedCommand) -> Vec<(String, String)> {
    pc.args
        .iter()
        .filter_map(|a| match &a.role {
            ArgRole::Positional { name, value, .. } => Some((name.clone(), value.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn cc_positionals_resolve_to_input() {
    let pc = parse_command(
        &CC,
        &argv(&["cc", "-c", "src/a.c", "-O2", "b.o", "-o", "out.o"]),
    );
    assert_eq!(
        positionals(&pc),
        vec![
            ("input".to_string(), "src/a.c".to_string()),
            ("input".to_string(), "b.o".to_string()),
        ],
        "every positional is tagged with its schema meaning"
    );
}

#[test]
fn ar_positionals_know_operation_archive_members() {
    let pc = parse_command(&AR, &argv(&["ar", "rcs", "libfoo.a", "a.o", "b.o"]));
    assert_eq!(
        positionals(&pc),
        vec![
            ("operation".to_string(), "rcs".to_string()),
            ("archive".to_string(), "libfoo.a".to_string()),
            ("member".to_string(), "a.o".to_string()),
            ("member".to_string(), "b.o".to_string()),
        ],
        "fixed-then-variadic schema gives each positional a distinct meaning"
    );

    // The archive knows it's an output; the members are inputs.
    let kinds: Vec<(String, Kind)> = pc
        .args
        .iter()
        .filter_map(|a| match &a.role {
            ArgRole::Positional { name, kind, .. } => Some((name.clone(), *kind)),
            _ => None,
        })
        .collect();
    assert!(kinds.contains(&("archive".to_string(), Kind::PathOut)));
    assert!(kinds.contains(&("member".to_string(), Kind::PathIn { dir: false })));
}

#[test]
fn flags_are_role_tagged_with_spans() {
    let pc = parse_command(&CC, &argv(&["cc", "-I", "inc", "a.c", "-o", "out.o"]));

    // -I consumes the following token, so its span covers both indices.
    let inc = pc
        .args
        .iter()
        .find(|a| matches!(&a.role, ArgRole::Flag { canonical, .. } if canonical == "-I"))
        .expect("found -I");
    assert_eq!(inc.span, Span { start: 0, end: 1 });

    // -o is known to be an output path.
    assert!(
        pc.args.iter().any(|a| matches!(
            &a.role,
            ArgRole::Flag { canonical, kind, .. } if canonical == "-o" && matches!(kind, Kind::PathOut)
        )),
        "the output flag carries its PathOut kind"
    );
}
