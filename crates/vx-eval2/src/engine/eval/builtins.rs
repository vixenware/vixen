use std::path::{Component, MAIN_SEPARATOR, Path as FsPath, PathBuf};

use facet_value::{VArray, VObject, Value as FacetValue};

use super::*;
use crate::value::compare_values;

pub(in crate::engine) fn slot_builtin_action(
    callee_name: &str,
    args: &[Value],
) -> Result<Option<(Arc<str>, Value, SlotAction<Value>, FunctionRef)>, EvalError> {
    let (slot_id_index, witness_index, action, resolver_index) = match callee_name {
        "slot_current" => (0usize, 1usize, SlotAction::Current, 2usize),
        "slot_refresh" => {
            if args.len() != 4 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "slot_refresh() requires slot id, witness, intent, and resolver",
                ));
            }
            (0usize, 1usize, SlotAction::Refresh(args[2].clone()), 3usize)
        }
        _ => return Ok(None),
    };

    let expected_len = match callee_name {
        "slot_current" => 3,
        "slot_refresh" => 4,
        _ => unreachable!(),
    };
    if args.len() != expected_len {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!(
                "{callee_name}() requires {} argument(s), got {}",
                expected_len,
                args.len()
            ),
        ));
    }

    let ValueRep::String(slot_id) = &args[slot_id_index].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{callee_name}() slot id must evaluate to String"),
        ));
    };
    let ValueRep::Function(resolver) = &args[resolver_index].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{callee_name}() resolver must evaluate to a function value"),
        ));
    };

    Ok(Some((
        slot_id.clone(),
        args[witness_index].clone(),
        action,
        resolver.clone(),
    )))
}

pub(in crate::engine) fn keyed_builtin_action(
    callee_name: &str,
    args: &[Value],
) -> Result<Option<(Value, Value, FunctionRef)>, EvalError> {
    if callee_name != "keyed" {
        return Ok(None);
    }
    if args.len() != 3 {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "keyed() requires key, joined input, and resolver",
        ));
    }
    let ValueRep::Function(resolver) = &args[2].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "keyed() resolver must evaluate to a function value",
        ));
    };
    Ok(Some((args[0].clone(), args[1].clone(), resolver.clone())))
}

pub(in crate::engine) fn branch_assumption_builtin_action(
    callee_name: &str,
    args: &[Value],
) -> Result<Option<Arc<str>>, EvalError> {
    if callee_name != "branch_assumption" {
        return Ok(None);
    }
    if args.len() != 1 {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "branch_assumption() requires the assumption name",
        ));
    }
    let ValueRep::String(name) = &args[0].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "branch_assumption() name must evaluate to String",
        ));
    };
    Ok(Some(name.clone()))
}

pub(in crate::engine) fn with_assumption_builtin_action(
    callee_name: &str,
    args: &[Value],
) -> Result<Option<(Arc<str>, Value, FunctionRef)>, EvalError> {
    if callee_name != "with_assumption" {
        return Ok(None);
    }
    if args.len() != 3 {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "with_assumption() requires name, value, and resolver",
        ));
    }
    let ValueRep::String(name) = &args[0].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "with_assumption() name must evaluate to String",
        ));
    };
    let ValueRep::Function(resolver) = &args[2].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "with_assumption() resolver must evaluate to a function value",
        ));
    };
    Ok(Some((name.clone(), args[1].clone(), resolver.clone())))
}

pub(in crate::engine) fn search_assumption_builtin_action(
    callee_name: &str,
    args: &[Value],
) -> Result<Option<(Arc<str>, Vec<Value>, FunctionRef)>, EvalError> {
    if callee_name != "search_assumption" {
        return Ok(None);
    }
    if args.len() != 3 {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "search_assumption() requires name, candidates, and resolver",
        ));
    }
    let ValueRep::String(name) = &args[0].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "search_assumption() name must evaluate to String",
        ));
    };
    let ValueRep::Array(candidates) = &args[1].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "search_assumption() candidates must evaluate to an array",
        ));
    };
    let ValueRep::Function(resolver) = &args[2].rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "search_assumption() resolver must evaluate to a function value",
        ));
    };
    Ok(Some((
        name.clone(),
        candidates.iter().cloned().collect(),
        resolver.clone(),
    )))
}

pub(in crate::engine) fn ready_builtin_value(value: Value) -> Option<EvalStep> {
    Some(EvalStep::Ready(value))
}

fn option_some(value: Value) -> Value {
    Value::structure("Some", vec![(Arc::from("value"), value)])
}

fn option_none() -> Value {
    Value::structure("None", Vec::<(Arc<str>, Value)>::new())
}

const INTERNAL_STREAM_TYPE_NAME: &str = "__Eval2Stream";
const INTERNAL_STREAM_CALLS_FIELD: &str = "calls";
const INTERNAL_STREAM_CALL_TYPE_NAME: &str = "__Eval2StreamCall";
const INTERNAL_STREAM_CALL_SOURCE_KIND_FIELD: &str = "source_kind";
const INTERNAL_STREAM_CALL_FUNCTION_FIELD: &str = "function";
const INTERNAL_STREAM_CALL_ARGS_FIELD: &str = "args";
const INTERNAL_STREAM_CALL_SOURCE_VALUE_FIELD: &str = "value";
const INTERNAL_STREAM_CALL_STAGES_FIELD: &str = "stages";
const INTERNAL_STREAM_CALL_SOURCE_KIND_VALUE: &str = "value";
const INTERNAL_STREAM_CALL_SOURCE_KIND_CALL: &str = "call";
const INTERNAL_STREAM_STAGE_TYPE_NAME: &str = "__Eval2StreamStage";
const INTERNAL_STREAM_STAGE_KIND_FIELD: &str = "kind";
const INTERNAL_STREAM_STAGE_FUNCTION_FIELD: &str = "function";
const INTERNAL_STREAM_STAGE_KIND_MAP: &str = "map";
const INTERNAL_STREAM_STAGE_KIND_FILTER: &str = "filter";
const INTERNAL_STREAM_STAGE_KIND_FLATTEN: &str = "flatten";
const INTERNAL_MERGED_TREE_TYPE_NAME: &str = "__Eval2MergedTree";
const INTERNAL_MERGED_TREE_TREES_FIELD: &str = "trees";

fn internal_stream_value(calls: Vec<Value>, meta: ValueMeta) -> Value {
    Value::structure(
        INTERNAL_STREAM_TYPE_NAME,
        [(Arc::from(INTERNAL_STREAM_CALLS_FIELD), Value::array(calls))],
    )
    .with_meta(meta)
}

#[derive(Debug, Clone)]
enum InternalStreamSource {
    Value(Value),
    Call {
        function: FunctionRef,
        args: Vec<Value>,
    },
}

#[derive(Debug, Clone)]
enum InternalStreamCall {
    Pipeline {
        source: InternalStreamSource,
        stages: Vec<StreamStage>,
    },
}

fn internal_stream_stage_value(stage: StreamStage) -> Value {
    let mut fields = vec![(
        Arc::from(INTERNAL_STREAM_STAGE_KIND_FIELD),
        Value::string(match &stage {
            StreamStage::Map(_) => INTERNAL_STREAM_STAGE_KIND_MAP,
            StreamStage::Filter(_) => INTERNAL_STREAM_STAGE_KIND_FILTER,
            StreamStage::Flatten => INTERNAL_STREAM_STAGE_KIND_FLATTEN,
        }),
    )];
    if let StreamStage::Map(function) | StreamStage::Filter(function) = stage {
        fields.push((
            Arc::from(INTERNAL_STREAM_STAGE_FUNCTION_FIELD),
            Value::function(function),
        ));
    }
    Value::structure(INTERNAL_STREAM_STAGE_TYPE_NAME, fields)
}

fn internal_stream_call_value(source: InternalStreamSource, stages: Vec<StreamStage>) -> Value {
    let (source_kind, source_fields): (&str, Vec<(Arc<str>, Value)>) = match source {
        InternalStreamSource::Value(value) => (
            INTERNAL_STREAM_CALL_SOURCE_KIND_VALUE,
            vec![(Arc::from(INTERNAL_STREAM_CALL_SOURCE_VALUE_FIELD), value)],
        ),
        InternalStreamSource::Call { function, args } => (
            INTERNAL_STREAM_CALL_SOURCE_KIND_CALL,
            vec![
                (
                    Arc::from(INTERNAL_STREAM_CALL_FUNCTION_FIELD),
                    Value::function(function),
                ),
                (
                    Arc::from(INTERNAL_STREAM_CALL_ARGS_FIELD),
                    Value::array(args),
                ),
            ],
        ),
    };
    let mut fields = vec![(
        Arc::from(INTERNAL_STREAM_CALL_SOURCE_KIND_FIELD),
        Value::string(source_kind),
    )];
    fields.extend(source_fields);
    fields.push((
        Arc::from(INTERNAL_STREAM_CALL_STAGES_FIELD),
        Value::array(
            stages
                .into_iter()
                .map(internal_stream_stage_value)
                .collect::<Vec<_>>(),
        ),
    ));
    Value::structure(INTERNAL_STREAM_CALL_TYPE_NAME, fields)
}

fn internal_stream_value_call(function: FunctionRef, args: Vec<Value>) -> Value {
    internal_stream_call_value(InternalStreamSource::Call { function, args }, Vec::new())
}

fn internal_stream_filter_call(function: FunctionRef, item: Value) -> Value {
    internal_stream_call_value(
        InternalStreamSource::Value(item),
        vec![StreamStage::Filter(function)],
    )
}

