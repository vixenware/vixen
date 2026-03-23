use super::*;

pub(in crate::engine) fn resume_suspension(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    suspension: EvalSuspension,
    result: Result<Value, EvalError>,
) -> Result<EvalStep, EvalError> {
    let EvalSuspension {
        resume_node,
        pending: _,
        branch_context: _,
        frames,
        call_stack,
    } = suspension;
    if call_stack.is_empty() {
        let mut current = result;
        let parent_id = state
            .scheduler
            .node_record(resume_node)
            .and_then(|record| record.parent_id);
        let mut index = 0;
        while index < frames.len() {
            match resume_frame(state, frames[index].clone(), current)? {
                ResumeOutcome::Continue(next) => current = next,
                ResumeOutcome::Suspended(mut new_suspension) => {
                    new_suspension
                        .frames
                        .extend(frames[index + 1..].iter().cloned());
                    return Ok(EvalStep::Suspended(new_suspension));
                }
            }
            index += 1;
        }
        if !matches!(
            state.scheduler.node_state(resume_node),
            Some(EvalNodeState::Ready(_)) | Some(EvalNodeState::Poison(_))
        ) {
            match &current {
                Ok(value) => record_ready_step(state, resume_node, parent_id, value),
                Err(error) => record_poison_step(state, resume_node, parent_id, error),
            }
        }
        match current {
            Ok(value) => Ok(EvalStep::Ready(value)),
            Err(error) => Err(error),
        }
    } else {
        state.with_call_stack(call_stack, |state| {
            let mut current = result;
            let parent_id = state
                .scheduler
                .node_record(resume_node)
                .and_then(|record| record.parent_id);
            let mut index = 0;
            while index < frames.len() {
                match resume_frame(state, frames[index].clone(), current)? {
                    ResumeOutcome::Continue(next) => current = next,
                    ResumeOutcome::Suspended(mut new_suspension) => {
                        new_suspension
                            .frames
                            .extend(frames[index + 1..].iter().cloned());
                        return Ok(EvalStep::Suspended(new_suspension));
                    }
                }
                index += 1;
            }
            if !matches!(
                state.scheduler.node_state(resume_node),
                Some(EvalNodeState::Ready(_)) | Some(EvalNodeState::Poison(_))
            ) {
                match &current {
                    Ok(value) => record_ready_step(state, resume_node, parent_id, value),
                    Err(error) => record_poison_step(state, resume_node, parent_id, error),
                }
            }
            match current {
                Ok(value) => Ok(EvalStep::Ready(value)),
                Err(error) => Err(error),
            }
        })
    }
}

