use super::*;

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

pub(in crate::engine) fn continue_array_find_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    items: Vec<Value>,
    predicate: FunctionRef,
    start_index: usize,
) -> Result<EvalStep, EvalError> {
    for index in start_index..items.len() {
        let item = items[index].clone();
        let Some(prepared) =
            prepare_function_call(state, &predicate, &[item.clone()], Some(node_id))?
        else {
            return Err(EvalError::new(
                EvalErrorKind::TypeError,
                "array_find() predicate must be a bodyful Vixen function",
            ));
        };
        let mut env = prepared.env;
        let matched = match state.with_source(prepared.source, |state| {
            eval_dependent_expr(state, node_id, &prepared.body, &mut env)
        })? {
            EvalStep::Ready(result) => result,
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::ArrayFindAfterPredicate {
                        node_id,
                        parent_id,
                        items,
                        predicate,
                        index,
                    },
                ));
            }
        };
        match matched.rep {
            ValueRep::Bool(true) => {
                return Ok(EvalStep::Ready(
                    item.project(ProjectionStep::Index(index as u64)),
                ));
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
    Err(EvalError::new(
        EvalErrorKind::TypeError,
        "array_find() found no matching element",
    ))
}

pub(in crate::engine) fn continue_array_fold_builtin(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    items: Vec<Value>,
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
        let mut env = prepared.env;
        accumulator = match state.with_source(prepared.source, |state| {
            eval_dependent_expr(state, node_id, &prepared.body, &mut env)
        })? {
            EvalStep::Ready(result) => result,
            EvalStep::Suspended(suspension) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::ArrayFoldAfterStep {
                        node_id,
                        parent_id,
                        items,
                        step,
                        index,
                    },
                ));
            }
        };
    }
    Ok(EvalStep::Ready(accumulator))
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
        let mut env = prepared.env;
        match state.with_branch_context(branch_context, |state| {
            state.with_source(prepared.source, |state| {
                eval_dependent_expr(state, node_id, &prepared.body, &mut env)
            })
        }) {
            Ok(EvalStep::Ready(value)) => return Ok(EvalStep::Ready(value)),
            Ok(EvalStep::Suspended(suspension)) => {
                return Ok(extend_suspension(
                    suspension,
                    ContinuationFrame::SearchAssumptionAfterBranch {
                        node_id,
                        parent_id,
                        name,
                        candidates,
                        resolver,
                        index,
                        attempts,
                        root_search_start,
                        search_start_index,
                        prior_branch_context,
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
    callee_name: &str,
    type_args: &[Ty],
    args: &[Value],
) -> Result<Option<EvalStep>, EvalError> {
    match callee_name {
        "Json.parse" | "Toml.parse" => {
            if args.len() != 1 {
                return Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    format!("{callee_name}() requires one Blob argument"),
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
                    let value = project_struct_field(args[0].clone(), "value".to_string())?;
                    Ok(ready_builtin_value(value))
                }
                "None" => Ok(ready_builtin_value(args[1].clone())),
                _ => Err(EvalError::new(
                    EvalErrorKind::TypeError,
                    "Option.unwrap_or() receiver must evaluate to Option",
                )),
            }
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
                items.iter().cloned().collect(),
                predicate.clone(),
                0,
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
                items.iter().cloned().collect(),
                args[1].clone(),
                step.clone(),
                0,
            )
            .map(Some)
        }
        _ => Ok(None),
    }
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
            let mut env = prepared.env;
            match state.with_source(prepared.source, |state| {
                eval_dependent_expr(state, node_id, &prepared.body, &mut env)
            })? {
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
                    ContinuationFrame::SlotAfterResolver {
                        node_id,
                        parent_id,
                        call_source,
                        slot_id,
                        witness,
                        action,
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
        let mut env = prepared.env;
        return match state.with_source(prepared.source, |state| {
            eval_dependent_expr(state, keyed_node_id, &prepared.body, &mut env)
        }) {
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
    let mut env = prepared.env;
    let prior_branch_context = state.branch_context.clone();
    let result = state.with_branch_context(branch_context, |state| {
        state.with_source(prepared.source, |state| {
            eval_dependent_expr(state, node_id, &prepared.body, &mut env)
        })
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
