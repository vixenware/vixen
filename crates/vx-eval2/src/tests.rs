use std::{
    collections::HashMap,
    env, fs,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    str,
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
    EvalWorkKind, EvalWorkState, FunctionRef, MapValue, PendingRef, ProjectionStep, Provenance,
    Resolution, ReuseClass, RuntimeCallOutcome, RuntimeHooks, RuntimeInputKind, RuntimeReady,
    RuntimeSlotPrepare, SlotAction, SlotDecision, SlotEventReason, SlotId, SlotResolutionKind,
    SlotResolved, SlotStore, SlotStoreReason, SlotVerification, SourceSpan, Value, ValueRep,
    channel_trace, envelope_to_value, eval_source_entry_at_place_with_args_with_source_id,
    eval_source_entry_at_place_with_args_with_source_id_traced,
    eval_source_entry_with_args_with_source_id, eval_source_entry_with_args_with_source_id_traced,
    eval_source_entry_with_args_with_source_id_with_sink,
    program::{
        AccessPlanKind, ArrayPlan, ArrayPlanItem, BinaryPlan, BlockStep, BuilderPlanKind,
        BytecodeOp, CallCallee, CallCalleePlanKind, ClosurePlan, CompiledProgram, ControlPlanKind,
        DirectCallCalleePlanKind, ExprExecKind, ExprKind, FailPlan, FalliblePlan, IfPlan,
        InterpolatedStringPlan, InterpolatedStringPlanPart, LiteralPlan, MapPlan, MatchPlan,
        ParenPlan, PrefixPlan, RangePlan, ReadyPlanKind, ResolvedNameRef, StructLiteralPlan,
        StructLiteralPlanPart, TuplePlan, WherePlan,
    },
    resolution_to_outcome, value_to_envelope,
};
use tempfile::tempdir;
use vx_eval2_proto::EvalOutcome;
use vx_parser::SyntaxKind;

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

fn tuple_item(value: &Value, index: usize) -> &Value {
    let ValueRep::Tuple(items) = &value.rep else {
        panic!("expected tuple value, got {value:?}");
    };
    items
        .get(index)
        .unwrap_or_else(|| panic!("expected tuple index {index} in {items:?}"))
}

fn range_value(value: &Value) -> &crate::RangeValue {
    let ValueRep::Range(range) = &value.rep else {
        panic!("expected range value, got {value:?}");
    };
    range
}

fn format_session_diagnostics(
    session: &EvalSession<'_, impl RuntimeHooks>,
    resolution: &Resolution,
) -> String {
    let mut lines = vec![format!("resolution: {resolution:?}")];
    let snapshot = session.graph_snapshot();
    let poisoned = snapshot
        .nodes
        .iter()
        .filter_map(|node| match &node.state {
            Some(EvalNodeState::Poison(error)) => Some((node, error)),
            _ => None,
        })
        .collect::<Vec<_>>();
    lines.push(format!("poisoned nodes: {}", poisoned.len()));

    for (index, (node, error)) in poisoned.iter().enumerate() {
        lines.push(format!("poison[{index}] node_id={:?}", node.node_id));
        lines.push(format!("  error: {error:?}"));
        if let Some(record) = &node.record {
            lines.push(format!("  kind: {:?}", record.kind));
            lines.push(format!("  source: {:?}", record.source));
        }
        lines.push(format!("  dependencies: {:?}", node.dependencies));
        lines.push(format!("  dependents: {:?}", node.dependents));
        for dependency in &node.dependencies {
            if let Some(dependency_node) = snapshot
                .nodes
                .iter()
                .find(|candidate| &candidate.node_id == dependency)
            {
                lines.push(format!(
                    "  dependency {:?} state: {:?}",
                    dependency, dependency_node.state
                ));
                lines.push(format!(
                    "  dependency {:?} record: {:?}",
                    dependency, dependency_node.record
                ));
            }
        }
        lines.push(format!(
            "  explanation: {:?}",
            session.explain_node(node.node_id)
        ));
        let node_trace = session
            .trace()
            .events
            .iter()
            .filter(|event| event.node_id == node.node_id)
            .cloned()
            .collect::<Vec<_>>();
        lines.push(format!("  trace_event_count: {}", node_trace.len()));
        for event in node_trace.iter().rev().take(12).rev() {
            lines.push(format!("    {event:?}"));
        }
    }

    let recent = session
        .trace()
        .events
        .iter()
        .rev()
        .take(20)
        .cloned()
        .collect::<Vec<_>>();
    lines.push("recent trace:".to_string());
    for event in recent.iter().rev() {
        lines.push(format!("  {event:?}"));
    }

    lines.join("\n")
}

fn maybe_print_session_profile(name: &str, session: &EvalSession<'_, impl RuntimeHooks>) {
    if env::var_os("VX_EVAL2_PRINT_PROFILE").is_some() {
        let limit = env::var("VX_EVAL2_PRINT_PROFILE_LIMIT")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(20);
        eprintln!("vx-eval2 profile test={name}");
        eprintln!("{}", session.profile_report(limit));
    }
    if env::var_os("VX_EVAL2_PRINT_CALL_PROFILE").is_some() {
        let limit = env::var("VX_EVAL2_PRINT_PROFILE_LIMIT")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(20);
        eprintln!("vx-eval2 call profile test={name}");
        eprintln!("{}", session.call_profile_report(limit));
    }
}

fn ready_root_ids(value: &Value) -> Vec<&str> {
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected roots array, got {value:?}");
    };
    items
        .iter()
        .map(|item| match &struct_field(item, "value").rep {
            ValueRep::String(text) => text.as_ref(),
            other => panic!("expected PackageId string value, got {other:?}"),
        })
        .collect()
}

fn keyed_ready_resolved_crates<'a>(snapshot: &'a crate::EvalGraphSnapshot) -> Vec<&'a Value> {
    snapshot
        .nodes
        .iter()
        .filter(|node| node.keyed.is_some())
        .filter_map(|node| match &node.state {
            Some(EvalNodeState::Ready(value)) => {
                let ValueRep::Struct(struct_value) = &value.rep else {
                    return None;
                };
                (struct_value.type_name.as_ref() == "ResolvedCrate").then_some(value)
            }
            _ => None,
        })
        .collect()
}

fn resolved_crates_contain_name(snapshot: &crate::EvalGraphSnapshot, name: &str) -> bool {
    keyed_ready_resolved_crates(snapshot)
        .iter()
        .any(|crate_value| {
            matches!(
                &struct_field(crate_value, "name").rep,
                ValueRep::String(text) if text.as_ref() == name
            )
        })
}

fn resolved_crate_version(snapshot: &crate::EvalGraphSnapshot, name: &str) -> Option<Arc<str>> {
    keyed_ready_resolved_crates(snapshot)
        .into_iter()
        .find(|crate_value| {
            matches!(
                &struct_field(crate_value, "name").rep,
                ValueRep::String(text) if text.as_ref() == name
            )
        })
        .map(
            |crate_value| match &struct_field(crate_value, "version").rep {
                ValueRep::String(text) => text.clone(),
                other => panic!("expected crate version string, got {other:?}"),
            },
        )
}

fn cargo_feature_join_http_responses() -> HashMap<String, String> {
    let mut http_responses = HashMap::new();
    http_responses.insert(
        "https://index.crates.io/config.json".to_string(),
        r#"{"dl":"https://crates.io/api/v1/crates"}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/sh/ar/shared".to_string(),
        r#"{"name":"shared","vers":"1.0.0","deps":[{"name":"defdep","req":"^1","features":[],"optional":true,"default_features":true,"target":null,"kind":"normal"},{"name":"leftdep","req":"^1","features":[],"optional":true,"default_features":true,"target":null,"kind":"normal"},{"name":"rightdep","req":"^1","features":[],"optional":true,"default_features":true,"target":null,"kind":"normal"}],"cksum":"3333333333333333333333333333333333333333333333333333333333333333","features":{},"features2":{"default":["dep:defdep"],"left":["dep:leftdep"],"right":["dep:rightdep"]},"yanked":false,"rust_version":null,"v":2}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/de/fd/defdep".to_string(),
        r#"{"name":"defdep","vers":"1.0.0","deps":[],"cksum":"4444444444444444444444444444444444444444444444444444444444444444","features":{},"features2":{},"yanked":false,"rust_version":null,"v":2}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/le/ft/leftdep".to_string(),
        r#"{"name":"leftdep","vers":"1.0.0","deps":[],"cksum":"5555555555555555555555555555555555555555555555555555555555555555","features":{},"features2":{},"yanked":false,"rust_version":null,"v":2}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/ri/gh/rightdep".to_string(),
        r#"{"name":"rightdep","vers":"1.0.0","deps":[],"cksum":"6666666666666666666666666666666666666666666666666666666666666666","features":{},"features2":{},"yanked":false,"rust_version":null,"v":2}"#.to_string(),
    );
    http_responses
}

fn cargo_workspace_inherited_registry_http_responses() -> HashMap<String, String> {
    let mut http_responses = HashMap::new();
    http_responses.insert(
        "https://index.crates.io/config.json".to_string(),
        r#"{"dl":"https://crates.io/api/v1/crates"}"#.to_string(),
    );
    for (name, file_name) in [
        ("tracing-subscriber", "tracing-subscriber-0.3.22.jsonl"),
        ("matchers", "matchers-0.2.0.jsonl"),
        ("regex-automata", "regex-automata-0.4.14.jsonl"),
        ("regex-syntax", "regex-syntax-0.8.10.jsonl"),
        ("nu-ansi-term", "nu-ansi-term-0.50.3.jsonl"),
        ("once_cell", "once_cell-1.21.3.jsonl"),
        ("sharded-slab", "sharded-slab-0.1.7.jsonl"),
        ("lazy_static", "lazy_static-1.5.0.jsonl"),
        ("thread_local", "thread_local-1.1.9.jsonl"),
        ("cfg-if", "cfg-if-1.0.4.jsonl"),
        ("tracing", "tracing-0.1.44.jsonl"),
        ("tracing-core", "tracing-core-0.1.36.jsonl"),
        ("pin-project-lite", "pin-project-lite-0.2.17.jsonl"),
    ] {
        http_responses.insert(
            local_sparse_package_url(name),
            fs::read_to_string(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../specs/eval/run/registry_real")
                    .join(file_name),
            )
            .unwrap_or_else(|error| panic!("read real {name} sparse index entry: {error}")),
        );
    }
    http_responses
}

fn cargo_sha2_hello_http_responses() -> HashMap<String, String> {
    let mut http_responses = HashMap::new();
    http_responses.insert(
        "https://index.crates.io/config.json".to_string(),
        r#"{"dl":"https://crates.io/api/v1/crates"}"#.to_string(),
    );
    for (name, file_name) in [
        ("sha2", "sha2-0.10.9.jsonl"),
        ("digest", "digest-0.10.7.jsonl"),
        ("cpufeatures", "cpufeatures-0.2.17.jsonl"),
        ("crypto-common", "crypto-common-0.1.7.jsonl"),
        ("block-buffer", "block-buffer-0.10.4.jsonl"),
        ("generic-array", "generic-array-0.14.7.jsonl"),
        ("typenum", "typenum-1.19.0.jsonl"),
        ("version_check", "version_check-0.9.5.jsonl"),
        ("cfg-if", "cfg-if-1.0.4.jsonl"),
    ] {
        http_responses.insert(
            local_sparse_package_url(name),
            fs::read_to_string(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../specs/eval/run/registry_real")
                    .join(file_name),
            )
            .unwrap_or_else(|error| panic!("read real {name} sparse index entry: {error}")),
        );
    }
    http_responses
}

struct ReadFileRuntime {
    root_dir: PathBuf,
}

impl RuntimeHooks for ReadFileRuntime {
    fn call(
        &self,
        _place: &str,
        callee: &FunctionRef,
        args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match callee.name.as_ref() {
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

struct CargoSketchRuntime {
    root_dir: PathBuf,
    http_responses: HashMap<String, String>,
}

fn local_crates_io_sparse_index_root() -> Option<PathBuf> {
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))?;
    let index_root = cargo_home.join("registry/index");
    fs::read_dir(index_root)
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            name.starts_with("index.crates.io-").then_some(path)
        })
}

fn local_sparse_package_rel_path(name: &str) -> String {
    match name.len() {
        0 => String::new(),
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => format!("3/{}/{name}", &name[0..1]),
        _ => format!("{}/{}/{}", &name[0..2], &name[2..4], name),
    }
}

fn local_sparse_package_url(name: &str) -> String {
    format!(
        "https://index.crates.io/{}",
        local_sparse_package_rel_path(name)
    )
}

fn sparse_cache_entry_to_jsonl(bytes: &[u8]) -> Option<String> {
    let mut entries = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;
    while index <= bytes.len() {
        if index == bytes.len() || bytes[index] == 0 {
            if start < index && bytes[start] == b'{' {
                entries.push(String::from_utf8(bytes[start..index].to_vec()).ok()?);
            }
            start = index.saturating_add(1);
        }
        index += 1;
    }
    (!entries.is_empty()).then(|| entries.join("\n"))
}

fn sparse_cache_subset_to_jsonl(bytes: &[u8], versions: &[&str]) -> Option<String> {
    let mut slices = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;
    while index <= bytes.len() {
        if index == bytes.len() || bytes[index] == 0 {
            slices.push(&bytes[start..index]);
            start = index.saturating_add(1);
        }
        index += 1;
    }

    let mut entries = Vec::new();
    for pair in slices.windows(2) {
        let Ok(version) = str::from_utf8(pair[0]) else {
            continue;
        };
        if !versions.iter().any(|candidate| candidate == &version) {
            continue;
        }
        let Ok(json) = String::from_utf8(pair[1].to_vec()) else {
            continue;
        };
        if json.starts_with('{') {
            entries.push(json);
        }
    }

    (!entries.is_empty()).then(|| entries.join("\n"))
}

fn local_sparse_package_subset_jsonl(name: &str, versions: &[&str]) -> Option<String> {
    let root = local_crates_io_sparse_index_root()?;
    let bytes = fs::read(
        root.join(".cache")
            .join(local_sparse_package_rel_path(name)),
    )
    .ok()?;
    sparse_cache_subset_to_jsonl(&bytes, versions)
}

fn local_sparse_index_http_response(url: &str) -> Option<String> {
    let root = local_crates_io_sparse_index_root()?;
    let rel = url.strip_prefix("https://index.crates.io/")?;
    if rel == "config.json" {
        return fs::read_to_string(root.join("config.json")).ok();
    }

    let bytes = fs::read(root.join(".cache").join(rel)).ok()?;
    sparse_cache_entry_to_jsonl(&bytes)
}

