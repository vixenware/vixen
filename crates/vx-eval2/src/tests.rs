use std::{
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use crate::{
    Claim, CrossingPolicy, EvalBranchContext, EvalCompletedWorkOutcome, EvalDependencyEdge,
    EvalError, EvalErrorKind, EvalEvent, EvalEventKind, EvalExplainTerminal,
    EvalKeyedNodeUpdateReason, EvalLeasedWorkState, EvalNodeId, EvalNodeKind, EvalNodeState,
    EvalProgram, EvalProgramImport, EvalProgramSource, EvalSession, EvalSessionStep, EvalTrace,
    EvalWorkKind, EvalWorkState, MapValue, PendingRef, ProjectionStep, Provenance, Resolution,
    ReuseClass, RuntimeCallOutcome, RuntimeHooks, RuntimeInputKind, RuntimeReady,
    RuntimeSlotPrepare, SlotAction, SlotDecision, SlotEventReason, SlotId, SlotResolutionKind,
    SlotResolved, SlotStore, SlotStoreReason, SlotVerification, SourceSpan, Value, ValueRep,
    channel_trace, envelope_to_value, eval_source_entry_at_place_with_args_with_source_id,
    eval_source_entry_at_place_with_args_with_source_id_traced,
    eval_source_entry_with_args_with_source_id, eval_source_entry_with_args_with_source_id_traced,
    eval_source_entry_with_args_with_source_id_with_sink, resolution_to_outcome, value_to_envelope,
};
use tempfile::tempdir;
use vx_eval2_proto::EvalOutcome;

fn expect_ready(resolution: Resolution) -> Value {
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready result, got {resolution:?}");
    };
    value
}

fn struct_field<'a>(value: &'a Value, name: &str) -> &'a Value {
    let ValueRep::Struct(struct_value) = &value.rep else {
        panic!("expected struct value, got {value:?}");
    };
    struct_value
        .fields
        .iter()
        .find_map(|(field_name, field_value)| (field_name.as_ref() == name).then_some(field_value))
        .unwrap_or_else(|| panic!("expected field {name:?} in {struct_value:?}"))
}

fn map_string_entry<'a>(value: &'a Value, key: &str) -> &'a Value {
    let ValueRep::Map(map) = &value.rep else {
        panic!("expected map value, got {value:?}");
    };
    map.entries
        .iter()
        .find_map(|(entry_key, entry_value)| {
            matches!(&entry_key.rep, ValueRep::String(text) if text.as_ref() == key)
                .then_some(entry_value)
        })
        .unwrap_or_else(|| panic!("expected key {key:?} in {map:?}"))
}

struct ReadFileRuntime {
    root_dir: PathBuf,
}

impl RuntimeHooks for ReadFileRuntime {
    fn call(
        &self,
        _place: &str,
        callee: &str,
        args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match callee {
            "read_file" => {
                if args.len() != 1 {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "read_file() requires exactly one string argument",
                    ));
                }
                let ValueRep::String(path) = &args[0].rep else {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "read_file() argument must evaluate to String",
                    ));
                };
                let full_path = self.root_dir.join(path.as_ref());
                let contents = fs::read_to_string(&full_path).map_err(|error| {
                    EvalError::new(
                        EvalErrorKind::UserFail,
                        format!("read_file() failed for {}: {error}", full_path.display()),
                    )
                })?;
                Ok(RuntimeCallOutcome::Ready(
                    Value::external("Blob", contents)
                        .with_provenance(Provenance::unknown(source.clone()))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue),
                ))
            }
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
    }
}

#[test]
fn current_populates_missing_slot_and_then_reuses_it() {
    let mut slots = SlotStore::<u64, String, String, String>::default();

    let initial = slots
        .resolve(
            "toolchain",
            "w1".to_string(),
            SlotAction::<String>::Current,
            |_| SlotVerification::Valid,
            |_| Ok::<_, EvalError>(SlotResolved::new(41)),
        )
        .expect("populate missing slot");
    assert_eq!(
        initial.kind,
        SlotResolutionKind::Stored {
            reason: SlotStoreReason::Missing,
        }
    );
    assert_eq!(initial.record.slot_id, SlotId::from("toolchain"));
    assert_eq!(initial.record.value, 41);
    assert_eq!(initial.record.meta.write_count, 1);
    assert_eq!(initial.record.meta.rewrite_count, 0);

    let reused = slots
        .resolve(
            "toolchain",
            "w1".to_string(),
            SlotAction::<String>::Current,
            |_| SlotVerification::Valid,
            |_| -> Result<SlotResolved<u64, String>, EvalError> {
                panic!("reused slot should not resolve again")
            },
        )
        .expect("reuse existing slot");
    assert_eq!(reused.kind, SlotResolutionKind::Reused);
    assert_eq!(reused.record.value, 41);
    assert_eq!(reused.record.meta.write_count, 1);
    assert_eq!(reused.record.meta.rewrite_count, 0);
}

#[test]
fn witness_changes_and_failed_verification_rewrite_in_place() {
    let mut slots = SlotStore::<u64, String, String, String>::default();

    slots
        .resolve(
            "toolchain",
            "w1".to_string(),
            SlotAction::<String>::Current,
            |_| SlotVerification::Valid,
            |_| Ok::<_, EvalError>(SlotResolved::new(41)),
        )
        .expect("populate slot");

    let witness_changed = slots
        .resolve(
            "toolchain",
            "w2".to_string(),
            SlotAction::<String>::Current,
            |_| SlotVerification::Valid,
            |_| Ok::<_, EvalError>(SlotResolved::new(42)),
        )
        .expect("rewrite after witness change");
    assert_eq!(
        witness_changed.kind,
        SlotResolutionKind::Stored {
            reason: SlotStoreReason::WitnessChanged,
        }
    );
    assert_eq!(slots.len(), 1);
    assert_eq!(witness_changed.record.slot_id, SlotId::from("toolchain"));
    assert_eq!(witness_changed.record.value, 42);
    assert_eq!(witness_changed.record.meta.write_count, 2);
    assert_eq!(witness_changed.record.meta.rewrite_count, 1);

    let claims_invalid = slots
        .resolve(
            "toolchain",
            "w2".to_string(),
            SlotAction::<String>::Current,
            |_| SlotVerification::invalid("claims no longer verify"),
            |_| Ok::<_, EvalError>(SlotResolved::new(43)),
        )
        .expect("rewrite after verification failure");
    assert_eq!(slots.len(), 1);
    assert_eq!(claims_invalid.record.slot_id, SlotId::from("toolchain"));
    assert_eq!(claims_invalid.record.value, 43);
    assert_eq!(claims_invalid.record.meta.write_count, 3);
    assert_eq!(claims_invalid.record.meta.rewrite_count, 2);
    assert_eq!(
        claims_invalid.record.meta.last_store_reason,
        SlotStoreReason::ClaimsInvalid {
            reason: "claims no longer verify".into(),
        }
    );
}

#[test]
fn refresh_forces_recompute_and_exposes_current_value_to_resolver() {
    let mut slots = SlotStore::<u64, String, String, String>::default();
    slots
        .resolve(
            "toolchain",
            "w1".to_string(),
            SlotAction::<String>::Current,
            |_| SlotVerification::Valid,
            |_| Ok::<_, EvalError>(SlotResolved::new(41)),
        )
        .expect("populate slot");

    let seen_current = Arc::new(Mutex::new(Vec::new()));
    let seen_current_clone = Arc::clone(&seen_current);
    let refreshed = slots
        .resolve(
            "toolchain",
            "w1".to_string(),
            SlotAction::Refresh("force-rediscover".to_string()),
            |_| SlotVerification::Valid,
            move |request| {
                seen_current_clone
                    .lock()
                    .expect("capture lock poisoned")
                    .push((
                        request.current.map(|record| record.value),
                        request.action.clone(),
                    ));
                Ok::<_, EvalError>(SlotResolved::new(99))
            },
        )
        .expect("refresh slot");

    assert_eq!(
        refreshed.kind,
        SlotResolutionKind::Stored {
            reason: SlotStoreReason::Refresh {
                intent: "force-rediscover".to_string(),
            },
        }
    );
    assert_eq!(slots.len(), 1);
    assert_eq!(refreshed.record.slot_id, SlotId::from("toolchain"));
    assert_eq!(refreshed.record.value, 99);
    assert_eq!(refreshed.record.meta.write_count, 2);
    assert_eq!(refreshed.record.meta.rewrite_count, 1);
    assert_eq!(
        refreshed.record.meta.last_store_reason,
        SlotStoreReason::Refresh {
            intent: "force-rediscover".to_string(),
        }
    );
    assert_eq!(
        seen_current
            .lock()
            .expect("capture lock poisoned")
            .as_slice(),
        &[(
            Some(41),
            SlotAction::Refresh("force-rediscover".to_string())
        )]
    );

    let current = slots
        .resolve(
            "toolchain",
            "w1".to_string(),
            SlotAction::<String>::Current,
            |_| SlotVerification::Valid,
            |_| -> Result<SlotResolved<u64, String>, EvalError> {
                panic!("refreshed slot should reuse its new current value")
            },
        )
        .expect("reuse refreshed slot");
    assert_eq!(current.kind, SlotResolutionKind::Reused);
    assert_eq!(current.record.value, 99);
}

#[test]
fn same_source_helper_function_is_evaluated() {
    let resolution = eval_source_entry_with_args_with_source_id(
        "fn helper(value: U64) { value }\nfn build() { helper(41) }",
        "same-source-helper",
        "build",
        &[],
    )
    .expect("evaluate same-source helper call");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready helper result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::U64(41));
}

#[test]
fn top_level_function_value_can_be_bound_and_called_indirectly() {
    let resolution = eval_source_entry_with_args_with_source_id(
        "fn helper(value: U64) { value }\nfn build() {\n  let f = helper;\n  f(41)\n}",
        "same-source-function-value",
        "build",
        &[],
    )
    .expect("evaluate indirect same-source function call");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready indirect helper result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::U64(41));
}

#[test]
fn same_source_helpers_can_call_each_other() {
    let resolution = eval_source_entry_with_args_with_source_id(
        "fn inner(value: U64) { value }\nfn outer() { inner(42) }\nfn build() { outer() }",
        "same-source-helper-chain",
        "build",
        &[],
    )
    .expect("evaluate nested helper call");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready nested helper result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::U64(42));
}

#[test]
fn block_local_named_function_can_be_called() {
    let resolution = eval_source_entry_with_args_with_source_id(
        "fn build() { fn helper(value: U64) { value } helper(42) }",
        "block-local-helper",
        "build",
        &[],
    )
    .expect("evaluate block-local helper");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready block-local helper result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::U64(42));
}

#[test]
fn block_local_named_function_captures_outer_binding() {
    let resolution = eval_source_entry_with_args_with_source_id(
        "fn build() {\n  let captured = 41;\n  fn helper() { captured }\n  helper()\n}",
        "block-local-capture",
        "build",
        &[],
    )
    .expect("evaluate block-local captured helper");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready block-local captured result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::U64(41));
}

#[test]
fn recursive_named_function_works() {
    let resolution = eval_source_entry_with_args_with_source_id(
        "fn recurse(flag: Bool) { if flag { recurse(false) } else { 41 } }\nfn build() { recurse(true) }",
        "recursive-function",
        "build",
        &[],
    )
    .expect("evaluate recursive function");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready recursive function result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::U64(41));
}

#[test]
fn json_parse_decodes_struct_from_blob_argument() {
    let value = expect_ready(
        eval_source_entry_with_args_with_source_id(
            r#"
struct Person {
  name: String,
  age: U64,
}

fn build(blob: Blob) {
  Json.parse[Person](blob)
}
"#,
            "json-parse-struct",
            "build",
            &[Value::external("Blob", r#"{"name":"Alice","age":30}"#)],
        )
        .expect("evaluate Json.parse"),
    );

    let ValueRep::Struct(person) = &value.rep else {
        panic!("expected parsed Person struct, got {value:?}");
    };
    assert_eq!(person.type_name.as_ref(), "Person");
    assert_eq!(
        struct_field(&value, "name").rep,
        ValueRep::String(Arc::from("Alice"))
    );
    assert_eq!(struct_field(&value, "age").rep, ValueRep::U64(30));
}

#[test]
fn json_parse_defaults_missing_optional_field_to_none() {
    let value = expect_ready(
        eval_source_entry_with_args_with_source_id(
            r#"
struct Config {
  name: String,
  description: String?,
}

fn build(blob: Blob) {
  Json.parse[Config](blob)
}
"#,
            "json-parse-optional",
            "build",
            &[Value::external("Blob", r#"{"name":"demo"}"#)],
        )
        .expect("evaluate Json.parse with missing optional field"),
    );

    assert_eq!(
        struct_field(&value, "name").rep,
        ValueRep::String(Arc::from("demo"))
    );
    let ValueRep::Struct(description) = &struct_field(&value, "description").rep else {
        panic!(
            "expected optional variant, got {:?}",
            struct_field(&value, "description")
        );
    };
    assert_eq!(description.type_name.as_ref(), "None");
    assert!(description.fields.is_empty());
}

#[test]
fn json_parse_decodes_array_and_map_fields() {
    let value = expect_ready(
        eval_source_entry_with_args_with_source_id(
            r#"
struct Config {
  values: Array[U64],
  labels: Map[String, U64],
}

fn build(blob: Blob) {
  Json.parse[Config](blob)
}
"#,
            "json-parse-array-map",
            "build",
            &[Value::external(
                "Blob",
                r#"{"values":[1,2,3],"labels":{"left":4,"right":5}}"#,
            )],
        )
        .expect("evaluate Json.parse with arrays and maps"),
    );

    let ValueRep::Array(values) = &struct_field(&value, "values").rep else {
        panic!(
            "expected values array, got {:?}",
            struct_field(&value, "values")
        );
    };
    assert_eq!(values.len(), 3);
    assert_eq!(values[0].rep, ValueRep::U64(1));
    assert_eq!(values[1].rep, ValueRep::U64(2));
    assert_eq!(values[2].rep, ValueRep::U64(3));

    let labels = struct_field(&value, "labels");
    assert_eq!(map_string_entry(labels, "left").rep, ValueRep::U64(4));
    assert_eq!(map_string_entry(labels, "right").rep, ValueRep::U64(5));
}

#[test]
fn toml_parse_decodes_struct_from_blob_argument() {
    let value = expect_ready(
        eval_source_entry_with_args_with_source_id(
            r#"
struct Config {
  name: String,
  port: U64,
}

fn build(blob: Blob) {
  Toml.parse[Config](blob)
}
"#,
            "toml-parse-struct",
            "build",
            &[Value::external("Blob", "name = \"demo\"\nport = 8080\n")],
        )
        .expect("evaluate Toml.parse"),
    );

    let ValueRep::Struct(config) = &value.rep else {
        panic!("expected parsed Config struct, got {value:?}");
    };
    assert_eq!(config.type_name.as_ref(), "Config");
    assert_eq!(
        struct_field(&value, "name").rep,
        ValueRep::String(Arc::from("demo"))
    );
    assert_eq!(struct_field(&value, "port").rep, ValueRep::U64(8080));
}

#[test]
fn toml_parse_decodes_manifest_read_from_runtime_file() {
    let dir = tempdir().expect("temp dir");
    fs::write(
        dir.path().join("Cargo.toml"),
        r#"[package]
name = "fixture-demo"
version = "0.1.0"
edition = "2024"
publish = false
"#,
    )
    .expect("write Cargo.toml");

    let runtime = ReadFileRuntime {
        root_dir: dir.path().to_path_buf(),
    };
    let mut session = EvalSession::new(
        r#"
struct Package {
  name: String,
  version: String,
  edition: String,
  publish: Bool,
}

struct Manifest {
  package: Package,
}

fn build() {
  let manifest = Toml.parse[Manifest](read_file("Cargo.toml"));
  manifest.package
}
"#,
        "toml-parse-runtime-file",
        "build",
        Vec::new(),
        &runtime,
        "local",
    );

    let Resolution::Ready(value) = session
        .run()
        .expect("evaluate Toml.parse against runtime file")
    else {
        panic!("expected parsed Package struct result");
    };

    let ValueRep::Struct(package) = &value.rep else {
        panic!("expected parsed Package struct, got {value:?}");
    };
    assert_eq!(package.type_name.as_ref(), "Package");
    assert_eq!(
        struct_field(&value, "name").rep,
        ValueRep::String(Arc::from("fixture-demo"))
    );
    assert_eq!(
        struct_field(&value, "version").rep,
        ValueRep::String(Arc::from("0.1.0"))
    );
    assert_eq!(
        struct_field(&value, "edition").rep,
        ValueRep::String(Arc::from("2024"))
    );
    assert_eq!(struct_field(&value, "publish").rep, ValueRep::Bool(false));
}

#[test]
fn binary_equality_supports_authored_branching() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  if "serde" == "serde" (
    1
  ) else (
    2
  )
}
"#,
        "binary-equality",
        "build",
        &[],
    )
    .expect("evaluate authored equality");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready equality result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::U64(1));
}

#[test]
fn array_equality_checks_all_elements() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  [1, 2] == [9, 2]
}
"#,
        "array-equality-all-elements",
        "build",
        &[],
    )
    .expect("evaluate array equality");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready array equality result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::Bool(false));
}

#[test]
fn array_inequality_checks_all_elements() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  [1, 2] != [9, 2]
}
"#,
        "array-inequality-all-elements",
        "build",
        &[],
    )
    .expect("evaluate array inequality");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready array inequality result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::Bool(true));
}

#[test]
fn struct_equality_checks_all_fields() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
struct Pair {
  left: U64,
  right: U64,
}

fn build() {
  Pair { left: 1, right: 2 } == Pair { left: 9, right: 2 }
}
"#,
        "struct-equality-all-fields",
        "build",
        &[],
    )
    .expect("evaluate struct equality");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready struct equality result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::Bool(false));
}

#[test]
fn struct_inequality_checks_all_fields() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
struct Pair {
  left: U64,
  right: U64,
}

fn build() {
  Pair { left: 1, right: 2 } != Pair { left: 9, right: 2 }
}
"#,
        "struct-inequality-all-fields",
        "build",
        &[],
    )
    .expect("evaluate struct inequality");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready struct inequality result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::Bool(true));
}

#[test]
fn map_equality_checks_all_entries() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  %{ "left" => 1, "right" => 2 } == %{ "left" => 9, "right" => 2 }
}
"#,
        "map-equality-all-entries",
        "build",
        &[],
    )
    .expect("evaluate map equality");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready map equality result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::Bool(false));
}

#[test]
fn map_inequality_checks_all_entries() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  %{ "left" => 1, "right" => 2 } != %{ "left" => 9, "right" => 2 }
}
"#,
        "map-inequality-all-entries",
        "build",
        &[],
    )
    .expect("evaluate map inequality");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready map inequality result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::Bool(true));
}

#[test]
fn map_get_returns_clear_error_for_missing_key() {
    let error = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  map_get(%{ "left" => 1 }, "right")
}
"#,
        "map-get-missing-key",
        "build",
        &[],
    )
    .expect_err("map_get missing key should fail");

    assert_eq!(error.kind, EvalErrorKind::TypeError);
    assert!(error.message.contains("map_get() key was not found"));
}

#[test]
fn map_get_requires_map_as_first_argument() {
    let error = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  map_get("not a map", "right")
}
"#,
        "map-get-first-arg-type",
        "build",
        &[],
    )
    .expect_err("map_get non-map first argument should fail");

    assert_eq!(error.kind, EvalErrorKind::TypeError);
    assert!(
        error
            .message
            .contains("map_get() first argument must evaluate to a map")
    );
}

#[test]
fn map_get_supports_non_string_keys() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  map_get(%{ 1 => "one", 2 => "two" }, 2)
}
"#,
        "map-get-u64-key",
        "build",
        &[],
    )
    .expect("evaluate map_get with non-string key");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready map_get result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("two".into()));
}