fn internal_stream_calls(value: &Value) -> Result<Vec<InternalStreamCall>, EvalError> {
    let ValueRep::Struct(stream) = &value.rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "Stream.to_array() receiver must evaluate to Stream",
        ));
    };
    if stream.type_name.as_ref() != INTERNAL_STREAM_TYPE_NAME {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "Stream.to_array() receiver must evaluate to Stream",
        ));
    }
    let Some(calls) = stream
        .fields
        .iter()
        .find_map(|(name, value)| (name.as_ref() == INTERNAL_STREAM_CALLS_FIELD).then_some(value))
    else {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            "internal stream value is missing calls",
        ));
    };
    let ValueRep::Array(calls) = &calls.rep else {
        return Err(EvalError::new(
            EvalErrorKind::Internal,
            "internal stream calls must evaluate to Array",
        ));
    };
    calls
        .iter()
        .map(|value| {
            let ValueRep::Struct(call) = &value.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    "internal stream call must evaluate to Struct",
                ));
            };
            if call.type_name.as_ref() != INTERNAL_STREAM_CALL_TYPE_NAME {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    "internal stream call has the wrong type",
                ));
            }
            let source_kind = call
                .fields
                .iter()
                .find_map(|(name, value)| {
                    (name.as_ref() == INTERNAL_STREAM_CALL_SOURCE_KIND_FIELD).then_some(value)
                })
                .ok_or_else(|| {
                    EvalError::new(
                        EvalErrorKind::Internal,
                        "internal stream call is missing a source kind",
                    )
                })?;
            let ValueRep::String(source_kind) = &source_kind.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    "internal stream call source kind must evaluate to String",
                ));
            };
            let stages_value = call
                .fields
                .iter()
                .find_map(|(name, value)| {
                    (name.as_ref() == INTERNAL_STREAM_CALL_STAGES_FIELD).then_some(value)
                })
                .ok_or_else(|| {
                    EvalError::new(
                        EvalErrorKind::Internal,
                        "internal stream call is missing stages",
                    )
                })?;
            let ValueRep::Array(stages_value) = &stages_value.rep else {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    "internal stream stages must evaluate to Array",
                ));
            };
            let stages = stages_value
                .iter()
                .map(|value| {
                    let ValueRep::Struct(stage) = &value.rep else {
                        return Err(EvalError::new(
                            EvalErrorKind::Internal,
                            "internal stream stage must evaluate to Struct",
                        ));
                    };
                    if stage.type_name.as_ref() != INTERNAL_STREAM_STAGE_TYPE_NAME {
                        return Err(EvalError::new(
                            EvalErrorKind::Internal,
                            "internal stream stage has the wrong type",
                        ));
                    }
                    let kind = stage
                        .fields
                        .iter()
                        .find_map(|(name, value)| {
                            (name.as_ref() == INTERNAL_STREAM_STAGE_KIND_FIELD).then_some(value)
                        })
                        .ok_or_else(|| {
                            EvalError::new(
                                EvalErrorKind::Internal,
                                "internal stream stage is missing a kind",
                            )
                        })?;
                    let ValueRep::String(kind) = &kind.rep else {
                        return Err(EvalError::new(
                            EvalErrorKind::Internal,
                            "internal stream stage kind must evaluate to String",
                        ));
                    };
                    match kind.as_ref() {
                        INTERNAL_STREAM_STAGE_KIND_MAP | INTERNAL_STREAM_STAGE_KIND_FILTER => {
                            let function = stage
                                .fields
                                .iter()
                                .find_map(|(name, value)| {
                                    (name.as_ref() == INTERNAL_STREAM_STAGE_FUNCTION_FIELD)
                                        .then_some(value)
                                })
                                .ok_or_else(|| {
                                    EvalError::new(
                                        EvalErrorKind::Internal,
                                        "internal stream stage is missing a function",
                                    )
                                })?;
                            let ValueRep::Function(function) = &function.rep else {
                                return Err(EvalError::new(
                                    EvalErrorKind::Internal,
                                    "internal stream stage function must evaluate to Function",
                                ));
                            };
                            match kind.as_ref() {
                                INTERNAL_STREAM_STAGE_KIND_MAP => {
                                    Ok(StreamStage::Map(function.clone()))
                                }
                                INTERNAL_STREAM_STAGE_KIND_FILTER => {
                                    Ok(StreamStage::Filter(function.clone()))
                                }
                                _ => unreachable!(),
                            }
                        }
                        INTERNAL_STREAM_STAGE_KIND_FLATTEN => Ok(StreamStage::Flatten),
                        _ => Err(EvalError::new(
                            EvalErrorKind::Internal,
                            "internal stream stage has an unknown kind",
                        )),
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            let source = match source_kind.as_ref() {
                INTERNAL_STREAM_CALL_SOURCE_KIND_VALUE => {
                    let value = call
                        .fields
                        .iter()
                        .find_map(|(name, value)| {
                            (name.as_ref() == INTERNAL_STREAM_CALL_SOURCE_VALUE_FIELD)
                                .then_some(value)
                        })
                        .ok_or_else(|| {
                            EvalError::new(
                                EvalErrorKind::Internal,
                                "internal stream call is missing a source value",
                            )
                        })?;
                    InternalStreamSource::Value(value.clone())
                }
                INTERNAL_STREAM_CALL_SOURCE_KIND_CALL => {
                    let function = call
                        .fields
                        .iter()
                        .find_map(|(name, value)| {
                            (name.as_ref() == INTERNAL_STREAM_CALL_FUNCTION_FIELD).then_some(value)
                        })
                        .ok_or_else(|| {
                            EvalError::new(
                                EvalErrorKind::Internal,
                                "internal stream call is missing a function",
                            )
                        })?;
                    let ValueRep::Function(function) = &function.rep else {
                        return Err(EvalError::new(
                            EvalErrorKind::Internal,
                            "internal stream call function must evaluate to Function",
                        ));
                    };
                    let args = call
                        .fields
                        .iter()
                        .find_map(|(name, value)| {
                            (name.as_ref() == INTERNAL_STREAM_CALL_ARGS_FIELD).then_some(value)
                        })
                        .ok_or_else(|| {
                            EvalError::new(
                                EvalErrorKind::Internal,
                                "internal stream call is missing arguments",
                            )
                        })?;
                    let ValueRep::Array(args) = &args.rep else {
                        return Err(EvalError::new(
                            EvalErrorKind::Internal,
                            "internal stream call args must evaluate to Array",
                        ));
                    };
                    InternalStreamSource::Call {
                        function: function.clone(),
                        args: args.iter().cloned().collect(),
                    }
                }
                _ => {
                    return Err(EvalError::new(
                        EvalErrorKind::Internal,
                        "internal stream call has an unknown source kind",
                    ));
                }
            };
            Ok(InternalStreamCall::Pipeline { source, stages })
        })
        .collect()
}

pub(super) fn internal_merged_tree_value(
    items: Vec<Value>,
    provenance: Provenance,
) -> Result<Value, EvalError> {
    for item in &items {
        if !is_tree_like_value(item) {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Tree merge inputs must evaluate to Tree",
            ));
        }
    }
    let meta = aggregate_child_meta(provenance, items.iter());
    Ok(Value::structure(
        INTERNAL_MERGED_TREE_TYPE_NAME,
        [(
            Arc::from(INTERNAL_MERGED_TREE_TREES_FIELD),
            Value::array(items),
        )],
    )
    .with_meta(meta))
}

fn is_tree_like_value(value: &Value) -> bool {
    match &value.rep {
        ValueRep::External(external) => external.kind.as_ref() == "Tree",
        ValueRep::Struct(struct_value) => {
            struct_value.type_name.as_ref() == INTERNAL_MERGED_TREE_TYPE_NAME
        }
        _ => false,
    }
}

pub(in crate::engine) enum StreamJoinState {
    Ready(Vec<Value>),
    Waiting(EvalNodeId),
}

fn queue_stream_prepared_call(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    function: &FunctionRef,
    args: &[Value],
    error_message: &'static str,
) -> Result<EvalNodeId, EvalError> {
    let Some(prepared) = prepare_function_call(state, function, args, state.current_parent())?
    else {
        return Err(EvalError::new(EvalErrorKind::TypeError, error_message));
    };
    Ok(queue_child_prepared_entry(state, prepared))
}

fn stream_stage_error_message(stage: &StreamStage) -> &'static str {
    match stage {
        StreamStage::Map(_) => "Stream.map() mapper must be a bodyful Vixen function",
        StreamStage::Filter(_) => "Stream.filter() predicate must be a bodyful Vixen function",
        StreamStage::Flatten => "Stream.flatten() inputs must evaluate to Stream or Array",
    }
}

fn advance_stream_join_items_from_call(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    call: InternalStreamCall,
    trailing_stages: Vec<StreamStage>,
    source_error_message: &'static str,
) -> Result<Vec<StreamJoinItem>, EvalError> {
    match call {
        InternalStreamCall::Pipeline { source, mut stages } => {
            stages.extend(trailing_stages);
            match source {
                InternalStreamSource::Value(value) => {
                    expand_stream_join_items_from_value(state, value, stages, source_error_message)
                }
                InternalStreamSource::Call { function, args } => {
                    Ok(vec![StreamJoinItem::Pending {
                        child_node: queue_stream_prepared_call(
                            state,
                            &function,
                            &args,
                            source_error_message,
                        )?,
                        waiting_on: StreamPendingKind::SourceCall,
                        remaining_stages: stages,
                    }])
                }
            }
        }
    }
}

fn expand_stream_join_items_from_value(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    value: Value,
    remaining_stages: Vec<StreamStage>,
    source_error_message: &'static str,
) -> Result<Vec<StreamJoinItem>, EvalError> {
    let Some((stage, rest)) = remaining_stages.split_first() else {
        return Ok(vec![StreamJoinItem::Ready(value)]);
    };
    match stage {
        StreamStage::Map(function) | StreamStage::Filter(function) => {
            let child_node = queue_stream_prepared_call(
                state,
                function,
                std::slice::from_ref(&value),
                stream_stage_error_message(stage),
            )?;
            Ok(vec![StreamJoinItem::Pending {
                child_node,
                waiting_on: match stage {
                    StreamStage::Map(_) => StreamPendingKind::Map,
                    StreamStage::Filter(_) => StreamPendingKind::Filter { input: value },
                    StreamStage::Flatten => unreachable!(),
                },
                remaining_stages: rest.to_vec(),
            }])
        }
        StreamStage::Flatten => match &value.rep {
            ValueRep::Array(items) => {
                let mut expanded = Vec::new();
                for item in items.iter().cloned() {
                    expanded.extend(expand_stream_join_items_from_value(
                        state,
                        item,
                        rest.to_vec(),
                        source_error_message,
                    )?);
                }
                Ok(expanded)
            }
            ValueRep::Struct(struct_value)
                if struct_value.type_name.as_ref() == INTERNAL_STREAM_TYPE_NAME =>
            {
                let nested_calls = internal_stream_calls(&value)?;
                let mut expanded = Vec::new();
                for call in nested_calls {
                    expanded.extend(advance_stream_join_items_from_call(
                        state,
                        call,
                        rest.to_vec(),
                        source_error_message,
                    )?);
                }
                Ok(expanded)
            }
            _ => Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Stream.flatten() inputs must evaluate to Stream or Array",
            )),
        },
    }
}

fn initial_stream_join_items(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    call: InternalStreamCall,
    source_error_message: &'static str,
) -> Result<Vec<StreamJoinItem>, EvalError> {
    advance_stream_join_items_from_call(state, call, Vec::new(), source_error_message)
}