impl RuntimeHooks for CargoSketchRuntime {
    fn call(
        &self,
        _place: &str,
        callee: &FunctionRef,
        args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        match callee.name.as_ref() {
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
            "Path" => {
                if args.len() != 1 {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "Path() requires exactly one string argument",
                    ));
                }
                let ValueRep::String(path) = &args[0].rep else {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "Path() argument must evaluate to String",
                    ));
                };
                Ok(RuntimeCallOutcome::Ready(
                    Value::external("Path", path.clone())
                        .with_provenance(Provenance::unknown(source.clone()))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue),
                ))
            }
            "Tree" => {
                if args.len() != 1 {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "Tree() requires exactly one path-like argument",
                    ));
                }
                let descriptor = match &args[0].rep {
                    ValueRep::String(path) => path.clone(),
                    ValueRep::External(external) if external.kind.as_ref() == "Path" => {
                        external.descriptor.clone()
                    }
                    _ => {
                        return Err(EvalError::new(
                            EvalErrorKind::TypeError,
                            "Tree() argument must evaluate to String or Path",
                        ));
                    }
                };
                Ok(RuntimeCallOutcome::Ready(
                    Value::external("Tree", descriptor)
                        .with_provenance(Provenance::unknown(source.clone()))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue),
                ))
            }
            "Tree.file" => {
                if args.len() != 2 {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "Tree.file() requires receiver and one path-like argument",
                    ));
                }
                let ValueRep::External(tree) = &args[0].rep else {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "Tree.file() receiver must evaluate to Tree",
                    ));
                };
                if tree.kind.as_ref() != "Tree" {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "Tree.file() receiver must evaluate to Tree",
                    ));
                }
                let rel_path = match &args[1].rep {
                    ValueRep::String(path) => path.clone(),
                    ValueRep::External(external) if external.kind.as_ref() == "Path" => {
                        external.descriptor.clone()
                    }
                    _ => {
                        return Err(EvalError::new(
                            EvalErrorKind::TypeError,
                            "Tree.file() path must evaluate to String or Path",
                        ));
                    }
                };
                let full_path = self
                    .root_dir
                    .join(tree.descriptor.as_ref())
                    .join(rel_path.as_ref());
                let contents = fs::read_to_string(&full_path).map_err(|error| {
                    EvalError::new(
                        EvalErrorKind::UserFail,
                        format!("Tree.file() failed for {}: {error}", full_path.display()),
                    )
                })?;
                Ok(RuntimeCallOutcome::Ready(
                    Value::external("Blob", contents)
                        .with_provenance(Provenance::unknown(source.clone()))
                        .with_reuse(ReuseClass::Observed)
                        .with_crossing(CrossingPolicy::ByValue),
                ))
            }
            "Http.get" => {
                if args.len() != 1 {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "Http.get() requires exactly one string argument",
                    ));
                }
                let ValueRep::String(url) = &args[0].rep else {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "Http.get() argument must evaluate to String",
                    ));
                };
                let contents = self
                    .http_responses
                    .get(url.as_ref())
                    .cloned()
                    .or_else(|| local_sparse_index_http_response(url.as_ref()))
                    .ok_or_else(|| {
                        EvalError::new(
                            EvalErrorKind::UserFail,
                            format!("Http.get() had no fixture response for {url}"),
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
fn compiled_program_lowers_block_expr_to_block_steps() {
    let program = CompiledProgram::new(
        "lowered-block",
        "fn build() {\n  let captured = 41;\n  fn helper() { captured }\n  helper()\n}",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    assert_eq!(block_body.steps.len(), 3);
    assert!(matches!(block_body.steps[0], BlockStep::Let { .. }));
    assert!(matches!(block_body.steps[1], BlockStep::FnDecl(_)));
    assert!(matches!(block_body.steps[2], BlockStep::Expr(_)));
}

#[test]
fn compiled_program_lowers_block_expr_to_bytecode_body() {
    let program = CompiledProgram::new(
        "lowered-block-bytecode",
        "fn build() {\n  let captured = 41;\n  fn helper() { captured }\n  helper()\n}",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let bytecode = program
        .resolve_bytecode_body(block.body.into())
        .expect("resolve lowered block bytecode body");
    assert_eq!(bytecode.ops.len(), 6);
    assert!(matches!(bytecode.ops[0], BytecodeOp::EvalReady(_)));
    assert!(matches!(bytecode.ops[1], BytecodeOp::BindLet { .. }));
    assert!(matches!(bytecode.ops[2], BytecodeOp::BindFnDecl { .. }));
    assert!(matches!(bytecode.ops[3], BytecodeOp::EvalBody(_)));
    assert!(matches!(bytecode.ops[4], BytecodeOp::SetLast));
    assert!(matches!(bytecode.ops[5], BytecodeOp::ReturnLast));
}

#[test]
fn compiled_program_lowers_ready_name_refs_to_bytecode_local_loads() {
    let program = CompiledProgram::new(
        "lowered-bytecode-ready-name-ref",
        "fn build() { let x = 41; x }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let bytecode = program
        .resolve_bytecode_body(block.body.into())
        .expect("resolve lowered block bytecode body");
    assert_eq!(bytecode.ops.len(), 5);
    assert!(matches!(bytecode.ops[0], BytecodeOp::EvalReady(_)));
    assert!(matches!(bytecode.ops[1], BytecodeOp::BindLet { .. }));
    assert!(matches!(bytecode.ops[2], BytecodeOp::LoadLocal { .. }));
    assert!(matches!(bytecode.ops[3], BytecodeOp::SetLast));
    assert!(matches!(bytecode.ops[4], BytecodeOp::ReturnLast));
}

#[test]
fn compiled_program_lowers_parameter_name_refs_to_bytecode_local_loads() {
    let program = CompiledProgram::new(
        "lowered-bytecode-parameter-name-ref",
        "fn build(value: Bool) { value }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let bytecode = program
        .resolve_bytecode_body(block.body.into())
        .expect("resolve lowered block bytecode body");
    assert_eq!(bytecode.ops.len(), 3);
    assert!(matches!(bytecode.ops[0], BytecodeOp::LoadLocal { .. }));
    assert!(matches!(bytecode.ops[1], BytecodeOp::SetLast));
    assert!(matches!(bytecode.ops[2], BytecodeOp::ReturnLast));
}

#[test]
fn compiled_program_lowers_nested_block_expr_to_nested_bytecode_op() {
    let program = CompiledProgram::new("lowered-bytecode-nested-block", "fn build() { { 41 } }");
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let bytecode = program
        .resolve_bytecode_body(block.body.into())
        .expect("resolve lowered block bytecode body");
    assert_eq!(bytecode.ops.len(), 3);
    assert!(matches!(bytecode.ops[0], BytecodeOp::EvalBytecodeBody(_)));
    assert!(matches!(bytecode.ops[1], BytecodeOp::SetLast));
    assert!(matches!(bytecode.ops[2], BytecodeOp::ReturnLast));
}

#[test]
fn compiled_program_lowers_if_expr_to_bytecode_ops() {
    let program = CompiledProgram::new(
        "lowered-bytecode-if",
        "fn build(value: Bool) { if value { 1 } else { 2 } }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let bytecode = program
        .resolve_bytecode_body(block.body.into())
        .expect("resolve lowered block bytecode body");
    assert_eq!(bytecode.ops.len(), 4);
    assert!(matches!(bytecode.ops[0], BytecodeOp::LoadLocal { .. }));
    assert!(matches!(bytecode.ops[1], BytecodeOp::EvalIfBranches { .. }));
    assert!(matches!(bytecode.ops[2], BytecodeOp::SetLast));
    assert!(matches!(bytecode.ops[3], BytecodeOp::ReturnLast));
}

#[test]
fn compiled_program_lowers_prefix_expr_to_bytecode_ops() {
    let program = CompiledProgram::new(
        "lowered-bytecode-prefix",
        "fn build(value: Bool) { !value }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let bytecode = program
        .resolve_bytecode_body(block.body.into())
        .expect("resolve lowered block bytecode body");
    assert_eq!(bytecode.ops.len(), 4);
    assert!(matches!(bytecode.ops[0], BytecodeOp::LoadLocal { .. }));
    assert!(matches!(bytecode.ops[1], BytecodeOp::ApplyPrefix(_)));
    assert!(matches!(bytecode.ops[2], BytecodeOp::SetLast));
    assert!(matches!(bytecode.ops[3], BytecodeOp::ReturnLast));
}

#[test]
fn compiled_program_lowers_binary_expr_to_bytecode_ops() {
    let program = CompiledProgram::new(
        "lowered-bytecode-binary",
        "fn build(a: U64, b: U64) { a + b }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let bytecode = program
        .resolve_bytecode_body(block.body.into())
        .expect("resolve lowered block bytecode body");
    assert_eq!(bytecode.ops.len(), 4);
    assert!(matches!(bytecode.ops[0], BytecodeOp::LoadLocal { .. }));
    assert!(matches!(bytecode.ops[1], BytecodeOp::EvalBinary { .. }));
    assert!(matches!(bytecode.ops[2], BytecodeOp::SetLast));
    assert!(matches!(bytecode.ops[3], BytecodeOp::ReturnLast));
}

#[test]
fn compiled_program_lowers_field_expr_to_bytecode_ops() {
    let program = CompiledProgram::new("lowered-bytecode-field", r#"fn build() { ("hi").len }"#);
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let bytecode = program
        .resolve_bytecode_body(block.body.into())
        .expect("resolve lowered block bytecode body");
    assert_eq!(bytecode.ops.len(), 4);
    assert!(matches!(bytecode.ops[0], BytecodeOp::EvalReady(_)));
    assert!(matches!(bytecode.ops[1], BytecodeOp::ProjectField { .. }));
    assert!(matches!(bytecode.ops[2], BytecodeOp::SetLast));
    assert!(matches!(bytecode.ops[3], BytecodeOp::ReturnLast));
}

#[test]
fn compiled_program_lowers_fail_expr_to_bytecode_ops() {
    let program = CompiledProgram::new("lowered-bytecode-fail", r#"fn build() { fail "boom" }"#);
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let bytecode = program
        .resolve_bytecode_body(block.body.into())
        .expect("resolve lowered block bytecode body");
    assert_eq!(bytecode.ops.len(), 4);
    assert!(matches!(bytecode.ops[0], BytecodeOp::EvalReady(_)));
    assert!(matches!(bytecode.ops[1], BytecodeOp::RaiseUserFail { .. }));
    assert!(matches!(bytecode.ops[2], BytecodeOp::SetLast));
    assert!(matches!(bytecode.ops[3], BytecodeOp::ReturnLast));
}

#[test]
fn compiled_program_lowers_call_expr_to_bytecode_ops() {
    let program = CompiledProgram::new("lowered-bytecode-call", "fn build() { helper(42) }");
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(call_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain call expression");
    };
    let bytecode = program
        .resolve_bytecode_body(block.body.into())
        .expect("resolve lowered block bytecode body");
    let call_body = program
        .resolve_expr_bytecode_body(call_expr)
        .expect("resolve lowered call bytecode body")
        .expect("call expr should lower to its own bytecode body");
    let call_bytecode = program
        .resolve_bytecode_body(call_body)
        .expect("resolve nested call bytecode body");
    assert_eq!(bytecode.ops.len(), 3);
    assert!(matches!(bytecode.ops[0], BytecodeOp::EvalBody(_)));
    assert!(matches!(bytecode.ops[1], BytecodeOp::SetLast));
    assert!(matches!(bytecode.ops[2], BytecodeOp::ReturnLast));
    assert!(matches!(call_bytecode.ops[0], BytecodeOp::StartCall(_)));
    assert!(matches!(call_bytecode.ops[1], BytecodeOp::SetLast));
    assert!(matches!(call_bytecode.ops[2], BytecodeOp::ReturnLast));
}

#[test]
fn compiled_program_lowers_where_expr_to_bytecode_ops() {
    let program = CompiledProgram::new(
        "lowered-bytecode-where",
        "fn helper() { 1 }\nfn build() { helper() where { value: \"serde\", needle: \"ser\" } }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(where_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain where expression");
    };
    let bytecode = program
        .resolve_bytecode_body(block.body.into())
        .expect("resolve lowered block bytecode body");
    let where_body = program
        .resolve_expr_bytecode_body(where_expr)
        .expect("resolve lowered where bytecode body")
        .expect("where expr should lower to its own bytecode body");
    let where_bytecode = program
        .resolve_bytecode_body(where_body)
        .expect("resolve nested where bytecode body");
    assert_eq!(bytecode.ops.len(), 3);
    assert!(matches!(bytecode.ops[0], BytecodeOp::EvalBody(_)));
    assert!(matches!(bytecode.ops[1], BytecodeOp::SetLast));
    assert!(matches!(bytecode.ops[2], BytecodeOp::ReturnLast));
    assert!(matches!(where_bytecode.ops[0], BytecodeOp::StartWhere(_)));
    assert!(matches!(where_bytecode.ops[1], BytecodeOp::SetLast));
    assert!(matches!(where_bytecode.ops[2], BytecodeOp::ReturnLast));
}

#[test]
fn compiled_program_lowers_match_expr_to_bytecode_ops() {
    let program = CompiledProgram::new(
        "lowered-bytecode-match",
        "fn build(value: Bool) { match value { true => 1, false => 2 } }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(match_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain match expression");
    };
    let bytecode = program
        .resolve_bytecode_body(block.body.into())
        .expect("resolve lowered block bytecode body");
    let match_body = program
        .resolve_expr_bytecode_body(match_expr)
        .expect("resolve lowered match bytecode body")
        .expect("match expr should lower to its own bytecode body");
    let match_bytecode = program
        .resolve_bytecode_body(match_body)
        .expect("resolve nested match bytecode body");
    assert_eq!(bytecode.ops.len(), 3);
    assert!(matches!(bytecode.ops[0], BytecodeOp::EvalBody(_)));
    assert!(matches!(bytecode.ops[1], BytecodeOp::SetLast));
    assert!(matches!(bytecode.ops[2], BytecodeOp::ReturnLast));
    assert!(matches!(match_bytecode.ops[0], BytecodeOp::StartMatch(_)));
    assert!(matches!(match_bytecode.ops[1], BytecodeOp::SetLast));
    assert!(matches!(match_bytecode.ops[2], BytecodeOp::ReturnLast));
}

#[test]
fn compiled_program_lowers_direct_name_call_callee_to_call_callee_plan() {
    let program = CompiledProgram::new("lowered-call-callee", "fn build() { helper(42) }");
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(call_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain call expression");
    };
    let expr = program
        .resolve_expr_id(call_expr)
        .expect("resolve call expr");
    let ExprKind::Call(call) = expr.kind() else {
        panic!("expected lowered step to be a call expr, got {expr:?}");
    };
    assert!(matches!(call.callee, Some(CallCallee::NameRef(_))));
}

#[test]
fn compiled_program_caches_string_literal_exec_plan() {
    let program = CompiledProgram::new("cached-literal-plan", "fn build() { \"hello\" }");
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(literal_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain literal expression");
    };
    let plan = program
        .resolve_expr_plan(literal_expr)
        .expect("resolve cached literal plan");
    assert_eq!(plan.kind(), ExprExecKind::Ready);
    assert_eq!(plan.ready_kind(), ReadyPlanKind::Literal);
    let literal = program
        .resolve_literal_plan(plan.literal_plan_id(literal_expr.source()))
        .expect("resolve cached literal payload");
    let LiteralPlan::String { value, .. } = literal else {
        panic!("expected cached string literal payload, got {literal:?}");
    };
    assert_eq!(value.as_ref(), "hello");
}

#[test]
fn compiled_program_caches_resolved_name_ref_exec_plan() {
    let program = CompiledProgram::new(
        "cached-name-ref-plan",
        "fn helper() { 1 }\nfn build() { helper }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(name_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain name ref expression");
    };
    let plan = program
        .resolve_expr_plan(name_expr)
        .expect("resolve cached name ref plan");
    assert_eq!(plan.kind(), ExprExecKind::Ready);
    assert_eq!(plan.ready_kind(), ReadyPlanKind::NameRef);
    let name_ref = program
        .resolve_name_ref_plan(plan.name_ref_plan_id(name_expr.source()))
        .expect("resolve cached name ref payload");
    assert!(matches!(
        name_ref.resolved,
        Some(ResolvedNameRef::Function { .. })
    ));
}

#[test]
fn compiled_program_caches_resolved_call_exec_plan() {
    let program = CompiledProgram::new(
        "cached-call-plan",
        "fn helper(value: U64) { value }\nfn build() { helper(42) }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(call_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain call expression");
    };
    let plan = program
        .resolve_expr_plan(call_expr)
        .expect("resolve cached call plan");
    assert_eq!(plan.kind(), ExprExecKind::Access);
    assert_eq!(plan.access_kind(), AccessPlanKind::Call);
    let call = program
        .resolve_call_plan(plan.call_plan_id(call_expr.source()))
        .expect("resolve cached call payload");
    let Some(callee) = call.callee.as_ref() else {
        panic!("expected cached call callee to lower to a direct plan");
    };
    assert_eq!(callee.kind(), CallCalleePlanKind::Direct);
    let direct = program
        .resolve_direct_call_callee_plan(callee.direct_call_callee_plan_id(call_expr.source()))
        .expect("resolve direct call callee payload");
    assert_eq!(direct.kind(), DirectCallCalleePlanKind::ResolvedFunction);
}

#[test]
fn compiled_program_caches_static_member_call_callee_direct_plan() {
    let program = CompiledProgram::new(
        "cached-static-member-call-plan",
        "fn build() { String.len(\"hi\") }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(call_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain call expression");
    };
    let plan = program
        .resolve_expr_plan(call_expr)
        .expect("resolve cached call plan");
    assert_eq!(plan.kind(), ExprExecKind::Access);
    assert_eq!(plan.access_kind(), AccessPlanKind::Call);
    let call = program
        .resolve_call_plan(plan.call_plan_id(call_expr.source()))
        .expect("resolve cached call payload");
    let Some(callee) = call.callee.as_ref() else {
        panic!("expected cached static member callee to lower to a direct plan");
    };
    assert_eq!(callee.kind(), CallCalleePlanKind::Direct);
    let direct = program
        .resolve_direct_call_callee_plan(callee.direct_call_callee_plan_id(call_expr.source()))
        .expect("resolve direct static member payload");
    assert_eq!(direct.kind(), DirectCallCalleePlanKind::StaticMember);
}

#[test]
fn compiled_program_caches_dynamic_call_callee_as_body_ref() {
    let program = CompiledProgram::new(
        "cached-dynamic-call-callee-plan",
        "fn build() { ((value) => value)(42) }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(call_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain call expression");
    };
    let plan = program
        .resolve_expr_plan(call_expr)
        .expect("resolve cached call plan");
    assert_eq!(plan.kind(), ExprExecKind::Access);
    assert_eq!(plan.access_kind(), AccessPlanKind::Call);
    let call = program
        .resolve_call_plan(plan.call_plan_id(call_expr.source()))
        .expect("resolve cached call payload");
    let Some(callee) = call.callee.as_ref() else {
        panic!("expected cached dynamic call callee");
    };
    assert_eq!(callee.kind(), CallCalleePlanKind::Expr);
    assert_ne!(callee.expr_body().plan().kind(), ExprExecKind::Expr);
}

#[test]
fn compiled_program_lowers_resolved_function_call_callee_to_call_callee_plan() {
    let program = CompiledProgram::new(
        "lowered-resolved-function-call-callee",
        "fn helper(value: U64) { value }\nfn build() { helper(42) }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(call_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain call expression");
    };
    let expr = program
        .resolve_expr_id(call_expr)
        .expect("resolve call expr");
    let ExprKind::Call(call) = expr.kind() else {
        panic!("expected lowered step to be a call expr, got {expr:?}");
    };
    assert!(matches!(
        call.callee,
        Some(CallCallee::ResolvedFunction { .. })
    ));
}

#[test]
fn compiled_program_lowers_field_call_callee_to_call_callee_plan() {
    let program = CompiledProgram::new(
        "lowered-field-call-callee",
        "fn build(value: String) { value.len() }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(call_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain call expression");
    };
    let expr = program
        .resolve_expr_id(call_expr)
        .expect("resolve call expr");
    let ExprKind::Call(call) = expr.kind() else {
        panic!("expected lowered step to be a call expr, got {expr:?}");
    };
    let Some(CallCallee::Field(field)) = &call.callee else {
        panic!("expected lowered call callee to be a field plan");
    };
    assert_eq!(field.field_name.as_deref(), Some("len"));
}

#[test]
fn compiled_program_lowers_static_member_call_callee_to_call_callee_plan() {
    let program = CompiledProgram::new(
        "lowered-static-member-call-callee",
        "fn build() { String.len(\"hi\") }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(call_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain call expression");
    };
    let expr = program
        .resolve_expr_id(call_expr)
        .expect("resolve call expr");
    let ExprKind::Call(call) = expr.kind() else {
        panic!("expected lowered step to be a call expr, got {expr:?}");
    };
    assert!(matches!(call.callee, Some(CallCallee::StaticMember { .. })));
}

#[test]
fn compiled_program_lowers_name_ref_expr_to_resolved_plan() {
    let program = CompiledProgram::new(
        "lowered-name-ref-expr",
        "fn helper() { 1 }\nfn build() { helper }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(name_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain name ref expression");
    };
    let expr = program
        .resolve_expr_id(name_expr)
        .expect("resolve name ref expr");
    let ExprKind::NameRef(name_ref) = expr.kind() else {
        panic!("expected lowered step to be a name ref expr, got {expr:?}");
    };
    assert!(matches!(
        name_ref.resolved,
        Some(ResolvedNameRef::Function { .. })
    ));
}

#[test]
fn compiled_program_lowers_static_member_field_expr_to_plan() {
    let program = CompiledProgram::new(
        "lowered-static-member-field-expr",
        "fn build() { String.len }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(field_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain field expression");
    };
    let expr = program
        .resolve_expr_id(field_expr)
        .expect("resolve field expr");
    let ExprKind::Field(field) = expr.kind() else {
        panic!("expected lowered step to be a field expr, got {expr:?}");
    };
    assert!(field.static_member.is_some());
    assert_eq!(field.field_name.as_deref(), Some("len"));
}

#[test]
fn compiled_program_does_not_lower_shadowed_upper_name_field_expr_to_plan() {
    let program = CompiledProgram::new(
        "lowered-shadowed-static-member-field-expr",
        "fn String() { 1 }\nfn build() { String.len }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(field_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain field expression");
    };
    let expr = program
        .resolve_expr_id(field_expr)
        .expect("resolve field expr");
    let ExprKind::Field(field) = expr.kind() else {
        panic!("expected lowered step to be a field expr, got {expr:?}");
    };
    assert!(field.static_member.is_none());
}

#[test]
fn compiled_program_lowers_where_expr_to_compiled_call_plan() {
    let program = CompiledProgram::new(
        "lowered-where-call-plan",
        "fn helper() { 1 }\nfn build() { helper() where { value: \"serde\", needle: \"ser\" } }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(where_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain where expression");
    };
    let expr = program
        .resolve_expr_id(where_expr)
        .expect("resolve where expr");
    let ExprKind::Where(where_expr) = expr.kind() else {
        panic!("expected lowered step to be a where expr, got {expr:?}");
    };
    let Some(call) = where_expr.call.as_ref() else {
        panic!("expected where expr to contain compiled call plan");
    };
    assert!(matches!(
        call.callee,
        Some(CallCallee::ResolvedFunction { .. })
    ));
    assert_eq!(call.positional_args.len(), 0);
}

#[test]
fn compiled_program_caches_where_exec_plan() {
    let program = CompiledProgram::new(
        "cached-where-plan",
        "fn helper() { 1 }\nfn build() { helper() where { value: \"serde\", needle: \"ser\" } }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(where_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain where expression");
    };
    let plan = program
        .resolve_expr_plan(where_expr)
        .expect("resolve cached where plan");
    assert_eq!(plan.kind(), ExprExecKind::Control);
    assert_eq!(plan.control_kind(), ControlPlanKind::Where);
    let WherePlan {
        call,
        keyword_bindings,
        ..
    } = program
        .resolve_where_plan(plan.where_plan_id(where_expr.source()))
        .expect("resolve cached where payload");
    let call = call
        .as_ref()
        .expect("cached where plan should contain call");
    let Some(callee) = call.callee.as_ref() else {
        panic!("expected cached where callee to lower to a direct plan");
    };
    assert_eq!(callee.kind(), CallCalleePlanKind::Direct);
    let direct = program
        .resolve_direct_call_callee_plan(callee.direct_call_callee_plan_id(body.source()))
        .expect("resolve direct where callee payload");
    assert_eq!(direct.kind(), DirectCallCalleePlanKind::ResolvedFunction);
    assert_eq!(keyword_bindings.len(), 2);
}

#[test]
fn compiled_program_caches_fallible_exec_plan() {
    let program = CompiledProgram::new("cached-fallible-plan", "fn build() { read_file? }");
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(fallible_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain fallible expression");
    };
    let plan = program
        .resolve_expr_plan(fallible_expr)
        .expect("resolve cached fallible plan");
    assert_eq!(plan.kind(), ExprExecKind::Control);
    assert_eq!(plan.control_kind(), ControlPlanKind::Fallible);
    let FalliblePlan { expr, .. } = program
        .resolve_fallible_plan(plan.fallible_plan_id(fallible_expr.source()))
        .expect("resolve cached fallible payload");
    assert!(expr.is_some());
}

#[test]
fn compiled_program_caches_if_exec_plan() {
    let program = CompiledProgram::new("cached-if-plan", "fn build() { if true { 1 } else { 2 } }");
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(if_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain if expression");
    };
    let plan = program
        .resolve_expr_plan(if_expr)
        .expect("resolve cached if plan");
    assert_eq!(plan.kind(), ExprExecKind::Control);
    assert_eq!(plan.control_kind(), ControlPlanKind::If);
    let IfPlan {
        condition,
        then_branch,
        else_branch,
        ..
    } = program
        .resolve_if_plan(plan.if_plan_id(if_expr.source()))
        .expect("resolve cached if payload");
    assert!(condition.is_some());
    assert!(then_branch.is_some());
    assert!(else_branch.is_some());
}

#[test]
fn compiled_program_caches_prefix_exec_plan() {
    let program = CompiledProgram::new("cached-prefix-plan", "fn build() { !true }");
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(prefix_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain prefix expression");
    };
    let plan = program
        .resolve_expr_plan(prefix_expr)
        .expect("resolve cached prefix plan");
    assert_eq!(plan.kind(), ExprExecKind::Control);
    assert_eq!(plan.control_kind(), ControlPlanKind::Prefix);
    let PrefixPlan { operator, expr, .. } = program
        .resolve_prefix_plan(plan.prefix_plan_id(prefix_expr.source()))
        .expect("resolve cached prefix payload");
    assert_eq!(*operator, Some(SyntaxKind::BANG));
    assert!(expr.is_some());
}

#[test]
fn compiled_program_caches_binary_exec_plan() {
    let program = CompiledProgram::new("cached-binary-plan", "fn build() { 1 + 2 }");
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(binary_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain binary expression");
    };
    let plan = program
        .resolve_expr_plan(binary_expr)
        .expect("resolve cached binary plan");
    assert_eq!(plan.kind(), ExprExecKind::Control);
    assert_eq!(plan.control_kind(), ControlPlanKind::Binary);
    let BinaryPlan {
        lhs, rhs, operator, ..
    } = program
        .resolve_binary_plan(plan.binary_plan_id(binary_expr.source()))
        .expect("resolve cached binary payload");
    assert!(lhs.is_some());
    assert!(rhs.is_some());
    assert_eq!(*operator, Some(SyntaxKind::PLUS));
}

