use super::*;
use std::sync::Arc;

pub(in crate::engine) fn resume_suspension(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    suspension: EvalSuspension,
    result: Result<Value, EvalError>,
) -> Result<EvalStep, EvalError> {
    let EvalSuspension {
        resume_node,
        pending: _,
        branch_context: _,
        continuation,
        work_invocation,
        call_stack,
    } = suspension;
    if !call_stack.is_empty() {
        state.with_call_stack(call_stack.as_ref().to_vec(), |state| {
            resume_suspension_inner(state, resume_node, continuation, result)
        })
    } else if work_invocation.is_some() {
        state.with_work_invocation(work_invocation, |state| {
            resume_suspension_inner(state, resume_node, continuation, result)
        })
    } else {
        resume_suspension_inner(state, resume_node, continuation, result)
    }
}

fn resume_suspension_inner(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    resume_node: EvalNodeId,
    continuation: EvalSuspensionContinuation,
    result: Result<Value, EvalError>,
) -> Result<EvalStep, EvalError> {
    match continuation {
        EvalSuspensionContinuation::Frames(frames) => {
            resume_frame_sequence(state, resume_node, frames.as_ref(), None, result)
        }
        EvalSuspensionContinuation::Bytecode {
            prefix,
            node_id,
            parent_id,
            bytecode,
        } => resume_frame_sequence(
            state,
            resume_node,
            prefix.as_ref(),
            Some((node_id, parent_id, bytecode)),
            result,
        ),
    }
}

fn resume_frame_sequence(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    resume_node: EvalNodeId,
    frames: &[ContinuationFrame],
    bytecode_tail: Option<(EvalNodeId, Option<EvalNodeId>, BytecodeState)>,
    result: Result<Value, EvalError>,
) -> Result<EvalStep, EvalError> {
    let mut current = result;
    let resume_parent_id = state
        .scheduler
        .node_record(resume_node)
        .and_then(|record| record.parent_id);
    let mut index = 0;
    while index < frames.len() {
        match resume_frame(state, frames[index].clone(), current)? {
            ResumeOutcome::Continue(next) => current = next,
            ResumeOutcome::Suspended(new_suspension) => {
                let mut suspended = append_suspension_frames(new_suspension, &frames[index + 1..]);
                if let Some((node_id, parent_id, bytecode)) = bytecode_tail.clone() {
                    suspended = append_suspension_bytecode(suspended, node_id, parent_id, bytecode);
                }
                return Ok(EvalStep::Suspended(suspended));
            }
        }
        index += 1;
    }
    if let Some((node_id, parent_id, bytecode)) = bytecode_tail {
        match resume_bytecode_tail(state, node_id, parent_id, bytecode, current)? {
            ResumeOutcome::Continue(next) => current = next,
            ResumeOutcome::Suspended(new_suspension) => {
                return Ok(EvalStep::Suspended(new_suspension));
            }
        }
    }
    if !matches!(
        state.scheduler.node_state(resume_node),
        Some(EvalNodeState::Ready(_)) | Some(EvalNodeState::Poison(_))
    ) {
        match &current {
            Ok(value) => record_ready_step(state, resume_node, resume_parent_id, value),
            Err(error) => record_poison_step(state, resume_node, resume_parent_id, error),
        }
    }
    match current {
        Ok(value) => Ok(EvalStep::Ready(value)),
        Err(error) => Err(error),
    }
}

pub(in crate::engine) fn resume_bytecode_tail(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    node_id: EvalNodeId,
    parent_id: Option<EvalNodeId>,
    mut bytecode: BytecodeState,
    input: Result<Value, EvalError>,
) -> Result<ResumeOutcome, EvalError> {
    let result = match input {
        Ok(value) => {
            bytecode.acc = Some(value);
            continue_bytecode_body(state, node_id, parent_id, bytecode)
        }
        Err(error) => match bytecode.pending.take() {
            Some(BytecodePendingState::Fallible { fallible_source }) => continue_bytecode_fallible(
                state,
                node_id,
                &mut bytecode,
                fallible_source,
                None,
                Some(Err(error)),
            ),
            Some(pending) => {
                bytecode.pending = Some(pending);
                Err(error)
            }
            None => Err(error),
        },
    };
    Ok(conclude_resumed_node(state, node_id, parent_id, result))
}