pub(in crate::engine) fn collect_stream_join_items(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    items: &mut Vec<StreamJoinItem>,
) -> Result<StreamJoinState, EvalError> {
    loop {
        let mut made_progress = false;
        let mut first_waiting = None;
        let mut index = 0;

        while index < items.len() {
            let pending = match items.get(index) {
                Some(StreamJoinItem::Pending {
                    child_node,
                    waiting_on,
                    remaining_stages,
                }) => Some((*child_node, waiting_on.clone(), remaining_stages.clone())),
                _ => None,
            };
            let Some((child_node, waiting_on, remaining_stages)) = pending else {
                index += 1;
                continue;
            };

            match state.scheduler.node_state(child_node) {
                Some(EvalNodeState::Ready(value)) => {
                    let replacement = match waiting_on {
                        StreamPendingKind::SourceCall | StreamPendingKind::Map => {
                            expand_stream_join_items_from_value(
                                state,
                                value.clone(),
                                remaining_stages,
                                "Stream stages can only resume bodyful mapped streams",
                            )?
                        }
                        StreamPendingKind::Filter { input } => match &value.rep {
                            ValueRep::Bool(true) => expand_stream_join_items_from_value(
                                state,
                                input,
                                remaining_stages,
                                "Stream stages can only resume bodyful mapped streams",
                            )?,
                            ValueRep::Bool(false) => vec![StreamJoinItem::Skipped],
                            _ => {
                                return Err(EvalError::new(
                                    EvalErrorKind::TypeError,
                                    "Stream.filter() predicate must return Bool",
                                ));
                            }
                        },
                    };
                    items.splice(index..=index, replacement);
                    made_progress = true;
                    break;
                }
                Some(EvalNodeState::Poison(error)) => return Err(error.clone()),
                _ => {
                    first_waiting.get_or_insert(child_node);
                    index += 1;
                }
            }
        }

        if made_progress {
            continue;
        }

        if let Some(child_node) = first_waiting {
            return Ok(StreamJoinState::Waiting(child_node));
        }

        let values = items
            .iter()
            .filter_map(|item| match item {
                StreamJoinItem::Ready(value) => Some(value.clone()),
                StreamJoinItem::Skipped => None,
                StreamJoinItem::Pending { .. } => None,
            })
            .collect();
        return Ok(StreamJoinState::Ready(values));
    }
}

pub(in crate::engine) fn stream_map_entries(
    values: Vec<Value>,
    stream_provenance: Provenance,
) -> Result<Value, EvalError> {
    let mut entries = Vec::with_capacity(values.len());
    for value in values {
        let ValueRep::Tuple(items) = &value.rep else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Stream.to_map() items must evaluate to (key, value) tuples",
            ));
        };
        if items.len() != 2 {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Stream.to_map() items must evaluate to (key, value) tuples",
            ));
        }
        entries.push((items[0].clone(), items[1].clone()));
    }
    entries.sort_by(|(lhs, _), (rhs, _)| compare_values(lhs, rhs));
    for pair in entries.windows(2) {
        if values_equal(&pair[0].0.rep, &pair[1].0.rep)? {
            return Err(EvalError::new(
                EvalErrorKind::UserFail,
                "Stream.to_map() encountered duplicate key",
            ));
        }
    }
    let meta = aggregate_map_entries_meta(stream_provenance, entries.iter());
    Ok(Value::map(entries).with_meta(meta))
}

pub(in crate::engine) fn continue_array_find_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    find: ArrayFindState,
) -> Result<EvalStep, EvalError> {
    for index in find.index..find.items.len() {
        let item = find.items[index].clone();
        let Some(prepared) =
            prepare_function_call(state, &find.predicate, &[item.clone()], Some(node_id))?
        else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "array_find() predicate must be a bodyful Vixen function",
            ));
        };
        let matched = match eval_dependent_prepared_entry(state, node_id, prepared)? {
            EvalStep::Ready(result) => result,
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::ArrayFind {
                        node_id,
                        parent_id,
                        find: ArrayFindState { index, ..find },
                    },
                ));
            }
        };
        match matched.rep {
            ValueRep::Bool(true) => {
                let item = item.project(ProjectionStep::Index(index as u64));
                return Ok(EvalStep::Ready(if find.wrap_in_option {
                    option_some(item)
                } else {
                    item
                }));
            }
            ValueRep::Bool(false) => {}
            _ => {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_find() predicate must return Bool",
                ));
            }
        }
    }
    if find.wrap_in_option {
        Ok(EvalStep::Ready(option_none()))
    } else {
        Err(EvalError::new(
            EvalErrorKind::TypeError,
            "array_find() found no matching element",
        ))
    }
}

pub(in crate::engine) fn continue_array_fold_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    items: Arc<[Value]>,
    mut accumulator: Value,
    step: FunctionRef,
    start_index: usize,
) -> Result<EvalStep, EvalError> {
    for index in start_index..items.len() {
        let item = items[index].clone();
        let Some(prepared) =
            prepare_function_call(state, &step, &[accumulator.clone(), item], Some(node_id))?
        else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "array_fold() step must be a bodyful Vixen function",
            ));
        };
        accumulator = match eval_dependent_prepared_entry(state, node_id, prepared)? {
            EvalStep::Ready(result) => result,
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::ArrayFold {
                        node_id,
                        parent_id,
                        fold: ArrayFoldState { items, step, index },
                    },
                ));
            }
        };
    }
    Ok(EvalStep::Ready(accumulator))
}

pub(in crate::engine) fn array_filter_stream_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    items: Vec<Value>,
    predicate: FunctionRef,
    stream_provenance: Provenance,
) -> Result<EvalStep, EvalError> {
    let mut calls = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        if prepare_function_call(
            state,
            &predicate,
            std::slice::from_ref(&item),
            state.stack.last().copied(),
        )?
        .is_none()
        {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Array.filter() predicate must be a bodyful Vixen function",
            ));
        }
        calls.push(internal_stream_filter_call(
            predicate.clone(),
            item.project(ProjectionStep::Index(index as u64)),
        ));
    }
    Ok(EvalStep::Ready(internal_stream_value(
        calls,
        ValueMeta::new(
            stream_provenance,
            ReuseClass::Stable,
            CrossingPolicy::ByValue,
        ),
    )))
}

pub(in crate::engine) fn continue_array_map_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    items: Vec<Value>,
    mapper: FunctionRef,
    stream_provenance: Provenance,
) -> Result<EvalStep, EvalError> {
    let mut calls = Vec::with_capacity(items.len());
    for item in items {
        if prepare_function_call(
            state,
            &mapper,
            std::slice::from_ref(&item),
            state.stack.last().copied(),
        )?
        .is_none()
        {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Array.map() mapper must be a bodyful Vixen function",
            ));
        }
        calls.push(internal_stream_value_call(mapper.clone(), vec![item]));
    }
    Ok(EvalStep::Ready(internal_stream_value(
        calls,
        ValueMeta::new(
            stream_provenance,
            ReuseClass::Stable,
            CrossingPolicy::ByValue,
        ),
    )))
}

pub(in crate::engine) fn continue_array_any_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    any: ArrayAnyState,
) -> Result<EvalStep, EvalError> {
    for index in any.index..any.items.len() {
        let item = any.items[index].clone();
        let Some(prepared) = prepare_function_call(state, &any.predicate, &[item], Some(node_id))?
        else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Array.any() predicate must be a bodyful Vixen function",
            ));
        };
        match eval_dependent_prepared_entry(state, node_id, prepared)? {
            EvalStep::Ready(result) => match result.rep {
                ValueRep::Bool(true) => return Ok(EvalStep::Ready(Value::bool(true))),
                ValueRep::Bool(false) => {}
                _ => {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "Array.any() predicate must return Bool",
                    ));
                }
            },
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::ArrayAny {
                        node_id,
                        parent_id,
                        any: ArrayAnyState { index, ..any },
                    },
                ));
            }
        }
    }
    Ok(EvalStep::Ready(Value::bool(false)))
}

pub(in crate::engine) fn continue_array_filter_map_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    items: Arc<[Value]>,
    mapper: FunctionRef,
    mut mapped: Vec<Value>,
    mapper_result: Value,
    index: usize,
) -> Result<EvalStep, EvalError> {
    match &mapper_result.rep {
        ValueRep::Struct(option) if option.type_name.as_ref() == "Some" => {
            mapped.push(project_struct_field(mapper_result, Arc::from("value"))?);
        }
        ValueRep::Struct(option) if option.type_name.as_ref() == "None" => {}
        _ => {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Array.filter_map() mapper must return Option",
            ));
        }
    }

    for next_index in (index + 1)..items.len() {
        let item = items[next_index].clone();
        let Some(prepared) = prepare_function_call(state, &mapper, &[item], Some(node_id))? else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Array.filter_map() mapper must be a bodyful Vixen function",
            ));
        };
        match eval_dependent_prepared_entry(state, node_id, prepared)? {
            EvalStep::Ready(result) => match &result.rep {
                ValueRep::Struct(option) if option.type_name.as_ref() == "Some" => {
                    mapped.push(project_struct_field(result, Arc::from("value"))?);
                }
                ValueRep::Struct(option) if option.type_name.as_ref() == "None" => {}
                _ => {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "Array.filter_map() mapper must return Option",
                    ));
                }
            },
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::ArrayFilterMap {
                        node_id,
                        parent_id,
                        filter_map: ArrayFilterMapState {
                            items,
                            mapper,
                            mapped,
                            index: next_index,
                        },
                    },
                ));
            }
        }
    }

    let meta = aggregate_child_meta(
        Provenance::unknown(SourceSpan::synthetic("<array-filter-map>")),
        mapped.iter(),
    );
    Ok(EvalStep::Ready(Value::array(mapped).with_meta(meta)))
}

pub(in crate::engine) fn map_map_stream_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    entries: Vec<(Value, Value)>,
    mapper: FunctionRef,
    stream_provenance: Provenance,
) -> Result<EvalStep, EvalError> {
    let mut calls = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        let Some(prepared) = prepare_function_call(
            state,
            &mapper,
            &[key.clone(), value.clone()],
            state.stack.last().copied(),
        )?
        else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "Map.map() mapper must be a bodyful Vixen function",
            ));
        };
        let _ = prepared;
        calls.push(internal_stream_value_call(mapper.clone(), vec![key, value]));
    }
    Ok(EvalStep::Ready(internal_stream_value(
        calls,
        ValueMeta::new(
            stream_provenance,
            ReuseClass::Stable,
            CrossingPolicy::ByValue,
        ),
    )))
}

fn ensure_unary_bodyful_stream_function(
    function: &FunctionRef,
    error_message: &'static str,
) -> Result<(), EvalError> {
    if function.body.is_none() || function.has_keyword_parameters || function.parameters.len() != 1
    {
        return Err(EvalError::new(EvalErrorKind::TypeError, error_message));
    }
    Ok(())
}

pub(in crate::engine) fn branch_attempt_value(candidate: Value, error: EvalError) -> Value {
    let conflict = error
        .detail
        .unwrap_or_else(|| Value::string(error.message.as_ref()));
    let fields = vec![
        (Arc::from("value"), candidate),
        (Arc::from("conflict"), conflict),
    ];
    let meta = aggregate_child_meta(
        Provenance::unknown(SourceSpan::synthetic("<branch-attempt>")),
        fields.iter().map(|(_, value)| value),
    );
    Value::structure("BranchAttempt", fields).with_meta(meta)
}

pub(in crate::engine) fn branch_conflict_error(name: Arc<str>, attempts: Vec<Value>) -> EvalError {
    let fields = vec![
        (Arc::from("assumption"), Value::string(name)),
        (Arc::from("attempts"), Value::array(attempts)),
    ];
    let meta = aggregate_child_meta(
        Provenance::unknown(SourceSpan::synthetic("<branch-conflict>")),
        fields.iter().map(|(_, value)| value),
    );
    user_fail_error(Value::structure("BranchConflict", fields).with_meta(meta))
}