#[test]
fn map_entries_preserves_insertion_order() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  map_entries(%{ "left" => 1, "right" => 2 })
}
"#,
        "map-entries-order",
        "build",
        &[],
    )
    .expect("evaluate map_entries");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready map_entries result, got {resolution:?}");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!(
            "expected map_entries to return an array, got {:?}",
            value.rep
        );
    };
    assert_eq!(items.len(), 2);

    let ValueRep::Map(MapValue { entries: first }) = &items[0].rep else {
        panic!(
            "expected first map_entries item to be a map, got {:?}",
            items[0].rep
        );
    };
    let ValueRep::Map(MapValue { entries: second }) = &items[1].rep else {
        panic!(
            "expected second map_entries item to be a map, got {:?}",
            items[1].rep
        );
    };

    assert_eq!(first[0].0.rep, ValueRep::String("key".into()));
    assert_eq!(first[0].1.rep, ValueRep::String("left".into()));
    assert_eq!(first[1].0.rep, ValueRep::String("value".into()));
    assert_eq!(first[1].1.rep, ValueRep::U64(1));

    assert_eq!(second[0].0.rep, ValueRep::String("key".into()));
    assert_eq!(second[0].1.rep, ValueRep::String("right".into()));
    assert_eq!(second[1].0.rep, ValueRep::String("value".into()));
    assert_eq!(second[1].1.rep, ValueRep::U64(2));
}

#[test]
fn map_entries_requires_map_as_first_argument() {
    let error = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  map_entries("not a map")
}
"#,
        "map-entries-first-arg-type",
        "build",
        &[],
    )
    .expect_err("map_entries non-map first argument should fail");

    assert_eq!(error.kind, EvalErrorKind::TypeError);
    assert!(
        error
            .message
            .contains("map_entries() first argument must evaluate to a map")
    );
}

#[test]
fn map_entries_preserves_non_string_keys() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  map_entries(%{ 1 => "one" })
}
"#,
        "map-entries-u64-key",
        "build",
        &[],
    )
    .expect("evaluate map_entries with non-string key");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready map_entries result, got {resolution:?}");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!(
            "expected map_entries to return an array, got {:?}",
            value.rep
        );
    };
    let ValueRep::Map(MapValue { entries }) = &items[0].rep else {
        panic!(
            "expected map_entries item to be a map, got {:?}",
            items[0].rep
        );
    };
    assert_eq!(entries[0].1.rep, ValueRep::U64(1));
    assert_eq!(entries[1].1.rep, ValueRep::String("one".into()));
}

#[test]
fn map_entries_aggregates_metadata_from_contents() {
    struct MetadataRuntime;

    impl RuntimeHooks for MetadataRuntime {
        fn call(
            &self,
            place: &str,
            callee: &str,
            _args: &[Value],
            source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            match (place, callee) {
                ("win", "host_claimed_value") => Ok(RuntimeCallOutcome::Ready(
                    Value::string("observed")
                        .with_provenance(Provenance::host_probe(
                            source.clone(),
                            place,
                            "claimed value",
                        ))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue)
                        .with_claim(Claim::Arch {
                            value: "x86_64".to_string(),
                        }),
                )),
                ("win", "host_blocked_value") => Ok(RuntimeCallOutcome::Ready(Value::external(
                    "tree",
                    "/workspace",
                ))),
                _ => Ok(RuntimeCallOutcome::Unsupported),
            }
        }
    }

    let Resolution::Ready(value) = eval_source_entry_at_place_with_args_with_source_id(
        r#"
fn host_claimed_value();
fn host_blocked_value();

fn build() {
  map_entries(%{ "blocked" => host_blocked_value(), "observed" => host_claimed_value() })
}
"#,
        "map-entries-metadata",
        "build",
        &[],
        &MetadataRuntime,
        "win",
    )
    .expect("evaluate metadata-preserving map_entries") else {
        panic!("expected ready map_entries metadata result");
    };

    assert_eq!(value.meta.reuse, ReuseClass::Live);
    assert_eq!(value.meta.crossing, CrossingPolicy::Blocked);
    assert!(value.meta.claims.iter().any(|claim| {
        matches!(
            claim,
            Claim::Arch { value } if value == "x86_64"
        )
    }));
}

#[test]
fn array_first_returns_first_element() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  array_first(["left", "right"])
}
"#,
        "array-first",
        "build",
        &[],
    )
    .expect("evaluate array_first");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready array_first result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("left".into()));
}

#[test]
fn array_first_requires_array_as_first_argument() {
    let error = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  array_first("not an array")
}
"#,
        "array-first-first-arg-type",
        "build",
        &[],
    )
    .expect_err("array_first non-array first argument should fail");

    assert_eq!(error.kind, EvalErrorKind::TypeError);
    assert!(
        error
            .message
            .contains("array_first() first argument must evaluate to an array")
    );
}

#[test]
fn array_first_returns_clear_error_for_empty_array() {
    let error = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  array_first([])
}
"#,
        "array-first-empty",
        "build",
        &[],
    )
    .expect_err("array_first empty array should fail");

    assert_eq!(error.kind, EvalErrorKind::TypeError);
    assert!(error.message.contains("array_first() array was empty"));
}

#[test]
fn array_rest_returns_tail_preserving_order() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  array_rest(["seed", "codec", "logger"])
}
"#,
        "array-rest-tail",
        "build",
        &[],
    )
    .expect("evaluate array_rest");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready array_rest result, got {resolution:?}");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!(
            "expected array_rest to return an array, got {:?}",
            value.rep
        );
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].rep, ValueRep::String("codec".into()));
    assert_eq!(items[1].rep, ValueRep::String("logger".into()));
}

#[test]
fn array_rest_requires_array_as_first_argument() {
    let error = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  array_rest("not an array")
}
"#,
        "array-rest-first-arg-type",
        "build",
        &[],
    )
    .expect_err("array_rest non-array first argument should fail");

    assert_eq!(error.kind, EvalErrorKind::TypeError);
    assert!(
        error
            .message
            .contains("array_rest() first argument must evaluate to an array")
    );
}

#[test]
fn array_rest_aggregates_metadata_from_remaining_contents() {
    struct MetadataRuntime;

    impl RuntimeHooks for MetadataRuntime {
        fn call(
            &self,
            place: &str,
            callee: &str,
            _args: &[Value],
            source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            match (place, callee) {
                ("win", "host_claimed_value") => Ok(RuntimeCallOutcome::Ready(
                    Value::string("observed")
                        .with_provenance(Provenance::host_probe(
                            source.clone(),
                            place,
                            "claimed value",
                        ))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue)
                        .with_claim(Claim::Arch {
                            value: "x86_64".to_string(),
                        }),
                )),
                ("win", "host_blocked_value") => Ok(RuntimeCallOutcome::Ready(Value::external(
                    "tree",
                    "/workspace",
                ))),
                _ => Ok(RuntimeCallOutcome::Unsupported),
            }
        }
    }

    let Resolution::Ready(value) = eval_source_entry_at_place_with_args_with_source_id(
        r#"
fn host_claimed_value();
fn host_blocked_value();

fn build() {
  array_rest([host_blocked_value(), host_claimed_value()])
}
"#,
        "array-rest-metadata",
        "build",
        &[],
        &MetadataRuntime,
        "win",
    )
    .expect("evaluate metadata-preserving array_rest") else {
        panic!("expected ready array_rest metadata result");
    };

    assert_eq!(value.meta.reuse, ReuseClass::Observed);
    assert_eq!(value.meta.crossing, CrossingPolicy::ByValue);
    assert!(value.meta.claims.iter().any(|claim| {
        matches!(
            claim,
            Claim::Arch { value } if value == "x86_64"
        )
    }));
}

#[test]
fn array_push_appends_item() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  array_push(["seed", "codec"], "logger")
}
"#,
        "array-push-append",
        "build",
        &[],
    )
    .expect("evaluate array_push");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready array_push result, got {resolution:?}");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!(
            "expected array_push to return an array, got {:?}",
            value.rep
        );
    };
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].rep, ValueRep::String("seed".into()));
    assert_eq!(items[1].rep, ValueRep::String("codec".into()));
    assert_eq!(items[2].rep, ValueRep::String("logger".into()));
}

#[test]
fn array_push_requires_array_as_first_argument() {
    let error = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  array_push("not an array", "logger")
}
"#,
        "array-push-first-arg-type",
        "build",
        &[],
    )
    .expect_err("array_push non-array first argument should fail");

    assert_eq!(error.kind, EvalErrorKind::TypeError);
    assert!(
        error
            .message
            .contains("array_push() first argument must evaluate to an array")
    );
}

#[test]
fn array_push_aggregates_metadata_from_contents() {
    struct MetadataRuntime;

    impl RuntimeHooks for MetadataRuntime {
        fn call(
            &self,
            place: &str,
            callee: &str,
            _args: &[Value],
            source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            match (place, callee) {
                ("win", "host_claimed_value") => Ok(RuntimeCallOutcome::Ready(
                    Value::string("observed")
                        .with_provenance(Provenance::host_probe(
                            source.clone(),
                            place,
                            "claimed value",
                        ))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue)
                        .with_claim(Claim::Arch {
                            value: "x86_64".to_string(),
                        }),
                )),
                ("win", "host_blocked_value") => Ok(RuntimeCallOutcome::Ready(Value::external(
                    "tree",
                    "/workspace",
                ))),
                _ => Ok(RuntimeCallOutcome::Unsupported),
            }
        }
    }

    let Resolution::Ready(value) = eval_source_entry_at_place_with_args_with_source_id(
        r#"
fn host_claimed_value();
fn host_blocked_value();

fn build() {
  array_push([host_blocked_value()], host_claimed_value())
}
"#,
        "array-push-metadata",
        "build",
        &[],
        &MetadataRuntime,
        "win",
    )
    .expect("evaluate metadata-preserving array_push") else {
        panic!("expected ready array_push metadata result");
    };

    assert_eq!(value.meta.reuse, ReuseClass::Live);
    assert_eq!(value.meta.crossing, CrossingPolicy::Blocked);
    assert!(value.meta.claims.iter().any(|claim| {
        matches!(
            claim,
            Claim::Arch { value } if value == "x86_64"
        )
    }));
}

#[test]
fn map_put_replaces_existing_entry() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  map_put(%{ "left" => 1, "right" => 2 }, "right", 9)
}
"#,
        "map-put-replace",
        "build",
        &[],
    )
    .expect("evaluate map_put replacement");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready map_put replacement result, got {resolution:?}");
    };
    let ValueRep::Map(MapValue { entries }) = &value.rep else {
        panic!(
            "expected map_put replacement to return a map, got {:?}",
            value.rep
        );
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0.rep, ValueRep::String("left".into()));
    assert_eq!(entries[0].1.rep, ValueRep::U64(1));
    assert_eq!(entries[1].0.rep, ValueRep::String("right".into()));
    assert_eq!(entries[1].1.rep, ValueRep::U64(9));
}

#[test]
fn map_put_inserts_new_entry_at_end() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  map_put(%{ "left" => 1 }, "right", 2)
}
"#,
        "map-put-insert",
        "build",
        &[],
    )
    .expect("evaluate map_put insertion");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready map_put insertion result, got {resolution:?}");
    };
    let ValueRep::Map(MapValue { entries }) = &value.rep else {
        panic!(
            "expected map_put insertion to return a map, got {:?}",
            value.rep
        );
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0.rep, ValueRep::String("left".into()));
    assert_eq!(entries[0].1.rep, ValueRep::U64(1));
    assert_eq!(entries[1].0.rep, ValueRep::String("right".into()));
    assert_eq!(entries[1].1.rep, ValueRep::U64(2));
}

#[test]
fn map_put_requires_map_as_first_argument() {
    let error = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  map_put("not a map", "right", 2)
}
"#,
        "map-put-first-arg-type",
        "build",
        &[],
    )
    .expect_err("map_put non-map first argument should fail");

    assert_eq!(error.kind, EvalErrorKind::TypeError);
    assert!(
        error
            .message
            .contains("map_put() first argument must evaluate to a map")
    );
}

#[test]
fn map_put_supports_non_string_keys() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  map_put(%{ 1 => "one" }, 2, "two")
}
"#,
        "map-put-u64-key",
        "build",
        &[],
    )
    .expect("evaluate map_put with non-string key");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready map_put non-string key result, got {resolution:?}");
    };
    let ValueRep::Map(MapValue { entries }) = &value.rep else {
        panic!(
            "expected map_put non-string key to return a map, got {:?}",
            value.rep
        );
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0.rep, ValueRep::U64(1));
    assert_eq!(entries[0].1.rep, ValueRep::String("one".into()));
    assert_eq!(entries[1].0.rep, ValueRep::U64(2));
    assert_eq!(entries[1].1.rep, ValueRep::String("two".into()));
}

#[test]
fn map_put_insert_preserves_observed_claim_metadata() {
    struct MetadataRuntime;

    impl RuntimeHooks for MetadataRuntime {
        fn call(
            &self,
            place: &str,
            callee: &str,
            _args: &[Value],
            source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            match (place, callee) {
                ("win", "host_claimed_value") => Ok(RuntimeCallOutcome::Ready(
                    Value::string("observed")
                        .with_provenance(Provenance::host_probe(
                            source.clone(),
                            place,
                            "claimed value",
                        ))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue)
                        .with_claim(Claim::Arch {
                            value: "x86_64".to_string(),
                        }),
                )),
                ("win", "host_blocked_value") => Ok(RuntimeCallOutcome::Ready(Value::external(
                    "tree",
                    "/workspace",
                ))),
                _ => Ok(RuntimeCallOutcome::Unsupported),
            }
        }
    }

    let Resolution::Ready(value) = eval_source_entry_at_place_with_args_with_source_id(
        r#"
fn host_claimed_value();

fn build() {
  map_put(%{ "seed" => 1 }, "observed", host_claimed_value())
}
"#,
        "map-put-metadata-insert",
        "build",
        &[],
        &MetadataRuntime,
        "win",
    )
    .expect("evaluate metadata-preserving map_put insert") else {
        panic!("expected ready map_put metadata insert result");
    };

    assert_eq!(value.meta.reuse, ReuseClass::Observed);
    assert_eq!(value.meta.crossing, CrossingPolicy::ByValue);
    assert!(value.meta.claims.iter().any(|claim| {
        matches!(
            claim,
            Claim::Arch { value } if value == "x86_64"
        )
    }));
}

#[test]
fn map_put_replace_preserves_existing_composite_metadata() {
    struct MetadataRuntime;

    impl RuntimeHooks for MetadataRuntime {
        fn call(
            &self,
            place: &str,
            callee: &str,
            _args: &[Value],
            source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            match (place, callee) {
                ("win", "host_claimed_value") => Ok(RuntimeCallOutcome::Ready(
                    Value::string("observed")
                        .with_provenance(Provenance::host_probe(
                            source.clone(),
                            place,
                            "claimed value",
                        ))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue)
                        .with_claim(Claim::Arch {
                            value: "x86_64".to_string(),
                        }),
                )),
                ("win", "host_blocked_value") => Ok(RuntimeCallOutcome::Ready(Value::external(
                    "tree",
                    "/workspace",
                ))),
                _ => Ok(RuntimeCallOutcome::Unsupported),
            }
        }
    }

    let Resolution::Ready(value) = eval_source_entry_at_place_with_args_with_source_id(
        r#"
fn host_claimed_value();
fn host_blocked_value();

fn build() {
  map_put(
    %{ "blocked" => host_blocked_value(), "observed" => host_claimed_value() },
    "observed",
    host_claimed_value(),
  )
}
"#,
        "map-put-metadata-replace",
        "build",
        &[],
        &MetadataRuntime,
        "win",
    )
    .expect("evaluate metadata-preserving map_put replacement") else {
        panic!("expected ready map_put metadata replacement result");
    };

    assert_eq!(value.meta.reuse, ReuseClass::Live);
    assert_eq!(value.meta.crossing, CrossingPolicy::Blocked);
    assert!(value.meta.claims.iter().any(|claim| {
        matches!(
            claim,
            Claim::Arch { value } if value == "x86_64"
        )
    }));
}

#[test]
fn array_find_returns_first_matching_element() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn is_codec(value: String) {
  value == "codec"
}

fn build() {
  array_find(["logger", "codec", "codec"], is_codec)
}
"#,
        "array-find-first-match",
        "build",
        &[],
    )
    .expect("evaluate array_find first match");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready array_find result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("codec".into()));
}

#[test]
fn array_find_closure_predicate_captures_outer_binding() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  let target = "codec";
  array_find(["logger", "codec", "codec"], ((value) => value == target))
}
"#,
        "array-find-captured-closure",
        "build",
        &[],
    )
    .expect("evaluate array_find captured closure");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready array_find closure result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("codec".into()));
}

#[test]
fn array_find_resumes_after_suspending_predicate_and_continues_search() {
    struct FirstPendingBoolRuntime {
        calls: Arc<AtomicUsize>,
    }

    impl RuntimeHooks for FirstPendingBoolRuntime {
        fn call(
            &self,
            place: &str,
            callee: &str,
            _args: &[Value],
            source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            match (place, callee) {
                ("win", "host_wait_bool") => {
                    if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        Ok(RuntimeCallOutcome::Pending {
                            description: "waiting for predicate result".into(),
                        })
                    } else {
                        Ok(RuntimeCallOutcome::Ready(
                            Value::bool(true).with_provenance(Provenance::host_probe(
                                source.clone(),
                                place,
                                "predicate ready",
                            )),
                        ))
                    }
                }
                _ => Ok(RuntimeCallOutcome::Unsupported),
            }
        }
    }

    let runtime = FirstPendingBoolRuntime {
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let mut session = EvalSession::new(
        r#"
fn host_wait_bool();

fn build() {
  array_find(
    ["logger", "codec"],
    ((value) => if host_wait_bool() ( value == "codec" ) else ( false )),
  )
}
"#,
        "array-find-pending",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    let Resolution::Pending(pending) = session.run().expect("start suspended array_find") else {
        panic!("expected pending array_find");
    };
    session
        .resolve_pending(&pending, Value::bool(true))
        .expect("resolve suspended predicate");

    let Resolution::Ready(value) = session.run().expect("resume suspended array_find") else {
        panic!("expected ready array_find after resume");
    };
    assert_eq!(value.rep, ValueRep::String("codec".into()));
}

#[test]
fn array_find_returns_clear_error_for_no_match() {
    let error = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  array_find([1, 2], ((value) => value == 9))
}
"#,
        "array-find-no-match",
        "build",
        &[],
    )
    .expect_err("array_find no match should fail");

    assert_eq!(error.kind, EvalErrorKind::TypeError);
    assert!(
        error
            .message
            .contains("array_find() found no matching element")
    );
}

#[test]
fn array_find_requires_array_as_first_argument() {
    let error = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  array_find("not an array", ((value) => true))
}
"#,
        "array-find-first-arg-type",
        "build",
        &[],
    )
    .expect_err("array_find non-array first argument should fail");

    assert_eq!(error.kind, EvalErrorKind::TypeError);
    assert!(
        error
            .message
            .contains("array_find() first argument must evaluate to an array")
    );
}

#[test]
fn array_find_requires_function_as_second_argument() {
    let error = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  array_find([1, 2], "not a function")
}
"#,
        "array-find-second-arg-type",
        "build",
        &[],
    )
    .expect_err("array_find non-function second argument should fail");

    assert_eq!(error.kind, EvalErrorKind::TypeError);
    assert!(
        error
            .message
            .contains("array_find() second argument must evaluate to a function value")
    );
}

#[test]
fn array_find_requires_bool_predicate_results() {
    let error = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  array_find([1], ((value) => value))
}
"#,
        "array-find-predicate-bool",
        "build",
        &[],
    )
    .expect_err("array_find non-bool predicate should fail");

    assert_eq!(error.kind, EvalErrorKind::TypeError);
    assert!(
        error
            .message
            .contains("array_find() predicate must return Bool")
    );
}