#[test]
fn compiled_program_caches_array_exec_plan() {
    let program = CompiledProgram::new("cached-array-plan", "fn build() { [1, 2] }");
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(array_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain array expression");
    };
    let plan = program
        .resolve_expr_plan(array_expr)
        .expect("resolve cached array plan");
    assert_eq!(plan.kind(), ExprExecKind::Builder);
    assert_eq!(plan.builder_kind(), BuilderPlanKind::Array);
    let ArrayPlan { items, .. } = program
        .resolve_array_plan(plan.array_plan_id(array_expr.source()))
        .expect("resolve cached array payload");
    assert_eq!(items.len(), 2);
    assert!(
        matches!(items[0], ArrayPlanItem::Expr(body) if body.plan().kind() == ExprExecKind::Ready)
    );
    assert!(
        matches!(items[1], ArrayPlanItem::Expr(body) if body.plan().kind() == ExprExecKind::Ready)
    );
}

#[test]
fn compiled_program_caches_tuple_exec_plan() {
    let program = CompiledProgram::new("cached-tuple-plan", "fn build() { (1, 2) }");
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(tuple_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain tuple expression");
    };
    let plan = program
        .resolve_expr_plan(tuple_expr)
        .expect("resolve cached tuple plan");
    assert_eq!(plan.kind(), ExprExecKind::Builder);
    assert_eq!(plan.builder_kind(), BuilderPlanKind::Tuple);
    let TuplePlan { items, .. } = program
        .resolve_tuple_plan(plan.tuple_plan_id(tuple_expr.source()))
        .expect("resolve cached tuple payload");
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|item| item.plan().kind() == ExprExecKind::Ready)
    );
}

#[test]
fn compiled_program_caches_range_exec_plan() {
    let program = CompiledProgram::new("cached-range-plan", "fn build() { 1..=2 }");
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(range_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain range expression");
    };
    let plan = program
        .resolve_expr_plan(range_expr)
        .expect("resolve cached range plan");
    assert_eq!(plan.kind(), ExprExecKind::Builder);
    assert_eq!(plan.builder_kind(), BuilderPlanKind::Range);
    let RangePlan {
        start,
        end,
        inclusive_end,
        ..
    } = program
        .resolve_range_plan(plan.range_plan_id(range_expr.source()))
        .expect("resolve cached range payload");
    assert!(start.is_some());
    assert!(end.is_some());
    assert!(*inclusive_end);
    assert_eq!(
        start.expect("range start").plan().kind(),
        ExprExecKind::Ready
    );
    assert_eq!(end.expect("range end").plan().kind(), ExprExecKind::Ready);
}

#[test]
fn compiled_program_caches_map_exec_plan() {
    let program = CompiledProgram::new(
        "cached-map-plan",
        "fn build() { %{ \"a\" => 1, \"b\" => 2 } }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(map_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain map expression");
    };
    let plan = program
        .resolve_expr_plan(map_expr)
        .expect("resolve cached map plan");
    assert_eq!(plan.kind(), ExprExecKind::Builder);
    assert_eq!(plan.builder_kind(), BuilderPlanKind::Map);
    let MapPlan { entries, .. } = program
        .resolve_map_plan(plan.map_plan_id(map_expr.source()))
        .expect("resolve cached map payload");
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|entry| {
        entry.key.plan().kind() == ExprExecKind::Ready
            && entry.value.plan().kind() == ExprExecKind::Ready
    }));
}

#[test]
fn compiled_program_caches_fail_exec_plan() {
    let program = CompiledProgram::new("cached-fail-plan", "fn build() { fail(\"boom\") }");
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(fail_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain fail expression");
    };
    let plan = program
        .resolve_expr_plan(fail_expr)
        .expect("resolve cached fail plan");
    assert_eq!(plan.kind(), ExprExecKind::Control);
    assert_eq!(plan.control_kind(), ControlPlanKind::Fail);
    let FailPlan { message, .. } = program
        .resolve_fail_plan(plan.fail_plan_id(fail_expr.source()))
        .expect("resolve cached fail payload");
    assert!(message.is_some());
}

#[test]
fn compiled_program_caches_interpolated_string_exec_plan() {
    let program = CompiledProgram::new(
        "cached-interpolated-string-plan",
        "fn build() { `hello {1}` }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(string_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain interpolated string expression");
    };
    let plan = program
        .resolve_expr_plan(string_expr)
        .expect("resolve cached interpolated string plan");
    assert_eq!(plan.kind(), ExprExecKind::Builder);
    assert_eq!(plan.builder_kind(), BuilderPlanKind::InterpolatedString);
    let InterpolatedStringPlan { parts, .. } = program
        .resolve_interpolated_string_plan(plan.interpolated_string_plan_id(string_expr.source()))
        .expect("resolve cached interpolated string payload");
    assert_eq!(parts.len(), 2);
    assert!(matches!(
        &parts[1],
        InterpolatedStringPlanPart::Interpolation(body)
            if body.plan().kind() == ExprExecKind::Ready
    ));
}

#[test]
fn compiled_program_caches_struct_literal_exec_plan() {
    let program = CompiledProgram::new(
        "cached-struct-literal-plan",
        "struct Box { value: U64 }\nfn build() { Box { value: 1 } }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(struct_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain struct literal expression");
    };
    let plan = program
        .resolve_expr_plan(struct_expr)
        .expect("resolve cached struct literal plan");
    assert_eq!(plan.kind(), ExprExecKind::Builder);
    assert_eq!(plan.builder_kind(), BuilderPlanKind::StructLiteral);
    let StructLiteralPlan {
        type_name, parts, ..
    } = program
        .resolve_struct_literal_plan(plan.struct_literal_plan_id(struct_expr.source()))
        .expect("resolve cached struct literal payload");
    assert_eq!(type_name.as_deref(), Some("Box"));
    assert_eq!(parts.len(), 1);
    assert!(matches!(
        &parts[0],
        StructLiteralPlanPart::Field { expr: Some(body), .. }
            if body.plan().kind() == ExprExecKind::Ready
    ));
}

#[test]
fn compiled_program_caches_paren_exec_plan() {
    let program = CompiledProgram::new("cached-paren-plan", "fn build() { (1) }");
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(paren_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain paren expression");
    };
    let plan = program
        .resolve_expr_plan(paren_expr)
        .expect("resolve cached paren plan");
    assert_eq!(plan.kind(), ExprExecKind::Access);
    assert_eq!(plan.access_kind(), AccessPlanKind::Paren);
    let ParenPlan { expr, .. } = program
        .resolve_paren_plan(plan.paren_plan_id(paren_expr.source()))
        .expect("resolve cached paren payload");
    assert!(matches!(
        expr,
        Some(body) if body.plan().kind() == ExprExecKind::Ready
    ));
}

#[test]
fn compiled_program_caches_closure_exec_plan() {
    let program = CompiledProgram::new("cached-closure-plan", "fn build() { ((value) => value) }");
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(closure_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain closure expression");
    };
    let mut closure_expr = closure_expr;
    let plan = program
        .resolve_expr_plan(closure_expr)
        .expect("resolve cached closure plan");
    let closure = match plan.kind() {
        ExprExecKind::Ready => {
            assert_eq!(plan.ready_kind(), ReadyPlanKind::Closure);
            plan.closure_plan_id(closure_expr.source())
        }
        ExprExecKind::Access => {
            assert_eq!(plan.access_kind(), AccessPlanKind::Paren);
            let ParenPlan { expr, .. } = program
                .resolve_paren_plan(plan.paren_plan_id(closure_expr.source()))
                .expect("resolve cached paren payload");
            closure_expr = expr.expect("paren-wrapped closure should have an inner expr");
            let plan = program
                .resolve_expr_plan(closure_expr)
                .expect("resolve cached inner closure plan");
            assert_eq!(plan.kind(), ExprExecKind::Ready);
            assert_eq!(plan.ready_kind(), ReadyPlanKind::Closure);
            plan.closure_plan_id(closure_expr.source())
        }
        _ => panic!("expected cached closure exec plan, got {plan:?}"),
    };
    let ClosurePlan {
        parameters, body, ..
    } = program
        .resolve_closure_plan(closure)
        .expect("resolve cached closure payload");
    assert_eq!(parameters.len(), 1);
    assert!(matches!(
        body,
        Some(body) if body.plan().kind() == ExprExecKind::Ready
    ));
}

#[test]
fn compiled_program_caches_match_exec_plan() {
    let program = CompiledProgram::new(
        "cached-match-plan",
        "fn build() { match true { true => 1, false => 2 } }",
    );
    let function = program.find_function("build").expect("find build function");
    let body = function.body.expect("build function should have a body");
    let expr = program
        .resolve_expr_id(body)
        .expect("resolve build body expr");
    let ExprKind::Block(block) = expr.kind() else {
        panic!("expected build body to lower to a block expression, got {expr:?}");
    };
    let block_body = program
        .resolve_block_body(block.body)
        .expect("resolve lowered block body");
    let BlockStep::Expr(match_expr) = block_body.steps[0] else {
        panic!("expected lowered block to contain match expression");
    };
    let plan = program
        .resolve_expr_plan(match_expr)
        .expect("resolve cached match plan");
    assert_eq!(plan.kind(), ExprExecKind::Control);
    assert_eq!(plan.control_kind(), ControlPlanKind::Match);
    let MatchPlan {
        scrutinee, arms, ..
    } = program
        .resolve_match_plan(plan.match_plan_id(match_expr.source()))
        .expect("resolve cached match payload");
    assert!(scrutinee.is_some());
    assert_eq!(arms.len(), 2);
    assert_eq!(
        scrutinee.expect("match scrutinee").plan().kind(),
        ExprExecKind::Ready
    );
    assert!(
        arms.iter()
            .all(|arm| arm.body.plan().kind() == ExprExecKind::Ready)
    );
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
fn json_parse_honors_field_rename_annotations() {
    let value = expect_ready(
        eval_source_entry_with_args_with_source_id(
            r#"
struct Release {
  #[rename = "vers"]
  version: String,
}

fn build(blob: Blob) {
  Json.parse[Release](blob)
}
"#,
            "json-parse-field-rename",
            "build",
            &[Value::external("Blob", r#"{"vers":"1.0.228"}"#)],
        )
        .expect("evaluate Json.parse with field rename"),
    );

    assert_eq!(
        struct_field(&value, "version").rep,
        ValueRep::String(Arc::from("1.0.228"))
    );
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
fn toml_serialize_omits_none_fields_in_structs() {
    let value = expect_ready(
        eval_source_entry_with_args_with_source_id(
            r#"
struct Config {
  name: String,
  description: String?,
}

fn build() {
  Toml.serialize(Config {
    name: "demo",
    description: None,
  })
}
"#,
            "toml-serialize-omits-none",
            "build",
            &[],
        )
        .expect("evaluate Toml.serialize"),
    );

    let ValueRep::External(external) = &value.rep else {
        panic!("expected Blob result, got {value:?}");
    };
    assert_eq!(external.kind.as_ref(), "Blob");
    assert!(external.descriptor.contains("name = \"demo\""));
    assert!(!external.descriptor.contains("description"));
}

#[test]
fn toml_parse_honors_kebab_case_rename_all() {
    let value = expect_ready(
        eval_source_entry_with_args_with_source_id(
            r#"
#[rename_all = "kebab-case"]
struct Package {
  rust_version: String,
  default_features: Bool,
}

fn build(blob: Blob) {
  Toml.parse[Package](blob)
}
"#,
            "toml-parse-rename-all",
            "build",
            &[Value::external(
                "Blob",
                "rust-version = \"1.56\"\ndefault-features = false\n",
            )],
        )
        .expect("evaluate Toml.parse with rename_all"),
    );

    assert_eq!(
        struct_field(&value, "rust_version").rep,
        ValueRep::String(Arc::from("1.56"))
    );
    assert_eq!(
        struct_field(&value, "default_features").rep,
        ValueRep::Bool(false)
    );
}

#[test]
fn toml_parse_applies_defaults_inside_untagged_detailed_dependency_specs() {
    let value = expect_ready(
        eval_source_entry_with_args_with_source_id(
            r#"
#[untagged]
enum CargoDependencySpec {
  struct Version { value: String },

  #[rename_all = "kebab-case"]
  struct Detailed {
    version: String?,
    default_features: Bool = true,
  },
}

struct Manifest {
  dependencies: Map[String, CargoDependencySpec] = %{},
}

fn build(blob: Blob) {
  Toml.parse[Manifest](blob).dependencies.get("shared")
}
"#,
            "toml-parse-untagged-detailed-defaults",
            "build",
            &[Value::external(
                "Blob",
                "[dependencies]\nshared = { version = \"1\" }\n",
            )],
        )
        .expect("evaluate Toml.parse with missing default-features"),
    );

    let ValueRep::Struct(detailed) = &value.rep else {
        panic!("expected parsed Detailed dependency, got {value:?}");
    };
    assert_eq!(detailed.type_name.as_ref(), "Detailed");
    assert_eq!(
        struct_field(&value, "default_features").rep,
        ValueRep::Bool(true)
    );
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
fn toml_parse_decodes_untagged_dependency_entries() {
    let dir = tempdir().expect("temp dir");
    fs::write(
        dir.path().join("Cargo.toml"),
        r#"[dependencies]
eyre = { workspace = true }
flate2 = "1"
"#,
    )
    .expect("write Cargo.toml");

    let runtime = ReadFileRuntime {
        root_dir: dir.path().to_path_buf(),
    };
    let mut session = EvalSession::new(
        r#"
#[untagged]
enum DependencySpec {
  struct Version {
    value: String,
  },

  struct Detailed {
    workspace: Bool?,
    version: String?,
  },
}

struct Manifest {
  dependencies: Map[String, DependencySpec],
}

fn build() {
  let manifest = Toml.parse[Manifest](read_file("Cargo.toml"));
  manifest.dependencies
}
"#,
        "toml-parse-untagged-dependencies",
        "build",
        Vec::new(),
        &runtime,
        "local",
    );

    let Resolution::Ready(value) = session
        .run()
        .expect("evaluate Toml.parse against untagged dependency file")
    else {
        panic!("expected parsed dependency map result");
    };

    let eyre = map_string_entry(&value, "eyre");
    let ValueRep::Struct(eyre_struct) = &eyre.rep else {
        panic!("expected eyre dependency struct, got {eyre:?}");
    };
    assert_eq!(eyre_struct.type_name.as_ref(), "Detailed");
    let workspace = struct_field(eyre, "workspace");
    let ValueRep::Struct(workspace_option) = &workspace.rep else {
        panic!("expected optional workspace field, got {workspace:?}");
    };
    assert_eq!(workspace_option.type_name.as_ref(), "Some");
    assert_eq!(struct_field(workspace, "value").rep, ValueRep::Bool(true));
    let version = struct_field(eyre, "version");
    let ValueRep::Struct(version_option) = &version.rep else {
        panic!("expected optional version field, got {version:?}");
    };
    assert_eq!(version_option.type_name.as_ref(), "None");

    let flate2 = map_string_entry(&value, "flate2");
    let ValueRep::Struct(flate2_struct) = &flate2.rep else {
        panic!("expected flate2 dependency struct, got {flate2:?}");
    };
    assert_eq!(flate2_struct.type_name.as_ref(), "Version");
    assert_eq!(
        struct_field(flate2, "value").rep,
        ValueRep::String(Arc::from("1"))
    );
}

#[test]
fn json_parse_decodes_flattened_struct_fields() {
    let value = expect_ready(
        eval_source_entry_with_args_with_source_id(
            r#"
struct SharedDep {
  version: String,
  optional: Bool?,
}

struct Dependency {
  #[flatten]
  shared: SharedDep,
  package: String?,
}

fn build(blob: Blob) {
  Json.parse[Dependency](blob)
}
"#,
            "json-parse-flattened-struct-fields",
            "build",
            &[Value::external(
                "Blob",
                r#"{"version":"1","optional":true,"package":"zlib"}"#,
            )],
        )
        .expect("evaluate Json.parse with flattened struct field"),
    );

    let shared = struct_field(&value, "shared");
    let ValueRep::Struct(shared_struct) = &shared.rep else {
        panic!("expected flattened shared struct, got {shared:?}");
    };
    assert_eq!(shared_struct.type_name.as_ref(), "SharedDep");
    assert_eq!(
        struct_field(shared, "version").rep,
        ValueRep::String(Arc::from("1"))
    );
    let optional = struct_field(shared, "optional");
    let ValueRep::Struct(optional_struct) = &optional.rep else {
        panic!("expected optional field in flattened struct, got {optional:?}");
    };
    assert_eq!(optional_struct.type_name.as_ref(), "Some");
    assert_eq!(struct_field(optional, "value").rep, ValueRep::Bool(true));

    let package = struct_field(&value, "package");
    let ValueRep::Struct(package_struct) = &package.rep else {
        panic!("expected optional package field, got {package:?}");
    };
    assert_eq!(package_struct.type_name.as_ref(), "Some");
    assert_eq!(
        struct_field(package, "value").rep,
        ValueRep::String(Arc::from("zlib"))
    );
}

#[test]
fn toml_parse_flattens_leftover_keys_into_string_map() {
    let value = expect_ready(
        eval_source_entry_with_args_with_source_id(
            r#"
struct Header {
  title: String,
  edition: String,
}

struct Doc {
  #[flatten]
  header: Header,
  #[flatten]
  extra: Map[String, String],
}

fn build(blob: Blob) {
  Toml.parse[Doc](blob)
}
"#,
            "toml-parse-flatten-leftover-map",
            "build",
            &[Value::external(
                "Blob",
                r#"title = "demo"
edition = "2024"
x-extra = "1"
y-extra = "2"
"#,
            )],
        )
        .expect("evaluate Toml.parse with flattened leftover map"),
    );

    let header = struct_field(&value, "header");
    let ValueRep::Struct(header_struct) = &header.rep else {
        panic!("expected flattened header struct, got {header:?}");
    };
    assert_eq!(header_struct.type_name.as_ref(), "Header");
    assert_eq!(
        struct_field(header, "title").rep,
        ValueRep::String(Arc::from("demo"))
    );
    assert_eq!(
        struct_field(header, "edition").rep,
        ValueRep::String(Arc::from("2024"))
    );

    let extra = struct_field(&value, "extra");
    let ValueRep::Map(extra_map) = &extra.rep else {
        panic!("expected flattened extra map, got {extra:?}");
    };
    assert_eq!(extra_map.entries.len(), 2);
    assert_eq!(
        map_string_entry(extra, "x-extra").rep,
        ValueRep::String(Arc::from("1"))
    );
    assert_eq!(
        map_string_entry(extra, "y-extra").rep,
        ValueRep::String(Arc::from("2"))
    );
}

#[test]
fn json_parse_decodes_enum_variant_with_rename_all() {
    let value = expect_ready(
        eval_source_entry_with_args_with_source_id(
            r#"
#[rename_all = "snake_case"]
enum DepKind {
  struct Normal,
  struct Build,
  struct Dev,
}

struct Dep {
  kind: DepKind?,
}

fn build(blob: Blob) {
  Json.parse[Dep](blob)
}
"#,
            "json-parse-enum-rename-all",
            "build",
            &[Value::external("Blob", r#"{"kind":"normal"}"#)],
        )
        .expect("evaluate Json.parse with enum rename_all"),
    );

    let kind = struct_field(&value, "kind");
    let ValueRep::Struct(option) = &kind.rep else {
        panic!("expected optional kind field, got {kind:?}");
    };
    assert_eq!(option.type_name.as_ref(), "Some");
    let nested = struct_field(kind, "value");
    let ValueRep::Struct(kind_variant) = &nested.rep else {
        panic!("expected enum variant, got {nested:?}");
    };
    assert_eq!(kind_variant.type_name.as_ref(), "Normal");
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
fn struct_literal_field_puns_use_bound_names_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
struct Pair {
  left: String,
  right: Bool,
}

fn build() {
  let left = "serde";
  let right = true;
  Pair { left, right }
}
"#,
        "struct-literal-field-puns-runtime",
        "build",
        &[],
    )
    .expect("evaluate struct literal field puns");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready struct literal field pun result, got {resolution:?}");
    };
    assert_eq!(
        struct_field(&value, "left").rep,
        ValueRep::String("serde".into())
    );
    assert_eq!(struct_field(&value, "right").rep, ValueRep::Bool(true));
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
fn string_len_method_returns_u64_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(name: String) {
  name.len()
}
"#,
        "string-len-method-runtime",
        "build",
        &[Value::string("serde")],
    )
    .expect("evaluate String.len()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready String.len() result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::U64(5));
}

#[test]
fn tuple_literal_evaluates_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  ("serde", 5)
}
"#,
        "tuple-literal-runtime",
        "build",
        &[],
    )
    .expect("evaluate tuple literal");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready tuple result, got {resolution:?}");
    };
    assert_eq!(tuple_item(&value, 0).rep, ValueRep::String("serde".into()));
    assert_eq!(tuple_item(&value, 1).rep, ValueRep::U64(5));
}

#[test]
fn tuple_let_destructuring_binds_elements_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  let (dep, feat) = ("serde?", "json");
  (dep.ends_with("?"), feat.len())
}
"#,
        "tuple-let-destructuring-runtime",
        "build",
        &[],
    )
    .expect("evaluate tuple let destructuring");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready tuple destructuring result, got {resolution:?}");
    };
    assert_eq!(tuple_item(&value, 0).rep, ValueRep::Bool(true));
    assert_eq!(tuple_item(&value, 1).rep, ValueRep::U64(4));
}

