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
        ContinuationFrame::Call {
            node_id,
            parent_id,
            mut call,
            pending,
        } => {
            let result = match input {
                Ok(value) => {
                    match pending {
                        CallResumeKind::Callee => call.callee = Some(value),
                        CallResumeKind::Arg => call.args.push(value),
                        CallResumeKind::JoinArgs => {
                            let mut args = Vec::with_capacity(call.queued_arg_nodes.len());
                            for child_node in &call.queued_arg_nodes {
                                match state.scheduler.node_state(*child_node) {
                                    Some(EvalNodeState::Ready(value)) => args.push(value.clone()),
                                    Some(EvalNodeState::Poison(error)) => {
                                        return Ok(ResumeOutcome::Continue(Err(error.clone())));
                                    }
                                    _ => {
                                        let pending = state
                                            .scheduler
                                            .alloc_child_dependency_pending(*child_node);
                                        return Ok(ResumeOutcome::Suspended(EvalSuspension {
                                            resume_node: node_id,
                                            pending,
                                            branch_context: state.branch_context.clone(),
                                            frames: vec![ContinuationFrame::Call {
                                                node_id,
                                                parent_id,
                                                call,
                                                pending: CallResumeKind::JoinArgs,
                                            }],
                                            call_stack: state.call_stack.clone(),
                                        }));
                                    }
                                }
                            }
                            call.args = args;
                            call.queued_arg_nodes.clear();
                        }
                    }
                    continue_call(state, node_id, parent_id, call)
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::BlockBody {
            node_id,
            parent_id,
            scope,
            last,
            body,
            next_step_index,
            pending,
        } => {
            let result = resume_block_body(
                state,
                node_id,
                parent_id,
                scope,
                last,
                body,
                next_step_index,
                pending,
                input,
            );
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::Array {
            node_id,
            parent_id,
            mut array,
        } => {
            let result = match input {
                Ok(value) => {
                    append_array_item(&mut array.items, value, array.spread)?;
                    continue_array_items(state, node_id, parent_id, array)
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
        ContinuationFrame::Tuple {
            node_id,
            parent_id,
            mut tuple,
        } => {
            let result = match input {
                Ok(value) => {
                    tuple.items.push(value);
                    continue_tuple_items(state, node_id, parent_id, tuple)
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
        ContinuationFrame::InterpolatedString {
            node_id,
            parent_id,
            mut string,
        } => {
            let result = match input {
                Ok(value) => {
                    string.segments.push(interpolation_segment_text(&value)?);
                    continue_interpolated_string_parts(state, node_id, parent_id, string)
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
        ContinuationFrame::Range {
            node_id,
            parent_id,
            range,
            pending,
        } => {
            let result = match input {
                Ok(value) => match pending {
                    RangeResumeKind::Start => continue_range_end(
                        state,
                        node_id,
                        parent_id,
                        Some(value),
                        range.end,
                        range.env,
                        range.range_source,
                        range.inclusive_end,
                    ),
                    RangeResumeKind::End => Ok(EvalStep::Ready(build_range_value(
                        range.start,
                        Some(value),
                        range.inclusive_end,
                        range.range_source,
                    )?)),
                },
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
        ContinuationFrame::Map {
            node_id,
            parent_id,
            mut map,
            pending,
        } => {
            let result = match input {
                Ok(value) => {
                    match pending {
                        MapResumeKind::Key => {
                            map.key = Some(value);
                        }
                        MapResumeKind::Value => {
                            let key = map
                                .key
                                .take()
                                .expect("map value resume should retain the current key");
                            map.value_expr = None;
                            map.entries.push((key, value));
                        }
                    }
                    continue_map_entries(state, node_id, parent_id, map)
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
        ContinuationFrame::If {
            node_id,
            parent_id,
            if_state,
        } => {
            let result = match input {
                Ok(condition) => continue_if(state, node_id, parent_id, if_state, condition),
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
        ContinuationFrame::Where {
            node_id,
            parent_id,
            mut where_state,
        } => {
            let result = match input {
                Ok(value) => {
                    let pending_keyword_name =
                        where_state.pending_keyword_name.take().ok_or_else(|| {
                            EvalError::new(
                                EvalErrorKind::Internal,
                                "where continuation is missing its pending keyword binding name",
                            )
                        })?;
                    where_state
                        .keyword_bindings
                        .push((pending_keyword_name, value));
                    continue_where(state, node_id, parent_id, where_state)
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::Prefix {
            node_id,
            parent_id,
            prefix,
        } => {
            let result = match input {
                Ok(value) => apply_prefix_operator(prefix.operator, value).map(EvalStep::Ready),
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::Binary {
            node_id,
            parent_id,
            mut binary,
            pending,
        } => {
            let result = match input {
                Ok(value) => {
                    match pending {
                        BinaryResumeKind::Lhs => binary.lhs = Some(value),
                        BinaryResumeKind::Rhs => {
                            let lhs = binary.lhs.take().ok_or_else(|| {
                                EvalError::new(
                                    EvalErrorKind::Internal,
                                    "binary continuation is missing its left-hand side value",
                                )
                            })?;
                            return Ok(conclude_resumed_node(
                                state,
                                node_id,
                                parent_id,
                                apply_binary_operator(lhs, binary.operator, value)
                                    .map(EvalStep::Ready),
                            ));
                        }
                    }
                    continue_binary(state, node_id, parent_id, binary)
                }
                Err(error) => Err(error),
            };
            Ok(conclude_resumed_node(state, node_id, parent_id, result))
        }
        ContinuationFrame::Struct {
            node_id,
            parent_id,
            mut struct_state,
        } => {
            let result = match input {
                Ok(value) => {
                    let part = struct_state
                        .pending_part
                        .take()
                        .expect("struct resume should retain the current literal part");
                    if let Err(error) = apply_struct_literal_part(
                        &struct_state.type_name,
                        &mut struct_state.fields,
                        part,
                        value,
                    ) {
                        return Ok(ResumeOutcome::Continue(Err(error)));
                    }
                    continue_struct_fields(state, node_id, parent_id, struct_state)
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
            let result = input.and_then(|receiver| {
                project_field_or_method_value(state, receiver, field.field_name, field.field_source)
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