#[test]
fn array_fold_builds_map_left_to_right() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  array_fold(
    ["codec", "logger"],
    %{ "seed" => "seed" },
    ((acc, item) => map_put(acc, item, item)),
  )
}
"#,
        "array-fold-left-to-right",
        "build",
        &[],
    )
    .expect("evaluate array_fold");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready array_fold result, got {resolution:?}");
    };
    let ValueRep::Map(MapValue { entries }) = &value.rep else {
        panic!("expected array_fold to return a map, got {:?}", value.rep);
    };
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].0.rep, ValueRep::String("seed".into()));
    assert_eq!(entries[1].0.rep, ValueRep::String("codec".into()));
    assert_eq!(entries[2].0.rep, ValueRep::String("logger".into()));
}

#[test]
fn array_fold_step_closure_captures_outer_binding() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  let version = "1.1.0";
  array_fold(
    ["codec", "logger"],
    %{ "seed" => "seed" },
    ((acc, item) => map_put(acc, item, version)),
  )
}
"#,
        "array-fold-captured-closure",
        "build",
        &[],
    )
    .expect("evaluate array_fold captured closure");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready array_fold closure result, got {resolution:?}");
    };
    let ValueRep::Map(MapValue { entries }) = &value.rep else {
        panic!("expected array_fold to return a map, got {:?}", value.rep);
    };
    assert_eq!(entries[1].1.rep, ValueRep::String("1.1.0".into()));
    assert_eq!(entries[2].1.rep, ValueRep::String("1.1.0".into()));
}

#[test]
fn array_fold_requires_array_as_first_argument() {
    let error = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  array_fold("not an array", %{ "seed" => "seed" }, ((acc, item) => acc))
}
"#,
        "array-fold-first-arg-type",
        "build",
        &[],
    )
    .expect_err("array_fold non-array first argument should fail");

    assert_eq!(error.kind, EvalErrorKind::TypeError);
    assert!(
        error
            .message
            .contains("array_fold() first argument must evaluate to an array")
    );
}

#[test]
fn array_fold_requires_function_as_third_argument() {
    let error = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  array_fold(["codec"], %{ "seed" => "seed" }, "not a function")
}
"#,
        "array-fold-third-arg-type",
        "build",
        &[],
    )
    .expect_err("array_fold non-function third argument should fail");

    assert_eq!(error.kind, EvalErrorKind::TypeError);
    assert!(
        error
            .message
            .contains("array_fold() third argument must evaluate to a function value")
    );
}

#[test]
fn array_fold_resumes_after_suspending_step_function() {
    struct FirstPendingBoolRuntime {
        calls: Arc<AtomicUsize>,
    }

    impl RuntimeHooks for FirstPendingBoolRuntime {
        fn call(
            &self,
            place: &str,
            callee: &str,
            _args: &[Value],
            source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            match (place, callee) {
                ("win", "host_wait_bool") => {
                    if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        Ok(RuntimeCallOutcome::Pending {
                            description: "waiting for fold step".into(),
                        })
                    } else {
                        Ok(RuntimeCallOutcome::Ready(
                            Value::bool(true).with_provenance(Provenance::host_probe(
                                source.clone(),
                                place,
                                "fold step ready",
                            )),
                        ))
                    }
                }
                _ => Ok(RuntimeCallOutcome::Unsupported),
            }
        }
    }

    let runtime = FirstPendingBoolRuntime {
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let mut session = EvalSession::new(
        r#"
fn host_wait_bool();

fn build() {
  array_fold(
    ["codec"],
    %{ "seed" => "seed" },
    ((acc, item) => if host_wait_bool() ( map_put(acc, item, item) ) else ( acc )),
  )
}
"#,
        "array-fold-pending",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    let Resolution::Pending(pending) = session.run().expect("start suspended array_fold") else {
        panic!("expected pending array_fold");
    };
    session
        .resolve_pending(&pending, Value::bool(true))
        .expect("resolve suspended fold step");

    let Resolution::Ready(value) = session.run().expect("resume suspended array_fold") else {
        panic!("expected ready array_fold after resume");
    };
    let ValueRep::Map(MapValue { entries }) = &value.rep else {
        panic!("expected array_fold to return a map, got {:?}", value.rep);
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].0.rep, ValueRep::String("codec".into()));
    assert_eq!(entries[1].1.rep, ValueRep::String("codec".into()));
}

#[test]
fn keyed_demands_join_array_inputs_and_share_one_canonical_node() {
    let mut session = EvalSession::new(
        r#"
fn resolve_pkg(ctx) {
  ctx.joined
}

fn build() {
  [
    keyed("logger", ["1.0.0"], resolve_pkg),
    keyed("logger", ["1.1.0"], resolve_pkg),
  ]
}
"#,
        "keyed-join-basic",
        "build",
        Vec::new(),
        &UnsupportedRuntime,
        "win",
    );

    let Resolution::Ready(value) = session.run().expect("evaluate keyed joined demands") else {
        panic!("expected ready keyed result");
    };
    let ValueRep::Array(rows) = &value.rep else {
        panic!(
            "expected keyed build to return an array, got {:?}",
            value.rep
        );
    };
    assert_eq!(rows.len(), 2);
    for row in rows.iter() {
        let ValueRep::Array(items) = &row.rep else {
            panic!(
                "expected each keyed result to be an array, got {:?}",
                row.rep
            );
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].rep, ValueRep::String("1.0.0".into()));
        assert_eq!(items[1].rep, ValueRep::String("1.1.0".into()));
    }

    let snapshot = session.graph_snapshot();
    let keyed_nodes = snapshot
        .nodes
        .iter()
        .filter_map(|node| node.keyed.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(keyed_nodes.len(), 1);
    let keyed = keyed_nodes[0];
    assert_eq!(keyed.resolver_name.as_ref(), "resolve_pkg");
    assert_eq!(keyed.last_update, EvalKeyedNodeUpdateReason::JoinedWidened);
    assert_eq!(keyed.revision, 2);
    assert_eq!(keyed.evaluated_revision, Some(2));
    let ValueRep::Array(items) = &keyed.joined.rep else {
        panic!(
            "expected keyed joined input to be an array, got {:?}",
            keyed.joined.rep
        );
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].rep, ValueRep::String("1.0.0".into()));
    assert_eq!(items[1].rep, ValueRep::String("1.1.0".into()));
}

#[test]
fn keyed_widening_replays_completed_dependent_exprs() {
    let mut session = EvalSession::new(
        r#"
fn resolve_pkg(ctx) {
  ctx.joined
}

fn build() {
  let first = [keyed("logger", ["1.0.0"], resolve_pkg)];
  let second = keyed("logger", ["1.1.0"], resolve_pkg);
  [first, second]
}
"#,
        "keyed-dependent-invalidation",
        "build",
        Vec::new(),
        &UnsupportedRuntime,
        "win",
    );

    let Resolution::Ready(value) = session
        .run()
        .expect("evaluate keyed widening with completed dependents")
    else {
        panic!("expected ready keyed dependent invalidation result");
    };
    let ValueRep::Array(rows) = &value.rep else {
        panic!(
            "expected keyed dependent build to return an array, got {:?}",
            value.rep
        );
    };
    assert_eq!(rows.len(), 2);

    let ValueRep::Array(first_items) = &rows[0].rep else {
        panic!(
            "expected first dependent result to be an array, got {:?}",
            rows[0].rep
        );
    };
    assert_eq!(first_items.len(), 1);
    let ValueRep::Array(first_joined) = &first_items[0].rep else {
        panic!(
            "expected first dependent inner result to be an array, got {:?}",
            first_items[0].rep
        );
    };
    assert_eq!(first_joined.len(), 2);
    assert_eq!(first_joined[0].rep, ValueRep::String("1.0.0".into()));
    assert_eq!(first_joined[1].rep, ValueRep::String("1.1.0".into()));

    let ValueRep::Array(second_items) = &rows[1].rep else {
        panic!(
            "expected second keyed result to be an array, got {:?}",
            rows[1].rep
        );
    };
    assert_eq!(second_items.len(), 2);
    assert_eq!(second_items[0].rep, ValueRep::String("1.0.0".into()));
    assert_eq!(second_items[1].rep, ValueRep::String("1.1.0".into()));
}

#[test]
fn keyed_demands_resume_cleanly_after_pending_resolution() {
    struct FirstPendingBoolRuntime {
        calls: Arc<AtomicUsize>,
    }

    impl RuntimeHooks for FirstPendingBoolRuntime {
        fn call(
            &self,
            place: &str,
            callee: &str,
            _args: &[Value],
            source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            match (place, callee) {
                ("win", "host_wait_bool") => {
                    if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        Ok(RuntimeCallOutcome::Pending {
                            description: "waiting for keyed resolver".into(),
                        })
                    } else {
                        Ok(RuntimeCallOutcome::Ready(
                            Value::bool(true).with_provenance(Provenance::host_probe(
                                source.clone(),
                                place,
                                "keyed resolver ready",
                            )),
                        ))
                    }
                }
                _ => Ok(RuntimeCallOutcome::Unsupported),
            }
        }
    }

    let runtime = FirstPendingBoolRuntime {
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let mut session = EvalSession::new(
        r#"
fn host_wait_bool();

fn resolve_pkg(ctx) {
  let _ready = host_wait_bool();
  ctx.joined
}

fn build() {
  [
    keyed("logger", ["1.0.0"], resolve_pkg),
    keyed("logger", ["1.1.0"], resolve_pkg),
  ]
}
"#,
        "keyed-join-pending",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    let Resolution::Pending(pending) = session.run().expect("start pending keyed demand") else {
        panic!("expected pending keyed demand");
    };
    let snapshot = session.graph_snapshot();
    let keyed = snapshot
        .nodes
        .iter()
        .filter_map(|node| node.keyed.as_ref())
        .next()
        .expect("expected one keyed node in snapshot");
    assert_eq!(keyed.revision, 2);
    assert_eq!(keyed.active_revision, Some(2));

    session
        .resolve_pending(&pending, Value::bool(true))
        .expect("resolve keyed pending runtime call");

    let Resolution::Ready(value) = session.run().expect("resume keyed demand") else {
        panic!("expected ready keyed demand after resume");
    };
    let ValueRep::Array(rows) = &value.rep else {
        panic!(
            "expected keyed build to return an array, got {:?}",
            value.rep
        );
    };
    assert_eq!(rows.len(), 2);
    for row in rows.iter() {
        let ValueRep::Array(items) = &row.rep else {
            panic!(
                "expected each keyed result to be an array, got {:?}",
                row.rep
            );
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].rep, ValueRep::String("1.0.0".into()));
        assert_eq!(items[1].rep, ValueRep::String("1.1.0".into()));
    }
}

#[test]
fn keyed_structured_conflict_retains_latest_joined_input() {
    let mut session = EvalSession::new(
        r#"
fn resolve_pkg(ctx) {
  fail Conflict {
    package: ctx.key,
    joined: ctx.joined,
  }
}

fn build() {
  [
    keyed("logger", ["1.0.0"], resolve_pkg),
    keyed("logger", ["1.1.0"], resolve_pkg),
  ]
}
"#,
        "keyed-conflict",
        "build",
        Vec::new(),
        &UnsupportedRuntime,
        "win",
    );

    let Resolution::Poison(error) = session.run().expect("evaluate keyed structured conflict")
    else {
        panic!("expected poison keyed structured conflict");
    };
    assert_eq!(error.kind, EvalErrorKind::UserFail);
    assert_eq!(error.message.as_ref(), "Conflict");
    let detail = error.detail.expect("expected keyed conflict detail");
    let ValueRep::Struct(struct_value) = &detail.rep else {
        panic!("expected structured keyed detail, got {:?}", detail.rep);
    };
    assert_eq!(struct_value.type_name.as_ref(), "Conflict");
    let joined = struct_value
        .fields
        .iter()
        .find(|(name, _)| name.as_ref() == "joined")
        .expect("expected joined field")
        .1
        .clone();
    let ValueRep::Array(items) = &joined.rep else {
        panic!("expected joined array, got {:?}", joined.rep);
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].rep, ValueRep::String("1.0.0".into()));
    assert_eq!(items[1].rep, ValueRep::String("1.1.0".into()));

    let snapshot = session.graph_snapshot();
    let keyed_node = snapshot
        .nodes
        .iter()
        .find(|node| node.keyed.is_some())
        .expect("expected keyed node in graph snapshot");
    let Some(EvalNodeState::Poison(keyed_error)) = &keyed_node.state else {
        panic!(
            "expected keyed node to be poisoned, got {:?}",
            keyed_node.state
        );
    };
    let keyed_detail = keyed_error
        .detail
        .clone()
        .expect("expected keyed node conflict detail");
    assert_eq!(keyed_detail, detail);
}

#[test]
fn function_values_do_not_support_authored_equality() {
    let error = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  let f = ((value) => value);
  f == f
}
"#,
        "binary-function-equality",
        "build",
        &[],
    )
    .expect_err("function equality should fail");
    assert_eq!(error.kind, EvalErrorKind::TypeError);
    assert!(
        error
            .message
            .contains("function values do not support authored equality")
    );
}

#[test]
fn declaration_without_body_falls_back_to_runtime_hooks() {
    let mut session = EvalSession::new(
        "fn host_os(); fn build() { host_os() }",
        "same-source-runtime-decl",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    let resolution = session.run().expect("evaluate runtime-backed declaration");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready runtime-backed result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("windows".into()));
}

#[test]
fn unsupported_call_target_still_errors_clearly() {
    let error = eval_source_entry_with_args_with_source_id(
        "fn build() { does_not_exist() }",
        "unsupported-call-target",
        "build",
        &[],
    )
    .expect_err("unsupported call target should surface an eval error");
    assert_eq!(error.kind, EvalErrorKind::Internal);
    assert!(
        error
            .message
            .contains("call target does_not_exist is not supported yet")
    );
}

fn test_program(sources: [(&str, &str); 2], imports: Vec<EvalProgramImport>) -> EvalProgram {
    EvalProgram::new(
        0,
        sources
            .into_iter()
            .map(|(source_id, source_text)| EvalProgramSource::new(source_id, source_text))
            .collect::<Vec<_>>(),
        imports,
    )
    .expect("build test eval program")
}

fn run_program(program: EvalProgram) -> Resolution {
    let runtime = UnsupportedRuntime;
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");
    session.run().expect("run eval program")
}

#[test]
fn session_stats_report_current_graph_and_work_counts() {
    let runtime = UnsupportedRuntime;
    let program =
        EvalProgram::single_source("session-stats", "fn build() { let answer = 41; answer }");
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");
    let Resolution::Ready(value) = session.run().expect("run stats session") else {
        panic!("expected ready stats session result");
    };
    assert_eq!(value.rep, ValueRep::U64(41));

    let stats = session.stats();
    assert!(stats.graph_node_count >= 2);
    assert_eq!(stats.node_record_count, session.node_records().len());
    assert_eq!(
        stats.dependency_edge_count,
        session.dependency_edges().len()
    );
    assert_eq!(stats.completed_work_count, session.completed_work().len());
    assert_eq!(stats.trace_event_count, session.trace().events.len());
    assert_eq!(stats.queued_work_count, session.queued_work().len());
    assert_eq!(stats.in_flight_work_count, session.in_flight_work().len());
    assert_eq!(stats.buffered_work_count, session.buffered_work().len());
    assert_eq!(stats.pending_count, session.pending_nodes().len());
}

struct UnsupportedRuntime;

impl RuntimeHooks for UnsupportedRuntime {
    fn call(
        &self,
        _place: &str,
        _callee: &str,
        _args: &[Value],
        _source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        Ok(RuntimeCallOutcome::Unsupported)
    }
}

#[derive(Clone, Default)]
struct SlotBuiltinRuntime {
    slots: Arc<Mutex<SlotStore<Value, Value, (), Value>>>,
}

impl SlotBuiltinRuntime {
    fn slot_event_ready(
        &self,
        slot_id: &str,
        resolution: &crate::SlotResolution<Value, Value, (), Value>,
    ) -> RuntimeReady {
        let event = match &resolution.kind {
            SlotResolutionKind::Reused => EvalEventKind::SlotReuse {
                slot: Arc::from(slot_id),
            },
            SlotResolutionKind::Stored {
                reason: SlotStoreReason::Missing,
            } => EvalEventKind::SlotPopulate {
                slot: Arc::from(slot_id),
                reason: SlotEventReason::Missing,
            },
            SlotResolutionKind::Stored { reason } => EvalEventKind::SlotRewrite {
                slot: Arc::from(slot_id),
                reason: match reason {
                    SlotStoreReason::Missing => SlotEventReason::Missing,
                    SlotStoreReason::WitnessChanged => SlotEventReason::WitnessChanged,
                    SlotStoreReason::ClaimsInvalid { reason } => SlotEventReason::ClaimsInvalid {
                        reason: reason.clone(),
                    },
                    SlotStoreReason::Refresh { intent } => SlotEventReason::Refresh {
                        intent: intent.clone(),
                    },
                },
            },
        };
        RuntimeReady {
            value: resolution.record.value.clone(),
            events: vec![event],
        }
    }

    fn verify_slot_record(
        &self,
        place: &str,
        record: &crate::SlotRecord<Value, Value, (), Value>,
    ) -> SlotVerification {
        if record.claims.is_empty() {
            return SlotVerification::Valid;
        }
        for claim in &record.claims {
            match claim {
                Claim::Which {
                    name,
                    expected_path,
                } => {
                    let actual = match (place, name.as_str()) {
                        ("win", "cl.exe") | ("win_same", "cl.exe") => Some("C:/VS/14.3/cl.exe"),
                        ("win_changed", "cl.exe") => Some("C:/VS/14.4/cl.exe"),
                        _ => None,
                    };
                    match (actual, expected_path.as_deref()) {
                        (Some(actual), Some(expected)) if actual == expected => {}
                        (Some(actual), Some(expected)) => {
                            return SlotVerification::invalid(format!(
                                "host.which(\"{name}\") changed: expected {expected}, got {actual}"
                            ));
                        }
                        (None, Some(expected)) => {
                            return SlotVerification::invalid(format!(
                                "host.which(\"{name}\") changed: expected {expected}, but it is now missing"
                            ));
                        }
                        (Some(actual), None) => {
                            return SlotVerification::invalid(format!(
                                "host.which(\"{name}\") changed: expected missing, got {actual}"
                            ));
                        }
                        (None, None) => {}
                    }
                }
                _ => {}
            }
        }
        SlotVerification::Valid
    }
}

impl RuntimeHooks for SlotBuiltinRuntime {
    fn call(
        &self,
        place: &str,
        callee: &str,
        args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match (callee, args) {
            ("host_which", [value]) => {
                let ValueRep::String(name) = &value.rep else {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "host_which() argument must evaluate to String",
                    ));
                };
                let path = match (place, name.as_ref()) {
                    ("win", "cl.exe") | ("win_same", "cl.exe") => "C:/VS/14.3/cl.exe",
                    ("win_changed", "cl.exe") => "C:/VS/14.4/cl.exe",
                    _ => {
                        return Ok(RuntimeCallOutcome::Unsupported);
                    }
                };
                Ok(RuntimeCallOutcome::Ready(
                    Value::string(path)
                        .with_provenance(Provenance::host_probe(
                            source.clone(),
                            place,
                            "host_which",
                        ))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue)
                        .with_claim(Claim::Which {
                            name: name.to_string(),
                            expected_path: Some(path.to_string()),
                        }),
                ))
            }
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
    }

    fn prepare_slot(
        &self,
        place: &str,
        slot_id: &str,
        witness: &Value,
        action: &SlotAction<Value>,
    ) -> Result<RuntimeSlotPrepare, EvalError> {
        let slots = self.slots.lock().expect("slot store lock poisoned");
        Ok(
            match slots.decide(
                slot_id.to_string(),
                &normalized_slot_witness(witness.clone()),
                action,
                |record| self.verify_slot_record(place, record),
            ) {
                SlotDecision::Reused(record) => RuntimeSlotPrepare::Reused(RuntimeReady {
                    value: record.value,
                    events: vec![EvalEventKind::SlotReuse {
                        slot: Arc::from(slot_id),
                    }],
                }),
                SlotDecision::Store { current, .. } => RuntimeSlotPrepare::Resolve {
                    current: current.map(|record| record.value),
                },
            },
        )
    }

    fn store_slot(
        &self,
        place: &str,
        slot_id: &str,
        witness: &Value,
        action: &SlotAction<Value>,
        resolved: &Value,
    ) -> Result<RuntimeReady, EvalError> {
        let mut slots = self.slots.lock().expect("slot store lock poisoned");
        let resolution = slots.resolve(
            slot_id.to_string(),
            normalized_slot_witness(witness.clone()),
            action.clone(),
            |record| self.verify_slot_record(place, record),
            |_| {
                Ok::<_, EvalError>(
                    SlotResolved::new(resolved.clone()).with_claims(resolved.meta.claims.clone()),
                )
            },
        )?;
        Ok(self.slot_event_ready(slot_id, &resolution))
    }
}