#[test]
fn string_split_once_method_returns_optional_tuple_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(name: String) {
  name.split_once("/")
}
"#,
        "string-split-once-method-runtime",
        "build",
        &[Value::string("serde/json")],
    )
    .expect("evaluate String.split_once()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready String.split_once() result, got {resolution:?}");
    };
    let tuple = struct_field(&value, "value");
    assert_eq!(tuple_item(tuple, 0).rep, ValueRep::String("serde".into()));
    assert_eq!(tuple_item(tuple, 1).rep, ValueRep::String("json".into()));
}

#[test]
fn match_over_option_variants_destructures_some_and_none_at_runtime() {
    let some = eval_source_entry_with_args_with_source_id(
        r#"
fn build(name: String?) {
  match name (
    Some { value } => value,
    None => "fallback",
  )
}
"#,
        "match-option-some-runtime",
        "build",
        &[Value::structure(
            "Some",
            vec![(Arc::from("value"), Value::string("serde"))],
        )],
    )
    .expect("evaluate match over Some");
    let Resolution::Ready(some) = some else {
        panic!("expected ready match Some result, got {some:?}");
    };
    assert_eq!(some.rep, ValueRep::String("serde".into()));

    let none = eval_source_entry_with_args_with_source_id(
        r#"
fn build(name: String?) {
  match name (
    Some { value } => value,
    None => "fallback",
  )
}
"#,
        "match-option-none-runtime",
        "build",
        &[Value::structure("None", Vec::new())],
    )
    .expect("evaluate match over None");
    let Resolution::Ready(none) = none else {
        panic!("expected ready match None result, got {none:?}");
    };
    assert_eq!(none.rep, ValueRep::String("fallback".into()));
}

#[test]
fn match_over_untagged_dependency_spec_builds_request_shape_at_runtime() {
    let detailed = eval_source_entry_with_args_with_source_id(
        r#"
#[untagged]
enum CargoDependencySpec {
  struct Version { value: String },
  struct Detailed {
    version: String?,
    optional: Bool,
  },
}

struct DependencyRequest {
  version: String?,
  optional: Bool,
}

fn request_from_dep(dep: CargoDependencySpec) {
  match dep (
    Version { value } => DependencyRequest {
      version: Some(value),
      optional: false,
    },
    Detailed { version, optional } => DependencyRequest {
      version,
      optional,
    },
  )
}

fn build() {
  request_from_dep(Detailed {
    version: Some("1"),
    optional: true,
  })
}
"#,
        "match-untagged-dependency-runtime",
        "build",
        &[],
    )
    .expect("evaluate match over untagged dependency spec");
    let Resolution::Ready(detailed) = detailed else {
        panic!("expected ready match over untagged dependency result, got {detailed:?}");
    };
    assert_eq!(
        struct_field(struct_field(&detailed, "version"), "value").rep,
        ValueRep::String("1".into())
    );
    assert_eq!(
        struct_field(&detailed, "optional").rep,
        ValueRep::Bool(true)
    );
}

#[test]
fn match_over_tuple_pattern_binds_elements_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(pair: (String, String)) {
  match pair (
    (left, right) => left.ends_with("?") && right == "json",
  )
}
"#,
        "match-tuple-pattern-runtime",
        "build",
        &[Value::tuple(vec![
            Value::string("serde?"),
            Value::string("json"),
        ])],
    )
    .expect("evaluate match over tuple pattern");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready match over tuple result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::Bool(true));
}

#[test]
fn range_literal_evaluates_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  0..=3
}
"#,
        "range-literal-runtime",
        "build",
        &[],
    )
    .expect("evaluate range literal");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready range result, got {resolution:?}");
    };
    let range = range_value(&value);
    assert_eq!(range.start, Some(0));
    assert_eq!(range.end, Some(3));
    assert!(range.inclusive_end);
}

#[test]
fn string_slice_method_accepts_range_argument_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(name: String) {
  name.slice(0..4)
}
"#,
        "string-slice-method-runtime",
        "build",
        &[Value::string("flate2")],
    )
    .expect("evaluate String.slice()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready String.slice() result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("flat".into()));
}

#[test]
fn string_literals_decode_escapes_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  [
    "line\nbreak",
    "tab\tstop",
    "quote:\"",
    "slash:\\",
    match "quote:\"" (
      "quote:\"" => true,
      _ => false,
    ),
  ]
}
"#,
        "string-literal-escapes-runtime",
        "build",
        &[],
    )
    .expect("evaluate escaped string literals");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready escaped string literal result, got {resolution:?}");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result, got {value:?}");
    };
    assert_eq!(items[0].rep, ValueRep::String("line\nbreak".into()));
    assert_eq!(items[1].rep, ValueRep::String("tab\tstop".into()));
    assert_eq!(items[2].rep, ValueRep::String("quote:\"".into()));
    assert_eq!(items[3].rep, ValueRep::String("slash:\\".into()));
    assert_eq!(items[4].rep, ValueRep::Bool(true));
}

#[test]
fn blob_utf8_method_returns_string_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(blob: Blob) {
  blob.utf8()
}
"#,
        "blob-utf8-method-runtime",
        "build",
        &[Value::external("Blob", "{\"name\":\"flate2\"}")],
    )
    .expect("evaluate Blob.utf8()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Blob.utf8() result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("{\"name\":\"flate2\"}".into()));
}

#[test]
fn path_to_string_method_normalizes_lexical_segments_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(path: Path) {
  path.to_string()
}
"#,
        "path-to-string-method-runtime",
        "build",
        &[Value::external("Path", "./crates/./leaf")],
    )
    .expect("evaluate Path.to_string()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Path.to_string() result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("crates/leaf".into()));
}

#[test]
fn json_parse_accepts_string_argument_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
struct Release {
  #[rename = "vers"]
  version: String,
}

fn build(text: String) {
  Json.parse[Release](text)
}
"#,
        "json-parse-string-argument-runtime",
        "build",
        &[Value::string(r#"{"vers":"1.1.9"}"#)],
    )
    .expect("evaluate Json.parse from String");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Json.parse String result, got {resolution:?}");
    };
    assert_eq!(
        struct_field(&value, "version").rep,
        ValueRep::String("1.1.9".into())
    );
}

#[test]
fn string_contains_method_reports_presence_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(name: String) {
  name.contains("ate")
}
"#,
        "string-contains-method-runtime",
        "build",
        &[Value::string("flate2")],
    )
    .expect("evaluate String.contains()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready String.contains() result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::Bool(true));
}

#[test]
fn string_is_empty_method_reports_empty_strings_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(text: String) {
  text.is_empty()
}
"#,
        "string-is-empty-method-runtime",
        "build",
        &[Value::string("")],
    )
    .expect("evaluate String.is_empty()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready String.is_empty() result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::Bool(true));
}

#[test]
fn prefix_bang_negates_bool_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  !false
}
"#,
        "prefix-bang-runtime",
        "build",
        &[],
    )
    .expect("evaluate ! prefix");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready ! result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::Bool(true));
}

#[test]
fn logical_or_short_circuits_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  true || fail("rhs should not run")
}
"#,
        "logical-or-short-circuit-runtime",
        "build",
        &[],
    )
    .expect("evaluate || short-circuit");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready || result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::Bool(true));
}

#[test]
fn logical_and_short_circuits_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  false && fail("rhs should not run")
}
"#,
        "logical-and-short-circuit-runtime",
        "build",
        &[],
    )
    .expect("evaluate && short-circuit");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready && result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::Bool(false));
}

#[test]
fn array_filter_stream_to_array_selects_matching_items_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(items: Array[String]) {
  items.filter((item) => item.contains("at")).to_array()
}
"#,
        "array-filter-method-runtime",
        "build",
        &[Value::array(vec![
            Value::string("flate2"),
            Value::string("serde"),
            Value::string("wat"),
        ])],
    )
    .expect("evaluate Array.filter().to_array()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Array.filter().to_array() result, got {resolution:?}");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result from Array.filter().to_array(), got {value:?}");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].rep, ValueRep::String("flate2".into()));
    assert_eq!(items[1].rep, ValueRep::String("wat".into()));
}

#[test]
fn array_map_stream_to_array_transforms_items_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(items: Array[String]) {
  items.map((item) => item.to_lower()).to_array()
}
"#,
        "array-map-method-runtime",
        "build",
        &[Value::array(vec![
            Value::string("FlAtE2"),
            Value::string("SeRdE"),
        ])],
    )
    .expect("evaluate Array.map().to_array()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Array.map().to_array() result, got {resolution:?}");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result from Array.map().to_array(), got {value:?}");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].rep, ValueRep::String("flate2".into()));
    assert_eq!(items[1].rep, ValueRep::String("serde".into()));
}

#[test]
fn array_any_method_short_circuits_matches_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(items: Array[String]) {
  items.any((item) => item == "serde")
}
"#,
        "array-any-method-runtime",
        "build",
        &[Value::array(vec![
            Value::string("flate2"),
            Value::string("serde"),
            Value::string("tar"),
        ])],
    )
    .expect("evaluate Array.any()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Array.any() result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::Bool(true));
}

#[test]
fn array_filter_map_method_collects_some_values_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn maybe_named(item: String) {
  if item.to_lower().contains("at") (
    Some(item.to_lower())
  ) else (
    None
  )
}

fn build(items: Array[String]) {
  items.filter_map(maybe_named)
}
"#,
        "array-filter-map-method-runtime",
        "build",
        &[Value::array(vec![
            Value::string("FlAtE2"),
            Value::string("serde"),
            Value::string("WAT"),
        ])],
    )
    .expect("evaluate Array.filter_map()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Array.filter_map() result, got {resolution:?}");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result from Array.filter_map(), got {value:?}");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].rep, ValueRep::String("flate2".into()));
    assert_eq!(items[1].rep, ValueRep::String("wat".into()));
}

#[test]
fn enum_variant_constructors_build_struct_values_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(flag: Bool, value: String) {
  if flag (
    Some(value)
  ) else (
    None
  )
}
"#,
        "enum-variant-constructors-runtime",
        "build",
        &[Value::bool(true), Value::string("flate2")],
    )
    .expect("evaluate enum variant constructors");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready enum constructor result, got {resolution:?}");
    };
    let ValueRep::Struct(option) = &value.rep else {
        panic!("expected struct result from enum constructor, got {value:?}");
    };
    assert_eq!(option.type_name.as_ref(), "Some");
    assert_eq!(option.fields.len(), 1);
    assert_eq!(option.fields[0].0.as_ref(), "value");
    assert_eq!(option.fields[0].1.rep, ValueRep::String("flate2".into()));
}

#[test]
fn enum_variants_can_be_referenced_with_type_prefix_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
enum Choice {
  struct Left,
  struct Right,
}

fn build(flag: Bool) {
  if flag (
    Choice.Left
  ) else (
    Choice.Right
  )
}
"#,
        "enum-variant-type-prefix-runtime",
        "build",
        &[Value::bool(true)],
    )
    .expect("evaluate type-prefixed enum variants");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready type-prefixed enum variant result, got {resolution:?}");
    };
    let ValueRep::Struct(choice) = &value.rep else {
        panic!("expected struct result from type-prefixed enum variant, got {value:?}");
    };
    assert_eq!(choice.type_name.as_ref(), "Left");
}

#[test]
fn type_prefixed_variant_struct_literals_use_variant_runtime_type() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
enum FeatureEdge {
  struct Local {
    feature: String,
  },
}

fn build() {
  FeatureEdge.Local { feature: "rust_backend" }
}
"#,
        "enum-variant-struct-literal-runtime",
        "build",
        &[],
    )
    .expect("evaluate type-prefixed enum variant struct literal");

    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready type-prefixed enum variant struct literal result, got {resolution:?}"
        );
    };
    let ValueRep::Struct(feature_edge) = &value.rep else {
        panic!(
            "expected struct result from type-prefixed enum variant struct literal, got {value:?}"
        );
    };
    assert_eq!(feature_edge.type_name.as_ref(), "Local");
    assert_eq!(feature_edge.fields.len(), 1);
    assert_eq!(feature_edge.fields[0].0.as_ref(), "feature");
    assert_eq!(
        feature_edge.fields[0].1.rep,
        ValueRep::String("rust_backend".into())
    );
}

#[test]
fn struct_literal_spread_merges_and_overrides_fields_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
struct FeatureExpansionState {
  pending: Array[String] = [],
  enabled_optional_deps: Array[String] = [],
  dependency_features: Array[String] = [],
}

fn build() {
  let state = FeatureExpansionState {
    pending: ["default"],
    enabled_optional_deps: [],
    dependency_features: [],
  };
  FeatureExpansionState {
    ..state,
    pending: ["rust_backend"],
  }
}
"#,
        "struct-literal-spread-runtime",
        "build",
        &[],
    )
    .expect("evaluate struct spread");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready struct spread result, got {resolution:?}");
    };
    let ValueRep::Struct(state) = &value.rep else {
        panic!("expected struct result from spread, got {value:?}");
    };
    assert_eq!(state.type_name.as_ref(), "FeatureExpansionState");
    let pending = struct_field(&value, "pending");
    let ValueRep::Array(items) = &pending.rep else {
        panic!("expected pending array from spread result, got {pending:?}");
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].rep, ValueRep::String("rust_backend".into()));
}

#[test]
fn array_flatten_method_concatenates_nested_arrays_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  [["flate2"], ["serde", "tar"]].flatten()
}
"#,
        "array-flatten-method-runtime",
        "build",
        &[],
    )
    .expect("evaluate Array.flatten()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Array.flatten() result, got {resolution:?}");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result from Array.flatten(), got {value:?}");
    };
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].rep, ValueRep::String("flate2".into()));
    assert_eq!(items[1].rep, ValueRep::String("serde".into()));
    assert_eq!(items[2].rep, ValueRep::String("tar".into()));
}

#[test]
fn array_to_array_method_returns_receiver_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(items: Array[String]) {
  items.to_array()
}
"#,
        "array-to-array-method-runtime",
        "build",
        &[Value::array(vec![
            Value::string("flate2"),
            Value::string("serde"),
        ])],
    )
    .expect("evaluate Array.to_array()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Array.to_array() result, got {resolution:?}");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result from Array.to_array(), got {value:?}");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].rep, ValueRep::String("flate2".into()));
    assert_eq!(items[1].rep, ValueRep::String("serde".into()));
}

#[test]
fn stream_filter_and_map_pipelines_work_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(items: Array[String]) {
  items
    .filter((item) => item.contains("A"))
    .map((item) => item.to_lower())
    .filter((item) => item.starts_with("wa"))
    .to_array()
}
"#,
        "stream-filter-map-pipeline-runtime",
        "build",
        &[Value::array(vec![
            Value::string("FlAtE2"),
            Value::string("SeRdE"),
            Value::string("WAT"),
        ])],
    )
    .expect("evaluate Stream.filter().map().filter().to_array()");

    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready Stream.filter().map().filter().to_array() result, got {resolution:?}"
        );
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!(
            "expected array result from Stream.filter().map().filter().to_array(), got {value:?}"
        );
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].rep, ValueRep::String("wat".into()));
}

#[test]
fn stream_to_array_sorts_results_by_universal_total_order_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  [3, 1, 2].map((item) => item).to_array()
}
"#,
        "stream-to-array-ordering-runtime",
        "build",
        &[],
    )
    .expect("evaluate Stream.to_array() sorting");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Stream.to_array() sorting result, got {resolution:?}");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result from Stream.to_array(), got {value:?}");
    };
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].rep, ValueRep::U64(1));
    assert_eq!(items[1].rep, ValueRep::U64(2));
    assert_eq!(items[2].rep, ValueRep::U64(3));
}

#[test]
fn stream_for_each_returns_unit_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(items: Array[String]) {
  items.map((item) => item.to_lower()).for_each()
}
"#,
        "stream-for-each-runtime",
        "build",
        &[Value::array(vec![
            Value::string("FlAtE2"),
            Value::string("SeRdE"),
        ])],
    )
    .expect("evaluate Stream.for_each()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Stream.for_each() result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::Unit);
}

#[test]
fn stream_to_map_collects_entry_streams_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(items: Array[String]) {
  items.map((item) => (item, item.len())).to_map()
}
"#,
        "stream-to-map-runtime",
        "build",
        &[Value::array(vec![
            Value::string("serde"),
            Value::string("flate2"),
        ])],
    )
    .expect("evaluate Stream.to_map()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Stream.to_map() result, got {resolution:?}");
    };
    let ValueRep::Map(map) = &value.rep else {
        panic!("expected map result from Stream.to_map(), got {value:?}");
    };
    assert_eq!(map.entries.len(), 2);
    assert_eq!(map.entries[0].0.rep, ValueRep::String("flate2".into()));
    assert_eq!(map.entries[0].1.rep, ValueRep::U64(6));
    assert_eq!(map.entries[1].0.rep, ValueRep::String("serde".into()));
    assert_eq!(map.entries[1].1.rep, ValueRep::U64(5));
}

#[test]
fn stream_to_map_rejects_duplicate_keys_at_runtime() {
    let error = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  ["serde", "serde"].map((item) => (item, item.len())).to_map()
}
"#,
        "stream-to-map-duplicate-runtime",
        "build",
        &[],
    )
    .expect_err("duplicate Stream.to_map() keys should fail");
    assert!(
        error.message.contains("duplicate key"),
        "unexpected error: {error:?}"
    );
}

#[test]
fn stream_flatten_flattens_mapped_arrays_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(items: Array[String]) {
  items
    .map((item) => [item.to_lower(), item])
    .flatten()
    .to_array()
}
"#,
        "stream-flatten-runtime",
        "build",
        &[Value::array(vec![
            Value::string("FlAtE2"),
            Value::string("SeRdE"),
        ])],
    )
    .expect("evaluate Stream.flatten().to_array()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Stream.flatten().to_array() result, got {resolution:?}");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result from Stream.flatten().to_array(), got {value:?}");
    };
    assert_eq!(items.len(), 4);
    assert_eq!(items[0].rep, ValueRep::String("FlAtE2".into()));
    assert_eq!(items[1].rep, ValueRep::String("SeRdE".into()));
    assert_eq!(items[2].rep, ValueRep::String("flate2".into()));
    assert_eq!(items[3].rep, ValueRep::String("serde".into()));
}

#[test]
fn array_flat_map_flattens_array_and_stream_mapper_results_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(items: Array[String]) {
  [
    items.flat_map((item) => [item.to_lower(), item]).to_array(),
    items.flat_map((item) => [item.to_lower()].map((lower) => lower)).to_array(),
  ]
}
"#,
        "array-flat-map-runtime",
        "build",
        &[Value::array(vec![
            Value::string("FlAtE2"),
            Value::string("SeRdE"),
        ])],
    )
    .expect("evaluate Array.flat_map().to_array()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Array.flat_map().to_array() result, got {resolution:?}");
    };
    let ValueRep::Array(groups) = &value.rep else {
        panic!("expected array result from Array.flat_map(), got {value:?}");
    };
    assert_eq!(groups.len(), 2);

    let ValueRep::Array(items) = &groups[0].rep else {
        panic!("expected flattened array result, got {:?}", groups[0]);
    };
    assert_eq!(items.len(), 4);
    assert_eq!(items[0].rep, ValueRep::String("FlAtE2".into()));
    assert_eq!(items[1].rep, ValueRep::String("SeRdE".into()));
    assert_eq!(items[2].rep, ValueRep::String("flate2".into()));
    assert_eq!(items[3].rep, ValueRep::String("serde".into()));

    let ValueRep::Array(stream_items) = &groups[1].rep else {
        panic!("expected flattened stream result, got {:?}", groups[1]);
    };
    assert_eq!(stream_items.len(), 2);
    assert_eq!(stream_items[0].rep, ValueRep::String("flate2".into()));
    assert_eq!(stream_items[1].rep, ValueRep::String("serde".into()));
}

#[test]
fn stream_flat_map_flattens_nested_results_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(items: Array[String]) {
  items
    .map((item) => item)
    .flat_map((item) => [item, item.to_lower()])
    .to_array()
}
"#,
        "stream-flat-map-runtime",
        "build",
        &[Value::array(vec![
            Value::string("FlAtE2"),
            Value::string("SeRdE"),
        ])],
    )
    .expect("evaluate Stream.flat_map().to_array()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Stream.flat_map().to_array() result, got {resolution:?}");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result from Stream.flat_map(), got {value:?}");
    };
    assert_eq!(items.len(), 4);
    assert_eq!(items[0].rep, ValueRep::String("FlAtE2".into()));
    assert_eq!(items[1].rep, ValueRep::String("SeRdE".into()));
    assert_eq!(items[2].rep, ValueRep::String("flate2".into()));
    assert_eq!(items[3].rep, ValueRep::String("serde".into()));
}

#[test]
fn map_map_stream_to_array_projects_entries_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(items: Map[String, String]) {
  items.map((name, _dep) => name).to_array()
}
"#,
        "map-map-method-runtime",
        "build",
        &[Value::map(vec![
            (Value::string("eyre"), Value::string("workspace")),
            (Value::string("flate2"), Value::string("1")),
        ])],
    )
    .expect("evaluate Map.map().to_array()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Map.map().to_array() result, got {resolution:?}");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result from Map.map().to_array(), got {value:?}");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].rep, ValueRep::String("eyre".into()));
    assert_eq!(items[1].rep, ValueRep::String("flate2".into()));
}