pub(in crate::engine) fn candidate_lists_equal(
    lhs: &[Value],
    rhs: &[Value],
) -> Result<bool, EvalError> {
    if lhs.len() != rhs.len() {
        return Ok(false);
    }
    lhs.iter()
        .zip(rhs.iter())
        .try_fold(true, |equal, (lhs, rhs)| {
            Ok(equal && values_equal(&lhs.rep, &rhs.rep)?)
        })
}

pub(in crate::engine) fn errors_equal_for_learning(
    lhs: &EvalError,
    rhs: &EvalError,
) -> Result<bool, EvalError> {
    if lhs.kind != rhs.kind || lhs.message != rhs.message {
        return Ok(false);
    }
    match (&lhs.detail, &rhs.detail) {
        (Some(lhs), Some(rhs)) => values_equal(&lhs.rep, &rhs.rep),
        (None, None) => Ok(true),
        _ => Ok(false),
    }
}

pub(in crate::engine) fn branch_conflict_attempts(error: &EvalError) -> Option<&[Value]> {
    let detail = error.detail.as_ref()?;
    let ValueRep::Struct(struct_value) = &detail.rep else {
        return None;
    };
    if struct_value.type_name.as_ref() != "BranchConflict" {
        return None;
    }
    let attempts_value = struct_value
        .fields
        .iter()
        .find(|(field, _)| field.as_ref() == "attempts")
        .map(|(_, value)| value)?;
    let ValueRep::Array(attempts) = &attempts_value.rep else {
        return None;
    };
    Some(&attempts[..])
}

pub(in crate::engine) fn local_branch_conflict_error(
    name: Arc<str>,
    error: &EvalError,
    attempt_offset: usize,
) -> Option<EvalError> {
    if attempt_offset == 0 {
        return Some(error.clone());
    }
    let attempts = branch_conflict_attempts(error)?;
    Some(branch_conflict_error(
        name,
        attempts[attempt_offset..].to_vec(),
    ))
}

pub(in crate::engine) fn learn_point_no_good(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    branch_context: &EvalBranchContext,
    assumption: &Arc<str>,
    candidates: &[Value],
    index: usize,
    resolver: &FunctionRef,
    error: &EvalError,
) -> Result<(), EvalError> {
    state.scheduler.record_learned_no_good(LearnedNoGood {
        branch_context: branch_context.clone(),
        assumption: assumption.clone(),
        candidates: Arc::from(candidates.to_vec()),
        start_index: index,
        end_index: index,
        resolver: resolver.clone(),
        error: error.clone(),
    })
}

pub(in crate::engine) fn learn_search_no_good(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    branch_context: &EvalBranchContext,
    assumption: &Arc<str>,
    candidates: &[Value],
    resolver: &FunctionRef,
    error: &EvalError,
) -> Result<(), EvalError> {
    state
        .scheduler
        .record_learned_search_no_good(LearnedSearchNoGood {
            branch_context: branch_context.clone(),
            assumption: assumption.clone(),
            candidates: Arc::from(candidates.to_vec()),
            resolver: resolver.clone(),
            error: error.clone(),
        })
}

pub(in crate::engine) fn continue_search_assumption_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    name: Arc<str>,
    candidates: Vec<Value>,
    resolver: FunctionRef,
    start_index: usize,
    mut attempts: Vec<Value>,
    root_search_start: usize,
    search_start_index: usize,
) -> Result<EvalStep, EvalError> {
    let prior_branch_context = state.branch_context.clone();
    let local_attempt_offset = search_start_index.saturating_sub(root_search_start);
    let fresh_local_search = attempts.len() == local_attempt_offset;
    let search_candidates = &candidates[search_start_index..];
    if fresh_local_search
        && let Some(error) = state.scheduler.learned_search_no_good(
            &prior_branch_context,
            &name,
            search_candidates,
            &resolver,
        )?
    {
        return Err(error);
    }
    for index in start_index..candidates.len() {
        let candidate = candidates[index].clone();
        if let Some(error) = state.scheduler.learned_no_good(
            &prior_branch_context,
            &name,
            &candidates,
            index,
            &resolver,
        )? {
            attempts.push(branch_attempt_value(candidate, error));
            continue;
        }
        let branch_context = prior_branch_context.with_assumption(name.clone(), candidate.clone());
        let Some(prepared) = prepare_function_call(state, &resolver, &[], Some(node_id))? else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "search_assumption() resolver must be a bodyful Vixen function",
            ));
        };
        match state.with_branch_context(branch_context, |state| {
            eval_dependent_prepared_entry(state, node_id, prepared)
        }) {
            Ok(EvalStep::Ready(value)) => return Ok(EvalStep::Ready(value)),
            Ok(EvalStep::Suspended(suspension)) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::SearchAssumption {
                        node_id,
                        parent_id,
                        search: SearchAssumptionState {
                            name,
                            candidates,
                            resolver,
                            index,
                            attempts,
                            root_search_start,
                            search_start_index,
                            prior_branch_context,
                        },
                    },
                ));
            }
            Err(error) if error.kind == EvalErrorKind::UserFail => {
                learn_point_no_good(
                    state,
                    &prior_branch_context,
                    &name,
                    &candidates,
                    index,
                    &resolver,
                    &error,
                )?;
                attempts.push(branch_attempt_value(candidate, error));
            }
            Err(error) => return Err(error),
        }
    }
    let error = branch_conflict_error(name.clone(), attempts);
    if !search_candidates.is_empty() {
        let local_error = local_branch_conflict_error(name.clone(), &error, local_attempt_offset)
            .unwrap_or_else(|| error.clone());
        learn_search_no_good(
            state,
            &prior_branch_context,
            &name,
            search_candidates,
            &resolver,
            &local_error,
        )?;
    }
    Err(error)
}

pub(in crate::engine) fn aggregate_map_entries_meta<'a>(
    provenance: Provenance,
    entries: impl IntoIterator<Item = &'a (Value, Value)>,
) -> ValueMeta {
    aggregate_child_meta(
        provenance,
        entries.into_iter().flat_map(|(key, value)| [key, value]),
    )
}

pub(in crate::engine) fn aggregate_array_items_meta<'a>(
    provenance: Provenance,
    items: impl IntoIterator<Item = &'a Value>,
) -> ValueMeta {
    aggregate_child_meta(provenance, items)
}

pub(in crate::engine) fn map_entry_value(
    map_provenance: &Provenance,
    index: usize,
    key: &Value,
    value: &Value,
) -> Value {
    let entry_provenance = map_provenance.project(ProjectionStep::Index(index as u64));
    let entry_fields = vec![
        (Value::string("key"), key.clone()),
        (Value::string("value"), value.clone()),
    ];
    let entry_meta = aggregate_map_entries_meta(entry_provenance, entry_fields.iter());
    Value::map(entry_fields).with_meta(entry_meta)
}

pub(in crate::engine) fn continue_builtin_call(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    function: &FunctionRef,
    callee_name: &str,
    call_source: SourceSpan,
    type_args: &[Ty],
    args: &[Value],
) -> Result<Option<EvalStep>, EvalError> {
    if !is_profiled_builtin_call_name(callee_name) {
        return Ok(None);
    }
    if state.track_calls {
        let frame = authored_call_frame(
            state,
            node_id,
            Arc::from(callee_name),
            args,
            call_source.clone(),
        );
        emit_authored_call_started(
            state,
            node_id,
            parent_id,
            call_source.clone(),
            &frame,
            args.len(),
        );
        return state.with_authored_call(frame, |state| {
            continue_builtin_call_inner(
                state,
                node_id,
                parent_id,
                function,
                callee_name,
                call_source.clone(),
                type_args,
                args,
            )
        });
    }
    continue_builtin_call_inner(
        state,
        node_id,
        parent_id,
        function,
        callee_name,
        call_source,
        type_args,
        args,
    )
}

fn is_profiled_builtin_call_name(callee_name: &str) -> bool {
    matches!(
        callee_name,
        "Json.parse"
            | "Toml.parse"
            | "Json.serialize"
            | "Toml.serialize"
            | "executor_class"
            | "discover"
            | "exec"
            | "Blob"
            | "Bytes"
            | "Blob.utf8"
            | "Path.to_string"
            | "String.len"
            | "String.is_empty"
            | "String.split_once"
            | "String.slice"
            | "String.contains"
            | "String.lines"
            | "String.replace"
            | "String.to_lower"
            | "String.to_u64"
            | "String.starts_with"
            | "String.ends_with"
            | "Map.get"
            | "Map.has"
            | "Option.unwrap_or"
            | "Array.filter"
            | "Array.map"
            | "Array.flat_map"
            | "Array.filter_map"
            | "Array.any"
            | "Array.find"
            | "Array.flatten"
            | "Array.merge"
            | "Array.to_array"
            | "Stream.for_each"
            | "Stream.flatten"
            | "Stream.filter"
            | "Stream.flat_map"
            | "Stream.map"
            | "Stream.merge"
            | "Stream.to_map"
            | "Stream.to_array"
            | "Tree.merge"
            | "Map.map"
            | "Map.to_array"
            | "map_get"
            | "map_put"
            | "map_entries"
            | "array_first"
            | "array_rest"
            | "array_push"
            | "array_find"
            | "array_fold"
    )
}

fn normalize_path_descriptor(path: &str) -> Arc<str> {
    let mut normalized = PathBuf::new();
    let mut prefix = None;
    let mut has_root = false;

    for component in FsPath::new(path).components() {
        match component {
            Component::Prefix(value) => prefix = Some(value.as_os_str().to_owned()),
            Component::RootDir => has_root = true,
            Component::CurDir => {}
            Component::ParentDir => {
                if normalized
                    .components()
                    .next_back()
                    .is_some_and(|last| !matches!(last, Component::ParentDir))
                {
                    normalized.pop();
                } else if !has_root {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }

    let mut result = PathBuf::new();
    if let Some(prefix) = prefix {
        result.push(prefix);
    }
    if has_root {
        result.push(PathBuf::from(std::path::MAIN_SEPARATOR.to_string()));
    }
    result.push(&normalized);

    let normalized = if result.as_os_str().is_empty() {
        if has_root {
            MAIN_SEPARATOR.to_string()
        } else {
            ".".to_string()
        }
    } else {
        result.to_string_lossy().into_owned()
    };
    Arc::from(normalized)
}

fn builtin_capture<'a>(function: &'a FunctionRef, name: &str) -> Option<&'a Value> {
    function
        .captures()
        .iter()
        .find_map(|(capture, value)| (capture.as_ref() == name).then_some(value))
}

fn builtin_capture_array(function: &FunctionRef, name: &str) -> Result<Vec<Value>, EvalError> {
    let Some(value) = builtin_capture(function, name) else {
        return Ok(Vec::new());
    };
    let ValueRep::Array(items) = &value.rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{name} capture must evaluate to an array"),
        ));
    };
    Ok(items.iter().cloned().collect())
}