fn normalized_slot_witness(value: Value) -> Value {
    match value.rep {
        ValueRep::Unit => Value::unit(),
        ValueRep::Bool(value) => Value::bool(value),
        ValueRep::U64(value) => Value::u64(value),
        ValueRep::String(value) => Value::string(value),
        ValueRep::Array(items) => Value::array(
            items
                .iter()
                .cloned()
                .map(normalized_slot_witness)
                .collect::<Vec<_>>(),
        ),
        ValueRep::Map(map) => Value::map(
            map.entries
                .iter()
                .map(|(key, value)| {
                    (
                        normalized_slot_witness(key.clone()),
                        normalized_slot_witness(value.clone()),
                    )
                })
                .collect::<Vec<_>>(),
        ),
        ValueRep::Struct(struct_value) => Value::structure(
            struct_value.type_name.clone(),
            struct_value
                .fields
                .iter()
                .map(|(name, value)| (name.clone(), normalized_slot_witness(value.clone())))
                .collect::<Vec<_>>(),
        ),
        ValueRep::External(external) => {
            Value::external(external.kind.clone(), external.descriptor.clone())
        }
        ValueRep::Function(function) => Value::new(ValueRep::Function(function)),
    }
}

#[test]
fn imported_helper_function_can_be_called() {
    let program = test_program(
        [
            ("main.vx", "fn build() { helper(41) }"),
            ("helpers.vx", "fn helper(value: U64) { value }"),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );

    let Resolution::Ready(value) = run_program(program) else {
        panic!("expected ready imported helper result");
    };
    assert_eq!(value.rep, ValueRep::U64(41));
}

#[test]
fn imported_helper_can_call_another_imported_or_local_helper() {
    let program = EvalProgram::new(
        0,
        vec![
            EvalProgramSource::new("main.vx", "fn build() { outer() }"),
            EvalProgramSource::new(
                "helpers.vx",
                "fn outer() { local_helper() }\nfn local_helper() { imported_helper() }",
            ),
            EvalProgramSource::new("shared.vx", "fn imported_helper() { 42 }"),
        ],
        vec![
            EvalProgramImport { from: 0, to: 1 },
            EvalProgramImport { from: 1, to: 2 },
        ],
    )
    .expect("build imported helper chain program");

    let Resolution::Ready(value) = run_program(program) else {
        panic!("expected ready imported helper chain result");
    };
    assert_eq!(value.rep, ValueRep::U64(42));
}

#[test]
fn stdlib_style_bodyful_function_executes_directly() {
    let program = test_program(
        [
            ("main.vx", "fn build() { std_direct() }"),
            ("crates/vx-stdlib/std/test.vx", "fn std_direct() { 41 }"),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );

    let Resolution::Ready(value) = run_program(program) else {
        panic!("expected ready stdlib-style bodyful result");
    };
    assert_eq!(value.rep, ValueRep::U64(41));
}

#[test]
fn stdlib_style_declaration_without_body_uses_runtime_hooks() {
    let program = test_program(
        [
            ("main.vx", "fn build() { host_os() }"),
            ("crates/vx-stdlib/std/test.vx", "fn host_os();"),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );

    let mut session = EvalSession::new_program(program, "build", Vec::new(), &TestRuntime, "win");
    let resolution = session
        .run()
        .expect("evaluate imported runtime-backed declaration");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready imported runtime-backed result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("windows".into()));
}

#[test]
fn unresolved_import_missing_target_still_errors_clearly() {
    let program = test_program(
        [
            ("main.vx", "fn build() { does_not_exist() }"),
            ("helpers.vx", "fn helper() { 41 }"),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );

    let Resolution::Poison(error) = run_program(program) else {
        panic!("expected poison for unresolved imported target");
    };
    assert_eq!(error.kind, EvalErrorKind::Internal);
    assert!(
        error
            .message
            .contains("call target does_not_exist is not supported yet")
    );
}

#[test]
fn eval_program_structurally_detects_stateful_slots() {
    let program = EvalProgram::single_source(
        "main.vx",
        "fn build() { slot_current (\"pure-slot\", \"w1\", resolve_slot) }\nfn resolve_slot(ctx) { ctx.witness }\n",
    );

    assert!(
        program
            .uses_stateful_slots()
            .expect("parse slot builtin program"),
        "slot builtins should be detected structurally regardless of whitespace layout"
    );
}

#[test]
fn whitespace_varied_slot_calls_still_execute() {
    let runtime = SlotBuiltinRuntime::default();
    let source = r#"
fn build_current(witness: String, desired: String) {
  slot_current (
    "whitespace-slot",
    witness,
    ((ctx) => desired)
  )
}

fn build_refresh(witness: String, desired: String, intent: String) {
  slot_refresh (
    "whitespace-slot",
    witness,
    intent,
    ((ctx) => desired)
  )
}
"#;

    let Resolution::Ready(first) = eval_source_entry_at_place_with_args_with_source_id(
        source,
        "whitespace-slot-call",
        "build_current",
        &[Value::string("w1"), Value::string("v1")],
        &runtime,
        "local",
    )
    .expect("populate whitespace-varied slot") else {
        panic!("expected ready whitespace-varied slot result");
    };
    assert_eq!(first.rep, ValueRep::String("v1".into()));

    let Resolution::Ready(refreshed) = eval_source_entry_at_place_with_args_with_source_id(
        source,
        "whitespace-slot-call",
        "build_refresh",
        &[
            Value::string("w1"),
            Value::string("v2"),
            Value::string("force"),
        ],
        &runtime,
        "local",
    )
    .expect("refresh whitespace-varied slot") else {
        panic!("expected ready whitespace-varied refresh result");
    };
    assert_eq!(refreshed.rep, ValueRep::String("v2".into()));
}

#[test]
fn closure_literal_can_be_bound_and_called() {
    let Resolution::Ready(value) = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  let id = ((value) => value);
  id("ok")
}
"#,
        "closure-bound-call",
        "build",
        &[],
    )
    .expect("evaluate bound closure call") else {
        panic!("expected ready bound closure result");
    };
    assert_eq!(value.rep, ValueRep::String("ok".into()));
}

#[test]
fn closure_captures_outer_value() {
    let Resolution::Ready(value) = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  let captured = "captured";
  let pick = ((ignored) => captured);
  pick("ignored")
}
"#,
        "closure-capture",
        "build",
        &[],
    )
    .expect("evaluate closure capture") else {
        panic!("expected ready closure capture result");
    };
    assert_eq!(value.rep, ValueRep::String("captured".into()));
}

#[test]
fn closure_can_be_passed_as_a_value() {
    let Resolution::Ready(value) = eval_source_entry_with_args_with_source_id(
        r#"
fn apply(value: String, f) {
  f(value)
}

fn build() {
  let echo = ((value) => value);
  apply("forwarded", echo)
}
"#,
        "closure-arg",
        "build",
        &[],
    )
    .expect("evaluate closure passed as value") else {
        panic!("expected ready closure arg result");
    };
    assert_eq!(value.rep, ValueRep::String("forwarded".into()));
}

#[test]
fn slot_builtins_accept_inline_closure_resolvers() {
    let runtime = SlotBuiltinRuntime::default();
    let source = r#"
fn build_current(witness: String) {
  slot_current("inline-slot", witness, ((ctx) => ctx.witness))
}

fn build_refresh(witness: String, value: String, intent: String) {
  slot_refresh("inline-slot", witness, intent, ((ctx) => value))
}
"#;

    let Resolution::Ready(first) = eval_source_entry_at_place_with_args_with_source_id(
        source,
        "inline-slot-closure",
        "build_current",
        &[Value::string("v1")],
        &runtime,
        "local",
    )
    .expect("populate inline closure slot") else {
        panic!("expected ready inline closure slot result");
    };
    assert_eq!(first.rep, ValueRep::String("v1".into()));

    let Resolution::Ready(refreshed) = eval_source_entry_at_place_with_args_with_source_id(
        source,
        "inline-slot-closure",
        "build_refresh",
        &[
            Value::string("v1"),
            Value::string("v2"),
            Value::string("force"),
        ],
        &runtime,
        "local",
    )
    .expect("refresh inline closure slot") else {
        panic!("expected ready refreshed inline closure slot result");
    };
    assert_eq!(refreshed.rep, ValueRep::String("v2".into()));
}

#[test]
fn authored_vixen_pure_slot_current_and_refresh_work() {
    let runtime = SlotBuiltinRuntime::default();
    let source = r#"
fn build_current(witness: String, desired: U64) {
  fn resolve_slot(ctx) { desired }
  slot_current("pure-slot", witness, resolve_slot)
}

fn build_refresh(witness: String, desired: U64, intent: String) {
  fn resolve_slot(ctx) { desired }
  slot_refresh("pure-slot", witness, intent, resolve_slot)
}
"#;

    let Resolution::Ready(first) = eval_source_entry_at_place_with_args_with_source_id(
        source,
        "authored-pure-slot",
        "build_current",
        &[Value::string("w1"), Value::u64(1)],
        &runtime,
        "local",
    )
    .expect("populate authored pure slot") else {
        panic!("expected ready authored pure slot result");
    };
    assert_eq!(first.rep, ValueRep::U64(1));

    let Resolution::Ready(reused) = eval_source_entry_at_place_with_args_with_source_id(
        source,
        "authored-pure-slot",
        "build_current",
        &[Value::string("w1"), Value::u64(999)],
        &runtime,
        "local",
    )
    .expect("reuse authored pure slot") else {
        panic!("expected ready reused authored pure slot result");
    };
    assert_eq!(reused.rep, ValueRep::U64(1));

    let Resolution::Ready(rewritten) = eval_source_entry_at_place_with_args_with_source_id(
        source,
        "authored-pure-slot",
        "build_current",
        &[Value::string("w2"), Value::u64(2)],
        &runtime,
        "local",
    )
    .expect("rerun authored pure slot after witness change") else {
        panic!("expected ready witness-changed authored pure slot result");
    };
    assert_eq!(rewritten.rep, ValueRep::U64(2));

    let Resolution::Ready(refreshed) = eval_source_entry_at_place_with_args_with_source_id(
        source,
        "authored-pure-slot",
        "build_refresh",
        &[Value::string("w2"), Value::u64(3), Value::string("refresh")],
        &runtime,
        "local",
    )
    .expect("refresh authored pure slot") else {
        panic!("expected ready refreshed authored pure slot result");
    };
    assert_eq!(refreshed.rep, ValueRep::U64(3));
}

#[test]
fn authored_vixen_observed_slot_current_and_refresh_work() {
    let runtime = SlotBuiltinRuntime::default();
    let source = r#"
fn build_current(name: String) {
  fn resolve_slot(ctx) { host_which(ctx.witness) }
  slot_current("toolchain-slot", name, resolve_slot)
}

fn build_refresh(name: String, intent: String) {
  fn resolve_slot(ctx) { host_which(ctx.witness) }
  slot_refresh("toolchain-slot", name, intent, resolve_slot)
}
"#;

    let Resolution::Ready(first) = eval_source_entry_at_place_with_args_with_source_id(
        source,
        "authored-observed-slot",
        "build_current",
        &[Value::string("cl.exe")],
        &runtime,
        "win",
    )
    .expect("populate authored observed slot") else {
        panic!("expected ready authored observed slot result");
    };
    assert_eq!(first.rep, ValueRep::String("C:/VS/14.3/cl.exe".into()));

    let Resolution::Ready(reused) = eval_source_entry_at_place_with_args_with_source_id(
        source,
        "authored-observed-slot",
        "build_current",
        &[Value::string("cl.exe")],
        &runtime,
        "win_same",
    )
    .expect("reuse authored observed slot") else {
        panic!("expected ready reused observed slot result");
    };
    assert_eq!(reused.rep, ValueRep::String("C:/VS/14.3/cl.exe".into()));

    let Resolution::Ready(rewritten) = eval_source_entry_at_place_with_args_with_source_id(
        source,
        "authored-observed-slot",
        "build_current",
        &[Value::string("cl.exe")],
        &runtime,
        "win_changed",
    )
    .expect("rerun authored observed slot after verification change") else {
        panic!("expected ready verification-rerun observed slot result");
    };
    assert_eq!(rewritten.rep, ValueRep::String("C:/VS/14.4/cl.exe".into()));

    let Resolution::Ready(refreshed) = eval_source_entry_at_place_with_args_with_source_id(
        source,
        "authored-observed-slot",
        "build_refresh",
        &[Value::string("cl.exe"), Value::string("force_rediscover")],
        &runtime,
        "win_changed",
    )
    .expect("refresh authored observed slot") else {
        panic!("expected ready refreshed observed slot result");
    };
    assert_eq!(refreshed.rep, ValueRep::String("C:/VS/14.4/cl.exe".into()));
}

#[test]
fn imported_resolver_helper_can_drive_slot_builtin() {
    let runtime = SlotBuiltinRuntime::default();
    let program = EvalProgram::new(
        0,
        vec![
            EvalProgramSource::new(
                "main.vx",
                "fn build_current(witness: String) { slot_current(\"imported-slot\", witness, imported_resolve) }",
            ),
            EvalProgramSource::new("helpers.vx", "fn imported_resolve(ctx) { ctx.witness }"),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    )
    .expect("build slot imported resolver program");

    let mut session = EvalSession::new_program(
        program.clone(),
        "build_current",
        vec![Value::string("v1")],
        &runtime,
        "local",
    );
    let Resolution::Ready(first) = session.run().expect("run imported slot resolver") else {
        panic!("expected ready imported slot resolver result");
    };
    assert_eq!(first.rep, ValueRep::String("v1".into()));

    let mut session = EvalSession::new_program(
        program,
        "build_current",
        vec![Value::string("v2")],
        &runtime,
        "local",
    );
    let Resolution::Ready(second) = session.run().expect("run imported slot resolver rewrite")
    else {
        panic!("expected ready imported slot resolver rewrite result");
    };
    assert_eq!(second.rep, ValueRep::String("v2".into()));
}

#[test]
fn refresh_rewrites_same_slot_id_not_new_lineage() {
    let runtime = SlotBuiltinRuntime::default();
    let source = r#"
fn build_current(value: U64) {
  fn resolve_slot(ctx) { value }
  slot_current("lineage-slot", "stable", resolve_slot)
}

fn build_refresh(value: U64, intent: String) {
  fn resolve_slot(ctx) { value }
  slot_refresh("lineage-slot", "stable", intent, resolve_slot)
}
"#;

    let _ = eval_source_entry_at_place_with_args_with_source_id(
        source,
        "slot-lineage",
        "build_current",
        &[Value::u64(1)],
        &runtime,
        "local",
    )
    .expect("populate lineage slot");
    let _ = eval_source_entry_at_place_with_args_with_source_id(
        source,
        "slot-lineage",
        "build_refresh",
        &[Value::u64(2), Value::string("force")],
        &runtime,
        "local",
    )
    .expect("refresh lineage slot");

    let slots = runtime.slots.lock().expect("slot store lock poisoned");
    let record = slots
        .get(&SlotId::from("lineage-slot"))
        .expect("lineage slot record should exist");
    assert_eq!(record.slot_id, SlotId::from("lineage-slot"));
    assert_eq!(record.meta.write_count, 2);
    assert_eq!(record.meta.rewrite_count, 1);
}

#[test]
fn slot_runtime_calls_emit_trace_events_with_refresh_intent() {
    let runtime = SlotBuiltinRuntime::default();
    let source = r#"
fn build(value: U64) {
  fn resolve_slot(ctx) { value }
  slot_current("toolchain", "w1", resolve_slot)
}
"#;
    let (initial, initial_trace) = eval_source_entry_at_place_with_args_with_source_id_traced(
        source,
        "slot-trace-current",
        "build",
        &[Value::u64(41)],
        &runtime,
        "local",
    )
    .expect("evaluate initial slot current");
    let Resolution::Ready(initial_value) = initial else {
        panic!("expected ready resolution, got {initial:?}");
    };
    assert_eq!(initial_value.as_u64(), Some(41));
    assert!(initial_trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::SlotPopulate {
                slot: "toolchain".into(),
                reason: SlotEventReason::Missing,
            }
    }));

    let refresh_source = r#"
fn build(value: U64) {
  fn resolve_slot(ctx) { value }
  slot_refresh("toolchain", "w1", "force-rediscover", resolve_slot)
}
"#;
    let (refreshed, refresh_trace) = eval_source_entry_at_place_with_args_with_source_id_traced(
        refresh_source,
        "slot-trace-refresh",
        "build",
        &[Value::u64(99)],
        &runtime,
        "local",
    )
    .expect("evaluate slot refresh");
    let Resolution::Ready(refreshed_value) = refreshed else {
        panic!("expected ready resolution, got {refreshed:?}");
    };
    assert_eq!(refreshed_value.as_u64(), Some(99));
    assert!(refresh_trace.events.iter().any(|event| {
        matches!(
            &event.kind,
            EvalEventKind::SlotRewrite {
                slot,
                reason: SlotEventReason::Refresh { intent },
            } if slot.as_ref() == "toolchain"
                && matches!(&intent.rep, ValueRep::String(value) if value.as_ref() == "force-rediscover")
        )
    }));

    let stored = runtime
        .slots
        .lock()
        .expect("slot lock poisoned")
        .get(&SlotId::from("toolchain"))
        .cloned()
        .expect("stored slot");
    assert_eq!(stored.value.as_u64(), Some(99));
    match stored.meta.last_store_reason {
        SlotStoreReason::Refresh { intent } => {
            assert!(matches!(
                &intent.rep,
                ValueRep::String(value) if value.as_ref() == "force-rediscover"
            ));
        }
        other => panic!("expected refresh store reason, got {other:?}"),
    }
}

#[test]
fn scalar_values_can_carry_claims_without_wrapper_enums() {
    let value = Value::u64(42)
        .with_provenance(Provenance::host_probe(
            SourceSpan::synthetic("discover-toolchain"),
            "windows-1",
            "discover toolchain",
        ))
        .with_claim(Claim::Arch {
            value: "x86_64".to_string(),
        });

    assert_eq!(value.as_u64(), Some(42));
    assert_eq!(value.meta.claims.len(), 1);
    assert!(matches!(value.rep, ValueRep::U64(42)));
}

#[test]
fn projection_updates_provenance_without_changing_payload_kind() {
    let value = Value::string("clang")
        .with_provenance(Provenance::runtime_input(
            SourceSpan::synthetic("toolchain"),
            RuntimeInputKind::DiscoverResult,
            "toolchain",
        ))
        .project(ProjectionStep::Field("cc".to_string()));

    assert!(matches!(value.rep, ValueRep::String(_)));
    assert_eq!(
        value.meta.provenance.path.as_slice(),
        [ProjectionStep::Field("cc".to_string())]
    );
}

#[test]
fn readiness_is_not_encoded_inside_value() {
    let ready = Resolution::Ready(Value::bool(true));
    let pending = Resolution::Pending(PendingRef::new(7, "waiting on discover"));

    assert!(matches!(ready, Resolution::Ready(_)));
    assert!(matches!(pending, Resolution::Pending(_)));
}