#[test]
fn map_to_array_method_returns_key_value_entries_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(items: Map[String, String]) {
  items.to_array()
}
"#,
        "map-to-array-method-runtime",
        "build",
        &[Value::map(vec![
            (Value::string("eyre"), Value::string("workspace")),
            (Value::string("flate2"), Value::string("1")),
        ])],
    )
    .expect("evaluate Map.to_array()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Map.to_array() result, got {resolution:?}");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result from Map.to_array(), got {value:?}");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(
        map_string_entry(&items[0], "key").rep,
        ValueRep::String("eyre".into())
    );
    assert_eq!(
        map_string_entry(&items[0], "value").rep,
        ValueRep::String("workspace".into())
    );
}

#[test]
fn cargo_style_dependency_name_pipeline_works_at_runtime() {
    let manifest = Value::structure(
        "Manifest",
        vec![
            (
                Arc::from("dependencies"),
                Value::map(vec![
                    (Value::string("eyre"), Value::string("workspace")),
                    (Value::string("flate2"), Value::string("1")),
                ]),
            ),
            (
                Arc::from("dev_dependencies"),
                Value::map(vec![(Value::string("insta"), Value::string("1"))]),
            ),
            (Arc::from("build_dependencies"), Value::map(Vec::new())),
        ],
    );
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
struct Manifest {
  dependencies: Map[String, String],
  dev_dependencies: Map[String, String],
  build_dependencies: Map[String, String],
}

struct Summary {
  has_eyre: Bool,
  has_flate2: Bool,
  has_build_deps: Bool,
}

fn dependency_names(manifest: Manifest) {
  [
    manifest.dependencies.map((name, _dep) => name).to_array(),
    manifest.dev_dependencies.map((name, _dep) => name).to_array(),
    manifest.build_dependencies.map((name, _dep) => name).to_array(),
  ].flatten()
}

fn build(manifest: Manifest) {
  let names = dependency_names(manifest);
  Summary {
    has_eyre: names.any((name) => name == "eyre"),
    has_flate2: names.any((name) => name == "flate2"),
    has_build_deps: names.any((name) => name == "build-only-placeholder"),
  }
}
"#,
        "cargo-style-dependency-name-pipeline-runtime",
        "build",
        &[manifest],
    )
    .expect("evaluate cargo-style dependency pipeline");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready cargo-style dependency pipeline result, got {resolution:?}");
    };
    assert_eq!(struct_field(&value, "has_eyre").rep, ValueRep::Bool(true));
    assert_eq!(struct_field(&value, "has_flate2").rep, ValueRep::Bool(true));
    assert_eq!(
        struct_field(&value, "has_build_deps").rep,
        ValueRep::Bool(false)
    );
}

#[test]
fn cargo_style_dependency_name_pipeline_works_from_toml_parse() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
#[untagged]
enum CargoDependencySpec {
  struct Version {
    value: String,
  },

  struct Detailed {
    workspace: Bool?,
  },
}

struct Manifest {
  dependencies: Map[String, CargoDependencySpec] = %{},
  dev_dependencies: Map[String, CargoDependencySpec] = %{},
  build_dependencies: Map[String, CargoDependencySpec] = %{},
}

struct Summary {
  has_eyre: Bool,
  has_flate2: Bool,
  has_build_deps: Bool,
}

fn dependency_names(manifest: Manifest) {
  [
    manifest.dependencies.map((name, _dep) => name).to_array(),
    manifest.dev_dependencies.map((name, _dep) => name).to_array(),
    manifest.build_dependencies.map((name, _dep) => name).to_array(),
  ].flatten()
}

fn build(text: String) {
  let manifest = Toml.parse[Manifest](text);
  let names = dependency_names(manifest);
  Summary {
    has_eyre: names.any((name) => name == "eyre"),
    has_flate2: names.any((name) => name == "flate2"),
    has_build_deps: names.any((name) => name == "build-only-placeholder"),
  }
}
"#,
        "cargo-style-dependency-name-pipeline-from-toml",
        "build",
        &[Value::string(
            r#"[package]
name = "xtask"

[dependencies]
eyre = { workspace = true }
flate2 = "1"
"#,
        )],
    )
    .expect("evaluate cargo-style dependency pipeline from TOML parse");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready cargo-style dependency pipeline TOML result, got {resolution:?}");
    };
    assert_eq!(struct_field(&value, "has_eyre").rep, ValueRep::Bool(true));
    assert_eq!(struct_field(&value, "has_flate2").rep, ValueRep::Bool(true));
    assert_eq!(
        struct_field(&value, "has_build_deps").rep,
        ValueRep::Bool(false)
    );
}

#[test]
fn zoo_cargo_manifest_requests_execute_against_real_manifest() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r#"
struct Summary {
  has_eyre: Bool,
  has_flate2: Bool,
  has_dev_deps: Bool,
  has_build_deps: Bool,
}

fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("xtask/Cargo.toml"));
  let requests =
    manifest_requests_with_workspace(
      manifest,
      workspace_root_dependencies_from_blob(read_file("Cargo.toml")),
    ) where {
    target: BuildTarget {
      triple: "aarch64-apple-darwin",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "macos",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "aarch64",
        },
      ],
    },
  };
  Summary {
    has_eyre: requests.any($.name == "eyre"),
    has_flate2: requests.any($.name == "flate2"),
    has_dev_deps: requests.any($.kind == "dev"),
    has_build_deps: requests.any($.kind == "build"),
  }
}
"#,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let runtime = ReadFileRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session
        .run()
        .expect("run zoo cargo manifest_requests against real manifest");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready zoo cargo manifest_requests result, got {resolution:?}");
    };

    assert_eq!(struct_field(&value, "has_eyre").rep, ValueRep::Bool(true));
    assert_eq!(struct_field(&value, "has_flate2").rep, ValueRep::Bool(true));
    assert_eq!(
        struct_field(&value, "has_dev_deps").rep,
        ValueRep::Bool(false)
    );
    assert_eq!(
        struct_field(&value, "has_build_deps").rep,
        ValueRep::Bool(false)
    );
}

#[test]
fn zoo_cargo_selects_highest_matching_real_registry_entry() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
struct Summary {
  version: String,
  first_normal_dep_req: String,
}

fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("xtask/Cargo.toml"));
  let requests =
    manifest_requests_with_workspace(
      manifest,
      workspace_root_dependencies_from_blob(read_file("Cargo.toml")),
    ) where {
    target: BuildTarget {
      triple: "aarch64-apple-darwin",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "macos",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "aarch64",
        },
      ],
    },
  };
  let request =
    match requests.find($.name == "flate2") (
      Some { value } => value,
      None => fail "missing flate2 request",
    );
  let entries = parse_sparse_entries(read_file("specs/eval/run/registry_real/flate2-two-entries.jsonl"));
  let best = highest_version(candidate_entries(entries, request));
  let first_normal =
    match best.deps.find($.kind.unwrap_or("normal") == "normal") (
      Some { value } => value,
      None => fail "missing normal dependency",
    );
  Summary {
    version: best.vers,
    first_normal_dep_req: first_normal.req,
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let runtime = ReadFileRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session
        .run()
        .expect("run zoo cargo highest-version selection");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready zoo cargo highest-version result, got {resolution:?}");
    };

    assert_eq!(
        struct_field(&value, "version").rep,
        ValueRep::String("1.1.9".into())
    );
    assert_eq!(
        struct_field(&value, "first_normal_dep_req").rep,
        ValueRep::String("^0.3.6".into())
    );
}

#[test]
fn zoo_cargo_semver_req_supports_comma_separated_bounds() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
struct Summary {
  matches_58: Bool,
  matches_60: Bool,
  matches_61: Bool,
  matches_62: Bool,
}

fn build() {
  let req = parse_semver_req(">=0.59, <=0.61");
  Summary {
    matches_58: matches_semver(Some { value: req }, parse_semver_version("0.58.0")),
    matches_60: matches_semver(Some { value: req }, parse_semver_version("0.60.0")),
    matches_61: matches_semver(Some { value: req }, parse_semver_version("0.61.0")),
    matches_62: matches_semver(Some { value: req }, parse_semver_version("0.62.0")),
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let runtime = ReadFileRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session
        .run()
        .expect("run zoo cargo comma-separated semver req");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready zoo cargo comma-separated semver req result, got {resolution:?}");
    };

    assert_eq!(
        struct_field(&value, "matches_58").rep,
        ValueRep::Bool(false)
    );
    assert_eq!(struct_field(&value, "matches_60").rep, ValueRep::Bool(true));
    assert_eq!(struct_field(&value, "matches_61").rep, ValueRep::Bool(true));
    assert_eq!(
        struct_field(&value, "matches_62").rep,
        ValueRep::Bool(false)
    );
}

#[test]
fn zoo_cargo_resolve_request_executes_real_registry_path() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let flate2_index = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../specs/eval/run/registry_real/flate2-two-entries.jsonl"),
    )
    .expect("read real flate2 sparse index entries");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("xtask/Cargo.toml"));
  let request =
    match (manifest_requests_with_workspace(
      manifest,
      workspace_root_dependencies_from_blob(read_file("Cargo.toml")),
    ) where {
      target: BuildTarget {
        triple: "aarch64-apple-darwin",
        cfg: [
          CfgFact.Name { name: "unix" },
          CfgFact.KeyValue {
            key: "target_os",
            value: "macos",
          },
          CfgFact.KeyValue {
            key: "target_arch",
            value: "aarch64",
          },
        ],
      },
    }).find($.name == "flate2") (
      Some { value } => value,
      None => fail "missing flate2 request",
    );
  resolve_request(request) where {
      config: SparseConfig {
        dl: "https://crates.io/api/v1/crates",
      },
      lock: None,
      target: BuildTarget {
        triple: "aarch64-apple-darwin",
        cfg: [
          CfgFact.Name { name: "unix" },
          CfgFact.KeyValue {
            key: "target_os",
            value: "macos",
          },
          CfgFact.KeyValue {
            key: "target_arch",
            value: "aarch64",
          },
        ],
      },
      workspace: Tree("."),
    }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let mut http_responses = HashMap::new();
    http_responses.insert(
        "https://index.crates.io/fl/at/flate2".to_string(),
        flate2_index,
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses,
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session
        .run()
        .expect("run zoo cargo resolve_request against real registry path");
    maybe_print_session_profile(
        "zoo_cargo_resolve_request_executes_real_registry_path",
        &session,
    );
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo resolve_request result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };

    assert_eq!(
        struct_field(&value, "version").rep,
        ValueRep::String("1.1.9".into())
    );

    let source = struct_field(&value, "source");
    assert_eq!(
        struct_field(source, "download_url").rep,
        ValueRep::String("https://crates.io/api/v1/crates/flate2/1.1.9/download".into())
    );

    let dependencies = struct_field(&value, "dependencies");
    let ValueRep::Array(items) = &dependencies.rep else {
        panic!("expected dependencies array, got {dependencies:?}");
    };
    let has_crc32fast = items.iter().any(|dep| {
        matches!(
            &struct_field(dep, "name").rep,
            ValueRep::String(name) if name.as_ref() == "crc32fast"
        )
    });
    let has_miniz_oxide = items.iter().any(|dep| {
        matches!(
            &struct_field(dep, "name").rep,
            ValueRep::String(name) if name.as_ref() == "miniz_oxide"
        )
    });
    let has_cloudflare = items.iter().any(|dep| {
        matches!(
            &struct_field(dep, "name").rep,
            ValueRep::String(name) if name.as_ref() == "cloudflare-zlib-sys"
        )
    });
    let has_targeted_miniz_oxide = items.iter().any(|dep| {
        if !matches!(
            &struct_field(dep, "name").rep,
            ValueRep::String(name) if name.as_ref() == "miniz_oxide"
        ) {
            return false;
        }
        match &struct_field(dep, "target").rep {
            ValueRep::Struct(value) => value.type_name.as_ref() == "Some",
            _ => false,
        }
    });
    assert!(has_crc32fast);
    assert!(has_miniz_oxide);
    assert!(!has_cloudflare);
    assert!(!has_targeted_miniz_oxide);
}

#[test]
fn zoo_cargo_resolve_manifest_backtracks_from_bad_locked_registry_candidate() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("specs/eval/run/registry_real/cargo-backtrack-root.toml"));
  demand_manifest(manifest) where {
    lock:
      Some {
        value:
          CargoLock {
            package: [
              LockedPackage {
                name: "logger",
                version: "1.0.0",
                source: None,
                checksum: None,
              },
            ],
          },
    },
    target: BuildTarget {
      triple: "x86_64-unknown-linux-gnu",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "linux",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "x86_64",
        },
      ],
    },
    workspace: Tree("."),
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let mut http_responses = HashMap::new();
    http_responses.insert(
        "https://index.crates.io/config.json".to_string(),
        r#"{"dl":"https://crates.io/api/v1/crates"}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/lo/gg/logger".to_string(),
        r#"{"name":"logger","vers":"1.0.0","deps":[{"name":"helper","req":"2","package":null,"features":[],"optional":false,"default_features":true,"target":null,"kind":null}],"cksum":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","features":{},"features2":{},"yanked":false,"rust_version":null}
{"name":"logger","vers":"1.1.0","deps":[],"cksum":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","features":{},"features2":{},"yanked":false,"rust_version":null}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/he/lp/helper".to_string(),
        r#"{"name":"helper","vers":"1.0.0","deps":[],"cksum":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","features":{},"features2":{},"yanked":false,"rust_version":null}"#.to_string(),
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses,
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session
        .run()
        .expect("run zoo cargo resolve_manifest backtracking test");
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo backtracking result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };
    let snapshot = session.graph_snapshot();
    let roots = ready_root_ids(&value);

    assert!(roots.contains(&"registry:logger@1.1.0"));
    assert_eq!(
        resolved_crate_version(&snapshot, "logger"),
        Some(Arc::from("1.1.0"))
    );
    assert!(!resolved_crates_contain_name(&snapshot, "helper"));
}

#[test]
fn zoo_cargo_resolve_request_executes_local_path_dependency() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("crates/vx-lsp/Cargo.toml"));
  let request = request_from_dep_at_with_workspace(
    "vx-parser",
    manifest.dependencies.get("vx-parser"),
    "normal",
    @crates/vx-lsp,
    workspace_root_dependencies_from_blob(read_file("Cargo.toml")),
  );
  let resolved = resolve_request(request) where {
    config: SparseConfig {
      dl: "unused",
    },
    lock: None,
    target: BuildTarget {
      triple: "aarch64-apple-darwin",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "macos",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "aarch64",
        },
      ],
    },
    workspace: Tree("."),
  };
  resolved
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses: HashMap::new(),
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session
        .run()
        .expect("run zoo cargo resolve_request against local path dependency");
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo local path resolve result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };

    assert_eq!(
        struct_field(&value, "name").rep,
        ValueRep::String("vx-parser".into())
    );
    assert_eq!(
        struct_field(&value, "version").rep,
        ValueRep::String("0.1.0".into())
    );
    let source = struct_field(&value, "source");
    let path = struct_field(source, "path");
    let ValueRep::External(external) = &path.rep else {
        panic!("expected external path, got {path:?}");
    };
    assert_eq!(external.kind.as_ref(), "Path");
    assert_eq!(external.descriptor.as_ref(), "crates/vx-lsp/../vx-parser");
    let dependencies = struct_field(&value, "dependencies");
    let ValueRep::Array(items) = &dependencies.rep else {
        panic!("expected dependencies array, got {dependencies:?}");
    };
    let has_rowan = items.iter().any(|dep| {
        matches!(
            &struct_field(dep, "name").rep,
            ValueRep::String(name) if name.as_ref() == "rowan"
        )
    });
    assert!(has_rowan);
}

#[test]
fn zoo_cargo_resolve_manifest_executes_local_workspace_manifest_with_real_registry_subset() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("crates/vx-parser/Cargo.toml"));
  demand_manifest_at(manifest, @crates/vx-parser) where {
    lock: None,
    target: BuildTarget {
      triple: "aarch64-apple-darwin",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "macos",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "aarch64",
        },
      ],
    },
    workspace: Tree("."),
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let mut http_responses = HashMap::new();
    http_responses.insert(
        "https://index.crates.io/config.json".to_string(),
        r#"{"dl":"https://crates.io/api/v1/crates"}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/ro/wa/rowan".to_string(),
        fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../specs/eval/run/registry_real/rowan-0.16.1.jsonl"),
        )
        .expect("read real rowan sparse index entries"),
    );
    http_responses.insert(
        "https://index.crates.io/co/un/countme".to_string(),
        fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../specs/eval/run/registry_real/countme-3.0.1.jsonl"),
        )
        .expect("read real countme sparse index entries"),
    );
    http_responses.insert(
        "https://index.crates.io/ha/sh/hashbrown".to_string(),
        fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../specs/eval/run/registry_real/hashbrown-0.14.5.jsonl"),
        )
        .expect("read real hashbrown sparse index entries"),
    );
    http_responses.insert(
        "https://index.crates.io/ru/st/rustc-hash".to_string(),
        fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../specs/eval/run/registry_real/rustc-hash-1.1.0.jsonl"),
        )
        .expect("read real rustc-hash sparse index entries"),
    );
    http_responses.insert(
        "https://index.crates.io/te/xt/text-size".to_string(),
        fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../specs/eval/run/registry_real/text-size-1.1.1.jsonl"),
        )
        .expect("read real text-size sparse index entries"),
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses,
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session
        .run()
        .expect("run zoo cargo resolve_manifest against local workspace manifest");
    maybe_print_session_profile(
        "zoo_cargo_resolve_manifest_executes_local_workspace_manifest_with_real_registry_subset",
        &session,
    );
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo local workspace manifest result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };
    let snapshot = session.graph_snapshot();
    let roots = ready_root_ids(&value);

    assert!(roots.contains(&"registry:rowan@0.16.1"));
    assert!(resolved_crates_contain_name(&snapshot, "rowan"));
    assert!(resolved_crates_contain_name(&snapshot, "countme"));
    assert!(resolved_crates_contain_name(&snapshot, "hashbrown"));
    assert!(resolved_crates_contain_name(&snapshot, "rustc-hash"));
    assert!(resolved_crates_contain_name(&snapshot, "text-size"));
    assert!(!resolved_crates_contain_name(&snapshot, "serde"));
    assert!(!resolved_crates_contain_name(&snapshot, "ahash"));
    assert!(!resolved_crates_contain_name(&snapshot, "allocator-api2"));
}

#[test]
fn zoo_cargo_resolve_manifest_executes_local_workspace_manifest_from_local_sparse_cache() {
    if local_crates_io_sparse_index_root().is_none() {
        return;
    }

    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("crates/vx-parser/Cargo.toml"));
  demand_manifest_at(manifest, @crates/vx-parser) where {
    lock: None,
    target: BuildTarget {
      triple: "aarch64-apple-darwin",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "macos",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "aarch64",
        },
      ],
    },
    workspace: Tree("."),
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let mut http_responses = HashMap::new();
    http_responses.insert(
        "https://index.crates.io/config.json".to_string(),
        fs::read_to_string(
            local_crates_io_sparse_index_root()
                .expect("local sparse index root")
                .join("config.json"),
        )
        .expect("read local sparse config"),
    );
    http_responses.insert(
        local_sparse_package_url("rowan"),
        local_sparse_package_subset_jsonl("rowan", &["0.16.1"])
            .expect("read local sparse subset for rowan"),
    );
    http_responses.insert(
        local_sparse_package_url("countme"),
        local_sparse_package_subset_jsonl("countme", &["3.0.1"])
            .expect("read local sparse subset for countme"),
    );
    http_responses.insert(
        local_sparse_package_url("hashbrown"),
        local_sparse_package_subset_jsonl("hashbrown", &["0.14.5"])
            .expect("read local sparse subset for hashbrown"),
    );
    http_responses.insert(
        local_sparse_package_url("rustc-hash"),
        local_sparse_package_subset_jsonl("rustc-hash", &["1.1.0"])
            .expect("read local sparse subset for rustc-hash"),
    );
    http_responses.insert(
        local_sparse_package_url("text-size"),
        local_sparse_package_subset_jsonl("text-size", &["1.1.1"])
            .expect("read local sparse subset for text-size"),
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses,
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session
        .run()
        .expect("run zoo cargo resolve_manifest against local sparse cache");
    maybe_print_session_profile(
        "zoo_cargo_resolve_manifest_executes_local_workspace_manifest_from_local_sparse_cache",
        &session,
    );
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo local sparse cache result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };
    let snapshot = session.graph_snapshot();
    let roots = ready_root_ids(&value);

    assert!(roots.contains(&"registry:rowan@0.16.1"));
    assert!(resolved_crates_contain_name(&snapshot, "rowan"));
    assert!(resolved_crates_contain_name(&snapshot, "countme"));
    assert!(resolved_crates_contain_name(&snapshot, "hashbrown"));
    assert!(resolved_crates_contain_name(&snapshot, "rustc-hash"));
    assert!(resolved_crates_contain_name(&snapshot, "text-size"));
    assert!(!resolved_crates_contain_name(&snapshot, "serde"));
    assert!(!resolved_crates_contain_name(&snapshot, "ahash"));
    assert!(!resolved_crates_contain_name(&snapshot, "allocator-api2"));
}