fn builtin_capture_map(
    function: &FunctionRef,
    name: &str,
) -> Result<Vec<(Value, Value)>, EvalError> {
    let Some(value) = builtin_capture(function, name) else {
        return Ok(Vec::new());
    };
    let ValueRep::Map(map) = &value.rep else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            format!("{name} capture must evaluate to a map"),
        ));
    };
    Ok(map.entries.iter().cloned().collect())
}

fn continue_builtin_call_inner(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    function: &FunctionRef,
    callee_name: &str,
    call_source: SourceSpan,
    type_args: &[Ty],
    args: &[Value],
) -> Result<Option<EvalStep>, EvalError> {
    match callee_name {
        "Blob" => {
            if !type_args.is_empty() {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Blob() does not accept type arguments",
                ));
            }
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Blob() requires exactly one string argument",
                ));
            }
            let ValueRep::String(text) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Blob() argument must evaluate to String",
                ));
            };
            let value = Value::external("Blob", text.clone())
                .with_provenance(args[0].meta.provenance.clone())
                .with_reuse(ReuseClass::Stable)
                .with_crossing(CrossingPolicy::ByValue);
            Ok(ready_builtin_value(value))
        }
        "Bytes" => {
            if !type_args.is_empty() {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Bytes() does not accept type arguments",
                ));
            }
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Bytes() requires exactly one string argument",
                ));
            }
            let ValueRep::String(text) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Bytes() argument must evaluate to String",
                ));
            };
            let value = Value::external("Bytes", text.clone())
                .with_provenance(args[0].meta.provenance.clone())
                .with_reuse(ReuseClass::Stable)
                .with_crossing(CrossingPolicy::ByValue);
            Ok(ready_builtin_value(value))
        }
        "Json.parse" | "Toml.parse" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    format!("{callee_name}() requires one Blob or String argument"),
                ));
            }
            if type_args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    format!("{callee_name}() requires exactly one type argument"),
                ));
            }
            let format = match callee_name {
                "Json.parse" => StructuredFormat::Json,
                "Toml.parse" => StructuredFormat::Toml,
                _ => unreachable!(),
            };
            let value =
                parse_blob_as_value(format, &args[0], &type_args[0], state.program.types())?;
            Ok(ready_builtin_value(value))
        }
        "Json.serialize" | "Toml.serialize" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    format!("{callee_name}() requires one argument"),
                ));
            }
            if !type_args.is_empty() {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    format!("{callee_name}() does not accept type arguments"),
                ));
            }
            let format = match callee_name {
                "Json.serialize" => StructuredFormat::Json,
                "Toml.serialize" => StructuredFormat::Toml,
                _ => unreachable!(),
            };
            let value = serialize_value_as_blob(format, &args[0])?;
            Ok(ready_builtin_value(value))
        }
        "executor_class" => {
            if !type_args.is_empty() {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "executor_class() does not accept type arguments",
                ));
            }
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "executor_class() requires exactly one string argument",
                ));
            }
            let ValueRep::String(label) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "executor_class() argument must evaluate to String",
                ));
            };
            let value = Value::external("ExecutorClass", label.clone()).with_provenance(
                Provenance::binding(call_source, "executor_class".to_string()),
            );
            Ok(ready_builtin_value(value))
        }
        "discover" => {
            if !type_args.is_empty() {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "discover() does not accept type arguments",
                ));
            }
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "discover() requires executor and probe arguments",
                ));
            }
            let ValueRep::External(executor) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "discover() executor must evaluate to ExecutorClass",
                ));
            };
            if executor.kind.as_ref() != "ExecutorClass" {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "discover() executor must evaluate to ExecutorClass",
                ));
            }
            let host = Value::external("Host", executor.descriptor.clone()).with_provenance(
                Provenance::binding(call_source.clone(), "discover".to_string()),
            );
            dispatch_call(
                state,
                node_id,
                parent_id,
                call_source,
                Arc::from([]),
                args[1].clone(),
                vec![host],
            )
            .map(Some)
        }
        "exec" => {
            if !type_args.is_empty() {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "exec() does not accept type arguments",
                ));
            }
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "exec() requires exactly one args array",
                ));
            }
            let ValueRep::Array(exec_args) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "exec() args must evaluate to Array[ExecInput]",
                ));
            };
            let Some(observer) = builtin_capture(function, "observer") else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "exec() requires an observer keyword capture",
                ));
            };
            let request = RuntimeExecRequest {
                args: exec_args.iter().cloned().collect(),
                env: builtin_capture_map(function, "env")?
                    .into_iter()
                    .map(|(key, value)| match key.rep {
                        ValueRep::String(name) => Ok((name, value)),
                        _ => Err(EvalError::new(
                            EvalErrorKind::TypeError,
                            "exec().env keys must evaluate to String",
                        )),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                mounts: builtin_capture_map(function, "mounts")?,
                writable: builtin_capture_array(function, "writable")?,
                create_dirs: builtin_capture_array(function, "create_dirs")?,
                executor: builtin_capture(function, "executor")
                    .cloned()
                    .unwrap_or_else(|| {
                        Value::external("ExecutorClass", "default").with_provenance(
                            Provenance::binding(call_source.clone(), "executor_class".to_string()),
                        )
                    }),
            };
            let RuntimeExecReady {
                process,
                vfs,
                events,
            } = state.runtime.exec(state.place, &request, &call_source)?;
            for event in events {
                state.emit(node_id, parent_id, Some(call_source.clone()), event);
            }
            dispatch_call(
                state,
                node_id,
                parent_id,
                call_source,
                Arc::from([]),
                observer.clone(),
                vec![process, vfs],
            )
            .map(Some)
        }
        "Blob.utf8" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Blob.utf8() requires receiver",
                ));
            }
            let ValueRep::External(external) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Blob.utf8() receiver must evaluate to Blob",
                ));
            };
            if external.kind.as_ref() != "Blob" {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Blob.utf8() receiver must evaluate to Blob",
                ));
            }
            Ok(ready_builtin_value(
                Value::string(external.descriptor.clone()).with_meta(args[0].meta.clone()),
            ))
        }
        "Path.to_string" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Path.to_string() requires receiver",
                ));
            }
            let ValueRep::External(external) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Path.to_string() receiver must evaluate to Path",
                ));
            };
            if external.kind.as_ref() != "Path" {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Path.to_string() receiver must evaluate to Path",
                ));
            }
            Ok(ready_builtin_value(
                Value::string(normalize_path_descriptor(external.descriptor.as_ref()))
                    .with_meta(args[0].meta.clone()),
            ))
        }
        "String.len" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.len() requires receiver",
                ));
            }
            let ValueRep::String(text) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.len() receiver must evaluate to String",
                ));
            };
            Ok(ready_builtin_value(
                Value::u64(text.len() as u64).with_meta(args[0].meta.clone()),
            ))
        }
        "String.is_empty" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.is_empty() requires receiver",
                ));
            }
            let ValueRep::String(text) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.is_empty() receiver must evaluate to String",
                ));
            };
            Ok(ready_builtin_value(
                Value::bool(text.is_empty()).with_meta(args[0].meta.clone()),
            ))
        }
        "String.split_once" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.split_once() requires receiver and separator",
                ));
            }
            let ValueRep::String(text) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.split_once() receiver must evaluate to String",
                ));
            };
            let ValueRep::String(separator) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.split_once() separator must evaluate to String",
                ));
            };
            let value = match text.split_once(separator.as_ref()) {
                Some((left, right)) => {
                    let left = Value::string(left)
                        .with_provenance(args[0].meta.provenance.project(ProjectionStep::Index(0)));
                    let right = Value::string(right)
                        .with_provenance(args[0].meta.provenance.project(ProjectionStep::Index(1)));
                    let tuple = Value::tuple(vec![left.clone(), right.clone()]).with_meta(
                        aggregate_child_meta(
                            args[0].meta.provenance.clone(),
                            [&left, &right].into_iter(),
                        ),
                    );
                    let meta = aggregate_child_meta(
                        args[0].meta.provenance.clone(),
                        std::iter::once(&tuple),
                    );
                    Value::structure("Some", vec![(Arc::from("value"), tuple)]).with_meta(meta)
                }
                None => Value::structure("None", Vec::new()).with_meta(args[0].meta.clone()),
            };
            Ok(ready_builtin_value(value))
        }
        "String.slice" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.slice() requires receiver and range",
                ));
            }
            let ValueRep::String(text) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.slice() receiver must evaluate to String",
                ));
            };
            let ValueRep::Range(range) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.slice() range must evaluate to Range",
                ));
            };

            let start = usize::try_from(range.start.unwrap_or(0)).map_err(|_| {
                EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.slice() range start does not fit in usize",
                )
            })?;
            let mut end =
                usize::try_from(range.end.unwrap_or(text.len() as u64)).map_err(|_| {
                    EvalError::new(
                        EvalErrorKind::TypeError,
                        "String.slice() range end does not fit in usize",
                    )
                })?;
            if range.inclusive_end {
                end = end.checked_add(1).ok_or_else(|| {
                    EvalError::new(
                        EvalErrorKind::TypeError,
                        "String.slice() inclusive range end overflowed",
                    )
                })?;
            }
            if start > end {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.slice() range start must be <= end",
                ));
            }
            let Some(slice) = text.get(start..end) else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.slice() range was out of bounds or not on UTF-8 boundaries",
                ));
            };
            let path = slice_projection_path(range);
            Ok(ready_builtin_value(Value::string(slice).with_provenance(
                args[0].meta.provenance.project(ProjectionStep::Path(path)),
            )))
        }
        "String.contains" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.contains() requires receiver and needle",
                ));
            }
            let ValueRep::String(text) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.contains() receiver must evaluate to String",
                ));
            };
            let ValueRep::String(needle) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.contains() needle must evaluate to String",
                ));
            };
            Ok(ready_builtin_value(
                Value::bool(text.contains(needle.as_ref())).with_meta(args[0].meta.clone()),
            ))
        }
        "String.lines" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.lines() requires receiver",
                ));
            }
            let ValueRep::String(text) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.lines() receiver must evaluate to String",
                ));
            };
            let items = text
                .lines()
                .enumerate()
                .map(|(index, line)| {
                    Value::string(line).with_provenance(
                        args[0]
                            .meta
                            .provenance
                            .project(ProjectionStep::Index(index as u64)),
                    )
                })
                .collect::<Vec<_>>();
            let meta = aggregate_child_meta(args[0].meta.provenance.clone(), items.iter());
            Ok(ready_builtin_value(Value::array(items).with_meta(meta)))
        }
        "String.replace" => {
            if args.len() != 3 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.replace() requires receiver, old, and new",
                ));
            }
            let ValueRep::String(text) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.replace() receiver must evaluate to String",
                ));
            };
            let ValueRep::String(old) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.replace() old must evaluate to String",
                ));
            };
            let ValueRep::String(new) = &args[2].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.replace() new must evaluate to String",
                ));
            };
            Ok(ready_builtin_value(
                Value::string(text.replace(old.as_ref(), new.as_ref())).with_provenance(
                    args[0]
                        .meta
                        .provenance
                        .project(ProjectionStep::Path(format!("replace({old},{new})"))),
                ),
            ))
        }
        "String.to_lower" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.to_lower() requires receiver",
                ));
            }
            let ValueRep::String(text) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.to_lower() receiver must evaluate to String",
                ));
            };
            Ok(ready_builtin_value(
                Value::string(text.to_lowercase()).with_provenance(
                    args[0]
                        .meta
                        .provenance
                        .project(ProjectionStep::Path("to_lower()".to_string())),
                ),
            ))
        }
        "String.to_u64" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.to_u64() requires receiver",
                ));
            }
            let ValueRep::String(text) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.to_u64() receiver must evaluate to String",
                ));
            };
            let parsed = text.parse::<u64>().map_err(|_| {
                EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.to_u64() receiver must contain a base-10 U64",
                )
            })?;
            Ok(ready_builtin_value(
                Value::u64(parsed).with_provenance(
                    args[0]
                        .meta
                        .provenance
                        .project(ProjectionStep::Path("to_u64()".to_string())),
                ),
            ))
        }
        "String.starts_with" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.starts_with() requires receiver and prefix",
                ));
            }
            let ValueRep::String(text) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.starts_with() receiver must evaluate to String",
                ));
            };
            let ValueRep::String(prefix) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.starts_with() prefix must evaluate to String",
                ));
            };
            Ok(ready_builtin_value(
                Value::bool(text.starts_with(prefix.as_ref())).with_meta(args[0].meta.clone()),
            ))
        }
        "String.ends_with" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.ends_with() requires receiver and suffix",
                ));
            }
            let ValueRep::String(text) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.ends_with() receiver must evaluate to String",
                ));
            };
            let ValueRep::String(suffix) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "String.ends_with() suffix must evaluate to String",
                ));
            };
            Ok(ready_builtin_value(
                Value::bool(text.ends_with(suffix.as_ref())).with_meta(args[0].meta.clone()),
            ))
        }
        "Map.get" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Map.get() requires receiver and key",
                ));
            }
            let value = map_lookup_value(
                &args[0],
                &args[1],
                "Map.get() receiver must evaluate to a map",
            )?
            .ok_or_else(|| {
                EvalError::new(EvalErrorKind::TypeError, "Map.get() key was not found")
            })?;
            Ok(ready_builtin_value(value))
        }
        "Map.has" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Map.has() requires receiver and key",
                ));
            }
            let present = map_lookup_value(
                &args[0],
                &args[1],
                "Map.has() receiver must evaluate to a map",
            )?
            .is_some();
            Ok(ready_builtin_value(
                Value::bool(present).with_meta(args[0].meta.clone()),
            ))
        }
        "Option.unwrap_or" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Option.unwrap_or() requires receiver and default value",
                ));
            }
            let ValueRep::Struct(option) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Option.unwrap_or() receiver must evaluate to Option",
                ));
            };
            match option.type_name.as_ref() {
                "Some" => {
                    let value = project_struct_field(args[0].clone(), Arc::from("value"))?;
                    Ok(ready_builtin_value(value))
                }
                "None" => Ok(ready_builtin_value(args[1].clone())),
                _ => Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Option.unwrap_or() receiver must evaluate to Option",
                )),
            }
        }
        "Array.filter" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.filter() requires receiver and predicate",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.filter() receiver must evaluate to Array",
                ));
            };
            let ValueRep::Function(predicate) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.filter() predicate must evaluate to a function value",
                ));
            };
            array_filter_stream_builtin(
                state,
                items.iter().cloned().collect(),
                predicate.clone(),
                args[0].meta.provenance.clone(),
            )
            .map(Some)
        }
        "Array.map" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.map() requires receiver and mapper",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.map() receiver must evaluate to Array",
                ));
            };
            let ValueRep::Function(mapper) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.map() mapper must evaluate to a function value",
                ));
            };
            continue_array_map_builtin(
                state,
                items.iter().cloned().collect(),
                mapper.clone(),
                args[0].meta.provenance.clone(),
            )
            .map(Some)
        }
        "Array.flat_map" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.flat_map() requires receiver and mapper",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.flat_map() receiver must evaluate to Array",
                ));
            };
            let ValueRep::Function(mapper) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.flat_map() mapper must evaluate to a function value",
                ));
            };
            let EvalStep::Ready(stream) = continue_array_map_builtin(
                state,
                items.iter().cloned().collect(),
                mapper.clone(),
                args[0].meta.provenance.clone(),
            )?
            else {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    "Array.flat_map() mapping pipeline must not suspend",
                ));
            };
            let calls = internal_stream_calls(&stream)?;
            let calls = calls
                .into_iter()
                .map(|call| match call {
                    InternalStreamCall::Pipeline { source, mut stages } => {
                        stages.push(StreamStage::Flatten);
                        internal_stream_call_value(source, stages)
                    }
                })
                .collect();
            Ok(ready_builtin_value(internal_stream_value(
                calls,
                args[0].meta.clone(),
            )))
        }
        "Array.filter_map" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.filter_map() requires receiver and mapper",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.filter_map() receiver must evaluate to Array",
                ));
            };
            let ValueRep::Function(mapper) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.filter_map() mapper must evaluate to a function value",
                ));
            };
            let items = items.iter().cloned().collect::<Vec<_>>();
            if items.is_empty() {
                return Ok(ready_builtin_value(Value::array(Vec::<Value>::new())));
            }
            let Some(prepared) =
                prepare_function_call(state, mapper, &[items[0].clone()], Some(node_id))?
            else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.filter_map() mapper must be a bodyful Vixen function",
                ));
            };
            match eval_dependent_prepared_entry(state, node_id, prepared)? {
                EvalStep::Ready(result) => continue_array_filter_map_builtin(
                    state,
                    node_id,
                    parent_id,
                    Arc::<[Value]>::from(items),
                    mapper.clone(),
                    Vec::new(),
                    result,
                    0,
                )
                .map(Some),
                EvalStep::Suspended(suspension) => Ok(Some(extend_suspension(
                    suspension,
                    ContinuationFrame::ArrayFilterMap {
                        node_id,
                        parent_id,
                        filter_map: ArrayFilterMapState {
                            items: Arc::<[Value]>::from(items),
                            mapper: mapper.clone(),
                            mapped: Vec::new(),
                            index: 0,
                        },
                    },
                ))),
            }
        }
        "Array.any" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.any() requires receiver and predicate",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.any() receiver must evaluate to Array",
                ));
            };
            let ValueRep::Function(predicate) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.any() predicate must evaluate to a function value",
                ));
            };
            continue_array_any_builtin(
                state,
                node_id,
                parent_id,
                ArrayAnyState {
                    items: items.iter().cloned().collect(),
                    predicate: predicate.clone(),
                    index: 0,
                },
            )
            .map(Some)
        }
        "Array.find" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.find() requires receiver and predicate",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.find() receiver must evaluate to Array",
                ));
            };
            let ValueRep::Function(predicate) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.find() predicate must evaluate to a function value",
                ));
            };
            continue_array_find_builtin(
                state,
                node_id,
                parent_id,
                ArrayFindState {
                    items: items.iter().cloned().collect(),
                    predicate: predicate.clone(),
                    index: 0,
                    wrap_in_option: true,
                },
            )
            .map(Some)
        }
        "Array.flatten" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.flatten() requires receiver",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.flatten() receiver must evaluate to Array",
                ));
            };
            let mut flattened = Vec::new();
            for item in items.iter() {
                let ValueRep::Array(inner) = &item.rep else {
                    return Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "Array.flatten() items must evaluate to Array",
                    ));
                };
                flattened.extend(inner.iter().cloned());
            }
            let meta = aggregate_child_meta(args[0].meta.provenance.clone(), flattened.iter());
            Ok(ready_builtin_value(Value::array(flattened).with_meta(meta)))
        }
        "Array.merge" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.merge() requires receiver",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.merge() receiver must evaluate to Array",
                ));
            };
            Ok(ready_builtin_value(internal_merged_tree_value(
                items.iter().cloned().collect(),
                args[0].meta.provenance.clone(),
            )?))
        }
        "Array.to_array" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.to_array() requires receiver",
                ));
            }
            let ValueRep::Array(_) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Array.to_array() receiver must evaluate to Array",
                ));
            };
            Ok(ready_builtin_value(args[0].clone()))
        }
        "Stream.to_array" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Stream.to_array() requires receiver",
                ));
            }
            let calls = internal_stream_calls(&args[0])?;
            let mut items = Vec::with_capacity(calls.len());
            for call in calls {
                items.extend(initial_stream_join_items(
                    state,
                    call,
                    "Stream.to_array() can only collect bodyful mapped streams",
                )?);
            }
            if items.is_empty() {
                let meta = ValueMeta::new(
                    args[0].meta.provenance.clone(),
                    ReuseClass::Stable,
                    CrossingPolicy::ByValue,
                );
                return Ok(ready_builtin_value(
                    Value::array(Vec::new()).with_meta(meta),
                ));
            }
            match collect_stream_join_items(state, &mut items)? {
                StreamJoinState::Ready(mut values) => {
                    values.sort_by(compare_values);
                    let meta = aggregate_child_meta(args[0].meta.provenance.clone(), values.iter());
                    Ok(ready_builtin_value(Value::array(values).with_meta(meta)))
                }
                StreamJoinState::Waiting(child_node_id) => Ok(Some(wait_on_child_dependency(
                    state,
                    node_id,
                    child_node_id,
                    vec![ContinuationFrame::StreamToArrayJoin {
                        node_id,
                        parent_id,
                        items,
                        stream_provenance: args[0].meta.provenance.clone(),
                    }],
                ))),
            }
        }
        "Stream.merge" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Stream.merge() requires receiver",
                ));
            }
            let calls = internal_stream_calls(&args[0])?;
            let mut items = Vec::with_capacity(calls.len());
            for call in calls {
                items.extend(initial_stream_join_items(
                    state,
                    call,
                    "Stream.merge() can only merge bodyful mapped streams",
                )?);
            }
            if items.is_empty() {
                return Ok(ready_builtin_value(internal_merged_tree_value(
                    Vec::new(),
                    args[0].meta.provenance.clone(),
                )?));
            }
            match collect_stream_join_items(state, &mut items)? {
                StreamJoinState::Ready(values) => Ok(ready_builtin_value(
                    internal_merged_tree_value(values, args[0].meta.provenance.clone())?,
                )),
                StreamJoinState::Waiting(child_node_id) => Ok(Some(wait_on_child_dependency(
                    state,
                    node_id,
                    child_node_id,
                    vec![ContinuationFrame::StreamMergeJoin {
                        node_id,
                        parent_id,
                        items,
                        stream_provenance: args[0].meta.provenance.clone(),
                    }],
                ))),
            }
        }
        "Stream.for_each" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Stream.for_each() requires receiver",
                ));
            }
            let calls = internal_stream_calls(&args[0])?;
            let mut items = Vec::with_capacity(calls.len());
            for call in calls {
                items.extend(initial_stream_join_items(
                    state,
                    call,
                    "Stream.for_each() can only force bodyful mapped streams",
                )?);
            }
            if items.is_empty() {
                return Ok(ready_builtin_value(
                    Value::unit().with_meta(args[0].meta.clone()),
                ));
            }
            match collect_stream_join_items(state, &mut items)? {
                StreamJoinState::Ready(_) => Ok(ready_builtin_value(
                    Value::unit().with_meta(args[0].meta.clone()),
                )),
                StreamJoinState::Waiting(child_node_id) => Ok(Some(wait_on_child_dependency(
                    state,
                    node_id,
                    child_node_id,
                    vec![ContinuationFrame::StreamForEachJoin {
                        node_id,
                        parent_id,
                        items,
                        stream_provenance: args[0].meta.provenance.clone(),
                    }],
                ))),
            }
        }
        "Stream.to_map" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Stream.to_map() requires receiver",
                ));
            }
            let calls = internal_stream_calls(&args[0])?;
            let mut items = Vec::with_capacity(calls.len());
            for call in calls {
                items.extend(initial_stream_join_items(
                    state,
                    call,
                    "Stream.to_map() can only collect bodyful mapped streams",
                )?);
            }
            if items.is_empty() {
                return Ok(ready_builtin_value(
                    Value::map(Vec::<(Value, Value)>::new()).with_meta(ValueMeta::new(
                        args[0].meta.provenance.clone(),
                        ReuseClass::Stable,
                        CrossingPolicy::ByValue,
                    )),
                ));
            }
            match collect_stream_join_items(state, &mut items)? {
                StreamJoinState::Ready(values) => Ok(ready_builtin_value(stream_map_entries(
                    values,
                    args[0].meta.provenance.clone(),
                )?)),
                StreamJoinState::Waiting(child_node_id) => Ok(Some(wait_on_child_dependency(
                    state,
                    node_id,
                    child_node_id,
                    vec![ContinuationFrame::StreamToMapJoin {
                        node_id,
                        parent_id,
                        items,
                        stream_provenance: args[0].meta.provenance.clone(),
                    }],
                ))),
            }
        }
        "Stream.filter" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Stream.filter() requires receiver and predicate",
                ));
            }
            let ValueRep::Function(predicate) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Stream.filter() predicate must evaluate to a function value",
                ));
            };
            ensure_unary_bodyful_stream_function(
                predicate,
                "Stream.filter() predicate must be a bodyful Vixen function",
            )?;
            let calls = internal_stream_calls(&args[0])?;
            let calls = calls
                .into_iter()
                .map(|call| match call {
                    InternalStreamCall::Pipeline { source, mut stages } => {
                        stages.push(StreamStage::Filter(predicate.clone()));
                        internal_stream_call_value(source, stages)
                    }
                })
                .collect();
            Ok(ready_builtin_value(internal_stream_value(
                calls,
                args[0].meta.clone(),
            )))
        }
        "Stream.flatten" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Stream.flatten() requires receiver",
                ));
            }
            let calls = internal_stream_calls(&args[0])?;
            let calls = calls
                .into_iter()
                .map(|call| match call {
                    InternalStreamCall::Pipeline { source, mut stages } => {
                        stages.push(StreamStage::Flatten);
                        internal_stream_call_value(source, stages)
                    }
                })
                .collect();
            Ok(ready_builtin_value(internal_stream_value(
                calls,
                args[0].meta.clone(),
            )))
        }
        "Stream.flat_map" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Stream.flat_map() requires receiver and mapper",
                ));
            }
            let ValueRep::Function(mapper) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Stream.flat_map() mapper must evaluate to a function value",
                ));
            };
            ensure_unary_bodyful_stream_function(
                mapper,
                "Stream.flat_map() mapper must be a bodyful Vixen function",
            )?;
            let calls = internal_stream_calls(&args[0])?;
            let calls = calls
                .into_iter()
                .map(|call| match call {
                    InternalStreamCall::Pipeline { source, mut stages } => {
                        stages.push(StreamStage::Map(mapper.clone()));
                        stages.push(StreamStage::Flatten);
                        internal_stream_call_value(source, stages)
                    }
                })
                .collect();
            Ok(ready_builtin_value(internal_stream_value(
                calls,
                args[0].meta.clone(),
            )))
        }
        "Stream.map" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Stream.map() requires receiver and mapper",
                ));
            }
            let ValueRep::Function(mapper) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Stream.map() mapper must evaluate to a function value",
                ));
            };
            ensure_unary_bodyful_stream_function(
                mapper,
                "Stream.map() mapper must be a bodyful Vixen function",
            )?;
            let calls = internal_stream_calls(&args[0])?;
            let calls = calls
                .into_iter()
                .map(|call| match call {
                    InternalStreamCall::Pipeline { source, mut stages } => {
                        stages.push(StreamStage::Map(mapper.clone()));
                        internal_stream_call_value(source, stages)
                    }
                })
                .collect();
            Ok(ready_builtin_value(internal_stream_value(
                calls,
                args[0].meta.clone(),
            )))
        }
        "Tree.merge" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Tree.merge() requires receiver and other tree",
                ));
            }
            Ok(ready_builtin_value(internal_merged_tree_value(
                vec![args[0].clone(), args[1].clone()],
                args[0].meta.provenance.clone(),
            )?))
        }
        "Map.map" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Map.map() requires receiver and mapper",
                ));
            }
            let ValueRep::Map(map) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Map.map() receiver must evaluate to Map",
                ));
            };
            let ValueRep::Function(mapper) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Map.map() mapper must evaluate to a function value",
                ));
            };
            map_map_stream_builtin(
                state,
                map.entries.iter().cloned().collect(),
                mapper.clone(),
                args[0].meta.provenance.clone(),
            )
            .map(Some)
        }
        "Map.to_array" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Map.to_array() requires receiver",
                ));
            }
            let ValueRep::Map(map) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Map.to_array() receiver must evaluate to Map",
                ));
            };
            let items = map
                .entries
                .iter()
                .enumerate()
                .map(|(index, (key, value))| {
                    map_entry_value(&args[0].meta.provenance, index, key, value)
                })
                .collect::<Vec<_>>();
            let meta = aggregate_child_meta(args[0].meta.provenance.clone(), items.iter());
            Ok(ready_builtin_value(Value::array(items).with_meta(meta)))
        }
        "map_get" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "map_get() requires map and key",
                ));
            }
            let value = map_lookup_value(
                &args[0],
                &args[1],
                "map_get() first argument must evaluate to a map",
            )?
            .ok_or_else(|| {
                EvalError::new(EvalErrorKind::TypeError, "map_get() key was not found")
            })?;
            Ok(ready_builtin_value(value))
        }
        "map_put" => {
            if args.len() != 3 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "map_put() requires map, key, and value",
                ));
            }
            let ValueRep::Map(map) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "map_put() first argument must evaluate to a map",
                ));
            };
            let key = &args[1];
            let value = &args[2];
            let mut entries = map.entries.iter().cloned().collect::<Vec<_>>();
            let mut replaced = false;
            for (entry_key, entry_value) in &mut entries {
                if values_equal(&entry_key.rep, &key.rep)? {
                    *entry_value = value.clone();
                    replaced = true;
                    break;
                }
            }
            if !replaced {
                entries.push((key.clone(), value.clone()));
            }
            let meta = aggregate_map_entries_meta(args[0].meta.provenance.clone(), entries.iter());
            Ok(ready_builtin_value(Value::map(entries).with_meta(meta)))
        }
        "map_entries" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "map_entries() requires map",
                ));
            }
            let ValueRep::Map(map) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "map_entries() first argument must evaluate to a map",
                ));
            };
            let items = map
                .entries
                .iter()
                .enumerate()
                .map(|(index, (key, value))| {
                    map_entry_value(&args[0].meta.provenance, index, key, value)
                })
                .collect::<Vec<_>>();
            let meta = aggregate_child_meta(args[0].meta.provenance.clone(), items.iter());
            Ok(ready_builtin_value(Value::array(items).with_meta(meta)))
        }
        "array_first" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_first() requires array",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_first() first argument must evaluate to an array",
                ));
            };
            let value = items.first().cloned().ok_or_else(|| {
                EvalError::new(EvalErrorKind::TypeError, "array_first() array was empty")
            })?;
            Ok(ready_builtin_value(value.project(ProjectionStep::Index(0))))
        }
        "array_rest" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_rest() requires array",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_rest() first argument must evaluate to an array",
                ));
            };
            let items = items.iter().skip(1).cloned().collect::<Vec<_>>();
            let meta = aggregate_array_items_meta(args[0].meta.provenance.clone(), items.iter());
            Ok(ready_builtin_value(Value::array(items).with_meta(meta)))
        }
        "array_push" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_push() requires array and item",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_push() first argument must evaluate to an array",
                ));
            };
            let mut items = items.iter().cloned().collect::<Vec<_>>();
            items.push(args[1].clone());
            let meta = aggregate_array_items_meta(args[0].meta.provenance.clone(), items.iter());
            Ok(ready_builtin_value(Value::array(items).with_meta(meta)))
        }
        "array_find" => {
            if args.len() != 2 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_find() requires array and predicate",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_find() first argument must evaluate to an array",
                ));
            };
            let ValueRep::Function(predicate) = &args[1].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_find() second argument must evaluate to a function value",
                ));
            };
            continue_array_find_builtin(
                state,
                node_id,
                parent_id,
                ArrayFindState {
                    items: items.iter().cloned().collect(),
                    predicate: predicate.clone(),
                    index: 0,
                    wrap_in_option: false,
                },
            )
            .map(Some)
        }
        "array_fold" => {
            if args.len() != 3 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_fold() requires array, initial, and step",
                ));
            }
            let ValueRep::Array(items) = &args[0].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_fold() first argument must evaluate to an array",
                ));
            };
            let ValueRep::Function(step) = &args[2].rep else {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "array_fold() third argument must evaluate to a function value",
                ));
            };
            continue_array_fold_builtin(
                state,
                node_id,
                parent_id,
                Arc::<[Value]>::from(items.iter().cloned().collect::<Vec<_>>()),
                args[1].clone(),
                step.clone(),
                0,
            )
            .map(Some)
        }
        _ => Ok(None),
    }
}