#[test]
fn generic_external_values_cover_boundary_cases_without_build_nouns() {
    let stable_scalar = Value::u64(42);
    let blocked_external = Value::external("tree", "/workspace");
    let proxied_external = Value::external("tree", "remote:17")
        .with_crossing(CrossingPolicy::ByProxy)
        .with_reuse(ReuseClass::Observed);

    assert_eq!(stable_scalar.meta.reuse, ReuseClass::Stable);
    assert_eq!(stable_scalar.meta.crossing, CrossingPolicy::ByValue);
    assert_eq!(blocked_external.meta.reuse, ReuseClass::Live);
    assert_eq!(blocked_external.meta.crossing, CrossingPolicy::Blocked);
    assert_eq!(proxied_external.meta.crossing, CrossingPolicy::ByProxy);
    assert_eq!(proxied_external.meta.reuse, ReuseClass::Observed);
}

#[test]
fn wire_round_trip_preserves_value_shape_and_metadata() {
    let value = Value::structure(
        "Toolchain",
        [(
            "cc".into(),
            Value::string("cl.exe")
                .with_provenance(Provenance::host_probe(
                    SourceSpan::synthetic("toolchain-probe"),
                    "win",
                    "toolchain probe",
                ))
                .with_claim(Claim::Which {
                    name: "cl.exe".to_string(),
                    expected_path: Some("C:/VS/cl.exe".to_string()),
                })
                .with_reuse(ReuseClass::Observed),
        )],
    )
    .with_provenance(Provenance::binding(
        SourceSpan::synthetic("toolchain-binding"),
        "toolchain",
    ))
    .with_crossing(CrossingPolicy::ByValue);

    let encoded = value_to_envelope(&value).expect("encode value");
    let decoded = envelope_to_value(encoded).expect("decode value");

    assert_eq!(decoded, value);
}

#[test]
fn function_values_fail_cleanly_at_the_wire_boundary() {
    let Resolution::Ready(value) = eval_source_entry_with_args_with_source_id(
        "fn build() { ((value) => value) }",
        "wire-function-result",
        "build",
        &[],
    )
    .expect("evaluate closure-valued result") else {
        panic!("expected ready closure-valued result");
    };

    let error = resolution_to_outcome(Resolution::Ready(value))
        .expect_err("function values should not serialize across the daemon boundary");
    assert_eq!(error.kind, EvalErrorKind::TypeError);
    assert!(
        error
            .message
            .contains("function values cannot cross the daemon boundary yet")
    );
}

#[test]
fn fail_expression_preserves_structured_conflict_detail() {
    let (resolution, _trace) = eval_source_entry_with_args_with_source_id_traced(
        r#"
fn build() {
  fail Conflict {
    package: "logger",
    versions: ["1.0.0", "1.1.0"],
  }
}
"#,
        "structured-fail",
        "build",
        &[],
    )
    .expect("evaluate structured fail");

    let Resolution::Poison(error) = resolution else {
        panic!("expected poison structured fail");
    };
    assert_eq!(error.kind, EvalErrorKind::UserFail);
    assert_eq!(error.message.as_ref(), "Conflict");
    let detail = error.detail.expect("expected structured conflict detail");
    let ValueRep::Struct(struct_value) = &detail.rep else {
        panic!("expected structured detail, got {:?}", detail.rep);
    };
    assert_eq!(struct_value.type_name.as_ref(), "Conflict");
    assert_eq!(struct_value.fields.len(), 2);
    assert_eq!(struct_value.fields[0].0.as_ref(), "package");
    assert_eq!(
        struct_value.fields[0].1.rep,
        ValueRep::String("logger".into())
    );
    assert_eq!(struct_value.fields[1].0.as_ref(), "versions");
    let ValueRep::Array(versions) = &struct_value.fields[1].1.rep else {
        panic!(
            "expected versions array, got {:?}",
            struct_value.fields[1].1.rep
        );
    };
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].rep, ValueRep::String("1.0.0".into()));
    assert_eq!(versions[1].rep, ValueRep::String("1.1.0".into()));
}

#[test]
fn poison_outcome_preserves_structured_conflict_detail_across_wire() {
    let (resolution, _trace) = eval_source_entry_with_args_with_source_id_traced(
        r#"
fn build() {
  fail Conflict {
    package: "logger",
    version: "1.1.0",
  }
}
"#,
        "structured-fail-wire",
        "build",
        &[],
    )
    .expect("evaluate structured fail");

    let Resolution::Poison(error) = resolution else {
        panic!("expected poison structured fail");
    };
    let expected_detail = error
        .detail
        .clone()
        .expect("expected structured conflict detail");
    let outcome = resolution_to_outcome(Resolution::Poison(error)).expect("serialize poison");
    let EvalOutcome::Poison {
        kind,
        message,
        detail,
    } = outcome
    else {
        panic!("expected poison outcome");
    };
    assert_eq!(kind, "UserFail");
    assert_eq!(message, "Conflict");
    let detail = detail.expect("expected structured poison detail on wire");
    let detail = envelope_to_value(detail).expect("decode structured poison detail");
    assert_eq!(detail, expected_detail);
}

#[test]
fn search_assumption_returns_first_successful_branch() {
    let (resolution, _trace) = eval_source_entry_with_args_with_source_id_traced(
        r#"
fn resolve_candidate() {
  let version = branch_assumption("version");
  if version == "1.0.0" (
    fail Conflict {
      version: version,
    }
  ) else (
    version
  )
}

fn build() {
  search_assumption("version", ["1.0.0", "1.1.0"], resolve_candidate)
}
"#,
        "search-assumption-success",
        "build",
        &[],
    )
    .expect("evaluate exact-search success");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready exact-search result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("1.1.0".into()));
}

#[test]
fn search_assumption_aggregates_conflicts_when_all_branches_fail() {
    let (resolution, _trace) = eval_source_entry_with_args_with_source_id_traced(
        r#"
fn resolve_candidate() {
  fail Conflict {
    version: branch_assumption("version"),
  }
}

fn build() {
  search_assumption("version", ["1.0.0", "1.1.0"], resolve_candidate)
}
"#,
        "search-assumption-conflicts",
        "build",
        &[],
    )
    .expect("evaluate exact-search conflict aggregation");

    let Resolution::Poison(error) = resolution else {
        panic!("expected poison exact-search conflict result, got {resolution:?}");
    };
    assert_eq!(error.kind, EvalErrorKind::UserFail);
    assert_eq!(error.message.as_ref(), "BranchConflict");
    let detail = error
        .detail
        .expect("expected aggregated branch conflict detail");
    let ValueRep::Struct(struct_value) = &detail.rep else {
        panic!("expected BranchConflict detail, got {:?}", detail.rep);
    };
    assert_eq!(struct_value.type_name.as_ref(), "BranchConflict");
    assert_eq!(struct_value.fields[0].0.as_ref(), "assumption");
    assert_eq!(
        struct_value.fields[0].1.rep,
        ValueRep::String("version".into())
    );
    assert_eq!(struct_value.fields[1].0.as_ref(), "attempts");
    let ValueRep::Array(attempts) = &struct_value.fields[1].1.rep else {
        panic!(
            "expected attempts array, got {:?}",
            struct_value.fields[1].1.rep
        );
    };
    assert_eq!(attempts.len(), 2);
    for (attempt, expected) in attempts.iter().zip(["1.0.0", "1.1.0"]) {
        let ValueRep::Struct(attempt_struct) = &attempt.rep else {
            panic!("expected BranchAttempt detail, got {:?}", attempt.rep);
        };
        assert_eq!(attempt_struct.type_name.as_ref(), "BranchAttempt");
        assert_eq!(attempt_struct.fields[0].0.as_ref(), "value");
        assert_eq!(
            attempt_struct.fields[0].1.rep,
            ValueRep::String(expected.into())
        );
        assert_eq!(attempt_struct.fields[1].0.as_ref(), "conflict");
        let ValueRep::Struct(conflict) = &attempt_struct.fields[1].1.rep else {
            panic!(
                "expected structured conflict payload, got {:?}",
                attempt_struct.fields[1].1.rep
            );
        };
        assert_eq!(conflict.type_name.as_ref(), "Conflict");
    }
}

#[test]
fn search_assumption_resumes_pending_branch_and_continues_to_later_success() {
    let mut session = EvalSession::new(
        r#"
fn host_wait();

fn resolve_candidate() {
  let version = branch_assumption("version");
  if version == "1.0.0" (
    let _ready = host_wait();
    fail Conflict {
      version: version,
    }
  ) else (
    version
  )
}

fn build() {
  search_assumption("version", ["1.0.0", "1.1.0"], resolve_candidate)
}
"#,
        "search-assumption-pending",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    let Resolution::Pending(pending) = session.run().expect("start suspended exact-search branch")
    else {
        panic!("expected pending exact-search branch");
    };
    session
        .resolve_pending(&pending, Value::string("ready"))
        .expect("resolve suspended exact-search branch");

    let Resolution::Ready(value) = session.run().expect("resume exact-search branch") else {
        panic!("expected ready exact-search result after resume");
    };
    assert_eq!(value.rep, ValueRep::String("1.1.0".into()));
}

#[test]
fn search_assumption_reuses_learned_point_no_goods_within_same_session() {
    let mut session = EvalSession::new(
        r#"
fn host_wait();

fn resolve_candidate() {
  let version = branch_assumption("version");
  if version == "1.0.0" (
    let _ready = host_wait();
    fail Conflict {
      version: version,
    }
  ) else (
    version
  )
}

fn search_once() {
  search_assumption("version", ["1.0.0", "1.1.0"], resolve_candidate)
}

fn build() {
  let first = search_once();
  let second = search_once();
  [first, second]
}
"#,
        "search-assumption-learned-point-no-good",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    let Resolution::Pending(pending) = session.run().expect("start first exact-search branch")
    else {
        panic!("expected pending exact-search branch");
    };
    session
        .resolve_pending(&pending, Value::string("ready"))
        .expect("resolve learned point no-good branch");

    let Resolution::Ready(value) = session
        .run()
        .expect("resume exact-search with learned point no-good")
    else {
        panic!("expected ready exact-search result after learning point no-good");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected result array, got {:?}", value.rep);
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].rep, ValueRep::String("1.1.0".into()));
    assert_eq!(items[1].rep, ValueRep::String("1.1.0".into()));
}

#[test]
fn search_assumption_widens_adjacent_identical_conflicts_into_one_exclusion() {
    let runtime = UnsupportedRuntime;
    let program = EvalProgram::single_source(
        "search-assumption-range-no-good",
        r#"
fn resolve_candidate() {
  let version = branch_assumption("version");
  if version == "1.2.0" (
    version
  ) else (
    fail Conflict {
      package: "logger",
      reason: "locked-by-root",
    }
  )
}

fn search_once() {
  search_assumption("version", ["1.0.0", "1.1.0", "1.2.0"], resolve_candidate)
}

fn build() {
  let first = search_once();
  let second = search_once();
  [first, second]
}
"#,
    );
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let Resolution::Ready(value) = session.run().expect("run widened no-good session") else {
        panic!("expected ready widened no-good session");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected result array, got {:?}", value.rep);
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].rep, ValueRep::String("1.2.0".into()));
    assert_eq!(items[1].rep, ValueRep::String("1.2.0".into()));

    let stats = session.stats();
    assert_eq!(stats.learned_no_good_count, 1);
    assert_eq!(stats.widened_no_good_count, 1);
}

#[test]
fn search_assumption_reuses_learned_failing_search_volume_within_same_session() {
    let mut session = EvalSession::new(
        r#"
fn host_wait();

fn resolve_inner_candidate() {
  let version = branch_assumption("version");
  let _ready = host_wait();
  fail Conflict {
    package: "logger",
    version: version,
    reason: "locked-by-root",
  }
}

fn run_inner() {
  search_assumption("version", ["1.0.0", "1.1.0"], resolve_inner_candidate)
}

fn run_inner_suffix() {
  search_assumption("version", ["1.1.0"], resolve_inner_candidate)
}

fn resolve_outer() {
  let outer = branch_assumption("outer");
  with_assumption(
    "outer",
    "shared",
    (() => if outer == "a" (run_inner()) else (run_inner_suffix())),
  )
}

fn build() {
  search_assumption("outer", ["a", "b"], resolve_outer)
}
"#,
        "search-assumption-search-volume-no-good",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    let mut pending_count = 0;
    let error = loop {
        match session.run().expect("run failing nested exact search") {
            Resolution::Pending(pending) => {
                pending_count += 1;
                session
                    .resolve_pending(&pending, Value::string("ready"))
                    .expect("resolve pending failing branch");
            }
            Resolution::Poison(error) => break error,
            resolution => panic!("expected pending or poison resolution, got {resolution:?}"),
        }
    };

    let stats = session.stats();
    assert_eq!(pending_count, 2);
    assert_eq!(error.kind, EvalErrorKind::UserFail);
    assert_eq!(error.message.as_ref(), "BranchConflict");
    assert_eq!(stats.learned_search_no_good_count, 2);
}

#[test]
fn entry_arguments_are_bound_as_runtime_inputs() {
    let resolution = eval_source_entry_with_args_with_source_id(
        "fn build(value: String) { value }",
        "entry-args",
        "build",
        &[Value::string("forty-two")],
    )
    .expect("evaluate entry with args");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("forty-two".into()));
    assert_eq!(value.meta.provenance.source.source_id, "entry-args");
    assert_eq!(
        value.meta.provenance.root,
        Provenance::runtime_input(
            value.meta.provenance.source.clone(),
            RuntimeInputKind::Argument,
            "value",
        )
        .root
    );
}

#[test]
fn entry_argument_count_mismatch_is_a_type_error() {
    let error = eval_source_entry_with_args_with_source_id(
        "fn build(value: String) { value }",
        "arity",
        "build",
        &[],
    )
    .expect_err("missing arg should fail");

    assert_eq!(error.kind, crate::EvalErrorKind::TypeError);
    assert!(
        error
            .message
            .contains("expects 1 positional argument(s), got 0")
    );
}

#[test]
fn traced_entry_arguments_emit_binding_and_ready_events() {
    let (resolution, trace) = eval_source_entry_with_args_with_source_id_traced(
        "fn build(value: String) { value }",
        "trace-bindings",
        "build",
        &[Value::string("forty-two")],
    )
    .expect("evaluate traced entry");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("forty-two".into()));

    assert_eq!(
        trace.events[0].kind,
        EvalEventKind::BindArgument {
            label: "value".into(),
        },
    );
    assert_eq!(trace.events[0].node_id.0, 1);
    assert_eq!(trace.events[0].parent_id, None);
    assert_eq!(trace.events[0].work_id, None);
    assert_eq!(trace.events[0].place.as_ref(), "local");
    assert_eq!(
        trace.events[0]
            .source
            .as_ref()
            .map(|source| source.source_id.as_str()),
        Some("trace-bindings"),
    );
    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::EnterExpr {
                kind: crate::EvalExprKind::NameRef,
            }
    }));
    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::ExitReady {
                reuse: ReuseClass::Stable,
                crossing: CrossingPolicy::ByValue,
            }
    }));
    assert!(trace.events.iter().all(|event| event.work_id.is_none()));
}

struct TestRuntime;

impl RuntimeHooks for TestRuntime {
    fn call(
        &self,
        place: &str,
        callee: &str,
        args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match (place, callee, args) {
            ("win", "host_os", []) => Ok(RuntimeCallOutcome::Ready(
                Value::string("windows")
                    .with_provenance(Provenance::host_probe(source.clone(), place, "host_os"))
                    .with_reuse(ReuseClass::Observed)
                    .with_crossing(CrossingPolicy::ByValue),
            )),
            ("win", "host_wait", []) => Ok(RuntimeCallOutcome::Pending {
                description: "waiting for remote host observation".into(),
            }),
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
    }
}

struct CountingRuntime {
    host_os_calls: Arc<AtomicUsize>,
    host_wait_calls: Arc<AtomicUsize>,
}

impl RuntimeHooks for CountingRuntime {
    fn call(
        &self,
        place: &str,
        callee: &str,
        _args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match (place, callee) {
            ("win", "host_os") => {
                self.host_os_calls.fetch_add(1, Ordering::SeqCst);
                Ok(RuntimeCallOutcome::Ready(
                    Value::string("windows")
                        .with_provenance(Provenance::host_probe(source.clone(), place, "host_os"))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue),
                ))
            }
            ("win", "host_wait") => {
                self.host_wait_calls.fetch_add(1, Ordering::SeqCst);
                Ok(RuntimeCallOutcome::Pending {
                    description: "waiting for remote host observation".into(),
                })
            }
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
    }
}

struct DoublePendingRuntime {
    host_os_calls: Arc<AtomicUsize>,
    wait_one_calls: Arc<AtomicUsize>,
    wait_two_calls: Arc<AtomicUsize>,
}

impl RuntimeHooks for DoublePendingRuntime {
    fn call(
        &self,
        place: &str,
        callee: &str,
        _args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match (place, callee) {
            ("win", "host_os") => {
                self.host_os_calls.fetch_add(1, Ordering::SeqCst);
                Ok(RuntimeCallOutcome::Ready(
                    Value::string("windows")
                        .with_provenance(Provenance::host_probe(source.clone(), place, "host_os"))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue),
                ))
            }
            ("win", "wait_one") => {
                self.wait_one_calls.fetch_add(1, Ordering::SeqCst);
                Ok(RuntimeCallOutcome::Pending {
                    description: "waiting for first remote result".into(),
                })
            }
            ("win", "wait_two") => {
                self.wait_two_calls.fetch_add(1, Ordering::SeqCst);
                Ok(RuntimeCallOutcome::Pending {
                    description: "waiting for second remote result".into(),
                })
            }
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
    }
}

struct ParallelObserveRuntime {
    active_calls: Arc<AtomicUsize>,
    max_calls: Arc<AtomicUsize>,
}

impl RuntimeHooks for ParallelObserveRuntime {
    fn call(
        &self,
        place: &str,
        callee: &str,
        _args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match (place, callee) {
            ("win", "host_os") => {
                let active = self.active_calls.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_calls.fetch_max(active, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(40));
                self.active_calls.fetch_sub(1, Ordering::SeqCst);
                Ok(RuntimeCallOutcome::Ready(
                    Value::string("windows")
                        .with_provenance(Provenance::host_probe(source.clone(), place, "host_os"))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue),
                ))
            }
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
    }
}

struct StaggeredRuntime;

impl RuntimeHooks for StaggeredRuntime {
    fn call(
        &self,
        place: &str,
        callee: &str,
        _args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match (place, callee) {
            ("win", "slow_value") => {
                thread::sleep(Duration::from_millis(50));
                Ok(RuntimeCallOutcome::Ready(
                    Value::string("slow")
                        .with_provenance(Provenance::host_probe(
                            source.clone(),
                            place,
                            "slow_value",
                        ))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue),
                ))
            }
            ("win", "fast_value") => {
                thread::sleep(Duration::from_millis(5));
                Ok(RuntimeCallOutcome::Ready(
                    Value::string("fast")
                        .with_provenance(Provenance::host_probe(
                            source.clone(),
                            place,
                            "fast_value",
                        ))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue),
                ))
            }
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
    }
}

struct PanicRuntime;

impl RuntimeHooks for PanicRuntime {
    fn call(
        &self,
        place: &str,
        callee: &str,
        _args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match (place, callee) {
            ("win", "panic_now") => panic!("panic runtime hook exploded"),
            ("win", "host_os") => Ok(RuntimeCallOutcome::Ready(
                Value::string("windows")
                    .with_provenance(Provenance::host_probe(source.clone(), place, "host_os"))
                    .with_reuse(ReuseClass::Observed)
                    .with_crossing(CrossingPolicy::ByValue),
            )),
            _ => Ok(RuntimeCallOutcome::Unsupported),
        }
    }
}

#[test]
fn traced_runtime_calls_emit_start_and_finish_events() {
    let (resolution, trace) = eval_source_entry_at_place_with_args_with_source_id_traced(
        "fn build() { host_os() }",
        "trace-runtime",
        "build",
        &[],
        &TestRuntime,
        "win",
    )
    .expect("evaluate traced runtime call");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("windows".into()));

    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::EnterExpr {
                kind: crate::EvalExprKind::Call,
            }
    }));
    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::RuntimeCallStarted {
                callee: "host_os".into(),
                argc: 0,
            }
    }));
    assert!(
        trace
            .events
            .iter()
            .any(|event| matches!(event.kind, EvalEventKind::DependOn { .. }))
    );
    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::RuntimeCallFinishedReady {
                reuse: ReuseClass::Observed,
                crossing: CrossingPolicy::ByValue,
            }
    }));
    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::ExitReady {
                reuse: ReuseClass::Observed,
                crossing: CrossingPolicy::ByValue,
            }
    }));
    assert!(
        trace
            .events
            .iter()
            .all(|event| event.place.as_ref() == "win")
    );
}