#[test]
fn zoo_cargo_resolve_request_executes_workspace_inherited_local_path_dependency() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("specs/eval/run/cargo_workspace_inherited_local_path/crates/app/Cargo.toml"));
  let request = request_from_dep_at_with_workspace(
    "leaf",
    manifest.dependencies.get("leaf"),
    "normal",
    @crates/app,
    workspace_root_dependencies_from_blob(read_file("specs/eval/run/cargo_workspace_inherited_local_path/Cargo.toml")),
  );
  resolve_request(request) where {
    config: SparseConfig {
      dl: "https://crates.io/api/v1/crates",
    },
    lock: None,
    target: BuildTarget {
      triple: "aarch64-apple-darwin",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "macos",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "aarch64",
        },
      ],
    },
    workspace: Tree("specs/eval/run/cargo_workspace_inherited_local_path"),
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses: cargo_feature_join_http_responses(),
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session
        .run()
        .expect("run zoo cargo resolve_request against workspace inherited local path dependency");
    maybe_print_session_profile(
        "zoo_cargo_resolve_request_executes_workspace_inherited_local_path_dependency",
        &session,
    );
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo workspace inherited local path result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };

    assert_eq!(
        struct_field(&value, "name").rep,
        ValueRep::String("leaf".into())
    );
    assert_eq!(
        struct_field(&value, "version").rep,
        ValueRep::String("0.1.0".into())
    );
    let source = struct_field(&value, "source");
    let path = struct_field(source, "path");
    let ValueRep::External(external) = &path.rep else {
        panic!("expected external path, got {path:?}");
    };
    assert_eq!(external.kind.as_ref(), "Path");
    assert_eq!(external.descriptor.as_ref(), "crates/app/../leaf");
    let dependencies = struct_field(&value, "dependencies");
    let ValueRep::Array(items) = &dependencies.rep else {
        panic!("expected dependencies array, got {dependencies:?}");
    };
    assert_eq!(items.len(), 1);
    assert!(matches!(
        &struct_field(&items[0], "name").rep,
        ValueRep::String(name) if name.as_ref() == "shared"
    ));
    assert!(matches!(
        &struct_field(&items[0], "default_features").rep,
        ValueRep::Bool(false)
    ));
    let features = struct_field(&items[0], "features");
    let ValueRep::Array(features) = &features.rep else {
        panic!("expected feature array, got {features:?}");
    };
    assert!(
        features.iter().any(
            |feature| matches!(&feature.rep, ValueRep::String(name) if name.as_ref() == "right")
        )
    );
}

#[test]
fn zoo_cargo_resolve_manifest_joins_local_path_aliases_by_resolved_path() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("specs/eval/run/cargo_local_path_alias/Cargo.toml"));
  demand_manifest_at(manifest, @.) where {
    lock: None,
    target: BuildTarget {
      triple: "aarch64-apple-darwin",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "macos",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "aarch64",
        },
      ],
    },
    workspace: Tree("specs/eval/run/cargo_local_path_alias"),
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let mut http_responses = HashMap::new();
    http_responses.insert(
        "https://index.crates.io/config.json".to_string(),
        r#"{"dl":"https://crates.io/api/v1/crates"}"#.to_string(),
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses,
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session
        .run()
        .expect("run zoo cargo demand_manifest against local path alias manifest");
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo local path alias result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };
    let snapshot = session.graph_snapshot();
    let roots = ready_root_ids(&value);

    assert_eq!(roots, vec!["path:crates/leaf"]);
    assert!(resolved_crates_contain_name(&snapshot, "leaf"));
}

#[test]
fn zoo_cargo_resolve_manifest_executes_workspace_inherited_registry_dependency() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("specs/eval/run/cargo_workspace_inherited_registry/crates/member/Cargo.toml"));
  demand_manifest_at(manifest, @specs/eval/run/cargo_workspace_inherited_registry/crates/member) where {
    lock: None,
    target: BuildTarget {
      triple: "aarch64-apple-darwin",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "macos",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "aarch64",
        },
      ],
    },
    workspace: Tree("specs/eval/run/cargo_workspace_inherited_registry"),
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses: cargo_workspace_inherited_registry_http_responses(),
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session
        .run()
        .expect("run zoo cargo resolve_manifest against workspace inherited registry dependency");
    maybe_print_session_profile(
        "zoo_cargo_resolve_manifest_executes_workspace_inherited_registry_dependency",
        &session,
    );
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo workspace inherited registry result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };
    let snapshot = session.graph_snapshot();
    let roots = ready_root_ids(&value);

    assert!(roots.contains(&"registry:tracing-subscriber@0.3.22"));
    assert!(resolved_crates_contain_name(
        &snapshot,
        "tracing-subscriber"
    ));
    assert!(resolved_crates_contain_name(&snapshot, "matchers"));
    assert!(resolved_crates_contain_name(&snapshot, "regex-automata"));
    assert!(resolved_crates_contain_name(&snapshot, "regex-syntax"));
    assert!(resolved_crates_contain_name(&snapshot, "nu-ansi-term"));
    assert!(resolved_crates_contain_name(&snapshot, "once_cell"));
    assert!(resolved_crates_contain_name(&snapshot, "sharded-slab"));
    assert!(resolved_crates_contain_name(&snapshot, "lazy_static"));
    assert!(resolved_crates_contain_name(&snapshot, "thread_local"));
    assert!(resolved_crates_contain_name(&snapshot, "cfg-if"));
    assert!(resolved_crates_contain_name(&snapshot, "tracing"));
    assert!(resolved_crates_contain_name(&snapshot, "tracing-core"));
    assert!(resolved_crates_contain_name(&snapshot, "pin-project-lite"));
    assert!(!resolved_crates_contain_name(&snapshot, "smallvec"));
    assert!(!resolved_crates_contain_name(&snapshot, "tracing-log"));
    assert!(!resolved_crates_contain_name(&snapshot, "log"));
    assert!(!resolved_crates_contain_name(&snapshot, "windows-sys"));
}

#[test]
fn zoo_cargo_resolve_workspace_member_entrypoint_executes_inherited_registry_dependency() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  demand_workspace_member_at(
    Tree("specs/eval/run/cargo_workspace_inherited_registry"),
    @crates/member,
  ) where {
    lock: None,
    target: BuildTarget {
      triple: "aarch64-apple-darwin",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "macos",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "aarch64",
        },
      ],
    },
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses: cargo_workspace_inherited_registry_http_responses(),
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session
        .run()
        .expect("run zoo cargo workspace member entrypoint");
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo workspace member entrypoint result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };
    let snapshot = session.graph_snapshot();
    let roots = ready_root_ids(&value);

    assert_eq!(roots, vec!["registry:tracing-subscriber@0.3.22"]);
    assert!(resolved_crates_contain_name(
        &snapshot,
        "tracing-subscriber"
    ));
}

#[test]
fn zoo_cargo_resolve_manifest_unifies_features_across_joined_registry_requests() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("specs/eval/run/cargo-feature-join-root.toml"));
  demand_manifest(manifest) where {
    lock: None,
    target: BuildTarget {
      triple: "x86_64-unknown-linux-gnu",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "linux",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "x86_64",
        },
      ],
    },
    workspace: Tree("."),
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let mut http_responses = cargo_feature_join_http_responses();
    http_responses.insert(
        "https://index.crates.io/le/ft/left".to_string(),
        r#"{"name":"left","vers":"1.0.0","deps":[{"name":"shared","req":"^1","features":["left"],"optional":false,"default_features":false,"target":null,"kind":"normal"}],"cksum":"1111111111111111111111111111111111111111111111111111111111111111","features":{},"features2":{},"yanked":false,"rust_version":null,"v":2}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/ri/gh/right".to_string(),
        r#"{"name":"right","vers":"1.0.0","deps":[{"name":"shared","req":"^1","features":["right"],"optional":false,"default_features":true,"target":null,"kind":"normal"}],"cksum":"2222222222222222222222222222222222222222222222222222222222222222","features":{},"features2":{},"yanked":false,"rust_version":null,"v":2}"#.to_string(),
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses,
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session.run().expect("run zoo cargo feature join manifest");
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo feature join result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };
    let snapshot = session.graph_snapshot();
    let roots = ready_root_ids(&value);

    assert!(roots.contains(&"registry:left@1.0.0"));
    assert!(roots.contains(&"registry:right@1.0.0"));
    assert!(resolved_crates_contain_name(&snapshot, "shared"));
    assert!(resolved_crates_contain_name(&snapshot, "defdep"));
    assert!(resolved_crates_contain_name(&snapshot, "leftdep"));
    assert!(resolved_crates_contain_name(&snapshot, "rightdep"));
}

#[test]
fn zoo_cargo_resolve_manifest_joins_normal_and_build_requests_for_same_package() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("specs/eval/run/cargo-kind-join-root.toml"));
  demand_manifest(manifest) where {
    lock: None,
    target: BuildTarget {
      triple: "x86_64-unknown-linux-gnu",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "linux",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "x86_64",
        },
      ],
    },
    workspace: Tree("."),
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses: cargo_feature_join_http_responses(),
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session.run().expect("run zoo cargo kind join manifest");
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo kind join result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };
    let snapshot = session.graph_snapshot();
    let roots = ready_root_ids(&value);

    assert_eq!(roots, ["registry:shared@1.0.0"]);
    assert!(resolved_crates_contain_name(&snapshot, "defdep"));
    assert!(resolved_crates_contain_name(&snapshot, "leftdep"));
    assert!(resolved_crates_contain_name(&snapshot, "rightdep"));
}

#[test]
fn zoo_cargo_resolve_manifest_joins_target_specific_and_unconditional_requests() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("specs/eval/run/cargo-target-join-root.toml"));
  demand_manifest(manifest) where {
    lock: None,
    target: BuildTarget {
      triple: "x86_64-unknown-linux-gnu",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "linux",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "x86_64",
        },
      ],
    },
    workspace: Tree("."),
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses: cargo_feature_join_http_responses(),
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session.run().expect("run zoo cargo target join manifest");
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo target join result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };
    let snapshot = session.graph_snapshot();
    let roots = ready_root_ids(&value);

    assert_eq!(roots, ["registry:shared@1.0.0"]);
    assert!(resolved_crates_contain_name(&snapshot, "defdep"));
    assert!(resolved_crates_contain_name(&snapshot, "leftdep"));
    assert!(resolved_crates_contain_name(&snapshot, "rightdep"));
}

#[test]
fn zoo_cargo_resolve_manifest_applies_weak_dependency_features_after_optional_enable() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("specs/eval/run/cargo-weak-feature-root.toml"));
  demand_manifest(manifest) where {
    lock: None,
    target: BuildTarget {
      triple: "x86_64-unknown-linux-gnu",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "linux",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "x86_64",
        },
      ],
    },
    workspace: Tree("."),
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let mut http_responses = HashMap::new();
    http_responses.insert(
        "https://index.crates.io/config.json".to_string(),
        r#"{"dl":"https://crates.io/api/v1/crates"}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/ca/rr/carrier".to_string(),
        r#"{"name":"carrier","vers":"1.0.0","deps":[{"name":"optdep","req":"^1","features":[],"optional":true,"default_features":false,"target":null,"kind":"normal"}],"cksum":"7777777777777777777777777777777777777777777777777777777777777777","features":{},"features2":{"weak":["optdep?/extra"],"enable":["dep:optdep"]},"yanked":false,"rust_version":null,"v":2}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/op/td/optdep".to_string(),
        r#"{"name":"optdep","vers":"1.0.0","deps":[{"name":"leafdep","req":"^1","features":[],"optional":true,"default_features":false,"target":null,"kind":"normal"}],"cksum":"8888888888888888888888888888888888888888888888888888888888888888","features":{},"features2":{"extra":["dep:leafdep"]},"yanked":false,"rust_version":null,"v":2}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/le/af/leafdep".to_string(),
        r#"{"name":"leafdep","vers":"1.0.0","deps":[],"cksum":"9999999999999999999999999999999999999999999999999999999999999999","features":{},"features2":{},"yanked":false,"rust_version":null,"v":2}"#.to_string(),
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses,
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session.run().expect("run zoo cargo weak feature manifest");
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo weak feature result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };
    let snapshot = session.graph_snapshot();
    let roots = ready_root_ids(&value);

    assert!(roots.contains(&"registry:carrier@1.0.0"));
    assert!(resolved_crates_contain_name(&snapshot, "carrier"));
    assert!(resolved_crates_contain_name(&snapshot, "optdep"));
    assert!(resolved_crates_contain_name(&snapshot, "leafdep"));
}

#[test]
fn zoo_cargo_resolve_manifest_handles_renamed_registry_dependencies() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("specs/eval/run/cargo-renamed-dependency-root.toml"));
  demand_manifest(manifest) where {
    lock: None,
    target: BuildTarget {
      triple: "x86_64-unknown-linux-gnu",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "linux",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "x86_64",
        },
      ],
    },
    workspace: Tree("."),
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let mut http_responses = HashMap::new();
    http_responses.insert(
        "https://index.crates.io/config.json".to_string(),
        r#"{"dl":"https://crates.io/api/v1/crates"}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/ca/rr/carrier".to_string(),
        r#"{"name":"carrier","vers":"1.0.0","deps":[{"name":"renamed","req":"^1","package":"actual","features":[],"optional":true,"default_features":false,"target":null,"kind":"normal"}],"cksum":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","features":{},"features2":{"plus":["dep:renamed","renamed?/bonus"]},"yanked":false,"rust_version":null,"v":2}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/ac/tu/actual".to_string(),
        r#"{"name":"actual","vers":"1.0.0","deps":[{"name":"defaultdep","req":"^1","features":[],"optional":true,"default_features":true,"target":null,"kind":"normal"},{"name":"bonusdep","req":"^1","features":[],"optional":true,"default_features":true,"target":null,"kind":"normal"}],"cksum":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","features":{},"features2":{"default":["dep:defaultdep"],"bonus":["dep:bonusdep"]},"yanked":false,"rust_version":null,"v":2}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/de/fa/defaultdep".to_string(),
        r#"{"name":"defaultdep","vers":"1.0.0","deps":[],"cksum":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","features":{},"features2":{},"yanked":false,"rust_version":null,"v":2}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/bo/nu/bonusdep".to_string(),
        r#"{"name":"bonusdep","vers":"1.0.0","deps":[],"cksum":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","features":{},"features2":{},"yanked":false,"rust_version":null,"v":2}"#.to_string(),
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses,
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session
        .run()
        .expect("run zoo cargo renamed dependency manifest");
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo renamed dependency result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };
    let snapshot = session.graph_snapshot();
    let roots = ready_root_ids(&value);

    assert!(roots.contains(&"registry:carrier@1.0.0"));
    assert!(resolved_crates_contain_name(&snapshot, "carrier"));
    assert!(resolved_crates_contain_name(&snapshot, "actual"));
    assert!(resolved_crates_contain_name(&snapshot, "bonusdep"));
    assert!(!resolved_crates_contain_name(&snapshot, "defaultdep"));
}

#[test]
fn zoo_cargo_resolve_manifest_ignores_root_dev_dependencies_for_build_like_resolution() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("specs/eval/run/cargo-build-root-ignores-dev-dependencies.toml"));
  demand_manifest(manifest) where {
    lock: None,
    target: BuildTarget {
      triple: "x86_64-unknown-linux-gnu",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "linux",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "x86_64",
        },
      ],
    },
    workspace: Tree("."),
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let mut http_responses = HashMap::new();
    http_responses.insert(
        "https://index.crates.io/config.json".to_string(),
        r#"{"dl":"https://crates.io/api/v1/crates"}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/ca/rr/carrier".to_string(),
        r#"{"name":"carrier","vers":"1.0.0","deps":[],"cksum":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","features":{},"features2":{},"yanked":false,"rust_version":null,"v":2}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/bu/il/builderdep".to_string(),
        r#"{"name":"builderdep","vers":"1.0.0","deps":[],"cksum":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","features":{},"features2":{},"yanked":false,"rust_version":null,"v":2}"#.to_string(),
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses,
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session
        .run()
        .expect("run zoo cargo build-like manifest resolution");
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo build-like result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };
    let snapshot = session.graph_snapshot();
    let roots = ready_root_ids(&value);

    assert!(roots.contains(&"registry:carrier@1.0.0"));
    assert!(roots.contains(&"registry:builderdep@1.0.0"));
    assert!(resolved_crates_contain_name(&snapshot, "carrier"));
    assert!(resolved_crates_contain_name(&snapshot, "builderdep"));
    assert!(!resolved_crates_contain_name(
        &snapshot,
        "tree-sitter-vixen"
    ));
}

#[test]
fn zoo_cargo_resolve_manifest_resolves_fixed_revision_github_dependency() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("specs/eval/run/cargo-github-git-root.toml"));
  demand_manifest(manifest) where {
    lock: None,
    target: BuildTarget {
      triple: "x86_64-unknown-linux-gnu",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "linux",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "x86_64",
        },
      ],
    },
    workspace: Tree("."),
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let mut http_responses = HashMap::new();
    http_responses.insert(
        "https://index.crates.io/config.json".to_string(),
        r#"{"dl":"https://crates.io/api/v1/crates"}"#.to_string(),
    );
    http_responses.insert(
        "https://raw.githubusercontent.com/bearcove/tree-sitter-vixen/9fe14bea91399543282035090086fec8a868fb6c/Cargo.toml".to_string(),
        r#"[package]
name = "tree-sitter-vixen"
version = "0.1.0"
edition = "2024"

[dependencies]
tree-sitter-language = "0.1"

[build-dependencies]
cc = "1.0.87"

[dev-dependencies]
ignored-devdep = { git = "https://github.com/example/ignored-devdep", rev = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef" }
"#
        .to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/tr/ee/tree-sitter-language".to_string(),
        r#"{"name":"tree-sitter-language","vers":"0.1.0","deps":[],"cksum":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","features":{},"features2":{},"yanked":false,"rust_version":null,"v":2}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/2/cc".to_string(),
        r#"{"name":"cc","vers":"1.0.87","deps":[],"cksum":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","features":{},"features2":{},"yanked":false,"rust_version":null,"v":2}"#.to_string(),
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses,
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session
        .run()
        .expect("run zoo cargo github git dependency manifest");
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo github git result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };
    let snapshot = session.graph_snapshot();
    let roots = ready_root_ids(&value);

    assert!(
        roots.contains(
            &"git:https://github.com/bearcove/tree-sitter-vixen#9fe14bea91399543282035090086fec8a868fb6c",
        )
    );
    assert!(resolved_crates_contain_name(&snapshot, "tree-sitter-vixen"));
    assert!(resolved_crates_contain_name(
        &snapshot,
        "tree-sitter-language"
    ));
    assert!(resolved_crates_contain_name(&snapshot, "cc"));
    assert!(!resolved_crates_contain_name(&snapshot, "ignored-devdep"));
}

#[test]
fn zoo_cargo_resolve_manifest_executes_transitive_registry_subset() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("specs/eval/run/registry_real/cargo-transitive-root.toml"));
  demand_manifest(manifest) where {
    lock: None,
    target: BuildTarget {
      triple: "x86_64-unknown-linux-gnu",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "linux",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "x86_64",
        },
      ],
    },
    workspace: Tree("."),
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let mut http_responses = HashMap::new();
    http_responses.insert(
        "https://index.crates.io/config.json".to_string(),
        r#"{"dl":"https://crates.io/api/v1/crates"}"#.to_string(),
    );
    http_responses.insert(
        "https://index.crates.io/cr/c3/crc32fast".to_string(),
        fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../specs/eval/run/registry_real/crc32fast-1.5.0.jsonl"),
        )
        .expect("read real crc32fast sparse index entries"),
    );
    http_responses.insert(
        "https://index.crates.io/mi/ni/miniz_oxide".to_string(),
        fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../specs/eval/run/registry_real/miniz_oxide-0.8.9.jsonl"),
        )
        .expect("read real miniz_oxide sparse index entries"),
    );
    http_responses.insert(
        "https://index.crates.io/cf/g-/cfg-if".to_string(),
        fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../specs/eval/run/registry_real/cfg-if-1.0.4.jsonl"),
        )
        .expect("read real cfg-if sparse index entries"),
    );
    http_responses.insert(
        "https://index.crates.io/ad/le/adler2".to_string(),
        fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../specs/eval/run/registry_real/adler2-2.0.1.jsonl"),
        )
        .expect("read real adler2 sparse index entries"),
    );
    http_responses.insert(
        "https://index.crates.io/si/md/simd-adler32".to_string(),
        fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../specs/eval/run/registry_real/simd-adler32-0.3.8.jsonl"),
        )
        .expect("read real simd-adler32 sparse index entries"),
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses,
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session
        .run()
        .expect("run zoo cargo resolve_manifest over transitive registry subset");
    maybe_print_session_profile(
        "zoo_cargo_resolve_manifest_executes_transitive_registry_subset",
        &session,
    );
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo resolve_manifest result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };
    let snapshot = session.graph_snapshot();
    let roots = ready_root_ids(&value);

    assert!(roots.contains(&"registry:crc32fast@1.5.0"));
    assert!(roots.contains(&"registry:miniz_oxide@0.8.9"));
    assert!(resolved_crates_contain_name(&snapshot, "crc32fast"));
    assert!(resolved_crates_contain_name(&snapshot, "cfg-if"));
    assert!(resolved_crates_contain_name(&snapshot, "miniz_oxide"));
    assert!(resolved_crates_contain_name(&snapshot, "adler2"));
    assert!(resolved_crates_contain_name(&snapshot, "simd-adler32"));
}