fn serialize_value_as_blob(format: StructuredFormat, value: &Value) -> Result<Value, EvalError> {
    let encoded = encode_value_as_facet(value, format)?;
    let text = match format {
        StructuredFormat::Json => facet_json::to_string(&encoded).map_err(|error| {
            EvalError::new(
                EvalErrorKind::UserFail,
                format!("{} failed: {error}", serialize_display_name(format)),
            )
        })?,
        StructuredFormat::Toml => facet_toml::to_string(&encoded).map_err(|error| {
            EvalError::new(
                EvalErrorKind::UserFail,
                format!("{} failed: {error}", serialize_display_name(format)),
            )
        })?,
    };
    Ok(Value::external("Blob", text)
        .with_reuse(ReuseClass::Stable)
        .with_crossing(CrossingPolicy::ByValue))
}

fn serialize_display_name(format: StructuredFormat) -> &'static str {
    match format {
        StructuredFormat::Json => "Json.serialize",
        StructuredFormat::Toml => "Toml.serialize",
    }
}

fn encode_value_as_facet(value: &Value, format: StructuredFormat) -> Result<FacetValue, EvalError> {
    match &value.rep {
        ValueRep::Unit => Ok(FacetValue::NULL),
        ValueRep::Bool(value) => Ok(FacetValue::from(*value)),
        ValueRep::U64(value) => Ok(FacetValue::from(*value)),
        ValueRep::String(value) => Ok(FacetValue::from(value.as_ref())),
        ValueRep::Array(items) | ValueRep::Tuple(items) => {
            let mut array = VArray::new();
            for item in items.iter() {
                array.push(encode_value_as_facet(item, format)?);
            }
            Ok(array.into())
        }
        ValueRep::Map(map) => {
            let mut object = VObject::new();
            for (key, value) in map.entries.iter() {
                let ValueRep::String(key) = &key.rep else {
                    return Err(EvalError::new(
                        EvalErrorKind::UserFail,
                        format!(
                            "{} only supports maps with String keys",
                            serialize_display_name(format)
                        ),
                    ));
                };
                if let Some(encoded) = encode_object_entry_value(value, format)? {
                    object.insert(key.as_ref(), encoded);
                }
            }
            Ok(object.into())
        }
        ValueRep::Struct(struct_value) if struct_value.type_name.as_ref() == "None" => {
            Ok(FacetValue::NULL)
        }
        ValueRep::Struct(struct_value) if struct_value.type_name.as_ref() == "Some" => {
            let Some((_, inner)) = struct_value
                .fields
                .iter()
                .find(|(field, _)| field.as_ref() == "value")
            else {
                return Err(EvalError::new(
                    EvalErrorKind::UserFail,
                    format!(
                        "{} cannot serialize Some without a value field",
                        serialize_display_name(format)
                    ),
                ));
            };
            encode_value_as_facet(inner, format)
        }
        ValueRep::Struct(struct_value) => {
            let mut object = VObject::new();
            for (field, value) in struct_value.fields.iter() {
                if let Some(encoded) = encode_object_entry_value(value, format)? {
                    object.insert(field.as_ref(), encoded);
                }
            }
            Ok(object.into())
        }
        ValueRep::Range(_) => Err(EvalError::new(
            EvalErrorKind::UserFail,
            format!(
                "{} does not support Range values yet",
                serialize_display_name(format)
            ),
        )),
        ValueRep::External(external) => Err(EvalError::new(
            EvalErrorKind::UserFail,
            format!(
                "{} does not support external {} values yet",
                serialize_display_name(format),
                external.kind
            ),
        )),
        ValueRep::Function(_) => Err(EvalError::new(
            EvalErrorKind::UserFail,
            format!(
                "{} does not support function values",
                serialize_display_name(format)
            ),
        )),
    }
}