#[test]
fn traced_fail_expression_emits_poison_exit() {
    let (resolution, trace) = eval_source_entry_with_args_with_source_id_traced(
        "fn build() { fail \"boom\" }",
        "trace-poison",
        "build",
        &[],
    )
    .expect("evaluate traced failure");

    let Resolution::Poison(error) = resolution else {
        panic!("expected poison resolution, got {resolution:?}");
    };
    assert_eq!(error.kind, EvalErrorKind::UserFail);
    assert_eq!(error.message.as_ref(), "boom");

    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::EnterExpr {
                kind: crate::EvalExprKind::Fail,
            }
    }));
    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::ExitPoison {
                kind: EvalErrorKind::UserFail,
                message: "boom".into(),
            }
    }));
}

#[test]
fn traced_runtime_calls_can_produce_pending_nodes() {
    let (resolution, trace) = eval_source_entry_at_place_with_args_with_source_id_traced(
        "fn build() { host_wait() }",
        "trace-pending",
        "build",
        &[],
        &TestRuntime,
        "win",
    )
    .expect("evaluate traced pending runtime call");

    let Resolution::Pending(pending) = resolution else {
        panic!("expected pending resolution, got {resolution:?}");
    };
    assert_eq!(
        pending.description.as_ref(),
        "waiting for remote host observation"
    );

    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::RegisterPending {
                pending: pending.clone(),
            }
    }));
    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::RuntimeCallFinishedPending {
                pending: pending.clone(),
            }
    }));
    assert!(trace.events.iter().any(|event| {
        event.kind
            == EvalEventKind::ExitPending {
                pending: pending.clone(),
            }
    }));
}

#[test]
fn session_can_resume_pending_runtime_call() {
    let mut session = EvalSession::new(
        "fn build() { host_wait() }",
        "session-pending",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );
    let queued_before_run = session.queued_work();
    assert_eq!(queued_before_run.len(), 1);
    assert_eq!(queued_before_run[0].kind, EvalWorkKind::StartEntry);
    assert_eq!(queued_before_run[0].node_id, EvalNodeId(0));
    assert_eq!(
        session.node_state(EvalNodeId(0)),
        Some(&EvalNodeState::Runnable {
            work: queued_before_run[0].id,
            pending: None,
        }),
    );

    let first = session.run().expect("first session run");
    let Resolution::Pending(pending) = first else {
        panic!("expected pending resolution, got {first:?}");
    };
    assert_eq!(session.pending_nodes(), vec![pending.clone()]);
    assert_eq!(
        session.node_state(EvalNodeId(0)),
        Some(&EvalNodeState::Pending(pending.clone()))
    );
    assert_eq!(
        session.node_state(EvalNodeId(1)),
        Some(&EvalNodeState::BlockedOnNode {
            dependency: EvalNodeId(2),
        })
    );
    assert_eq!(
        session.node_state(EvalNodeId(2)),
        Some(&EvalNodeState::Pending(pending.clone()))
    );
    assert_eq!(
        session.node_state(EvalNodeId(3)),
        Some(&EvalNodeState::Pending(pending.clone()))
    );

    session
        .resolve_pending(&pending, Value::string("windows"))
        .expect("resolve pending node");

    let second = session.run().expect("second session run");
    let Resolution::Ready(value) = second else {
        panic!("expected ready resolution, got {second:?}");
    };
    assert_eq!(value.rep, ValueRep::String("windows".into()));
    assert!(session.pending_nodes().is_empty());
    assert_eq!(
        session.node_state(EvalNodeId(0)),
        Some(&EvalNodeState::Ready(Value::string("windows")))
    );
    assert_eq!(
        session.node_state(EvalNodeId(1)),
        Some(&EvalNodeState::Ready(Value::string("windows")))
    );
    assert_eq!(
        session.node_state(EvalNodeId(pending.node_id)),
        Some(&EvalNodeState::Ready(Value::string("windows")))
    );
    assert!(
        session
            .trace()
            .events
            .iter()
            .any(|event| matches!(event.kind, EvalEventKind::RegisterPending { .. })),
        "session trace should retain the first suspension",
    );
}

#[test]
fn session_step_queues_root_expression_work_before_evaluating_it() {
    let mut session = EvalSession::new(
        "fn build() { host_wait() }",
        "session-step-root",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    let queued_before = session.queued_work();
    assert_eq!(queued_before.len(), 1);
    assert_eq!(queued_before[0].kind, EvalWorkKind::StartEntry);
    assert_eq!(queued_before[0].node_id, EvalNodeId(0));

    let first = session.step().expect("first session step");
    assert_eq!(first, EvalSessionStep::Progressed);
    assert_eq!(session.completed_work().len(), 1);
    assert_eq!(
        session.completed_work()[0].outcome,
        EvalCompletedWorkOutcome::Progressed
    );
    assert_eq!(
        session.completed_work()[0].work.kind,
        EvalWorkKind::StartEntry
    );

    let queued_after = session.queued_work();
    assert_eq!(queued_after.len(), 1);
    assert_eq!(queued_after[0].kind, EvalWorkKind::EvaluateExpr);
    assert_eq!(queued_after[0].node_id, EvalNodeId(1));
    assert_eq!(
        session.node_state(EvalNodeId(0)),
        Some(&EvalNodeState::Runnable {
            work: queued_after[0].id,
            pending: None,
        }),
    );
    assert_eq!(
        session.node_state(EvalNodeId(1)),
        Some(&EvalNodeState::Runnable {
            work: queued_after[0].id,
            pending: None,
        }),
    );

    let second = session.step().expect("second session step");
    assert_eq!(second, EvalSessionStep::Progressed);
    let child_work = session.queued_work();
    assert_eq!(child_work.len(), 1);
    assert_eq!(child_work[0].kind, EvalWorkKind::EvaluateExpr);
    assert_eq!(child_work[0].node_id, EvalNodeId(2));
    assert_eq!(session.completed_work().len(), 2);
    assert_eq!(
        session.completed_work()[1].work.kind,
        EvalWorkKind::EvaluateExpr
    );
    assert_eq!(
        session.completed_work()[1].outcome,
        EvalCompletedWorkOutcome::BlockedOnNode(EvalNodeId(2)),
    );
    assert!(
        session
            .trace()
            .events
            .iter()
            .any(|event| event.work_id == Some(queued_after[0].id))
    );
    let third = session.step().expect("third session step");
    let EvalSessionStep::Pending(pending) = third else {
        panic!("expected pending step, got {third:?}");
    };
    assert_eq!(
        pending.description.as_ref(),
        "waiting for remote host observation"
    );
    assert!(session.queued_work().is_empty());
    assert_eq!(session.completed_work().len(), 3);
    assert_eq!(
        session.completed_work()[2].work.kind,
        EvalWorkKind::EvaluateExpr
    );
    assert_eq!(
        session.completed_work()[2].outcome,
        EvalCompletedWorkOutcome::Pending(pending.clone()),
    );
    assert!(
        session
            .trace()
            .events
            .iter()
            .any(|event| event.work_id == Some(child_work[0].id))
    );
}

#[test]
fn session_queues_child_expression_work_and_resumes_parent_block() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls: host_os_calls.clone(),
        host_wait_calls: host_wait_calls.clone(),
    };
    let mut session = EvalSession::new(
        "fn build() { let os = host_os(); os }",
        "session-child-queue",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );

    let root_work = session.queued_work();
    assert_eq!(root_work.len(), 1);
    assert_eq!(root_work[0].kind, EvalWorkKind::EvaluateExpr);
    let root_node = root_work[0].node_id;

    assert_eq!(
        session.step().expect("root expression step"),
        EvalSessionStep::Progressed
    );
    let child_work = session.queued_work();
    assert_eq!(child_work.len(), 1);
    assert_eq!(child_work[0].kind, EvalWorkKind::EvaluateExpr);
    let child_node = child_work[0].node_id;
    assert_ne!(child_node, root_node);
    assert_eq!(
        session.node_state(root_node),
        Some(&EvalNodeState::BlockedOnNode {
            dependency: child_node,
        }),
    );
    assert_eq!(
        session.completed_work()[1].outcome,
        EvalCompletedWorkOutcome::BlockedOnNode(child_node),
    );

    assert_eq!(
        session.step().expect("child expression step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(host_os_calls.load(Ordering::SeqCst), 1);
    assert_eq!(host_wait_calls.load(Ordering::SeqCst), 0);
    let resumed = session.queued_work();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].kind, EvalWorkKind::ResumePending);
    assert_eq!(resumed[0].node_id, root_node);
    let Some(EvalNodeState::Ready(value)) = session.node_state(child_node) else {
        panic!("expected child node to be ready");
    };
    assert_eq!(value.rep, ValueRep::String("windows".into()));

    let mut progressed_after_child = 0;
    let value = loop {
        match session.step().expect("finish queued parent work") {
            EvalSessionStep::Progressed => {
                progressed_after_child += 1;
            }
            EvalSessionStep::Ready(value) => break value,
            other => panic!("expected progress or ready, got {other:?}"),
        }
    };
    assert!(progressed_after_child >= 1);
    assert_eq!(value.rep, ValueRep::String("windows".into()));
    assert_eq!(host_os_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn session_exposes_dependency_edges_and_blocked_node_explanations() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls,
        host_wait_calls,
    };
    let mut session = EvalSession::new(
        "fn build() { let os = host_os(); os }",
        "session-explain",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    let root_work = session.queued_work();
    let root_node = root_work[0].node_id;

    assert_eq!(
        session.step().expect("root expression step"),
        EvalSessionStep::Progressed
    );
    let child_work = session.queued_work();
    let child_node = child_work[0].node_id;

    assert!(session.dependency_edges().contains(&EvalDependencyEdge {
        parent: root_node,
        child: child_node,
    }));

    let explanation = session.explain_node(root_node);
    assert_eq!(explanation.chain, vec![root_node, child_node]);
    assert_eq!(
        explanation.terminal,
        EvalExplainTerminal::Runnable {
            node_id: child_node,
            work: child_work[0].id,
            pending: None,
        },
    );
}

#[test]
fn session_retains_node_records_with_kind_parent_and_source() {
    let mut session = EvalSession::new(
        "fn build(value: String) { value }",
        "session-node-records",
        "build",
        vec![Value::string("windows")],
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    let records = session.node_records();
    assert!(records.iter().any(|record| {
        record.node_id == EvalNodeId(0)
            && record.parent_id.is_none()
            && record.kind == EvalNodeKind::SyntheticRoot
    }));
    assert!(records.iter().any(|record| {
        record.node_id == EvalNodeId(1)
            && record.parent_id.is_none()
            && record.kind
                == EvalNodeKind::ArgumentBinding {
                    label: "value".into(),
                }
            && record
                .source
                .as_ref()
                .map(|source| source.source_id.as_str())
                == Some("session-node-records")
    }));
    assert!(records.iter().any(|record| {
        record.node_id == EvalNodeId(2)
            && record.parent_id == Some(EvalNodeId(0))
            && record.kind
                == EvalNodeKind::Expr {
                    kind: crate::EvalExprKind::Block,
                }
            && record
                .source
                .as_ref()
                .map(|source| source.source_id.as_str())
                == Some("session-node-records")
    }));
}

#[test]
fn session_graph_snapshot_tracks_node_and_work_lifecycle() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls,
        host_wait_calls,
    };
    let mut session = EvalSession::new(
        "fn build() { let os = host_os(); os }",
        "session-graph-snapshot",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root expression step"),
        EvalSessionStep::Progressed
    );

    let snapshot = session.graph_snapshot();
    assert!(snapshot.pending.is_empty());
    assert!(snapshot.completed.is_none());

    let root_node = snapshot
        .nodes
        .iter()
        .find(|node| {
            node.record.as_ref().is_some_and(|record| {
                record.kind
                    == EvalNodeKind::Expr {
                        kind: crate::EvalExprKind::Block,
                    }
                    && record.parent_id == Some(EvalNodeId(0))
            })
        })
        .expect("root block node should be recorded");
    let child_node = snapshot
        .nodes
        .iter()
        .find(|node| {
            node.record.as_ref().is_some_and(|record| {
                record.kind
                    == EvalNodeKind::Expr {
                        kind: crate::EvalExprKind::Call,
                    }
                    && record.parent_id == Some(root_node.node_id)
            })
        })
        .expect("child call node should be recorded");

    assert_eq!(
        root_node.state,
        Some(EvalNodeState::BlockedOnNode {
            dependency: child_node.node_id,
        }),
    );
    assert_eq!(root_node.dependencies, vec![child_node.node_id]);
    assert_eq!(child_node.dependents, vec![root_node.node_id]);
    assert_eq!(
        child_node.state,
        Some(EvalNodeState::Runnable {
            work: snapshot.work[2].work.id,
            pending: None,
        }),
    );

    assert_eq!(snapshot.work.len(), 3);
    assert_eq!(snapshot.work[0].work.kind, EvalWorkKind::StartEntry);
    assert_eq!(
        snapshot.work[0].state,
        EvalWorkState::Completed(EvalCompletedWorkOutcome::Progressed)
    );
    assert_eq!(snapshot.work[1].work.kind, EvalWorkKind::EvaluateExpr);
    assert_eq!(
        snapshot.work[1].state,
        EvalWorkState::Completed(EvalCompletedWorkOutcome::BlockedOnNode(child_node.node_id)),
    );
    assert_eq!(snapshot.work[2].work.kind, EvalWorkKind::EvaluateExpr);
    assert_eq!(snapshot.work[2].state, EvalWorkState::Queued);
    assert_eq!(snapshot.work[2].work.node_id, child_node.node_id);
}