#[test]
fn zoo_cargo_resolve_manifest_executes_sha2_hello_vertical_slice() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r##"
fn build() {
  let manifest = Toml.parse[CargoManifest](read_file("specs/eval/run/cargo_sha2_hello/Cargo.toml"));
  resolve_manifest_at(manifest, @specs/eval/run/cargo_sha2_hello) where {
    lock: None,
    target: BuildTarget {
      triple: "x86_64-unknown-linux-gnu",
      cfg: [
        CfgFact.Name { name: "unix" },
        CfgFact.KeyValue {
          key: "target_os",
          value: "linux",
        },
        CfgFact.KeyValue {
          key: "target_arch",
          value: "x86_64",
        },
      ],
    },
    workspace: Tree("."),
  }
}
"##,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let runtime = CargoSketchRuntime {
        root_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        http_responses: cargo_sha2_hello_http_responses(),
    };
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");

    let resolution = session
        .run()
        .expect("run zoo cargo resolve_manifest on sha2 hello vertical slice");
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo sha2 hello result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };
    let snapshot = session.graph_snapshot();
    let roots = ready_root_ids(struct_field(&value, "roots"));

    assert_eq!(roots, vec!["registry:sha2@0.10.9"]);
    assert_eq!(
        resolved_crate_version(&snapshot, "sha2").as_deref(),
        Some("0.10.9")
    );
    assert!(resolved_crates_contain_name(&snapshot, "sha2"));
    assert!(resolved_crates_contain_name(&snapshot, "digest"));
    assert!(resolved_crates_contain_name(&snapshot, "cpufeatures"));
    assert!(resolved_crates_contain_name(&snapshot, "cfg-if"));
    assert!(resolved_crates_contain_name(&snapshot, "block-buffer"));
    assert!(resolved_crates_contain_name(&snapshot, "crypto-common"));
    assert!(resolved_crates_contain_name(&snapshot, "generic-array"));
    assert!(resolved_crates_contain_name(&snapshot, "typenum"));
    assert!(resolved_crates_contain_name(&snapshot, "version_check"));
    assert!(!resolved_crates_contain_name(&snapshot, "libc"));
}

#[test]
fn zoo_cargo_dependency_target_matches_any_arch_cfg_expression() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r#"
fn build() {
  let dep = request_from_index_dep(IndexDependency {
    name: "cpufeatures",
    package: None,
    req: "^0.2",
    features: [],
    optional: false,
    default_features: true,
    target: Some {
      value: "cfg(any(target_arch = \"aarch64\", target_arch = \"x86_64\", target_arch = \"x86\"))",
    },
    kind: Some { value: "normal" },
  });

  [
    selector_matches(
      BuildTarget {
        triple: "x86_64-unknown-linux-gnu",
        cfg: [
          CfgFact.Name { name: "unix" },
          CfgFact.KeyValue {
            key: "target_os",
            value: "linux",
          },
          CfgFact.KeyValue {
            key: "target_arch",
            value: "x86_64",
          },
        ],
      },
      TargetSelector.Cfg {
        expr: CfgExpr.KeyValue {
          key: "target_arch",
          value: "x86_64",
        },
      },
    ),
    selector_matches(
      BuildTarget {
        triple: "x86_64-unknown-linux-gnu",
        cfg: [
          CfgFact.Name { name: "unix" },
          CfgFact.KeyValue {
            key: "target_os",
            value: "linux",
          },
          CfgFact.KeyValue {
            key: "target_arch",
            value: "x86_64",
          },
        ],
      },
      parse_target_selector("cfg(target_arch = \"x86_64\")"),
    ),
    dependency_target_matches(
      BuildTarget {
        triple: "x86_64-unknown-linux-gnu",
        cfg: [
          CfgFact.Name { name: "unix" },
          CfgFact.KeyValue {
            key: "target_os",
            value: "linux",
          },
          CfgFact.KeyValue {
            key: "target_arch",
            value: "x86_64",
          },
        ],
      },
      dep,
    ),
    dependency_target_matches(
      BuildTarget {
        triple: "wasm32-unknown-unknown",
        cfg: [
          CfgFact.KeyValue {
            key: "target_arch",
            value: "wasm32",
          },
        ],
      },
      dep,
    ),
  ]
}
"#,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let runtime = UnsupportedRuntime;
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");
    let resolution = session
        .run()
        .expect("run zoo cargo target selector match proof");
    let Resolution::Ready(value) = resolution else {
        panic!(
            "expected ready zoo cargo target selector match result\n{}",
            format_session_diagnostics(&session, &resolution)
        );
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result, got {value:?}");
    };
    let bools = items
        .iter()
        .map(|item| match item.rep {
            ValueRep::Bool(value) => value,
            ref other => panic!("expected bool result, got {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(bools, vec![true, true, true, false]);
}

#[test]
fn array_spread_flattens_array_items_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  [1, ..[2, 3], 4]
}
"#,
        "array-spread-runtime",
        "build",
        &[],
    )
    .expect("evaluate array spread");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready array spread result, got {resolution:?}");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result from spread, got {value:?}");
    };
    assert_eq!(items.len(), 4);
    assert_eq!(items[0].rep, ValueRep::U64(1));
    assert_eq!(items[1].rep, ValueRep::U64(2));
    assert_eq!(items[2].rep, ValueRep::U64(3));
    assert_eq!(items[3].rep, ValueRep::U64(4));
}

#[test]
fn string_lines_method_splits_newlines_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(text: String) {
  text.lines()
}
"#,
        "string-lines-method-runtime",
        "build",
        &[Value::string("one\ntwo")],
    )
    .expect("evaluate String.lines()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready String.lines() result, got {resolution:?}");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result from String.lines(), got {value:?}");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].rep, ValueRep::String("one".into()));
    assert_eq!(items[1].rep, ValueRep::String("two".into()));
}

#[test]
fn string_replace_method_rewrites_all_occurrences_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(template: String) {
  template.replace("{crate}", "flate2")
}
"#,
        "string-replace-method-runtime",
        "build",
        &[Value::string("https://x/{crate}/{crate}")],
    )
    .expect("evaluate String.replace()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready String.replace() result, got {resolution:?}");
    };
    assert_eq!(
        value.rep,
        ValueRep::String("https://x/flate2/flate2".into())
    );
}

#[test]
fn string_to_u64_method_parses_decimal_text_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(text: String) {
  text.to_u64()
}
"#,
        "string-to-u64-method-runtime",
        "build",
        &[Value::string("123")],
    )
    .expect("evaluate String.to_u64()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready String.to_u64() result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::U64(123));
}

#[test]
fn dollar_adapter_expressions_work_for_array_any_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  ["flate2", "serde"].any($ == "flate2")
}
"#,
        "dollar-adapter-array-any-runtime",
        "build",
        &[],
    )
    .expect("evaluate dollar adapter with Array.any()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready dollar adapter Array.any() result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::Bool(true));
}

#[test]
fn ordering_binary_operators_work_for_u64_and_string_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
struct Summary {
  u64_lt: Bool,
  u64_ge: Bool,
  string_lt: Bool,
  array_lt: Bool,
}

fn build() {
  Summary {
    u64_lt: 1 < 2,
    u64_ge: 3 >= 3,
    string_lt: "apple" < "banana",
    array_lt: [1, 2] < [1, 3],
  }
}
"#,
        "ordering-binary-operators-runtime",
        "build",
        &[],
    )
    .expect("evaluate ordering binary operators");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready ordering result, got {resolution:?}");
    };
    assert_eq!(struct_field(&value, "u64_lt").rep, ValueRep::Bool(true));
    assert_eq!(struct_field(&value, "u64_ge").rep, ValueRep::Bool(true));
    assert_eq!(struct_field(&value, "string_lt").rep, ValueRep::Bool(true));
    assert_eq!(struct_field(&value, "array_lt").rep, ValueRep::Bool(true));
}

#[test]
fn arithmetic_binary_operators_work_for_u64_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
struct Summary {
  plus: U64,
  minus: U64,
  times: U64,
  slash: U64,
  remainder: U64,
}

fn build() {
  Summary {
    plus: 2 + 3,
    minus: 7 - 2,
    times: 4 * 3,
    slash: 8 / 2,
    remainder: 7 % 3,
  }
}
"#,
        "arithmetic-binary-operators-runtime",
        "build",
        &[],
    )
    .expect("evaluate arithmetic binary operators");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready arithmetic result, got {resolution:?}");
    };
    assert_eq!(struct_field(&value, "plus").rep, ValueRep::U64(5));
    assert_eq!(struct_field(&value, "minus").rep, ValueRep::U64(5));
    assert_eq!(struct_field(&value, "times").rep, ValueRep::U64(12));
    assert_eq!(struct_field(&value, "slash").rep, ValueRep::U64(4));
    assert_eq!(struct_field(&value, "remainder").rep, ValueRep::U64(1));
}

#[test]
fn string_to_lower_method_lowercases_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(name: String) {
  name.to_lower()
}
"#,
        "string-to-lower-method-runtime",
        "build",
        &[Value::string("FlAtE2")],
    )
    .expect("evaluate String.to_lower()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready String.to_lower() result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::String("flate2".into()));
}

#[test]
fn interpolated_string_evaluates_at_runtime() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(name: String) {
  `https://index.crates.io/{name.to_lower()}`
}
"#,
        "interpolated-string-runtime",
        "build",
        &[Value::string("FlAtE2")],
    )
    .expect("evaluate interpolated string");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready interpolated string result, got {resolution:?}");
    };
    assert_eq!(
        value.rep,
        ValueRep::String("https://index.crates.io/flate2".into())
    );
}

#[test]
fn bound_string_method_value_can_be_called() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build(name: String) {
  let f = name.ends_with;
  f("de")
}
"#,
        "string-bound-method-value-runtime",
        "build",
        &[Value::string("serde")],
    )
    .expect("evaluate bound String.ends_with()");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready bound method result, got {resolution:?}");
    };
    assert_eq!(value.rep, ValueRep::Bool(true));
}

#[test]
fn map_has_and_get_methods_work_at_runtime() {
    let has = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  %{ "left" => 1, "right" => 2 }.has("right")
}
"#,
        "map-has-method-runtime",
        "build",
        &[],
    )
    .expect("evaluate Map.has()");
    let Resolution::Ready(has) = has else {
        panic!("expected ready Map.has() result, got {has:?}");
    };
    assert_eq!(has.rep, ValueRep::Bool(true));

    let get = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  let f = %{ "left" => 1, "right" => 2 }.get;
  f("left")
}
"#,
        "map-get-method-runtime",
        "build",
        &[],
    )
    .expect("evaluate bound Map.get()");
    let Resolution::Ready(get) = get else {
        panic!("expected ready Map.get() result, got {get:?}");
    };
    assert_eq!(get.rep, ValueRep::U64(1));
}

#[test]
fn option_unwrap_or_method_works_at_runtime() {
    let some = eval_source_entry_with_args_with_source_id(
        r#"
fn build(name: String?) {
  name.unwrap_or("fallback")
}
"#,
        "option-unwrap-or-some-runtime",
        "build",
        &[Value::structure(
            "Some",
            vec![(Arc::from("value"), Value::string("serde"))],
        )],
    )
    .expect("evaluate Option.unwrap_or() for Some");
    let Resolution::Ready(some) = some else {
        panic!("expected ready Option.unwrap_or() Some result, got {some:?}");
    };
    assert_eq!(some.rep, ValueRep::String("serde".into()));

    let none = eval_source_entry_with_args_with_source_id(
        r#"
fn build(name: String?) {
  name.unwrap_or("fallback")
}
"#,
        "option-unwrap-or-none-runtime",
        "build",
        &[Value::structure("None", Vec::new())],
    )
    .expect("evaluate Option.unwrap_or() for None");
    let Resolution::Ready(none) = none else {
        panic!("expected ready Option.unwrap_or() None result, got {none:?}");
    };
    assert_eq!(none.rep, ValueRep::String("fallback".into()));
}

#[test]
fn wire_round_trip_preserves_tuple_shape_and_metadata() {
    let value =
        Value::tuple(vec![Value::string("serde"), Value::u64(5)]).with_reuse(ReuseClass::Observed);
    let envelope = value_to_envelope(&value).expect("encode tuple to envelope");
    let decoded = envelope_to_value(envelope).expect("decode tuple from envelope");
    assert_eq!(decoded, value);
}

#[test]
fn wire_round_trip_preserves_range_shape_and_metadata() {
    let value = Value::range(Some(0), Some(3), true).with_reuse(ReuseClass::Observed);
    let envelope = value_to_envelope(&value).expect("encode range to envelope");
    let decoded = envelope_to_value(envelope).expect("decode range from envelope");
    assert_eq!(decoded, value);
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
            callee: &FunctionRef,
            _args: &[Value],
            source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            let callee = callee.name.as_ref();
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
            callee: &FunctionRef,
            _args: &[Value],
            source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            let callee = callee.name.as_ref();
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
            callee: &FunctionRef,
            _args: &[Value],
            source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            let callee = callee.name.as_ref();
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
            callee: &FunctionRef,
            _args: &[Value],
            source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            let callee = callee.name.as_ref();
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
            callee: &FunctionRef,
            _args: &[Value],
            source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            let callee = callee.name.as_ref();
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
            callee: &FunctionRef,
            _args: &[Value],
            source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            let callee = callee.name.as_ref();
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
fn array_method_find_returns_some_on_match() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  ["logger", "codec", "codec"].find((value) => value == "codec")
}
"#,
        "array-method-find-some",
        "build",
        &[],
    )
    .expect("evaluate Array.find with match");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Array.find result, got {resolution:?}");
    };
    let ValueRep::Struct(option) = &value.rep else {
        panic!("expected Some(..), got {value:?}");
    };
    assert_eq!(option.type_name.as_ref(), "Some");
    assert_eq!(
        struct_field(&value, "value").rep,
        ValueRep::String("codec".into())
    );
}

#[test]
fn array_method_find_returns_none_on_no_match() {
    let resolution = eval_source_entry_with_args_with_source_id(
        r#"
fn build() {
  [1, 2].find((value) => value == 9)
}
"#,
        "array-method-find-none",
        "build",
        &[],
    )
    .expect("evaluate Array.find without match");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready Array.find result, got {resolution:?}");
    };
    let ValueRep::Struct(option) = &value.rep else {
        panic!("expected None, got {value:?}");
    };
    assert_eq!(option.type_name.as_ref(), "None");
    assert!(option.fields.is_empty());
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
            callee: &FunctionRef,
            _args: &[Value],
            source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            let callee = callee.name.as_ref();
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
            callee: &FunctionRef,
            _args: &[Value],
            source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            let callee = callee.name.as_ref();
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

#[test]
fn session_profile_reports_elapsed_work_by_node() {
    let program = EvalProgram::single_source("session-profile", "fn build() { slow_value() }");
    let runtime = StaggeredRuntime;
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "win");
    let Resolution::Ready(value) = session.run().expect("run profile session") else {
        panic!("expected ready profile session result");
    };
    assert_eq!(value.rep, ValueRep::String("slow".into()));

    let profile = session.profile();
    assert!(profile.total_time >= Duration::from_millis(40));
    assert!(!profile.entries.is_empty());
    assert!(profile.entries.iter().any(|entry| {
        entry.work_kind == EvalWorkKind::EvaluateExpr
            && entry.total_time >= Duration::from_millis(40)
    }));

    let summary = session.profile_summary();
    assert_eq!(summary.total_time, profile.total_time);
    assert!(!summary.entries.is_empty());
    assert!(summary.entries.iter().any(|entry| {
        entry.work_kind == EvalWorkKind::EvaluateExpr
            && entry.total_time >= Duration::from_millis(40)
    }));

    let report = session.profile_report(5);
    assert!(report.contains("eval2 profile total="));
    assert!(report.contains("summary_entries="));
    assert!(report.contains("work=EvaluateExpr"));
}

#[test]
fn session_call_profile_tracks_nested_authored_calls_across_pending_resume() {
    struct ProfilePendingRuntime {
        calls: Arc<AtomicUsize>,
    }

    impl RuntimeHooks for ProfilePendingRuntime {
        fn call(
            &self,
            place: &str,
            callee: &FunctionRef,
            _args: &[Value],
            source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            let callee = callee.name.as_ref();
            match (place, callee) {
                ("win", "host_wait_bool") => {
                    if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        Ok(RuntimeCallOutcome::Pending {
                            description: "waiting for profile predicate".into(),
                        })
                    } else {
                        Ok(RuntimeCallOutcome::Ready(
                            Value::bool(true).with_provenance(Provenance::host_probe(
                                source.clone(),
                                place,
                                "host_wait_bool",
                            )),
                        ))
                    }
                }
                _ => Ok(RuntimeCallOutcome::Unsupported),
            }
        }
    }

    let runtime = ProfilePendingRuntime {
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let mut session = EvalSession::new(
        r#"
fn host_wait_bool();

fn leaf(value: U64) {
  if host_wait_bool() ( value ) else ( 0 )
}

fn outer(value: U64) {
  leaf(value)
}

fn build() {
  [outer(7), outer(7)]
}
"#,
        "session-call-profile",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );
    session.set_call_profiling_enabled(true);

    let Resolution::Pending(pending) = session.run().expect("start profiled session") else {
        panic!("expected pending profiled session");
    };
    session
        .resolve_pending(&pending, Value::bool(true))
        .expect("resolve profiled pending call");

    let Resolution::Ready(value) = session.run().expect("finish profiled session") else {
        panic!("expected ready profiled session");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected build array result, got {value:?}");
    };
    assert_eq!(items.len(), 2);

    let profile = session.call_profile();
    assert!(!profile.runs.is_empty());
    assert_eq!(profile.roots.len(), 1);
    assert_eq!(profile.roots[0].callee.as_ref(), "build");

    let build = &profile.roots[0];
    let outer = build
        .children
        .iter()
        .find(|entry| entry.callee.as_ref() == "outer")
        .expect("build should call outer");
    assert_eq!(outer.runs, 2);
    assert_eq!(outer.distinct_input_count, 1);
    assert_eq!(outer.repeated_input_count, 1);

    let leaf = outer
        .children
        .iter()
        .find(|entry| entry.callee.as_ref() == "leaf")
        .expect("outer should call leaf");
    assert_eq!(leaf.runs, 2);
    assert_eq!(leaf.distinct_input_count, 1);
    assert_eq!(leaf.repeated_input_count, 1);

    let summary = profile
        .summary
        .iter()
        .find(|entry| entry.callee.as_ref() == "leaf")
        .expect("leaf summary entry");
    assert_eq!(summary.runs, 2);
    assert_eq!(summary.distinct_input_count, 1);
    assert_eq!(summary.repeated_input_count, 1);

    let report = session.call_profile_report(8);
    assert!(report.contains("eval2 call profile attributed="));
    assert!(report.contains("call tree:"));
    assert!(report.contains("callee=build"));
    assert!(report.contains("callee=outer"));
    assert!(report.contains("callee=leaf"));
}

#[test]
fn session_call_profile_tracks_builtin_calls() {
    let runtime = UnsupportedRuntime;
    let mut session = EvalSession::new(
        r#"
fn build() {
  "ABC".to_lower().contains("ab")
}
"#,
        "session-call-profile-builtins",
        "build",
        Vec::new(),
        &runtime,
        "win",
    );
    session.set_call_profiling_enabled(true);

    let Resolution::Ready(value) = session.run().expect("run profiled builtin session") else {
        panic!("expected ready profiled builtin session");
    };
    let ValueRep::Bool(result) = value.rep else {
        panic!("expected bool result, got {value:?}");
    };
    assert!(result);

    let profile = session.call_profile();
    let summary = &profile.summary;
    assert!(
        summary
            .iter()
            .any(|entry| entry.callee.as_ref() == "String.to_lower")
    );
    assert!(
        summary
            .iter()
            .any(|entry| entry.callee.as_ref() == "String.contains")
    );

    let build = &profile.roots[0];
    assert!(
        build
            .children
            .iter()
            .any(|entry| entry.callee.as_ref() == "String.contains")
    );
    assert!(
        build
            .children
            .iter()
            .any(|entry| entry.callee.as_ref() == "String.to_lower")
    );
}

#[test]
fn session_call_profile_isolates_cargo_parse_semver_version() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r#"
fn build() {
  [
    parse_semver_version("1.2.3"),
    parse_semver_version("1.2.3"),
    parse_semver_version("1.2.3"),
    parse_semver_version("1.2.3"),
    parse_semver_version("1.2.3"),
    parse_semver_version("1.2.3"),
  ]
}
"#,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let runtime = UnsupportedRuntime;
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");
    session.set_call_profiling_enabled(true);

    let Resolution::Ready(value) = session.run().expect("run isolated semver version profile")
    else {
        panic!("expected ready isolated semver version profile result");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result, got {value:?}");
    };
    assert_eq!(items.len(), 6);
    maybe_print_session_profile(
        "session_call_profile_isolates_cargo_parse_semver_version",
        &session,
    );

    let summary = session.call_profile().summary;
    let entry = summary
        .iter()
        .find(|entry| entry.callee.as_ref() == "parse_semver_version")
        .expect("parse_semver_version summary entry");
    assert_eq!(entry.runs, 6);
    assert_eq!(entry.distinct_input_count, 1);
    assert_eq!(entry.repeated_input_count, 5);
}