pub(in crate::engine) fn resume_frame(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    frame: ContinuationFrame,
    input: Result<Value, EvalError>,
) -> Result<ResumeOutcome, EvalError> {
    match frame {
        ContinuationFrame::Fallible {
            node_id,
            parent_id,
            fallible_source,
        } => Ok(conclude_resumed_node(
            state,
            node_id,
            parent_id,
            Ok(wrap_fallible_result(input, fallible_source)),
        )),
        ContinuationFrame::RuntimeCall {
            node_id,
            parent_id,
            call_source,
        } => Ok(match input {
            Ok(value) => {
                state.emit(
                    node_id,
                    parent_id,
                    Some(call_source),
                    EvalEventKind::RuntimeCallFinishedReady {
                        reuse: value.meta.reuse.clone(),
                        crossing: value.meta.crossing.clone(),
                    },
                );
                record_ready_step(state, node_id, parent_id, &value);
                ResumeOutcome::Continue(Ok(value))
            }
            Err(error) => {
                state.emit(
                    node_id,
                    parent_id,
                    Some(call_source),
                    EvalEventKind::RuntimeCallFinishedPoison {
                        kind: error.kind,
                        message: error.message.clone(),
                    },
                );
                record_poison_step(state, node_id, parent_id, &error);
                ResumeOutcome::Continue(Err(error))
            }
        }),
        ContinuationFrame::Slot {
            node_id,
            parent_id,
            slot,
        } => {
            let result = match input {
                Ok(value) => commit_slot_result(
                    state,
                    node_id,
                    parent_id,
                    &slot.call_source,
                    &slot.slot_id,
                    &slot.witness,
                    &slot.action,
                    &value,
                ),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::Bytecode {
            node_id,
            parent_id,
            bytecode,
        } => resume_bytecode_tail(state, node_id, parent_id, bytecode, input),
        ContinuationFrame::StreamToArrayJoin {
            node_id,
            parent_id,
            mut items,
            stream_provenance,
        } => {
            let result = match input {
                Ok(_) => match collect_stream_join_items(state, &mut items) {
                    Ok(StreamJoinState::Ready(mut values)) => {
                        values.sort_by(crate::value::compare_values);
                        let meta = aggregate_child_meta(stream_provenance, values.iter());
                        Ok(EvalStep::Ready(Value::array(values).with_meta(meta)))
                    }
                    Ok(StreamJoinState::Waiting(child_node_id)) => {
                        let pending = state
                            .scheduler
                            .alloc_child_dependency_pending(child_node_id);
                        return Ok(ResumeOutcome::Suspended(EvalSuspension {
                            resume_node: node_id,
                            pending,
                            branch_context: state.branch_context.clone(),
                            continuation: continuation_from_frames(vec![
                                ContinuationFrame::StreamToArrayJoin {
                                    node_id,
                                    parent_id,
                                    items: items.clone(),
                                    stream_provenance: stream_provenance.clone(),
                                },
                            ]),
                            work_invocation: state.work_invocation.clone(),
                            call_stack: Arc::from(state.call_stack.clone()),
                        }));
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::StreamMergeJoin {
            node_id,
            parent_id,
            mut items,
            stream_provenance,
        } => {
            let result = match input {
                Ok(_) => match collect_stream_join_items(state, &mut items) {
                    Ok(StreamJoinState::Ready(values)) => Ok(EvalStep::Ready(
                        internal_merged_tree_value(values, stream_provenance)?,
                    )),
                    Ok(StreamJoinState::Waiting(child_node_id)) => {
                        let pending = state
                            .scheduler
                            .alloc_child_dependency_pending(child_node_id);
                        return Ok(ResumeOutcome::Suspended(EvalSuspension {
                            resume_node: node_id,
                            pending,
                            branch_context: state.branch_context.clone(),
                            continuation: continuation_from_frames(vec![
                                ContinuationFrame::StreamMergeJoin {
                                    node_id,
                                    parent_id,
                                    items: items.clone(),
                                    stream_provenance: stream_provenance.clone(),
                                },
                            ]),
                            work_invocation: state.work_invocation.clone(),
                            call_stack: Arc::from(state.call_stack.clone()),
                        }));
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::StreamForEachJoin {
            node_id,
            parent_id,
            mut items,
            stream_provenance,
        } => {
            let result = match input {
                Ok(_) => match collect_stream_join_items(state, &mut items) {
                    Ok(StreamJoinState::Ready(_)) => Ok(EvalStep::Ready(
                        Value::unit().with_provenance(stream_provenance),
                    )),
                    Ok(StreamJoinState::Waiting(child_node_id)) => {
                        let pending = state
                            .scheduler
                            .alloc_child_dependency_pending(child_node_id);
                        return Ok(ResumeOutcome::Suspended(EvalSuspension {
                            resume_node: node_id,
                            pending,
                            branch_context: state.branch_context.clone(),
                            continuation: continuation_from_frames(vec![
                                ContinuationFrame::StreamForEachJoin {
                                    node_id,
                                    parent_id,
                                    items: items.clone(),
                                    stream_provenance: stream_provenance.clone(),
                                },
                            ]),
                            work_invocation: state.work_invocation.clone(),
                            call_stack: Arc::from(state.call_stack.clone()),
                        }));
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::StreamToMapJoin {
            node_id,
            parent_id,
            mut items,
            stream_provenance,
        } => {
            let result = match input {
                Ok(_) => match collect_stream_join_items(state, &mut items) {
                    Ok(StreamJoinState::Ready(values)) => Ok(EvalStep::Ready(stream_map_entries(
                        values,
                        stream_provenance,
                    )?)),
                    Ok(StreamJoinState::Waiting(child_node_id)) => {
                        let pending = state
                            .scheduler
                            .alloc_child_dependency_pending(child_node_id);
                        return Ok(ResumeOutcome::Suspended(EvalSuspension {
                            resume_node: node_id,
                            pending,
                            branch_context: state.branch_context.clone(),
                            continuation: continuation_from_frames(vec![
                                ContinuationFrame::StreamToMapJoin {
                                    node_id,
                                    parent_id,
                                    items: items.clone(),
                                    stream_provenance: stream_provenance.clone(),
                                },
                            ]),
                            work_invocation: state.work_invocation.clone(),
                            call_stack: Arc::from(state.call_stack.clone()),
                        }));
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::RestoreBranchContext { branch_context } => {
            state.branch_context = branch_context;
            Ok(ResumeOutcome::Continue(input))
        }
        ContinuationFrame::SearchAssumption {
            node_id,
            parent_id,
            mut search,
        } => {
            let SearchAssumptionState {
                name,
                candidates,
                resolver,
                index,
                attempts,
                root_search_start,
                search_start_index,
                prior_branch_context,
            } = &mut search;
            state.branch_context = prior_branch_context.clone();
            match input {
                Ok(value) => Ok(ResumeOutcome::Continue(Ok(value))),
                Err(error) if error.kind == EvalErrorKind::UserFail => {
                    learn_point_no_good(
                        state,
                        prior_branch_context,
                        name,
                        candidates,
                        *index,
                        resolver,
                        &error,
                    )?;
                    attempts.push(branch_attempt_value(candidates[*index].clone(), error));
                    let result = continue_search_assumption_builtin(
                        state,
                        node_id,
                        parent_id,
                        name.clone(),
                        candidates.clone(),
                        resolver.clone(),
                        *index + 1,
                        std::mem::take(attempts),
                        *root_search_start,
                        *index + 1,
                    );
                    if let Err(error) = &result
                        && *search_start_index < candidates.len()
                    {
                        let local_attempt_offset =
                            search_start_index.saturating_sub(*root_search_start);
                        let local_error =
                            local_branch_conflict_error(name.clone(), error, local_attempt_offset)
                                .unwrap_or_else(|| error.clone());
                        learn_search_no_good(
                            state,
                            prior_branch_context,
                            name,
                            &candidates[*search_start_index..],
                            resolver,
                            &local_error,
                        )?;
                    }
                    Ok(conclude_resumed_node(state, node_id, parent_id, result))
                }
                Err(error) => Ok(ResumeOutcome::Continue(Err(error))),
            }
        }
        ContinuationFrame::StructJoin {
            node_id,
            parent_id,
            type_name,
            field_names,
            child_nodes,
            struct_source,
        } => {
            let result = match input {
                Ok(_) => {
                    let mut fields = Vec::with_capacity(child_nodes.len());
                    for (name, child_node) in field_names.iter().zip(child_nodes.iter()) {
                        match state.scheduler.node_state(*child_node) {
                            Some(EvalNodeState::Ready(value)) => {
                                fields.push((name.clone(), value.as_ref().clone()))
                            }
                            Some(EvalNodeState::Poison(error)) => {
                                return Ok(ResumeOutcome::Continue(Err(error.clone())));
                            }
                            _ => {
                                let pending =
                                    state.scheduler.alloc_child_dependency_pending(*child_node);
                                return Ok(ResumeOutcome::Suspended(EvalSuspension {
                                    resume_node: node_id,
                                    pending,
                                    branch_context: state.branch_context.clone(),
                                    continuation: continuation_from_frames(vec![
                                        ContinuationFrame::StructJoin {
                                            node_id,
                                            parent_id,
                                            type_name,
                                            field_names: field_names.clone(),
                                            child_nodes: child_nodes.clone(),
                                            struct_source: struct_source.clone(),
                                        },
                                    ]),
                                    work_invocation: state.work_invocation.clone(),
                                    call_stack: Arc::from(state.call_stack.clone()),
                                }));
                            }
                        }
                    }
                    let provenance = Provenance::literal(struct_source);
                    let meta =
                        aggregate_child_meta(provenance, fields.iter().map(|(_, value)| value));
                    Ok(EvalStep::Ready(
                        Value::structure(type_name, fields).with_meta(meta),
                    ))
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::ArrayFind {
            node_id,
            parent_id,
            find,
        } => {
            let result = match input {
                Ok(result) => match result.rep {
                    ValueRep::Bool(true) => {
                        let item = find.items[find.index]
                            .clone()
                            .project(ProjectionStep::Index(find.index as u64));
                        Ok(EvalStep::Ready(if find.wrap_in_option {
                            Value::structure("Some", vec![(Arc::from("value"), item)])
                        } else {
                            item
                        }))
                    }
                    ValueRep::Bool(false) => continue_array_find_builtin(
                        state,
                        node_id,
                        parent_id,
                        ArrayFindState {
                            index: find.index + 1,
                            ..find
                        },
                    ),
                    _ => Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "array_find() predicate must return Bool",
                    )),
                },
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::ArrayFold {
            node_id,
            parent_id,
            fold,
        } => {
            let result = match input {
                Ok(accumulator) => continue_array_fold_builtin(
                    state,
                    node_id,
                    parent_id,
                    fold.items,
                    accumulator,
                    fold.step,
                    fold.index + 1,
                ),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::ArrayFilterMap {
            node_id,
            parent_id,
            filter_map,
        } => {
            let result = match input {
                Ok(value) => continue_array_filter_map_builtin(
                    state,
                    node_id,
                    parent_id,
                    filter_map.items,
                    filter_map.mapper,
                    filter_map.mapped,
                    value,
                    filter_map.index,
                ),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::ArrayAny {
            node_id,
            parent_id,
            any,
        } => {
            let result = match input {
                Ok(result) => match result.rep {
                    ValueRep::Bool(true) => Ok(EvalStep::Ready(Value::bool(true))),
                    ValueRep::Bool(false) => continue_array_any_builtin(
                        state,
                        node_id,
                        parent_id,
                        ArrayAnyState {
                            index: any.index + 1,
                            ..any
                        },
                    ),
                    _ => Err(EvalError::new(
                        EvalErrorKind::TypeError,
                        "Array.any() predicate must return Bool",
                    )),
                },
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::Field {
            node_id,
            parent_id,
            field,
        } => {
            let result = input
                .and_then(|receiver| project_field_expr_value(state, receiver, field.field_expr));
            Ok(match result {
                Ok(value) => {
                    record_ready_step(state, node_id, parent_id, &value);
                    ResumeOutcome::Continue(Ok(value))
                }
                Err(error) => {
                    record_poison_step(state, node_id, parent_id, &error);
                    ResumeOutcome::Continue(Err(error))
                }
            })
        }
    }
}