fn encode_object_entry_value(
    value: &Value,
    format: StructuredFormat,
) -> Result<Option<FacetValue>, EvalError> {
    if matches!(format, StructuredFormat::Toml)
        && matches!(
            &value.rep,
            ValueRep::Struct(struct_value) if struct_value.type_name.as_ref() == "None"
        )
    {
        return Ok(None);
    }
    Ok(Some(encode_value_as_facet(value, format)?))
}

fn slice_projection_path(range: &RangeValue) -> String {
    let start = range
        .start
        .map_or(String::new(), |value: u64| value.to_string());
    let end = range
        .end
        .map_or(String::new(), |value: u64| value.to_string());
    let op = if range.inclusive_end { "..=" } else { ".." };
    format!("slice({start}{op}{end})")
}

fn map_lookup_value(
    map_value: &Value,
    key: &Value,
    type_error_message: &str,
) -> Result<Option<Value>, EvalError> {
    let ValueRep::Map(map) = &map_value.rep else {
        return Err(EvalError::new(EvalErrorKind::TypeError, type_error_message));
    };

    let mut selected = None;
    for (entry_key, entry_value) in map.entries.iter() {
        if values_equal(&entry_key.rep, &key.rep)? {
            selected = Some(entry_value.clone());
            break;
        }
    }

    Ok(selected.map(|value| {
        if let ValueRep::String(key) = &key.rep {
            value.project(ProjectionStep::MapKey(key.to_string()))
        } else {
            value
        }
    }))
}