pub(in crate::engine) fn resume_frame(
    state: &mut EvalState<'_, '_, impl RuntimeHooks, impl EvalEventSink>,
    frame: ContinuationFrame,
    input: Result<Value, EvalError>,
) -> Result<ResumeOutcome, EvalError> {
    match frame {
        ContinuationFrame::FinishNode { node_id, parent_id } => Ok(match input {
            Ok(value) => {
                record_ready_step(state, node_id, parent_id, &value);
                ResumeOutcome::Continue(Ok(value))
            }
            Err(error) => {
                record_poison_step(state, node_id, parent_id, &error);
                ResumeOutcome::Continue(Err(error))
            }
        }),
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
        ContinuationFrame::SlotAfterResolver {
            node_id,
            parent_id,
            call_source,
            slot_id,
            witness,
            action,
        } => {
            let result = match input {
                Ok(value) => commit_slot_result(
                    state,
                    node_id,
                    parent_id,
                    &call_source,
                    &slot_id,
                    &witness,
                    &action,
                    &value,
                ),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::CallJoin {
            node_id,
            parent_id,
            callee,
            call_source,
            type_args,
            child_nodes,
        } => {
            let result = match input {
                Ok(_) => {
                    let mut args = Vec::with_capacity(child_nodes.len());
                    for child_node in &child_nodes {
                        match state.scheduler.node_state(*child_node) {
                            Some(EvalNodeState::Ready(value)) => args.push(value.clone()),
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
                                    frames: vec![ContinuationFrame::CallJoin {
                                        node_id,
                                        parent_id,
                                        callee: callee.clone(),
                                        call_source,
                                        type_args: Arc::clone(&type_args),
                                        child_nodes: child_nodes.clone(),
                                    }],
                                    call_stack: state.call_stack.clone(),
                                }));
                            }
                        }
                    }
                    continue_call_from_args(
                        state,
                        node_id,
                        parent_id,
                        callee,
                        call_source,
                        type_args,
                        args,
                        Vec::new(),
                        empty_env(),
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::CallAfterCallee {
            node_id,
            parent_id,
            call_source,
            type_args,
            env,
            args,
            remaining_args,
        } => {
            let result = match input {
                Ok(callee) => continue_call_from_args(
                    state,
                    node_id,
                    parent_id,
                    callee,
                    call_source,
                    type_args,
                    args,
                    remaining_args,
                    env,
                ),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::BlockAfterLet {
            node_id,
            parent_id,
            mut scope,
            last,
            pattern,
            remaining_statements,
        } => {
            let result = match input {
                Ok(value) => {
                    bind_let_pattern(&mut scope, &pattern, value)?;
                    continue_block_statements(
                        state,
                        node_id,
                        parent_id,
                        scope,
                        last,
                        remaining_statements,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::BlockAfterExpr {
            node_id,
            parent_id,
            scope,
            remaining_statements,
        } => {
            let result = match input {
                Ok(value) => continue_block_statements(
                    state,
                    node_id,
                    parent_id,
                    scope,
                    value,
                    remaining_statements,
                ),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::CallAfterArg {
            node_id,
            parent_id,
            callee,
            call_source,
            type_args,
            mut args,
            remaining_args,
            env,
        } => {
            let result = match input {
                Ok(value) => {
                    args.push(value);
                    continue_call_from_args(
                        state,
                        node_id,
                        parent_id,
                        callee,
                        call_source,
                        type_args,
                        args,
                        remaining_args,
                        env,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::ArrayAfterItem {
            node_id,
            parent_id,
            mut items,
            spread,
            remaining_items,
            env,
            array_source,
        } => {
            let result = match input {
                Ok(value) => {
                    append_array_item(&mut items, value, spread)?;
                    continue_array_items(
                        state,
                        node_id,
                        parent_id,
                        items,
                        remaining_items,
                        env,
                        array_source,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::ArrayJoin {
            node_id,
            parent_id,
            child_nodes,
            array_source,
        } => {
            let result = match input {
                Ok(_) => {
                    let mut items = Vec::new();
                    for child_node in &child_nodes {
                        let (child_node_id, spread) = match child_node {
                            ArrayJoinItem::Expr(child_node_id) => (*child_node_id, false),
                            ArrayJoinItem::Spread(child_node_id) => (*child_node_id, true),
                        };
                        match state.scheduler.node_state(child_node_id) {
                            Some(EvalNodeState::Ready(value)) => {
                                append_array_item(&mut items, value.clone(), spread)?
                            }
                            Some(EvalNodeState::Poison(error)) => {
                                return Ok(ResumeOutcome::Continue(Err(error.clone())));
                            }
                            _ => {
                                let pending = state
                                    .scheduler
                                    .alloc_child_dependency_pending(child_node_id);
                                return Ok(ResumeOutcome::Suspended(EvalSuspension {
                                    resume_node: node_id,
                                    pending,
                                    branch_context: state.branch_context.clone(),
                                    frames: vec![ContinuationFrame::ArrayJoin {
                                        node_id,
                                        parent_id,
                                        child_nodes: child_nodes.clone(),
                                        array_source: array_source.clone(),
                                    }],
                                    call_stack: state.call_stack.clone(),
                                }));
                            }
                        }
                    }
                    let provenance = Provenance::literal(array_source);
                    let meta = aggregate_child_meta(provenance, items.iter());
                    Ok(EvalStep::Ready(Value::array(items).with_meta(meta)))
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
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
                            frames: vec![ContinuationFrame::StreamToArrayJoin {
                                node_id,
                                parent_id,
                                items: items.clone(),
                                stream_provenance: stream_provenance.clone(),
                            }],
                            call_stack: state.call_stack.clone(),
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
                            frames: vec![ContinuationFrame::StreamMergeJoin {
                                node_id,
                                parent_id,
                                items: items.clone(),
                                stream_provenance: stream_provenance.clone(),
                            }],
                            call_stack: state.call_stack.clone(),
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
                            frames: vec![ContinuationFrame::StreamForEachJoin {
                                node_id,
                                parent_id,
                                items: items.clone(),
                                stream_provenance: stream_provenance.clone(),
                            }],
                            call_stack: state.call_stack.clone(),
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
                            frames: vec![ContinuationFrame::StreamToMapJoin {
                                node_id,
                                parent_id,
                                items: items.clone(),
                                stream_provenance: stream_provenance.clone(),
                            }],
                            call_stack: state.call_stack.clone(),
                        }));
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::TupleAfterItem {
            node_id,
            parent_id,
            mut items,
            remaining_items,
            env,
            tuple_source,
        } => {
            let result = match input {
                Ok(value) => {
                    items.push(value);
                    continue_tuple_items(
                        state,
                        node_id,
                        parent_id,
                        items,
                        remaining_items,
                        env,
                        tuple_source,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::TupleJoin {
            node_id,
            parent_id,
            child_nodes,
            tuple_source,
        } => {
            let result = match input {
                Ok(_) => {
                    let mut items = Vec::with_capacity(child_nodes.len());
                    for child_node in &child_nodes {
                        match state.scheduler.node_state(*child_node) {
                            Some(EvalNodeState::Ready(value)) => items.push(value.clone()),
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
                                    frames: vec![ContinuationFrame::TupleJoin {
                                        node_id,
                                        parent_id,
                                        child_nodes: child_nodes.clone(),
                                        tuple_source: tuple_source.clone(),
                                    }],
                                    call_stack: state.call_stack.clone(),
                                }));
                            }
                        }
                    }
                    let provenance = Provenance::literal(tuple_source);
                    let meta = aggregate_child_meta(provenance, items.iter());
                    Ok(EvalStep::Ready(Value::tuple(items).with_meta(meta)))
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::InterpolatedStringAfterExpr {
            node_id,
            parent_id,
            mut segments,
            remaining_parts,
            env,
            string_source,
        } => {
            let result = match input {
                Ok(value) => {
                    segments.push(interpolation_segment_text(&value)?);
                    continue_interpolated_string_parts(
                        state,
                        node_id,
                        parent_id,
                        segments,
                        remaining_parts,
                        env,
                        string_source,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::InterpolatedStringJoin {
            node_id,
            parent_id,
            parts,
            expr_nodes,
            string_source,
        } => {
            let result = match input {
                Ok(_) => {
                    let mut segments = Vec::with_capacity(parts.len());
                    let mut expr_index = 0usize;
                    for part in &parts {
                        match part {
                            InterpolatedPart::Text(text) => segments.push(text.to_string()),
                            InterpolatedPart::Expr(_) => {
                                let child_node = *expr_nodes.get(expr_index).expect(
                                    "interpolated string expr node count should match parts",
                                );
                                expr_index += 1;
                                match state.scheduler.node_state(child_node) {
                                    Some(EvalNodeState::Ready(value)) => {
                                        segments.push(interpolation_segment_text(value)?);
                                    }
                                    Some(EvalNodeState::Poison(error)) => {
                                        return Ok(ResumeOutcome::Continue(Err(error.clone())));
                                    }
                                    _ => {
                                        let pending = state
                                            .scheduler
                                            .alloc_child_dependency_pending(child_node);
                                        return Ok(ResumeOutcome::Suspended(EvalSuspension {
                                            resume_node: node_id,
                                            pending,
                                            branch_context: state.branch_context.clone(),
                                            frames: vec![
                                                ContinuationFrame::InterpolatedStringJoin {
                                                    node_id,
                                                    parent_id,
                                                    parts: parts.clone(),
                                                    expr_nodes: expr_nodes.clone(),
                                                    string_source: string_source.clone(),
                                                },
                                            ],
                                            call_stack: state.call_stack.clone(),
                                        }));
                                    }
                                }
                            }
                        }
                    }
                    Ok(EvalStep::Ready(ready_interpolated_string_value(
                        segments,
                        string_source,
                    )))
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::RangeAfterStart {
            node_id,
            parent_id,
            end,
            env,
            range_source,
            inclusive_end,
        } => {
            let result = match input {
                Ok(start) => continue_range_end(
                    state,
                    node_id,
                    parent_id,
                    Some(start),
                    end,
                    env,
                    range_source,
                    inclusive_end,
                ),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::RangeAfterEnd {
            node_id,
            parent_id,
            start,
            range_source,
            inclusive_end,
        } => {
            let result = match input {
                Ok(end) => Ok(EvalStep::Ready(build_range_value(
                    start,
                    Some(end),
                    inclusive_end,
                    range_source,
                )?)),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::RangeJoin {
            node_id,
            parent_id,
            start_child,
            end_child,
            range_source,
            inclusive_end,
        } => {
            let result = match input {
                Ok(_) => {
                    let start = match start_child {
                        Some(child_node) => match state.scheduler.node_state(child_node) {
                            Some(EvalNodeState::Ready(value)) => Some(value.clone()),
                            Some(EvalNodeState::Poison(error)) => {
                                return Ok(ResumeOutcome::Continue(Err(error.clone())));
                            }
                            _ => {
                                let pending =
                                    state.scheduler.alloc_child_dependency_pending(child_node);
                                return Ok(ResumeOutcome::Suspended(EvalSuspension {
                                    resume_node: node_id,
                                    pending,
                                    branch_context: state.branch_context.clone(),
                                    frames: vec![ContinuationFrame::RangeJoin {
                                        node_id,
                                        parent_id,
                                        start_child,
                                        end_child,
                                        range_source: range_source.clone(),
                                        inclusive_end,
                                    }],
                                    call_stack: state.call_stack.clone(),
                                }));
                            }
                        },
                        None => None,
                    };
                    let end = match end_child {
                        Some(child_node) => match state.scheduler.node_state(child_node) {
                            Some(EvalNodeState::Ready(value)) => Some(value.clone()),
                            Some(EvalNodeState::Poison(error)) => {
                                return Ok(ResumeOutcome::Continue(Err(error.clone())));
                            }
                            _ => {
                                let pending =
                                    state.scheduler.alloc_child_dependency_pending(child_node);
                                return Ok(ResumeOutcome::Suspended(EvalSuspension {
                                    resume_node: node_id,
                                    pending,
                                    branch_context: state.branch_context.clone(),
                                    frames: vec![ContinuationFrame::RangeJoin {
                                        node_id,
                                        parent_id,
                                        start_child,
                                        end_child,
                                        range_source: range_source.clone(),
                                        inclusive_end,
                                    }],
                                    call_stack: state.call_stack.clone(),
                                }));
                            }
                        },
                        None => None,
                    };
                    Ok(EvalStep::Ready(build_range_value(
                        start,
                        end,
                        inclusive_end,
                        range_source,
                    )?))
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::MapAfterKey {
            node_id,
            parent_id,
            entries,
            value_expr,
            remaining_entries,
            mut env,
            map_source,
        } => {
            let result = match input {
                Ok(key) => {
                    let value =
                        match eval_dependent_expr_handle(state, node_id, value_expr, &mut env)? {
                            EvalStep::Ready(value) => value,
                            EvalStep::Suspended(suspension) => {
                                return Ok(ResumeOutcome::Suspended(extend_resume(
                                    suspension,
                                    ContinuationFrame::MapAfterValue {
                                        node_id,
                                        parent_id,
                                        entries,
                                        key,
                                        remaining_entries,
                                        env,
                                        map_source,
                                    },
                                )));
                            }
                        };
                    let mut entries = entries;
                    entries.push((key, value));
                    continue_map_entries(
                        state,
                        node_id,
                        parent_id,
                        entries,
                        remaining_entries,
                        env,
                        map_source,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::MapAfterValue {
            node_id,
            parent_id,
            mut entries,
            key,
            remaining_entries,
            env,
            map_source,
        } => {
            let result = match input {
                Ok(value) => {
                    entries.push((key, value));
                    continue_map_entries(
                        state,
                        node_id,
                        parent_id,
                        entries,
                        remaining_entries,
                        env,
                        map_source,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::FailMessage { node_id, parent_id } => {
            let result = match input {
                Ok(value) => Err(user_fail_error(value)),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::RestoreBranchContext { branch_context } => {
            state.branch_context = branch_context;
            Ok(ResumeOutcome::Continue(input))
        }
        ContinuationFrame::SearchAssumptionAfterBranch {
            node_id,
            parent_id,
            name,
            candidates,
            resolver,
            index,
            mut attempts,
            root_search_start,
            search_start_index,
            prior_branch_context,
        } => {
            state.branch_context = prior_branch_context.clone();
            match input {
                Ok(value) => Ok(ResumeOutcome::Continue(Ok(value))),
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
                    attempts.push(branch_attempt_value(candidates[index].clone(), error));
                    let result = continue_search_assumption_builtin(
                        state,
                        node_id,
                        parent_id,
                        name.clone(),
                        candidates.clone(),
                        resolver.clone(),
                        index + 1,
                        attempts,
                        root_search_start,
                        index + 1,
                    );
                    if let Err(error) = &result
                        && search_start_index < candidates.len()
                    {
                        let local_attempt_offset =
                            search_start_index.saturating_sub(root_search_start);
                        let local_error =
                            local_branch_conflict_error(name.clone(), error, local_attempt_offset)
                                .unwrap_or_else(|| error.clone());
                        learn_search_no_good(
                            state,
                            &prior_branch_context,
                            &name,
                            &candidates[search_start_index..],
                            &resolver,
                            &local_error,
                        )?;
                    }
                    Ok(conclude_resumed_node(state, node_id, parent_id, result))
                }
                Err(error) => Ok(ResumeOutcome::Continue(Err(error))),
            }
        }
        ContinuationFrame::IfCondition {
            node_id,
            parent_id,
            then_branch,
            else_branch,
            mut env,
        } => {
            let result = match input {
                Ok(condition) => {
                    let ValueRep::Bool(value) = condition.rep else {
                        return Err(EvalError::new(
                            EvalErrorKind::TypeError,
                            "if condition must evaluate to Bool",
                        ));
                    };
                    match eval_dependent_expr_handle(
                        state,
                        node_id,
                        if value { then_branch } else { else_branch },
                        &mut env,
                    )? {
                        EvalStep::Ready(value) => Ok(EvalStep::Ready(value)),
                        EvalStep::Suspended(suspension) => Ok(extend_suspension(
                            suspension,
                            ContinuationFrame::FinishNode { node_id, parent_id },
                        )),
                    }
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::MatchScrutinee {
            node_id,
            parent_id,
            arms,
            env,
        } => {
            let result = match input {
                Ok(scrutinee) => {
                    continue_match_arms(state, node_id, parent_id, scrutinee, arms, env)
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::WhereAfterKeyword {
            node_id,
            parent_id,
            call,
            pending_keyword_name,
            mut keyword_bindings,
            remaining_keyword_bindings,
            env,
        } => {
            let result = match input {
                Ok(value) => {
                    let expr = state.program.resolve_expr(call.key)?;
                    let ExprKind::Call(_) = expr.kind() else {
                        return Err(EvalError::new(
                            EvalErrorKind::Internal,
                            "where continuation expected a call expression",
                        ));
                    };
                    keyword_bindings.push((pending_keyword_name, value));
                    continue_where_keyword_bindings(
                        state,
                        node_id,
                        parent_id,
                        call,
                        keyword_bindings,
                        remaining_keyword_bindings,
                        env,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::PrefixOperand {
            node_id,
            parent_id,
            operator,
        } => {
            let result = match input {
                Ok(value) => apply_prefix_operator(operator, value).map(EvalStep::Ready),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::BinaryLhs {
            node_id,
            parent_id,
            operator,
            rhs,
            env,
        } => {
            let result = match input {
                Ok(lhs) => continue_binary_rhs(state, node_id, parent_id, lhs, operator, rhs, env),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::BinaryRhs {
            node_id,
            parent_id,
            lhs,
            operator,
        } => {
            let result = match input {
                Ok(rhs) => apply_binary_operator(lhs, operator, rhs).map(EvalStep::Ready),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::StructAfterPart {
            node_id,
            parent_id,
            type_name,
            mut fields,
            part,
            remaining_parts,
            env,
            struct_source,
        } => {
            let result = match input {
                Ok(value) => {
                    if let Err(error) =
                        apply_struct_literal_part(&type_name, &mut fields, part, value)
                    {
                        return Ok(ResumeOutcome::Continue(Err(error)));
                    }
                    continue_struct_fields(
                        state,
                        node_id,
                        parent_id,
                        type_name,
                        fields,
                        remaining_parts,
                        env,
                        struct_source,
                    )
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
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
                                fields.push((name.clone(), value.clone()))
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
                                    frames: vec![ContinuationFrame::StructJoin {
                                        node_id,
                                        parent_id,
                                        type_name,
                                        field_names: field_names.clone(),
                                        child_nodes: child_nodes.clone(),
                                        struct_source: struct_source.clone(),
                                    }],
                                    call_stack: state.call_stack.clone(),
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
        ContinuationFrame::ArrayFindAfterPredicate {
            node_id,
            parent_id,
            items,
            predicate,
            index,
            wrap_in_option,
        } => {
            let result = match input {
                Ok(result) => match result.rep {
                    ValueRep::Bool(true) => {
                        let item = items[index]
                            .clone()
                            .project(ProjectionStep::Index(index as u64));
                        Ok(EvalStep::Ready(if wrap_in_option {
                            Value::structure("Some", vec![(Arc::from("value"), item)])
                        } else {
                            item
                        }))
                    }
                    ValueRep::Bool(false) => continue_array_find_builtin(
                        state,
                        node_id,
                        parent_id,
                        items,
                        predicate,
                        index + 1,
                        wrap_in_option,
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
        ContinuationFrame::ArrayFoldAfterStep {
            node_id,
            parent_id,
            items,
            step,
            index,
        } => {
            let result = match input {
                Ok(accumulator) => continue_array_fold_builtin(
                    state,
                    node_id,
                    parent_id,
                    items,
                    accumulator,
                    step,
                    index + 1,
                ),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::ArrayFilterMapAfterMapper {
            node_id,
            parent_id,
            items,
            mapper,
            mapped,
            index,
        } => {
            let result = match input {
                Ok(value) => continue_array_filter_map_builtin(
                    state, node_id, parent_id, items, mapper, mapped, value, index,
                ),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::ArrayAnyAfterPredicate {
            node_id,
            parent_id,
            items,
            predicate,
            index,
        } => {
            let result = match input {
                Ok(result) => match result.rep {
                    ValueRep::Bool(true) => Ok(EvalStep::Ready(Value::bool(true))),
                    ValueRep::Bool(false) => continue_array_any_builtin(
                        state,
                        node_id,
                        parent_id,
                        items,
                        predicate,
                        index + 1,
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
        ContinuationFrame::FieldReceiver {
            node_id,
            parent_id,
            field_name,
            field_source,
        } => {
            let result = input.and_then(|receiver| {
                project_field_or_method_value(state, receiver, field_name, field_source)
            });
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

pub(in crate::engine) fn extend_resume(
    mut suspension: EvalSuspension,
    frame: ContinuationFrame,
) -> EvalSuspension {
    suspension.frames.push(frame);
    suspension
}