#[test]
fn session_can_expose_multiple_runnable_array_children_at_once() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls,
        host_wait_calls,
    };
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_wait()] }",
        "session-array-frontier",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root array step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array expression step"),
        EvalSessionStep::Progressed
    );

    let queued = session.queued_work();
    assert_eq!(queued.len(), 2);
    assert!(
        queued
            .iter()
            .all(|work| work.kind == EvalWorkKind::EvaluateExpr)
    );

    let snapshot = session.graph_snapshot();
    let array_node = snapshot
        .nodes
        .iter()
        .find(|node| {
            node.record.as_ref().is_some_and(|record| {
                record.kind
                    == EvalNodeKind::Expr {
                        kind: crate::EvalExprKind::Array,
                    }
            })
        })
        .expect("array node should be recorded");

    let child_call_nodes = snapshot
        .nodes
        .iter()
        .filter(|node| {
            node.record.as_ref().is_some_and(|record| {
                record.parent_id == Some(array_node.node_id)
                    && record.kind
                        == EvalNodeKind::Expr {
                            kind: crate::EvalExprKind::Call,
                        }
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(child_call_nodes.len(), 2);
    assert_eq!(
        array_node.dependencies,
        child_call_nodes
            .iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        array_node.state,
        Some(EvalNodeState::BlockedOnNode {
            dependency: child_call_nodes[0].node_id,
        }),
    );
    assert!(
        child_call_nodes
            .iter()
            .all(|node| matches!(node.state, Some(EvalNodeState::Runnable { .. })))
    );
}

#[test]
fn session_can_expose_multiple_runnable_call_arguments_at_once() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls,
        host_wait_calls,
    };
    let mut session = EvalSession::new(
        "fn build() { pair(host_os(), host_wait()) }",
        "session-call-frontier",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("pair call step"),
        EvalSessionStep::Progressed
    );

    let queued = session.queued_work();
    assert_eq!(queued.len(), 2);
    assert!(
        queued
            .iter()
            .all(|work| work.kind == EvalWorkKind::EvaluateExpr)
    );

    let snapshot = session.graph_snapshot();
    let call_node = snapshot
        .nodes
        .iter()
        .find(|node| {
            node.record.as_ref().is_some_and(|record| {
                record.kind
                    == EvalNodeKind::Expr {
                        kind: crate::EvalExprKind::Call,
                    }
            }) && node.dependencies.len() == 2
        })
        .expect("pair call node should be recorded");
    let arg_call_nodes = snapshot
        .nodes
        .iter()
        .filter(|node| {
            node.record.as_ref().is_some_and(|record| {
                record.parent_id == Some(call_node.node_id)
                    && record.kind
                        == EvalNodeKind::Expr {
                            kind: crate::EvalExprKind::Call,
                        }
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(arg_call_nodes.len(), 2);
    assert_eq!(
        call_node.dependencies,
        arg_call_nodes
            .iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        call_node.state,
        Some(EvalNodeState::BlockedOnNode {
            dependency: arg_call_nodes[0].node_id,
        }),
    );
    assert!(
        arg_call_nodes
            .iter()
            .all(|node| matches!(node.state, Some(EvalNodeState::Runnable { .. })))
    );
}

#[test]
fn session_can_expose_multiple_runnable_struct_fields_at_once() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls,
        host_wait_calls,
    };
    let mut session = EvalSession::new(
        "fn build() { Pair { left: host_os(), right: host_wait() } }",
        "session-struct-frontier",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("struct literal step"),
        EvalSessionStep::Progressed
    );

    let queued = session.queued_work();
    assert_eq!(queued.len(), 2);
    assert!(
        queued
            .iter()
            .all(|work| work.kind == EvalWorkKind::EvaluateExpr)
    );

    let snapshot = session.graph_snapshot();
    let struct_node = snapshot
        .nodes
        .iter()
        .find(|node| {
            node.record.as_ref().is_some_and(|record| {
                record.kind
                    == EvalNodeKind::Expr {
                        kind: crate::EvalExprKind::StructLiteral,
                    }
            }) && node.dependencies.len() == 2
        })
        .expect("struct node should be recorded");
    let field_call_nodes = snapshot
        .nodes
        .iter()
        .filter(|node| {
            node.record.as_ref().is_some_and(|record| {
                record.parent_id == Some(struct_node.node_id)
                    && record.kind
                        == EvalNodeKind::Expr {
                            kind: crate::EvalExprKind::Call,
                        }
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(field_call_nodes.len(), 2);
    assert_eq!(
        struct_node.dependencies,
        field_call_nodes
            .iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        struct_node.state,
        Some(EvalNodeState::BlockedOnNode {
            dependency: field_call_nodes[0].node_id,
        }),
    );
    assert!(
        field_call_nodes
            .iter()
            .all(|node| matches!(node.state, Some(EvalNodeState::Runnable { .. })))
    );
}

#[test]
fn session_can_step_back_to_a_prior_scheduler_state() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls,
        host_wait_calls,
    };
    let mut session = EvalSession::new(
        "fn build() { let os = host_os(); os }",
        "session-step-back",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    let root_work = session.queued_work();
    let root_node = root_work[0].node_id;

    assert_eq!(
        session.step().expect("root expression step"),
        EvalSessionStep::Progressed
    );
    let child_work = session.queued_work();
    let child_node = child_work[0].node_id;
    assert_eq!(
        session.node_state(root_node),
        Some(&EvalNodeState::BlockedOnNode {
            dependency: child_node,
        }),
    );
    assert!(session.can_step_back());

    assert!(session.step_back());
    let queued_after_rewind = session.queued_work();
    assert_eq!(queued_after_rewind.len(), 1);
    assert_eq!(queued_after_rewind[0].kind, EvalWorkKind::EvaluateExpr);
    assert_eq!(queued_after_rewind[0].node_id, root_node);
    assert_eq!(
        session.node_state(root_node),
        Some(&EvalNodeState::Runnable {
            work: queued_after_rewind[0].id,
            pending: None,
        }),
    );
    assert_eq!(
        session.dependency_edges(),
        &[EvalDependencyEdge {
            parent: EvalNodeId(0),
            child: root_node,
        }]
    );
    assert_eq!(session.completed_work().len(), 1);

    assert!(session.step_back());
    let queued_at_start = session.queued_work();
    assert_eq!(queued_at_start.len(), 1);
    assert_eq!(queued_at_start[0].kind, EvalWorkKind::StartEntry);
    assert_eq!(queued_at_start[0].node_id, EvalNodeId(0));
    assert!(session.completed_work().is_empty());
    assert!(!session.step_back());
}

#[test]
fn session_checkpoint_can_restore_graph_and_work_state() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls,
        host_wait_calls,
    };
    let mut session = EvalSession::new(
        "fn build() { let os = host_os(); os }",
        "session-checkpoint",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    let checkpoint = session.checkpoint();
    let expected_graph = session.graph_snapshot();
    let expected_trace = session.trace().clone();

    assert_eq!(
        session.step().expect("root expression step"),
        EvalSessionStep::Progressed
    );
    assert_ne!(session.graph_snapshot(), expected_graph);

    session.restore(checkpoint);
    assert_eq!(session.graph_snapshot(), expected_graph);
    assert_eq!(session.trace(), &expected_trace);
}

#[test]
fn prepared_work_can_execute_on_a_scoped_worker_thread() {
    let runtime = TestRuntime;
    let mut session = EvalSession::new(
        "fn build() { host_wait() }",
        "prepared-worker",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    let prepared = session
        .prepare_step()
        .expect("start-entry work should exist");
    let completion = thread::scope(|scope| {
        scope
            .spawn(move || prepared.execute().expect("prepared work should execute"))
            .join()
            .expect("worker thread should not panic")
    });
    assert_eq!(completion.work().kind, EvalWorkKind::StartEntry);
    assert_eq!(*completion.outcome(), EvalCompletedWorkOutcome::Progressed);

    let step = session
        .apply_completion(completion)
        .expect("completion should apply");
    assert_eq!(step, EvalSessionStep::Progressed);
    assert_eq!(session.completed_work().len(), 1);
    assert_eq!(
        session.completed_work()[0].work.kind,
        EvalWorkKind::StartEntry
    );
    let queued = session.queued_work();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].kind, EvalWorkKind::EvaluateExpr);
}

#[test]
fn sibling_prepared_block_work_survives_checkpoint_restore_and_resolves_handles() {
    let mut session = EvalSession::new(
        "fn build() { [{ let os = host_os(); os }, { let os = host_os(); os }] }",
        "prepared-checkpoint-blocks",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(session.program_parse_count(), 1);
    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root expression step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("first leased sibling");
    let second = session.prepare_step().expect("second leased sibling");
    let checkpoint = session.checkpoint();

    let (first_completion, second_completion) = thread::scope(|scope| {
        let first_join = scope.spawn(move || first.execute().expect("first work should execute"));
        let second_join =
            scope.spawn(move || second.execute().expect("second work should execute"));
        (
            first_join.join().expect("first worker should not panic"),
            second_join.join().expect("second worker should not panic"),
        )
    });

    session.restore(checkpoint);
    assert_eq!(session.program_parse_count(), 1);

    assert_eq!(
        session
            .apply_completion(first_completion)
            .expect("first completion should apply"),
        EvalSessionStep::Progressed,
    );
    assert_eq!(
        session
            .apply_completion(second_completion)
            .expect("second completion should apply"),
        EvalSessionStep::Progressed,
    );

    let resolution = session.run().expect("resume queued parent work");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    let ValueRep::Array(items) = value.rep else {
        panic!("expected array value");
    };
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|item| item.rep == ValueRep::String("windows".into()))
    );
    assert_eq!(session.program_parse_count(), 1);
}

#[test]
fn stale_prepared_work_completion_is_rejected() {
    let runtime = TestRuntime;
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "stale-prepared-work",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let completion = session
        .prepare_step()
        .expect("leased sibling should exist")
        .execute()
        .expect("prepared work should execute");
    assert!(completion.lease_sequence() > 0);

    assert!(session.step_back());

    let error = session
        .apply_completion(completion)
        .expect_err("stale completion should be rejected");
    assert_eq!(error.kind, EvalErrorKind::Internal);
    assert!(
        error.message.contains("stale eval work completion"),
        "unexpected error message: {}",
        error.message
    );
}

#[test]
fn old_completion_is_rejected_after_step_back_and_re_lease() {
    let mut session = EvalSession::new(
        "fn build() { host_os() }",
        "prepared-release-stale",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("leased root work");
    let first_work = first.work().clone();
    let old_completion = first.execute().expect("old completion should execute");

    assert!(session.step_back());

    let released = session.prepare_step().expect("re-leased root work");
    assert_eq!(released.work().node_id, first_work.node_id);
    assert!(
        released.lease_sequence() > old_completion.lease_sequence(),
        "re-leased work must receive a fresh lease identity",
    );

    let error = session
        .apply_completion(old_completion)
        .expect_err("old completion should be stale after re-lease");
    assert_eq!(error.kind, EvalErrorKind::Internal);
    assert!(
        error.message.contains("stale eval work completion"),
        "unexpected error message: {}",
        error.message
    );

    assert_eq!(
        session
            .apply_completion(released.execute().expect("fresh completion"))
            .expect("fresh completion should apply"),
        EvalSessionStep::Progressed,
    );
}

#[test]
fn preparing_twice_never_returns_the_same_work_twice() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "prepared-distinct-work",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(session.queued_work().len(), 2);
    assert!(session.in_flight_work().is_empty());

    let first = session.prepare_step().expect("first leased sibling");
    let first_work = first.work().clone();
    assert_eq!(session.queued_work().len(), 1);
    assert_eq!(session.in_flight_work(), vec![first_work.clone()]);
    assert!(session.work_records().iter().any(|record| {
        record.work.id == first_work.id && record.state == EvalWorkState::InFlight
    }));

    let second = session.prepare_step().expect("second leased sibling");
    let second_work = second.work().clone();
    assert_ne!(first_work.id, second_work.id);
    assert!(session.queued_work().is_empty());
    assert_eq!(session.in_flight_work().len(), 2);
    assert_eq!(
        session
            .graph_snapshot()
            .work
            .iter()
            .filter(|record| record.state == EvalWorkState::InFlight)
            .count(),
        2
    );
    assert!(session.prepare_step().is_none());
}

#[test]
fn run_returns_waiting_when_only_in_flight_work_remains() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "prepared-run-waiting",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let _first = session.prepare_step().expect("first leased sibling");
    let _second = session.prepare_step().expect("second leased sibling");

    assert!(session.queued_work().is_empty());
    assert!(session.pending_nodes().is_empty());
    assert_eq!(
        session.step().expect("waiting session step"),
        EvalSessionStep::WaitingOnInFlight
    );
    assert_eq!(session.run().expect("waiting run"), Resolution::Waiting);
}

#[test]
fn sibling_prepared_work_can_execute_on_threads_and_preserve_live_state() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "prepared-sibling-threads",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("first leased sibling");
    let first_work = first.work().clone();
    let second = session.prepare_step().expect("second leased sibling");
    let second_work = second.work().clone();

    let (first_completion, second_completion) = thread::scope(|scope| {
        let first_join = scope.spawn(move || first.execute().expect("first work should execute"));
        let second_join =
            scope.spawn(move || second.execute().expect("second work should execute"));
        (
            first_join.join().expect("first worker should not panic"),
            second_join.join().expect("second worker should not panic"),
        )
    });

    assert_eq!(
        session
            .apply_completion(first_completion)
            .expect("first completion should apply"),
        EvalSessionStep::Progressed,
    );
    assert_eq!(session.in_flight_work(), vec![second_work.clone()]);
    let queued = session.queued_work();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].kind, EvalWorkKind::ResumePending);
    assert_ne!(queued[0].node_id, first_work.node_id);

    assert_eq!(
        session
            .apply_completion(second_completion)
            .expect("second completion should apply"),
        EvalSessionStep::Progressed,
    );
    assert!(session.in_flight_work().is_empty());
    assert_eq!(session.queued_work().len(), 1);
    assert_eq!(session.queued_work()[0].kind, EvalWorkKind::ResumePending);
    assert_ne!(session.queued_work()[0].id, first_work.id);

    let resolution = session.run().expect("resume queued parent work");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    let ValueRep::Array(items) = value.rep else {
        panic!("expected array value");
    };
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|item| item.rep == ValueRep::String("windows".into()))
    );
}

#[test]
fn apply_completion_is_transactional_when_delta_merge_fails() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "prepared-transactional-merge",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("first leased sibling");
    let before_graph = session.graph_snapshot();
    let before_trace = session.trace().clone();
    let before_completed = session.completed_work().to_vec();
    let before_work = session.work_records();
    let duplicate = session.first_queued_work_for_testing();

    let mut completion = first.execute().expect("completion should execute");
    completion.inject_duplicate_queued_work(duplicate);

    let error = session
        .apply_completion(completion)
        .expect_err("corrupted delta merge should fail");
    assert_eq!(error.kind, EvalErrorKind::Internal);
    assert!(
        error
            .message
            .contains("already exists during completion merge"),
        "unexpected error message: {}",
        error.message
    );
    assert_eq!(session.graph_snapshot(), before_graph);
    assert_eq!(session.trace(), &before_trace);
    assert_eq!(session.completed_work(), before_completed.as_slice());
    assert_eq!(session.work_records(), before_work);
}

#[test]
fn out_of_order_sibling_completion_is_buffered_until_earlier_lease_commits() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "prepared-out-of-order",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("first leased sibling");
    let first_work = first.work().clone();
    let second = session.prepare_step().expect("second leased sibling");
    let second_work = second.work().clone();
    let first_completion = first.execute().expect("first completion");
    let second_completion = second.execute().expect("second completion");
    let second_wave_id = second_completion.wave_id();
    let second_lease_sequence = second_completion.lease_sequence();
    let second_outcome = second_completion.outcome().clone();

    assert_eq!(
        session
            .apply_completion(second_completion)
            .expect("out-of-order completion should buffer"),
        EvalSessionStep::WaitingOnInFlight,
    );
    assert_eq!(session.in_flight_work(), vec![first_work.clone()]);
    assert!(session.queued_work().is_empty());
    assert_eq!(session.buffered_work().len(), 1);
    assert_eq!(session.buffered_work()[0].wave_id, second_wave_id);
    assert_eq!(
        session.buffered_work()[0].lease_sequence,
        second_lease_sequence
    );
    assert_eq!(session.buffered_work()[0].work, second_work.clone());
    assert_eq!(session.buffered_work()[0].outcome, second_outcome.clone());
    let active_wave = session
        .active_wave()
        .expect("buffered siblings should retain a wave");
    assert_eq!(active_wave.wave_id, second_wave_id);
    assert_eq!(active_wave.work.len(), 2);
    assert_eq!(active_wave.work[0].work.id, first_work.id);
    assert_eq!(active_wave.work[0].state, EvalLeasedWorkState::InFlight);
    assert_eq!(active_wave.work[1].work.id, second_work.id);
    assert_eq!(
        active_wave.work[1].state,
        EvalLeasedWorkState::Buffered(second_outcome.clone()),
    );
    assert!(session.work_records().iter().any(|record| {
        record.work.id == second_work.id
            && record.state == EvalWorkState::Buffered(second_outcome.clone())
    }));
    assert_eq!(
        session.explain_node(second_work.node_id).terminal,
        EvalExplainTerminal::Buffered {
            node_id: second_work.node_id,
            work: second_work.id,
            pending: None,
            lease_sequence: second_lease_sequence,
            outcome: second_outcome.clone(),
        },
    );
    assert_eq!(
        session.run().expect("session should still be waiting"),
        Resolution::Waiting
    );
    assert!(
        session
            .completed_work()
            .iter()
            .all(|completed| completed.work.id != second_work.id),
        "buffered completion must not be visible as committed work yet",
    );

    assert_eq!(
        session
            .apply_completion(first_completion)
            .expect("first completion should commit and drain"),
        EvalSessionStep::Progressed,
    );
    assert!(session.active_wave().is_none());
    assert!(session.buffered_work().is_empty());
    assert!(session.in_flight_work().is_empty());
    assert_eq!(
        session
            .completed_work()
            .iter()
            .rev()
            .take(2)
            .map(|completed| completed.work.id)
            .collect::<Vec<_>>(),
        vec![second_work.id, first_work.id],
    );

    let resolution = session.run().expect("resume queued parent work");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    let ValueRep::Array(items) = value.rep else {
        panic!("expected array value");
    };
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|item| item.rep == ValueRep::String("windows".into()))
    );
}

#[test]
fn buffered_sibling_completions_drain_in_lease_order_once_gap_closes() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os(), host_os()] }",
        "prepared-buffered-drain",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("first leased sibling");
    let first_work = first.work().clone();
    let second = session.prepare_step().expect("second leased sibling");
    let second_work = second.work().clone();
    let third = session.prepare_step().expect("third leased sibling");
    let third_work = third.work().clone();

    let first_completion = first.execute().expect("first completion");
    let second_completion = second.execute().expect("second completion");
    let third_completion = third.execute().expect("third completion");
    let second_lease_sequence = second_completion.lease_sequence();
    let third_lease_sequence = third_completion.lease_sequence();

    assert_eq!(
        session
            .apply_completion(third_completion)
            .expect("third completion should buffer"),
        EvalSessionStep::WaitingOnInFlight,
    );
    assert_eq!(
        session
            .apply_completion(second_completion)
            .expect("second completion should also buffer"),
        EvalSessionStep::WaitingOnInFlight,
    );
    assert_eq!(session.in_flight_work(), vec![first_work.clone()]);
    assert_eq!(
        session
            .buffered_work()
            .iter()
            .map(|buffered| buffered.lease_sequence)
            .collect::<Vec<_>>(),
        vec![second_lease_sequence, third_lease_sequence],
    );
    assert!(session.completed_work().iter().all(|completed| {
        completed.work.id != second_work.id && completed.work.id != third_work.id
    }));

    assert_eq!(
        session
            .apply_completion(first_completion)
            .expect("first completion should drain buffered siblings"),
        EvalSessionStep::Progressed,
    );
    assert!(session.buffered_work().is_empty());
    assert_eq!(
        session
            .completed_work()
            .iter()
            .rev()
            .take(3)
            .map(|completed| completed.work.id)
            .collect::<Vec<_>>(),
        vec![third_work.id, second_work.id, first_work.id],
    );

    let resolution = session.run().expect("resume queued parent work");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    let ValueRep::Array(items) = value.rep else {
        panic!("expected array value");
    };
    assert_eq!(items.len(), 3);
    assert!(
        items
            .iter()
            .all(|item| item.rep == ValueRep::String("windows".into()))
    );
}

#[test]
fn graph_snapshot_reflects_buffered_work() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "prepared-buffered-graph",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("first leased sibling");
    let second = session.prepare_step().expect("second leased sibling");
    let second_work = second.work().clone();
    let second_outcome = second.execute().expect("second completion");
    let second_wave_id = second_outcome.wave_id();
    let second_lease_sequence = second_outcome.lease_sequence();
    let second_result = second_outcome.outcome().clone();

    assert_eq!(
        session
            .apply_completion(second_outcome)
            .expect("second completion should buffer"),
        EvalSessionStep::WaitingOnInFlight,
    );

    let snapshot = session.graph_snapshot();
    let active_wave = snapshot
        .active_wave
        .clone()
        .expect("snapshot should retain the buffered wave");
    assert_eq!(active_wave.wave_id, second_wave_id);
    assert_eq!(active_wave.work.len(), 2);
    assert!(snapshot.work.iter().any(|record| {
        record.work.id == second_work.id
            && record.state == EvalWorkState::Buffered(second_result.clone())
    }));
    assert_eq!(
        session.buffered_work(),
        vec![crate::EvalBufferedWork {
            wave_id: second_wave_id,
            lease_sequence: second_lease_sequence,
            work: second_work,
            outcome: second_result,
        }],
    );

    let _first_completion = first.execute().expect("first completion");
}

#[test]
fn run_parallel_executes_sibling_frontier_on_worker_threads() {
    let active_calls = Arc::new(AtomicUsize::new(0));
    let max_calls = Arc::new(AtomicUsize::new(0));
    let runtime = ParallelObserveRuntime {
        active_calls,
        max_calls: max_calls.clone(),
    };
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "parallel-ready",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    let resolution = session
        .run_parallel(2)
        .expect("parallel run should succeed");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    let ValueRep::Array(items) = value.rep else {
        panic!("expected array value");
    };
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|item| item.rep == ValueRep::String("windows".into()))
    );
    assert!(
        max_calls.load(Ordering::SeqCst) >= 2,
        "parallel driver should overlap sibling runtime calls",
    );
}

#[test]
fn run_parallel_commits_out_of_order_worker_results_in_lease_order() {
    let mut session = EvalSession::new(
        "fn build() { [slow_value(), fast_value()] }",
        "parallel-out-of-order",
        "build",
        Vec::new(),
        &StaggeredRuntime,
        "win",
    );

    let resolution = session
        .run_parallel(2)
        .expect("parallel run should succeed");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    let ValueRep::Array(items) = value.rep else {
        panic!("expected array value");
    };
    assert_eq!(
        items
            .iter()
            .map(|item| match &item.rep {
                ValueRep::String(value) => value.as_ref().to_string(),
                other => panic!("expected string item, got {other:?}"),
            })
            .collect::<Vec<_>>(),
        vec!["slow".to_string(), "fast".to_string()],
    );
    assert_eq!(
        session
            .completed_work()
            .iter()
            .filter_map(|completed| match &completed.outcome {
                EvalCompletedWorkOutcome::Ready(value) => match &value.rep {
                    ValueRep::String(value) => Some(value.to_string()),
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec!["slow".to_string(), "fast".to_string()],
    );
}

#[test]
fn run_parallel_returns_pending_when_external_work_suspends() {
    let mut session = EvalSession::new(
        "fn build() { host_wait() }",
        "parallel-pending",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    let resolution = session
        .run_parallel(2)
        .expect("parallel run should succeed");
    let Resolution::Pending(pending) = resolution else {
        panic!("expected pending resolution, got {resolution:?}");
    };
    assert_eq!(
        pending.description.as_ref(),
        "waiting for remote host observation"
    );
    assert_eq!(session.pending_nodes(), vec![pending]);
}

#[test]
fn run_parallel_returns_poison_when_evaluation_fails() {
    let mut session = EvalSession::new(
        "fn build() { fail \"boom\" }",
        "parallel-poison",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    let resolution = session
        .run_parallel(2)
        .expect("parallel run should succeed");
    let Resolution::Poison(error) = resolution else {
        panic!("expected poison resolution, got {resolution:?}");
    };
    assert_eq!(error.kind, EvalErrorKind::UserFail);
    assert_eq!(error.message.as_ref(), "boom");
}

#[test]
fn run_parallel_returns_waiting_when_external_in_flight_work_blocks_commit() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "parallel-waiting",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("external lease should exist");
    let first_work = first.work().clone();

    assert_eq!(
        session
            .run_parallel(1)
            .expect("parallel driver should wait"),
        Resolution::Waiting,
    );
    assert_eq!(session.in_flight_work(), vec![first_work]);
    assert_eq!(session.buffered_work().len(), 1);
    let active_wave = session
        .active_wave()
        .expect("waiting session should expose its wave");
    assert_eq!(active_wave.work.len(), 2);
    assert!(matches!(
        session
            .explain_node(session.buffered_work()[0].work.node_id)
            .terminal,
        EvalExplainTerminal::Buffered { .. }
    ));
}

#[test]
fn run_parallel_leaves_wave_uncommitted_when_one_worker_returns_error() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "parallel-error-wave",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );
    session.set_branch_context(EvalBranchContext::new("__test_worker_error__"));

    let before_graph = session.graph_snapshot();
    let before_work = session.work_records();
    let before_completed = session.completed_work().to_vec();
    let before_trace = session.trace().clone();
    let before_active_wave = session.active_wave();
    let before_in_flight = session.in_flight_work();
    let before_buffered = session.buffered_work();

    let error = session
        .run_parallel(2)
        .expect_err("wave error should surface");
    assert_eq!(error.kind, EvalErrorKind::Internal);
    assert_eq!(error.message.as_ref(), "injected test worker error");
    assert_eq!(session.graph_snapshot(), before_graph);
    assert_eq!(session.work_records(), before_work);
    assert_eq!(session.completed_work(), before_completed.as_slice());
    assert_eq!(session.trace(), &before_trace);
    assert_eq!(session.active_wave(), before_active_wave);
    assert_eq!(session.in_flight_work(), before_in_flight);
    assert_eq!(session.buffered_work(), before_buffered);
}

#[test]
fn run_parallel_surfaces_worker_panics_instead_of_waiting_forever() {
    let mut session = EvalSession::new(
        "fn build() { [panic_now(), host_os()] }",
        "parallel-panic",
        "build",
        Vec::new(),
        &PanicRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let before_graph = session.graph_snapshot();
    let before_work = session.work_records();
    let before_completed = session.completed_work().to_vec();
    let before_trace = session.trace().clone();
    let before_active_wave = session.active_wave();
    let before_in_flight = session.in_flight_work();
    let before_buffered = session.buffered_work();

    let panic = catch_unwind(AssertUnwindSafe(|| session.run_parallel(2)))
        .expect_err("worker panic should unwind through run_parallel");
    let message = panic
        .downcast_ref::<&'static str>()
        .map(|message| (*message).to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .expect("panic payload should be string-like");
    assert!(
        message.contains("panic runtime hook exploded"),
        "unexpected panic payload: {message}",
    );
    assert_eq!(session.graph_snapshot(), before_graph);
    assert_eq!(session.work_records(), before_work);
    assert_eq!(session.completed_work(), before_completed.as_slice());
    assert_eq!(session.trace(), &before_trace);
    assert_eq!(session.active_wave(), before_active_wave);
    assert_eq!(session.in_flight_work(), before_in_flight);
    assert_eq!(session.buffered_work(), before_buffered);
}

#[test]
fn failed_wave_completions_do_not_become_usable_on_retry() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "parallel-retry",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );
    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );
    session.set_branch_context(EvalBranchContext::new("__test_worker_error__"));
    let before_graph = session.graph_snapshot();
    let before_work = session.work_records();
    let before_completed = session.completed_work().to_vec();

    let error = session
        .run_parallel(2)
        .expect_err("first wave should fail before commit");
    assert_eq!(error.kind, EvalErrorKind::Internal);
    assert_eq!(error.message.as_ref(), "injected test worker error");
    assert_eq!(session.graph_snapshot(), before_graph);
    assert_eq!(session.work_records(), before_work);
    assert_eq!(session.completed_work(), before_completed.as_slice());
    assert!(session.in_flight_work().is_empty());
    assert!(session.buffered_work().is_empty());
    assert!(session.active_wave().is_none());
    session.set_branch_context(EvalBranchContext::new("retry-ok"));

    let resolution = session
        .run_parallel(2)
        .expect("retry should lease fresh work and succeed");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution on retry, got {resolution:?}");
    };
    let ValueRep::Array(items) = value.rep else {
        panic!("expected array value");
    };
    assert_eq!(
        items
            .iter()
            .map(|item| match &item.rep {
                ValueRep::String(value) => value.as_ref().to_string(),
                other => panic!("expected string item, got {other:?}"),
            })
            .collect::<Vec<_>>(),
        vec!["windows".to_string(), "windows".to_string()],
    );
}

