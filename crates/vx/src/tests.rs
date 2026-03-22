use super::*;

#[test]
fn parse_eval_arg_converts_nested_json_values() {
    let value = parse_eval_arg(r#"{"name":"serde","versions":[1,2]}"#).expect("parse arg");
    let ValueRep::Map(map) = &value.rep else {
        panic!("expected map arg, got {:?}", value.rep);
    };

    assert_eq!(map.entries.len(), 2);
    assert!(map.entries.iter().any(|(key, value)| {
        matches!((&key.rep, &value.rep), (ValueRep::String(key), ValueRep::String(value)) if key.as_ref() == "name" && value.as_ref() == "serde")
    }));
    assert!(map.entries.iter().any(|(key, value)| {
        matches!((&key.rep, &value.rep), (ValueRep::String(key), ValueRep::Array(items)) if key.as_ref() == "versions" && items.len() == 2)
    }));
}

#[test]
fn read_eval_source_uses_parent_directory_for_runtime_root() {
    let (_source, source_id, root_dir) = read_eval_source("Cargo.toml").expect("read source");

    assert!(source_id.ends_with("Cargo.toml"));
    assert_eq!(root_dir, std::env::current_dir().expect("cwd"));
}

#[test]
fn local_eval_runtime_supports_read_file() {
    let runtime = LocalEvalRuntime {
        root_dir: std::env::current_dir().expect("cwd"),
    };
    let outcome = runtime
        .call(
            "local",
            "read_file",
            &[EvalValue::string("Cargo.toml")],
            &SourceSpan::synthetic("test"),
        )
        .expect("runtime call");

    let RuntimeCallOutcome::Ready(value) = outcome else {
        panic!("expected ready outcome, got {outcome:?}");
    };
    assert!(matches!(value.rep, ValueRep::External(_)));
}