pub(in crate::engine) fn slot_context_value(
    slot_id: Arc<str>,
    witness: Value,
    action: &SlotAction<Value>,
    current: Option<Value>,
) -> Value {
    let (is_refresh, intent) = match action {
        SlotAction::Current => (false, Value::unit()),
        SlotAction::Refresh(intent) => (true, intent.clone()),
    };
    let current_present = current.is_some();
    Value::structure(
        "SlotContext",
        vec![
            (Arc::from("slot_id"), Value::string(slot_id)),
            (Arc::from("witness"), witness),
            (Arc::from("has_current"), Value::bool(current_present)),
            (Arc::from("current"), current.unwrap_or_else(Value::unit)),
            (Arc::from("is_refresh"), Value::bool(is_refresh)),
            (Arc::from("intent"), intent),
        ],
    )
}

pub(in crate::engine) fn emit_runtime_ready_events(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    source: &SourceSpan,
    ready: &RuntimeReady,
) {
    for event in &ready.events {
        state.emit(node_id, parent_id, Some(source.clone()), event.clone());
    }
}

pub(in crate::engine) fn commit_slot_result(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    call_source: &SourceSpan,
    slot_id: &Arc<str>,
    witness: &Value,
    action: &SlotAction<Value>,
    resolved: &Value,
) -> Result<EvalStep, EvalError> {
    let ready =
        state
            .runtime
            .store_slot(state.place, slot_id.as_ref(), witness, action, resolved)?;
    emit_runtime_ready_events(state, node_id, parent_id, call_source, &ready);
    Ok(EvalStep::Ready(ready.value))
}

pub(in crate::engine) fn continue_slot_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    call_source: SourceSpan,
    slot_id: Arc<str>,
    witness: Value,
    action: SlotAction<Value>,
    resolver: FunctionRef,
) -> Result<EvalStep, EvalError> {
    match state
        .runtime
        .prepare_slot(state.place, slot_id.as_ref(), &witness, &action)?
    {
        RuntimeSlotPrepare::Reused(ready) => {
            emit_runtime_ready_events(state, node_id, parent_id, &call_source, &ready);
            Ok(EvalStep::Ready(ready.value))
        }
        RuntimeSlotPrepare::Resolve { current } => {
            let resolver_context =
                slot_context_value(slot_id.clone(), witness.clone(), &action, current);
            let Some(prepared) =
                prepare_function_call(state, &resolver, &[resolver_context], Some(node_id))?
            else {
                return Err(EvalError::new(
                    EvalErrorKind::Internal,
                    "slot resolver must be a bodyful Vixen function",
                ));
            };
            match eval_dependent_prepared_entry(state, node_id, prepared)? {
                EvalStep::Ready(value) => commit_slot_result(
                    state,
                    node_id,
                    parent_id,
                    &call_source,
                    &slot_id,
                    &witness,
                    &action,
                    &value,
                ),
                EvalStep::Suspended(suspension) => Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::Slot {
                        node_id,
                        parent_id,
                        slot: SlotState {
                            call_source,
                            slot_id,
                            witness,
                            action,
                        },
                    },
                )),
            }
        }
    }
}

pub(in crate::engine) fn continue_keyed_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    _parent_id: Option<EvalNodeId>,
    call_source: SourceSpan,
    key: Value,
    joined: Value,
    resolver: FunctionRef,
) -> Result<EvalStep, EvalError> {
    let keyed_node_id = state.scheduler.demand_keyed_node(
        resolver,
        key,
        joined,
        state.branch_context.clone(),
        call_source.clone(),
    )?;
    state.emit_current_dependency(keyed_node_id, Some(call_source.clone()));

    if let Some(result) = state.scheduler.keyed_resolution(keyed_node_id) {
        return result.map(EvalStep::Ready);
    }

    if !state.queue_child_work {
        let prepared = prepare_keyed_call(state, keyed_node_id)?;
        return match eval_dependent_prepared_entry(state, keyed_node_id, prepared) {
            Ok(EvalStep::Ready(value)) => {
                state
                    .scheduler
                    .record_node_state(keyed_node_id, EvalNodeState::Ready(value.clone()));
                let _ = state.scheduler.finalize_keyed_completion(keyed_node_id)?;
                Ok(EvalStep::Ready(value))
            }
            Ok(EvalStep::Suspended(suspension)) => {
                let pending = suspension.pending.clone();
                state.scheduler.register_suspension(suspension);
                state
                    .scheduler
                    .record_node_state(keyed_node_id, EvalNodeState::Pending(pending.clone()));
                Ok(EvalStep::Suspended(EvalSuspension {
                    resume_node: node_id,
                    pending,
                    branch_context: state.branch_context.clone(),
                    frames: Vec::new(),
                    call_stack: state.call_stack.clone(),
                }))
            }
            Err(error) => {
                state
                    .scheduler
                    .record_node_state(keyed_node_id, EvalNodeState::Poison(error.clone()));
                Err(error)
            }
        };
    }

    Ok(wait_on_child_dependency(
        state,
        node_id,
        keyed_node_id,
        Vec::new(),
    ))
}

pub(in crate::engine) fn continue_branch_assumption_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    name: Arc<str>,
) -> Result<EvalStep, EvalError> {
    let Some(value) = state.branch_context().assumption(name.as_ref()).cloned() else {
        return Err(EvalError::new(
            EvalErrorKind::UserFail,
            format!("branch assumption {name:?} is not set"),
        ));
    };
    Ok(EvalStep::Ready(value))
}

pub(in crate::engine) fn continue_with_assumption_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    name: Arc<str>,
    value: Value,
    resolver: FunctionRef,
) -> Result<EvalStep, EvalError> {
    let branch_context = state.branch_context().with_assumption(name, value);
    let Some(prepared) = prepare_function_call(state, &resolver, &[], Some(node_id))? else {
        return Err(EvalError::new(
            EvalErrorKind::TypeError,
            "with_assumption() resolver must be a bodyful Vixen function",
        ));
    };
    let prior_branch_context = state.branch_context.clone();
    let result = state.with_branch_context(branch_context, |state| {
        eval_dependent_prepared_entry(state, node_id, prepared)
    })?;
    match result {
        EvalStep::Ready(value) => Ok(EvalStep::Ready(value)),
        EvalStep::Suspended(suspension) => Ok(extend_suspension(
            suspension,
            ContinuationFrame::RestoreBranchContext {
                branch_context: prior_branch_context,
            },
        )),
    }
}