#[test]
fn branch_context_survives_checkpoint_restore() {
    let mut session = EvalSession::new(
        "fn build() { host_os() }",
        "branch-context-checkpoint",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );
    let original = EvalBranchContext::new("alpha");
    session.set_branch_context(original.clone());
    let checkpoint = session.checkpoint();

    session.set_branch_context(EvalBranchContext::new("beta"));
    assert_ne!(session.branch_context(), &original);

    session.restore(checkpoint);
    assert_eq!(session.branch_context(), &original);
    assert_eq!(session.graph_snapshot().branch_context, original);
}

#[test]
fn branch_context_survives_prepare_execute_and_apply_completion() {
    let mut session = EvalSession::new(
        "fn build() { host_os() }",
        "branch-context-prepare",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );
    let context = EvalBranchContext::new("branch-a");
    session.set_branch_context(context.clone());

    let prepared = session
        .prepare_step()
        .expect("start-entry work should exist");
    assert_eq!(prepared.branch_context(), &context);
    let completion = prepared.execute().expect("prepared work should execute");
    assert_eq!(completion.branch_context(), &context);
    let _step = session
        .apply_completion(completion)
        .expect("completion should apply");
    assert_eq!(session.branch_context(), &context);
    assert_eq!(session.graph_snapshot().branch_context, context);
}

#[test]
fn branch_context_does_not_leak_across_unrelated_sessions() {
    let mut first = EvalSession::new(
        "fn build() { host_os() }",
        "branch-context-first",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );
    let second = EvalSession::new(
        "fn build() { host_os() }",
        "branch-context-second",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    let first_context = EvalBranchContext::new("first-branch");
    first.set_branch_context(first_context.clone());
    assert_eq!(first.branch_context(), &first_context);
    assert_eq!(second.branch_context(), &EvalBranchContext::default());
}

#[test]
fn branch_assumption_reads_session_assumption() {
    let mut session = EvalSession::new(
        r#"
fn build() {
  branch_assumption("version")
}
"#,
        "branch-assumption-read",
        "build",
        Vec::new(),
        &UnsupportedRuntime,
        "win",
    );
    session.set_branch_context(
        EvalBranchContext::default().with_assumption("version", Value::string("1.1.0")),
    );

    let Resolution::Ready(value) = session.run().expect("evaluate branch assumption") else {
        panic!("expected ready branch assumption result");
    };
    assert_eq!(value.rep, ValueRep::String("1.1.0".into()));
}

#[test]
fn with_assumption_scopes_and_restores_branch_context() {
    let mut session = EvalSession::new(
        r#"
fn build() {
  [
    with_assumption("version", "1.0.0", (() => branch_assumption("version"))),
    branch_assumption("version"),
  ]
}
"#,
        "with-assumption-scope",
        "build",
        Vec::new(),
        &UnsupportedRuntime,
        "win",
    );
    let root_context =
        EvalBranchContext::default().with_assumption("version", Value::string("base"));
    session.set_branch_context(root_context.clone());

    let Resolution::Ready(value) = session
        .run()
        .expect("evaluate scoped branch assumption result")
    else {
        panic!("expected ready scoped branch assumption result");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result, got {:?}", value.rep);
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].rep, ValueRep::String("1.0.0".into()));
    assert_eq!(items[1].rep, ValueRep::String("base".into()));
    assert_eq!(session.branch_context(), &root_context);
}

#[test]
fn with_assumption_resumes_pending_under_derived_context() {
    let mut session = EvalSession::new(
        r#"
fn host_wait();

fn build() {
  [
    with_assumption("version", "1.0.0", (() => {
      let _ready = host_wait();
      branch_assumption("version")
    })),
    branch_assumption("version"),
  ]
}
"#,
        "with-assumption-pending",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );
    let root_context =
        EvalBranchContext::default().with_assumption("version", Value::string("base"));
    session.set_branch_context(root_context.clone());

    let Resolution::Pending(pending) = session
        .run()
        .expect("start suspended derived branch assumption")
    else {
        panic!("expected pending derived branch assumption");
    };
    session
        .resolve_pending(&pending, Value::string("ready"))
        .expect("resolve derived branch pending value");

    let Resolution::Ready(value) = session
        .run()
        .expect("resume derived branch assumption result")
    else {
        panic!("expected ready derived branch assumption result");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result, got {:?}", value.rep);
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].rep, ValueRep::String("1.0.0".into()));
    assert_eq!(items[1].rep, ValueRep::String("base".into()));
    assert_eq!(session.branch_context(), &root_context);
}

#[test]
fn keyed_nodes_are_scoped_by_branch_context() {
    let mut session = EvalSession::new(
        r#"
fn resolve_pkg(ctx) {
  branch_assumption("version")
}

fn build() {
  [
    with_assumption("version", "1.0.0", (() => keyed("logger", [], resolve_pkg))),
    with_assumption("version", "1.0.0", (() => keyed("logger", [], resolve_pkg))),
    with_assumption("version", "1.1.0", (() => keyed("logger", [], resolve_pkg))),
  ]
}
"#,
        "keyed-branch-context-scope",
        "build",
        Vec::new(),
        &UnsupportedRuntime,
        "win",
    );

    let Resolution::Ready(value) = session
        .run()
        .expect("evaluate keyed nodes under branch contexts")
    else {
        panic!("expected ready keyed branch-context result");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result, got {:?}", value.rep);
    };
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].rep, ValueRep::String("1.0.0".into()));
    assert_eq!(items[1].rep, ValueRep::String("1.0.0".into()));
    assert_eq!(items[2].rep, ValueRep::String("1.1.0".into()));

    let snapshot = session.graph_snapshot();
    let keyed_nodes = snapshot
        .nodes
        .iter()
        .filter_map(|node| node.keyed.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(keyed_nodes.len(), 2);

    let first_context =
        EvalBranchContext::default().with_assumption("version", Value::string("1.0.0"));
    let second_context =
        EvalBranchContext::default().with_assumption("version", Value::string("1.1.0"));
    assert_eq!(
        keyed_nodes
            .iter()
            .filter(|node| node.branch_context == first_context)
            .count(),
        1
    );
    assert_eq!(
        keyed_nodes
            .iter()
            .filter(|node| node.branch_context == second_context)
            .count(),
        1
    );
}

#[test]
fn run_parallel_can_drive_multiple_frontier_waves_to_completion() {
    let active_calls = Arc::new(AtomicUsize::new(0));
    let max_calls = Arc::new(AtomicUsize::new(0));
    let runtime = ParallelObserveRuntime {
        active_calls,
        max_calls: max_calls.clone(),
    };
    let mut session = EvalSession::new(
        "fn build() { [[host_os(), host_os()], [host_os(), host_os()]] }",
        "parallel-multi-wave",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    let resolution = session
        .run_parallel(4)
        .expect("parallel run should succeed");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    let ValueRep::Array(rows) = value.rep else {
        panic!("expected outer array value");
    };
    assert_eq!(rows.len(), 2);
    for row in rows.iter() {
        let ValueRep::Array(items) = &row.rep else {
            panic!("expected nested array row");
        };
        assert_eq!(items.len(), 2);
        assert!(
            items
                .iter()
                .all(|item| item.rep == ValueRep::String("windows".into()))
        );
    }
    assert!(
        session
            .completed_work()
            .iter()
            .filter(|completed| completed.work.kind == EvalWorkKind::ResumePending)
            .count()
            >= 2,
        "multi-wave drive should include resumed parent work",
    );
    assert!(
        max_calls.load(Ordering::SeqCst) >= 2,
        "later waves should still execute sibling runtime calls in parallel",
    );
    assert!(session.active_wave().is_none());
    assert!(session.buffered_work().is_empty());
}

#[test]
fn buffered_completion_merge_failure_leaves_live_session_state_unchanged() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os(), host_os()] }",
        "prepared-buffered-transactional-merge",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let first = session.prepare_step().expect("first leased sibling");
    let first_work = first.work().clone();
    let second = session.prepare_step().expect("second leased sibling");
    let duplicate = session.first_queued_work_for_testing();

    let first_completion = first.execute().expect("first completion");
    let mut second_completion = second.execute().expect("second completion");
    second_completion.inject_duplicate_queued_work(duplicate);

    assert_eq!(
        session
            .apply_completion(second_completion)
            .expect("second completion should buffer"),
        EvalSessionStep::Progressed,
    );

    let before_graph = session.graph_snapshot();
    let before_trace = session.trace().clone();
    let before_completed = session.completed_work().to_vec();
    let before_work = session.work_records();

    let error = session
        .apply_completion(first_completion)
        .expect_err("buffered merge failure should roll back the whole apply");
    assert_eq!(error.kind, EvalErrorKind::Internal);
    assert!(
        error
            .message
            .contains("already exists during completion merge"),
        "unexpected error message: {}",
        error.message
    );
    assert_eq!(session.graph_snapshot(), before_graph);
    assert_eq!(session.trace(), &before_trace);
    assert_eq!(session.completed_work(), before_completed.as_slice());
    assert_eq!(session.work_records(), before_work);
    assert_eq!(session.in_flight_work(), vec![first_work]);
}

#[test]
fn step_back_and_checkpoint_define_in_flight_work_behavior() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "prepared-step-back",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(
        session.step().expect("start-entry step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("root block step"),
        EvalSessionStep::Progressed
    );
    assert_eq!(
        session.step().expect("array step"),
        EvalSessionStep::Progressed
    );

    let leased = session.prepare_step().expect("leased sibling");
    let leased_work = leased.work().clone();
    let leased_checkpoint = session.checkpoint();

    assert_eq!(session.queued_work().len(), 1);
    assert_eq!(session.in_flight_work(), vec![leased_work.clone()]);
    assert!(session.can_step_back());
    assert!(session.step_back());
    assert_eq!(session.queued_work().len(), 2);
    assert!(session.in_flight_work().is_empty());

    session.restore(leased_checkpoint);
    assert_eq!(session.queued_work().len(), 1);
    assert_eq!(session.in_flight_work(), vec![leased_work]);
}

#[test]
fn session_resume_does_not_replay_ready_runtime_calls_before_pending_point() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let host_wait_calls = Arc::new(AtomicUsize::new(0));
    let runtime = CountingRuntime {
        host_os_calls: host_os_calls.clone(),
        host_wait_calls: host_wait_calls.clone(),
    };
    let mut session = EvalSession::new(
        "fn build() { let os = host_os(); let wait = host_wait(); os }",
        "session-local-resume",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    let first = session.run().expect("first session run");
    let Resolution::Pending(pending) = first else {
        panic!("expected pending resolution, got {first:?}");
    };
    assert_eq!(host_os_calls.load(Ordering::SeqCst), 1);
    assert_eq!(host_wait_calls.load(Ordering::SeqCst), 1);

    session
        .resolve_pending(&pending, Value::string("done"))
        .expect("resolve pending node");

    let second = session.run().expect("second session run");
    let Resolution::Ready(value) = second else {
        panic!("expected ready resolution, got {second:?}");
    };
    assert_eq!(value.rep, ValueRep::String("windows".into()));
    assert_eq!(
        host_os_calls.load(Ordering::SeqCst),
        1,
        "ready runtime calls before the pending point should not rerun",
    );
    assert_eq!(
        host_wait_calls.load(Ordering::SeqCst),
        1,
        "the pending runtime call should resume from the resolved value",
    );
}

#[test]
fn resolving_pending_node_requeues_resume_work_record() {
    let mut session = EvalSession::new(
        "fn build() { host_wait() }",
        "session-queued-work",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    let first = session.run().expect("first session run");
    let Resolution::Pending(pending) = first else {
        panic!("expected pending resolution, got {first:?}");
    };
    assert!(session.queued_work().is_empty());
    let blocked_call_node = session
        .trace()
        .events
        .iter()
        .find_map(|event| match &event.kind {
            EvalEventKind::RegisterPending {
                pending: registered,
            } if registered == &pending => Some(event.node_id),
            _ => None,
        })
        .expect("trace should record the blocked call node");

    session
        .resolve_pending(&pending, Value::string("windows"))
        .expect("resolve pending node");

    let queued = session.queued_work();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].kind, EvalWorkKind::ResumePending);
    assert_eq!(queued[0].node_id, blocked_call_node);
    assert_eq!(queued[0].pending.as_ref(), Some(&pending));
    assert_eq!(
        session.node_state(blocked_call_node),
        Some(&EvalNodeState::Runnable {
            work: queued[0].id,
            pending: Some(pending.clone()),
        }),
    );
    let runnable = session.runnable_nodes();
    assert_eq!(runnable.len(), 1);
    assert_eq!(runnable[0].node_id, blocked_call_node);
    assert_eq!(runnable[0].work_id, queued[0].id);
    assert_eq!(runnable[0].kind, EvalWorkKind::ResumePending);
    assert_eq!(runnable[0].pending.as_ref(), Some(&pending));
    assert_eq!(
        session.node_state(EvalNodeId(pending.node_id)),
        Some(&EvalNodeState::Ready(Value::string("windows"))),
    );

    let second = session.run().expect("second session run");
    let Resolution::Ready(value) = second else {
        panic!("expected ready resolution, got {second:?}");
    };
    assert_eq!(value.rep, ValueRep::String("windows".into()));
    assert!(session.queued_work().is_empty());
    assert_eq!(session.completed_work().len(), 5);
    assert_eq!(
        session.completed_work()[3].work.kind,
        EvalWorkKind::ResumePending
    );
    assert_eq!(
        session.completed_work()[3].outcome,
        EvalCompletedWorkOutcome::Ready(Value::string("windows")),
    );
}

#[test]
fn session_can_progress_through_multiple_pending_runtime_calls() {
    let host_os_calls = Arc::new(AtomicUsize::new(0));
    let wait_one_calls = Arc::new(AtomicUsize::new(0));
    let wait_two_calls = Arc::new(AtomicUsize::new(0));
    let runtime = DoublePendingRuntime {
        host_os_calls: host_os_calls.clone(),
        wait_one_calls: wait_one_calls.clone(),
        wait_two_calls: wait_two_calls.clone(),
    };
    let mut session = EvalSession::new(
        "fn build() { let os = host_os(); let first = wait_one(); let second = wait_two(); os }",
        "session-two-pending",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );

    let first = session.run().expect("first session run");
    let Resolution::Pending(first_pending) = first else {
        panic!("expected first pending resolution, got {first:?}");
    };
    assert_eq!(host_os_calls.load(Ordering::SeqCst), 1);
    assert_eq!(wait_one_calls.load(Ordering::SeqCst), 1);
    assert_eq!(wait_two_calls.load(Ordering::SeqCst), 0);

    session
        .resolve_pending(&first_pending, Value::string("one"))
        .expect("resolve first pending node");

    let second = session.run().expect("second session run");
    let Resolution::Pending(second_pending) = second else {
        panic!("expected second pending resolution, got {second:?}");
    };
    assert_ne!(first_pending.node_id, second_pending.node_id);
    assert_eq!(
        host_os_calls.load(Ordering::SeqCst),
        1,
        "ready work before the first pending point should not replay",
    );
    assert_eq!(wait_one_calls.load(Ordering::SeqCst), 1);
    assert_eq!(wait_two_calls.load(Ordering::SeqCst), 1);

    session
        .resolve_pending(&second_pending, Value::string("two"))
        .expect("resolve second pending node");

    let third = session.run().expect("third session run");
    let Resolution::Ready(value) = third else {
        panic!("expected ready resolution, got {third:?}");
    };
    assert_eq!(value.rep, ValueRep::String("windows".into()));
    assert_eq!(host_os_calls.load(Ordering::SeqCst), 1);
    assert_eq!(wait_one_calls.load(Ordering::SeqCst), 1);
    assert_eq!(wait_two_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn eval2_core_trace_types_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<Value>();
    assert_send_sync::<Resolution>();
    assert_send_sync::<EvalEvent>();
    assert_send_sync::<EvalTrace>();
    assert_send_sync::<crate::SourceSpan>();
}

#[test]
fn evaluation_can_stream_events_from_a_background_thread() {
    let (mut sink, receiver) = channel_trace();
    let handle = thread::spawn(move || {
        eval_source_entry_with_args_with_source_id_with_sink(
            "fn build(value: String) { value }",
            "threaded-trace",
            "build",
            &[Value::string("forty-two")],
            &mut sink,
        )
    });

    let resolution = handle
        .join()
        .expect("background eval thread should not panic")
        .expect("background eval should succeed");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready resolution, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("forty-two".into()));

    let events = receiver.iter().collect::<Vec<_>>();
    assert!(!events.is_empty(), "channel sink should receive events");
    assert_eq!(
        events[0].kind,
        EvalEventKind::BindArgument {
            label: "value".into(),
        },
    );
    assert!(events.iter().all(|event| event.place.as_ref() == "local"));
}