#[test]
fn session_call_profile_isolates_cargo_parse_semver_req() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let program = test_program(
        [
            (
                "main.vx",
                r#"
fn build() {
  [
    parse_semver_req("^1.2.3"),
    parse_semver_req("^1.2.3"),
    parse_semver_req("^1.2.3"),
    parse_semver_req("^1.2.3"),
    parse_semver_req("^1.2.3"),
    parse_semver_req("^1.2.3"),
  ]
}
"#,
            ),
            ("cargo-resolve.vx", &zoo_source),
        ],
        vec![EvalProgramImport { from: 0, to: 1 }],
    );
    let runtime = UnsupportedRuntime;
    let mut session = EvalSession::new_program(program, "build", Vec::new(), &runtime, "local");
    session.set_call_profiling_enabled(true);

    let Resolution::Ready(value) = session.run().expect("run isolated semver req profile") else {
        panic!("expected ready isolated semver req profile result");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result, got {value:?}");
    };
    assert_eq!(items.len(), 6);
    maybe_print_session_profile(
        "session_call_profile_isolates_cargo_parse_semver_req",
        &session,
    );

    let profile = session.call_profile();
    let req = profile
        .summary
        .iter()
        .find(|entry| entry.callee.as_ref() == "parse_semver_req")
        .expect("parse_semver_req summary entry");
    assert_eq!(req.runs, 6);
    assert_eq!(req.distinct_input_count, 1);
    assert_eq!(req.repeated_input_count, 5);

    let version = profile
        .summary
        .iter()
        .find(|entry| entry.callee.as_ref() == "parse_semver_version")
        .expect("parse_semver_version summary entry");
    assert_eq!(version.runs, 6);
    assert_eq!(version.distinct_input_count, 1);
    assert_eq!(version.repeated_input_count, 5);
}

struct UnsupportedRuntime;

impl RuntimeHooks for UnsupportedRuntime {
    fn call(
        &self,
        _place: &str,
        _callee: &FunctionRef,
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
        callee: &FunctionRef,
        args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        let callee = callee.name.as_ref();
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
        ValueRep::Tuple(items) => Value::tuple(
            items
                .iter()
                .cloned()
                .map(normalized_slot_witness)
                .collect::<Vec<_>>(),
        ),
        ValueRep::Range(range) => Value::range(range.start, range.end, range.inclusive_end),
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
fn runtime_hooks_receive_where_keyword_bindings_in_callee_captures() {
    struct KeywordRuntime;

    impl RuntimeHooks for KeywordRuntime {
        fn call(
            &self,
            _place: &str,
            callee: &FunctionRef,
            args: &[Value],
            _source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            if callee.name() != "acquire" {
                return Ok(RuntimeCallOutcome::Unsupported);
            }
            let [url] = args else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "acquire() should receive one positional argument",
                ));
            };
            let ValueRep::String(url) = &url.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "acquire() url should evaluate to String",
                ));
            };
            let digest = callee
                .captures()
                .iter()
                .find(|(name, _)| name.as_ref() == "digest")
                .map(|(_, value)| value)
                .expect("digest capture");
            let extract = callee
                .captures()
                .iter()
                .find(|(name, _)| name.as_ref() == "extract")
                .map(|(_, value)| value)
                .expect("extract capture");
            Ok(RuntimeCallOutcome::Ready(Value::string(format!(
                "{url}|{:?}|{:?}",
                digest.rep, extract.rep
            ))))
        }
    }

    let resolution = eval_source_entry_at_place_with_args_with_source_id(
        r#"
fn build() {
  acquire("https://example.test/src.tar.gz") where {
    digest: "sha256:00112233",
    extract: "tar.gz",
  }
}
"#,
        "runtime-where-keywords",
        "build",
        &[],
        &KeywordRuntime,
        "local",
    )
    .expect("evaluate runtime call with where bindings");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready runtime capture result, got {resolution:?}");
    };
    assert_eq!(
        value.rep,
        ValueRep::String(
            "https://example.test/src.tar.gz|String(\"sha256:00112233\")|String(\"tar.gz\")".into()
        )
    );
}

#[test]
fn where_resumes_pending_keyword_bindings_before_runtime_call() {
    struct PendingKeywordRuntime;

    impl RuntimeHooks for PendingKeywordRuntime {
        fn call(
            &self,
            place: &str,
            callee: &FunctionRef,
            args: &[Value],
            _source: &SourceSpan,
        ) -> Result<RuntimeCallOutcome, EvalError> {
            match (place, callee.name(), args) {
                ("win", "host_wait", []) => Ok(RuntimeCallOutcome::Pending {
                    description: "waiting for where keyword binding".into(),
                }),
                ("win", "acquire", [url]) => {
                    let ValueRep::String(url) = &url.rep else {
                        return Err(EvalError::new(
                            EvalErrorKind::TypeError,
                            "acquire() url should evaluate to String",
                        ));
                    };
                    let digest = callee
                        .captures()
                        .iter()
                        .find(|(name, _)| name.as_ref() == "digest")
                        .map(|(_, value)| value)
                        .expect("digest capture");
                    Ok(RuntimeCallOutcome::Ready(Value::string(format!(
                        "{url}|{:?}",
                        digest.rep
                    ))))
                }
                _ => Ok(RuntimeCallOutcome::Unsupported),
            }
        }
    }

    let mut session = EvalSession::new(
        r#"
fn host_wait();
fn acquire(url: String);

fn build() {
  acquire("https://example.test/src.tar.gz") where {
    digest: host_wait(),
  }
}
"#,
        "runtime-where-pending-keyword",
        "build",
        Vec::new(),
        &PendingKeywordRuntime,
        "win",
    );

    let Resolution::Pending(pending) = session.run().expect("start pending where keyword binding")
    else {
        panic!("expected pending where keyword binding");
    };
    session
        .resolve_pending(&pending, Value::string("sha256:44556677"))
        .expect("resolve where keyword binding");

    let Resolution::Ready(value) = session.run().expect("resume where keyword binding result")
    else {
        panic!("expected ready result after pending where keyword binding");
    };
    assert_eq!(
        value.rep,
        ValueRep::String("https://example.test/src.tar.gz|String(\"sha256:44556677\")".into())
    );
}

#[test]
fn fallible_operator_wraps_ready_runtime_values_in_some() {
    let resolution = eval_source_entry_at_place_with_args_with_source_id(
        r#"
fn host_os();

fn build() {
  host_os()?
}
"#,
        "fallible-runtime-ready",
        "build",
        &[],
        &TestRuntime,
        "win",
    )
    .expect("evaluate fallible ready runtime call");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready fallible runtime result, got {resolution:?}");
    };
    let ValueRep::Struct(option) = &value.rep else {
        panic!("expected Some option, got {:?}", value.rep);
    };
    assert_eq!(option.type_name.as_ref(), "Some");
    assert_eq!(
        struct_field(&value, "value").rep,
        ValueRep::String("windows".into())
    );
}

#[test]
fn fallible_operator_converts_runtime_errors_into_none() {
    let resolution = eval_source_entry_at_place_with_args_with_source_id(
        r#"
fn host_missing();

fn build() {
  host_missing()?
}
"#,
        "fallible-runtime-error",
        "build",
        &[],
        &TestRuntime,
        "win",
    )
    .expect("evaluate fallible missing runtime call");

    let Resolution::Ready(value) = resolution else {
        panic!("expected ready fallible none result, got {resolution:?}");
    };
    let ValueRep::Struct(option) = &value.rep else {
        panic!("expected None option, got {:?}", value.rep);
    };
    assert_eq!(option.type_name.as_ref(), "None");
}

#[test]
fn fallible_operator_resumes_pending_runtime_values_into_some() {
    let mut session = EvalSession::new(
        r#"
fn host_wait();

fn build() {
  host_wait()?
}
"#,
        "fallible-runtime-pending-ready",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    let Resolution::Pending(pending) = session.run().expect("start fallible pending runtime call")
    else {
        panic!("expected pending fallible runtime call");
    };

    session
        .resolve_pending(&pending, Value::string("windows"))
        .expect("resolve fallible pending value");

    let Resolution::Ready(value) = session.run().expect("finish fallible pending runtime call")
    else {
        panic!("expected ready fallible runtime result");
    };
    let ValueRep::Struct(option) = &value.rep else {
        panic!("expected Some option, got {:?}", value.rep);
    };
    assert_eq!(option.type_name.as_ref(), "Some");
    assert_eq!(
        struct_field(&value, "value").rep,
        ValueRep::String("windows".into())
    );
}

#[test]
fn fallible_operator_converts_pending_runtime_poison_into_none() {
    let mut session = EvalSession::new(
        r#"
fn host_wait();

fn build() {
  host_wait()?
}
"#,
        "fallible-runtime-pending-poison",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    let Resolution::Pending(pending) = session.run().expect("start fallible pending runtime call")
    else {
        panic!("expected pending fallible runtime call");
    };

    session
        .poison_pending(
            &pending,
            EvalError::new(EvalErrorKind::UserFail, "host observation failed"),
        )
        .expect("poison fallible pending value");

    let Resolution::Ready(value) = session.run().expect("finish fallible pending runtime call")
    else {
        panic!("expected ready fallible none result");
    };
    let ValueRep::Struct(option) = &value.rep else {
        panic!("expected None option, got {:?}", value.rep);
    };
    assert_eq!(option.type_name.as_ref(), "None");
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
fn external_values_support_authored_equality_by_kind_and_descriptor() {
    let equal = eval_source_entry_with_args_with_source_id(
        "fn build(left: Path, right: Path) { left == right }",
        "external-equality-equal",
        "build",
        &[
            Value::external("Path", "src/main.rs"),
            Value::external("Path", "src/main.rs"),
        ],
    )
    .expect("evaluate external equality");
    let Resolution::Ready(equal) = equal else {
        panic!("expected ready equality result, got {equal:?}");
    };
    assert_eq!(equal.rep, ValueRep::Bool(true));

    let unequal = eval_source_entry_with_args_with_source_id(
        "fn build(left: Path, right: Path) { left == right }",
        "external-equality-unequal",
        "build",
        &[
            Value::external("Path", "src/main.rs"),
            Value::external("Path", "src/lib.rs"),
        ],
    )
    .expect("evaluate external inequality");
    let Resolution::Ready(unequal) = unequal else {
        panic!("expected ready inequality result, got {unequal:?}");
    };
    assert_eq!(unequal.rep, ValueRep::Bool(false));
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
        callee: &FunctionRef,
        args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        let callee = callee.name.as_ref();
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
        callee: &FunctionRef,
        _args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        let callee = callee.name.as_ref();
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
        callee: &FunctionRef,
        _args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        let callee = callee.name.as_ref();
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
        callee: &FunctionRef,
        _args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        let callee = callee.name.as_ref();
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
        callee: &FunctionRef,
        _args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        let callee = callee.name.as_ref();
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
        callee: &FunctionRef,
        _args: &[Value],
        source: &SourceSpan,
    ) -> Result<RuntimeCallOutcome, EvalError> {
        let callee = callee.name.as_ref();
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

    let value = loop {
        match session.step().expect("finish queued parent work") {
            EvalSessionStep::Progressed => {}
            EvalSessionStep::Ready(value) => break value,
            other => panic!("expected progress or ready, got {other:?}"),
        }
    };
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
fn prepared_work_and_completion_report_observed_base_generation() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "prepared-base-generation",
        "build",
        Vec::new(),
        &TestRuntime,
        "win",
    );

    assert_eq!(session.base_generation(), 0);
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

    let leased_generation = session.base_generation();
    let first = session.prepare_step().expect("first leased sibling");
    let second = session.prepare_step().expect("second leased sibling");
    let first_work_id = first.work().id;
    let second_work_id = second.work().id;
    assert_eq!(first.base_generation(), leased_generation);
    assert_eq!(second.base_generation(), leased_generation);

    let second_completion = second.execute().expect("second completion");
    assert_eq!(second_completion.base_generation(), leased_generation);
    assert_eq!(
        session
            .apply_completion(second_completion)
            .expect("out-of-order completion should buffer"),
        EvalSessionStep::WaitingOnInFlight,
    );
    assert_eq!(session.base_generation(), leased_generation);

    let first_completion = first.execute().expect("first completion");
    assert_eq!(first_completion.base_generation(), leased_generation);
    assert_eq!(
        session
            .apply_completion(first_completion)
            .expect("first completion should commit"),
        EvalSessionStep::Progressed,
    );
    assert!(session.base_generation() >= 2);
    assert!(
        session
            .completed_work()
            .iter()
            .filter(
                |work| matches!(work.work.id, id if id == first_work_id || id == second_work_id)
            )
            .all(|work| work.base_generation == leased_generation)
    );
}

#[test]
fn stale_completion_requeues_leased_work_when_validation_fails() {
    let mut session = EvalSession::new(
        "fn build() { [host_os(), host_os()] }",
        "prepared-validation-replay",
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

    let leased_generation = session.base_generation();
    let first = session.prepare_step().expect("first leased sibling");
    let first_node = first.work().node_id;
    let second = session.prepare_step().expect("second leased sibling");
    let second_work = second.work().clone();

    let mut second_completion = second.execute().expect("second completion");
    second_completion.inject_node_state_read_for_validation(first_node);
    assert_eq!(
        session
            .apply_completion(second_completion)
            .expect("out-of-order completion should buffer"),
        EvalSessionStep::WaitingOnInFlight,
    );

    let first_completion = first.execute().expect("first completion");
    assert_eq!(
        session
            .apply_completion(first_completion)
            .expect("first completion should commit and replay stale sibling"),
        EvalSessionStep::Progressed,
    );
    assert!(session.buffered_work().is_empty());
    assert!(
        session
            .queued_work()
            .iter()
            .any(|work| work.id == second_work.id),
        "stale leased work should be requeued, got {:?}",
        session.queued_work()
    );
    assert!(
        session
            .completed_work()
            .iter()
            .all(|completed| completed.work.id != second_work.id),
        "stale completion must not commit before replay",
    );

    let replayed = session.prepare_step().expect("replayed sibling");
    assert_eq!(replayed.work().id, second_work.id);
    assert!(replayed.base_generation() > leased_generation);
    assert_eq!(
        session
            .apply_completion(replayed.execute().expect("replayed completion"))
            .expect("replayed completion should apply"),
        EvalSessionStep::Progressed,
    );

    let Resolution::Ready(value) = session.run().expect("final replayed resolution") else {
        panic!("expected ready replayed resolution");
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
fn stale_buffered_completion_replays_against_new_base_snapshot() {
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
        "prepared-replay-keyed",
        "build",
        Vec::new(),
        &UnsupportedRuntime,
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

    let leased_generation = session.base_generation();
    let first = session.prepare_step().expect("first keyed sibling");
    let second = session.prepare_step().expect("second keyed sibling");

    let second_completion = second.execute().expect("second completion");
    assert_eq!(
        session
            .apply_completion(second_completion)
            .expect("out-of-order completion should buffer"),
        EvalSessionStep::WaitingOnInFlight,
    );

    let first_completion = first.execute().expect("first completion");
    assert_eq!(
        session
            .apply_completion(first_completion)
            .expect("first completion should commit and replay stale sibling"),
        EvalSessionStep::Progressed,
    );
    assert!(session.buffered_work().is_empty());
    assert!(
        !session.queued_work().is_empty(),
        "stale sibling replay should leave follow-up work queued, got {:?}",
        session.queued_work()
    );
    assert!(
        session.base_generation() > leased_generation,
        "stale sibling handling should advance the base snapshot",
    );

    let Resolution::Ready(value) = session.run().expect("final keyed resolution") else {
        panic!("expected ready keyed resolution");
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
            panic!("expected keyed result array, got {:?}", row.rep);
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].rep, ValueRep::String("1.0.0".into()));
        assert_eq!(items[1].rep, ValueRep::String("1.1.0".into()));
    }

    let keyed_nodes = session
        .graph_snapshot()
        .nodes
        .into_iter()
        .filter(|node| node.keyed.is_some())
        .collect::<Vec<_>>();
    assert_eq!(keyed_nodes.len(), 1);
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
fn run_parallel_array_fold_runtime_steps_remain_sequential_today() {
    let active_calls = Arc::new(AtomicUsize::new(0));
    let max_calls = Arc::new(AtomicUsize::new(0));
    let runtime = ParallelObserveRuntime {
        active_calls,
        max_calls: max_calls.clone(),
    };
    let mut session = EvalSession::new(
        r#"
fn build() {
  array_fold(
    [1, 2, 3, 4],
    0,
    ((acc, item) => {
      let _os = host_os();
      acc + item
    }),
  )
}
"#,
        "parallel-array-fold-sequential",
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
    assert_eq!(value.rep, ValueRep::U64(10));
    assert_eq!(
        max_calls.load(Ordering::SeqCst),
        1,
        "array_fold currently drives runtime-heavy steps one at a time",
    );
}

#[test]
fn run_parallel_array_map_to_array_can_overlap_runtime_steps() {
    let active_calls = Arc::new(AtomicUsize::new(0));
    let max_calls = Arc::new(AtomicUsize::new(0));
    let runtime = ParallelObserveRuntime {
        active_calls,
        max_calls: max_calls.clone(),
    };
    let mut session = EvalSession::new(
        r#"
fn build() {
  [1, 2, 3, 4].map((_item) => host_os()).to_array()
}
"#,
        "parallel-array-map-sequential",
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
    let ValueRep::Array(items) = value.rep else {
        panic!("expected array value");
    };
    assert_eq!(items.len(), 4);
    assert!(
        items
            .iter()
            .all(|item| item.rep == ValueRep::String("windows".into()))
    );
    assert!(
        max_calls.load(Ordering::SeqCst) >= 2,
        "Array.map(...).to_array() should overlap sibling runtime calls",
    );
}

#[test]
fn run_parallel_array_map_to_for_each_can_overlap_runtime_steps() {
    let active_calls = Arc::new(AtomicUsize::new(0));
    let max_calls = Arc::new(AtomicUsize::new(0));
    let runtime = ParallelObserveRuntime {
        active_calls,
        max_calls: max_calls.clone(),
    };
    let mut session = EvalSession::new(
        r#"
fn build() {
  [1, 2, 3, 4].map((_item) => host_os()).for_each()
}
"#,
        "parallel-array-map-for-each-stream",
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
    assert_eq!(value.rep, ValueRep::Unit);
    assert!(
        max_calls.load(Ordering::SeqCst) >= 2,
        "Array.map(...).for_each() should overlap sibling runtime calls",
    );
}

#[test]
fn run_parallel_array_filter_to_array_can_overlap_runtime_steps() {
    let active_calls = Arc::new(AtomicUsize::new(0));
    let max_calls = Arc::new(AtomicUsize::new(0));
    let runtime = ParallelObserveRuntime {
        active_calls,
        max_calls: max_calls.clone(),
    };
    let mut session = EvalSession::new(
        r#"
fn build() {
  [1, 2, 3, 4].filter((_item) => host_os() == "windows").to_array()
}
"#,
        "parallel-array-filter-stream",
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
    let ValueRep::Array(items) = value.rep else {
        panic!("expected array value");
    };
    assert_eq!(items.len(), 4);
    assert!(
        max_calls.load(Ordering::SeqCst) >= 2,
        "Array.filter(...).to_array() should overlap sibling runtime calls",
    );
}

#[test]
fn run_parallel_map_map_to_array_can_overlap_runtime_steps() {
    let active_calls = Arc::new(AtomicUsize::new(0));
    let max_calls = Arc::new(AtomicUsize::new(0));
    let runtime = ParallelObserveRuntime {
        active_calls,
        max_calls: max_calls.clone(),
    };
    let mut session = EvalSession::new(
        r#"
fn build() {
  %{ "a" => 1, "b" => 2, "c" => 3, "d" => 4 }.map((_name, _value) => host_os()).to_array()
}
"#,
        "parallel-map-map-stream",
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
    let ValueRep::Array(items) = value.rep else {
        panic!("expected array value");
    };
    assert_eq!(items.len(), 4);
    assert!(
        items
            .iter()
            .all(|item| item.rep == ValueRep::String("windows".into()))
    );
    assert!(
        max_calls.load(Ordering::SeqCst) >= 2,
        "Map.map(...).to_array() should overlap sibling runtime calls",
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
fn keyed_nodes_distinguish_resolver_captures() {
    let mut session = EvalSession::new(
        r#"
fn build() {
  let mk = ((prefix) => ((ctx) => prefix));
  [
    keyed("logger", [], mk("a")),
    keyed("logger", [], mk("a")),
    keyed("logger", [], mk("b")),
  ]
}
"#,
        "keyed-resolver-captures",
        "build",
        Vec::new(),
        &UnsupportedRuntime,
        "win",
    );

    let Resolution::Ready(value) = session.run().expect("evaluate keyed closure captures") else {
        panic!("expected ready keyed closure-capture result");
    };
    let ValueRep::Array(items) = &value.rep else {
        panic!("expected array result, got {:?}", value.rep);
    };
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].rep, ValueRep::String("a".into()));
    assert_eq!(items[1].rep, ValueRep::String("a".into()));
    assert_eq!(items[2].rep, ValueRep::String("b".into()));

    let snapshot = session.graph_snapshot();
    let keyed_nodes = snapshot
        .nodes
        .iter()
        .filter_map(|node| node.keyed.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(keyed_nodes.len(), 2);
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
