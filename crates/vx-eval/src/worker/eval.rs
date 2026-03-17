use super::*;

impl Engine {
    pub(super) fn force_value_shallow(
        &self,
        value: Value,
    ) -> BoxFuture<'_, Result<Value, PoisonError>> {
        Box::pin(async move {
            match value {
                Value::Pending(node_id) => self.force_pending_shallow(node_id).await,
                Value::Discovered(discovered) => {
                    let inner = self.force_value_shallow(*discovered.value).await?;
                    Ok(self.attach_obligations(inner, discovered.obligations.iter().cloned()))
                }
                other => Ok(other),
            }
        })
    }

    pub(super) fn demand_value_shallow(
        &self,
        value: Value,
    ) -> BoxFuture<'_, Result<Value, PoisonError>> {
        Box::pin(async move {
            match value {
                Value::Pending(node_id) => {
                    self.demand_value_shallow(self.force_pending_shallow(node_id).await?)
                        .await
                }
                Value::Discovered(discovered) => {
                    let inner = self.demand_value_shallow(*discovered.value).await?;
                    Ok(self.attach_obligations(inner, discovered.obligations.iter().cloned()))
                }
                other => Ok(other),
            }
        })
    }

    pub(super) fn force_value(&self, value: Value) -> BoxFuture<'_, Result<Value, PoisonError>> {
        Box::pin(async move {
            match value {
                Value::Pending(node_id) => {
                    let value = Scheduler::with_initial_stack(
                        self.arc_self(),
                        self.current_demand_stack_ids(),
                    )
                    .resolve_node_shallow(node_id)
                    .await?;
                    self.force_value(value).await
                }
                Value::Discovered(discovered) => {
                    let inner = self.force_value(*discovered.value).await?;
                    Ok(self.attach_obligations(inner, discovered.obligations.iter().cloned()))
                }
                Value::Tuple(elems) => {
                    let forced = self.force_many(elems.iter().cloned().collect()).await?;
                    Ok(Value::Tuple(Arc::from(forced)))
                }
                Value::Array(items) => {
                    let forced = self.force_many(items.iter().cloned().collect()).await?;
                    Ok(Value::Array(Arc::from(forced)))
                }
                Value::Map(map) => {
                    let keys = map
                        .entries
                        .iter()
                        .map(|(key, _)| key.clone())
                        .collect::<Vec<_>>();
                    let values = map
                        .entries
                        .iter()
                        .map(|(_, value)| value.clone())
                        .collect::<Vec<_>>();
                    let forced_keys = self.force_many(keys).await?;
                    let forced_values = self.force_many(values).await?;
                    Ok(Value::Map(MapValue {
                        entries: Arc::from(
                            forced_keys
                                .into_iter()
                                .zip(forced_values)
                                .collect::<Vec<_>>(),
                        ),
                    }))
                }
                Value::Tree(tree) => match tree {
                    TreeValue::Inline { entries, .. } => {
                        let names = entries
                            .iter()
                            .map(|(name, _)| name.clone())
                            .collect::<Vec<_>>();
                        let values = entries
                            .iter()
                            .map(|(_, value)| value.clone())
                            .collect::<Vec<_>>();
                        let forced = self.force_many(values).await?;
                        self.validate_tree_entries(&forced)?;
                        Ok(Value::Tree(TreeValue::inline(Arc::from(
                            names.into_iter().zip(forced).collect::<Vec<_>>(),
                        ))))
                    }
                    TreeValue::FsDir(path) => Ok(Value::Tree(TreeValue::FsDir(path))),
                    TreeValue::Store(root) => Ok(Value::Tree(TreeValue::Store(root))),
                },
                Value::Struct(struct_value) => {
                    let names = struct_value
                        .fields
                        .iter()
                        .map(|(name, _)| name.clone())
                        .collect::<Vec<_>>();
                    let values = struct_value
                        .fields
                        .iter()
                        .map(|(_, value)| value.clone())
                        .collect::<Vec<_>>();
                    let forced = self.force_many(values).await?;
                    let fields = names.into_iter().zip(forced).collect::<Vec<_>>();
                    Ok(Value::Struct(StructValue {
                        type_name: struct_value.type_name,
                        fields: Arc::from(fields),
                    }))
                }
                other => Ok(other),
            }
        })
    }

    // r[impl eval.concurrency]
    // r[impl error.propagation]
    pub(super) async fn force_many(&self, values: Vec<Value>) -> Result<Vec<Value>, PoisonError> {
        use tokio::task::JoinSet;

        let mut join_set = JoinSet::new();
        let engine = self.arc_self();
        let demand_stack = self.current_demand_stack_ids();

        for (idx, value) in values.into_iter().enumerate() {
            let engine = Arc::clone(&engine);
            let stack = demand_stack.clone();
            join_set.spawn(async move {
                let result = DEMAND_STACK
                    .scope(RefCell::new(stack), async {
                        engine.force_value(value).await
                    })
                    .await;
                (idx, result)
            });
        }

        let mut results: Vec<Option<Result<Value, PoisonError>>> =
            (0..join_set.len()).map(|_| None).collect();
        while let Some(join_result) = join_set.join_next().await {
            match join_result {
                Ok((idx, result)) => {
                    results[idx] = Some(result);
                }
                Err(join_error) => {
                    if join_error.is_panic() {
                        std::panic::resume_unwind(join_error.into_panic());
                    }
                    return Err(self.internal_error("force_many: task was cancelled"));
                }
            }
        }

        let mut forced = Vec::with_capacity(results.len());
        let mut errors = Vec::new();
        for result in results {
            match result.expect("all tasks should complete") {
                Ok(value) => forced.push(value),
                Err(error) => errors.push(error),
            }
        }
        if errors.is_empty() {
            Ok(forced)
        } else {
            Err(self.aggregate_poison_errors("forcing dependencies", errors))
        }
    }

    pub(crate) async fn inspect_value(&self, value: Value) -> Result<InspectedValue, PoisonError> {
        self.inspect_value_inner(value, &mut Vec::new()).await
    }

    pub(super) fn inspect_value_inner<'a>(
        &'a self,
        value: Value,
        visiting: &'a mut Vec<NodeId>,
    ) -> BoxFuture<'a, Result<InspectedValue, PoisonError>> {
        Box::pin(async move {
            match value {
                Value::Pending(node_id) => match self.node_state(node_id)? {
                    SchedulerNodeState::Pending | SchedulerNodeState::Evaluating => {
                        let mut pending = BTreeSet::new();
                        pending.insert(node_id);
                        Ok(InspectedValue {
                            value: Value::Pending(node_id),
                            pending,
                            errors: Vec::new(),
                        })
                    }
                    SchedulerNodeState::Resolved(value) => {
                        if visiting.contains(&node_id) {
                            return Err(self.circular_dependency_error(visiting, node_id));
                        }
                        if visiting.len() + 1 > self.max_demand_depth {
                            return Err(self.demand_depth_exceeded_error(visiting, node_id));
                        }
                        visiting.push(node_id);
                        let result = self.inspect_value_inner(value, visiting).await;
                        let popped = visiting.pop();
                        debug_assert_eq!(popped, Some(node_id));
                        result
                    }
                    SchedulerNodeState::Poisoned(error) => Ok(InspectedValue {
                        value: Value::Pending(node_id),
                        pending: BTreeSet::new(),
                        errors: vec![error],
                    }),
                },
                Value::Discovered(discovered) => {
                    let inspected = self
                        .inspect_value_inner(*discovered.value, visiting)
                        .await?;
                    Ok(InspectedValue {
                        value: self.attach_obligations(
                            inspected.value,
                            discovered.obligations.iter().cloned(),
                        ),
                        pending: inspected.pending,
                        errors: inspected.errors,
                    })
                }
                Value::Tuple(elems) => {
                    let (elems, pending, errors) =
                        self.inspect_list(elems.iter().cloned(), visiting).await?;
                    Ok(InspectedValue {
                        value: Value::Tuple(Arc::from(elems)),
                        pending,
                        errors,
                    })
                }
                Value::Array(items) => {
                    let (items, pending, errors) =
                        self.inspect_list(items.iter().cloned(), visiting).await?;
                    Ok(InspectedValue {
                        value: Value::Array(Arc::from(items)),
                        pending,
                        errors,
                    })
                }
                Value::Map(map) => {
                    let mut entries = Vec::with_capacity(map.entries.len());
                    let mut pending = BTreeSet::new();
                    let mut errors = Vec::new();
                    for (key, value) in map.entries.iter() {
                        let key = self.inspect_value_inner(key.clone(), visiting).await?;
                        let value = self.inspect_value_inner(value.clone(), visiting).await?;
                        pending.extend(key.pending);
                        pending.extend(value.pending);
                        errors.extend(key.errors);
                        errors.extend(value.errors);
                        entries.push((key.value, value.value));
                    }
                    Ok(InspectedValue {
                        value: Value::Map(MapValue {
                            entries: Arc::from(entries),
                        }),
                        pending,
                        errors,
                    })
                }
                Value::ExecPath(path) => match path {
                    ExecPathValue::InTree { tree, suffix } => {
                        let tree = self.inspect_tree(tree, visiting).await?;
                        Ok(InspectedValue {
                            value: Value::ExecPath(ExecPathValue::InTree {
                                tree: match tree.value {
                                    Value::Tree(tree) => tree,
                                    other => {
                                        return Err(self.internal_error(&format!(
                                            "expected Tree while inspecting ExecPath, got {}",
                                            other.type_name()
                                        )));
                                    }
                                },
                                suffix,
                            }),
                            pending: tree.pending,
                            errors: tree.errors,
                        })
                    }
                    ExecPathValue::InBlob { blob, leaf } => Ok(InspectedValue::resolved(
                        Value::ExecPath(ExecPathValue::InBlob { blob, leaf }),
                    )),
                },
                Value::Tree(tree) => self.inspect_tree(tree, visiting).await,
                Value::Struct(struct_value) => {
                    let mut fields = Vec::with_capacity(struct_value.fields.len());
                    let mut pending = BTreeSet::new();
                    let mut errors = Vec::new();
                    for (name, value) in struct_value.fields.iter() {
                        let value = self.inspect_value_inner(value.clone(), visiting).await?;
                        pending.extend(value.pending);
                        errors.extend(value.errors);
                        fields.push((name.clone(), value.value));
                    }
                    Ok(InspectedValue {
                        value: Value::Struct(StructValue {
                            type_name: struct_value.type_name,
                            fields: Arc::from(fields),
                        }),
                        pending,
                        errors,
                    })
                }
                Value::Function(function) => {
                    let mut captures = Vec::with_capacity(function.captures.len());
                    let mut pending = BTreeSet::new();
                    let mut errors = Vec::new();
                    for (name, value) in function.captures.iter() {
                        let value = self.inspect_value_inner(value.clone(), visiting).await?;
                        pending.extend(value.pending);
                        errors.extend(value.errors);
                        captures.push((name.clone(), value.value));
                    }
                    Ok(InspectedValue {
                        value: Value::Function(FunctionValue {
                            module_id: function.module_id,
                            def: function.def,
                            code_identity: function.code_identity,
                            captures: Arc::from(captures),
                        }),
                        pending,
                        errors,
                    })
                }
                Value::Partial(partial) => {
                    let function = self
                        .inspect_value_inner(partial.function.clone(), visiting)
                        .await?;
                    let (args, mut pending, mut errors) = self
                        .inspect_list(partial.args.iter().cloned(), visiting)
                        .await?;
                    pending.extend(function.pending);
                    errors.extend(function.errors);
                    let mut kwargs = Vec::with_capacity(partial.kwargs.len());
                    for (name, value) in partial.kwargs.iter() {
                        let value = self.inspect_value_inner(value.clone(), visiting).await?;
                        pending.extend(value.pending);
                        errors.extend(value.errors);
                        kwargs.push((name.clone(), value.value));
                    }
                    let kwargs_expr = match &partial.kwargs_expr {
                        Some(kwargs_expr) => {
                            let value = self
                                .inspect_value_inner((**kwargs_expr).clone(), visiting)
                                .await?;
                            pending.extend(value.pending);
                            errors.extend(value.errors);
                            Some(Box::new(value.value))
                        }
                        None => None,
                    };
                    Ok(InspectedValue {
                        value: Value::Partial(Arc::new(PartialValue {
                            function: function.value,
                            args: Arc::from(args),
                            kwargs: Arc::from(kwargs),
                            kwargs_expr,
                        })),
                        pending,
                        errors,
                    })
                }
                Value::Builtin(builtin) => {
                    let (builtin, pending, errors) =
                        self.inspect_builtin(builtin, visiting).await?;
                    Ok(InspectedValue {
                        value: Value::Builtin(builtin),
                        pending,
                        errors,
                    })
                }
                other => Ok(InspectedValue::resolved(other)),
            }
        })
    }

    pub(super) async fn inspect_list(
        &self,
        values: impl IntoIterator<Item = Value>,
        visiting: &mut Vec<NodeId>,
    ) -> Result<InspectedList, PoisonError> {
        let mut out = Vec::new();
        let mut pending = BTreeSet::new();
        let mut errors = Vec::new();
        for value in values {
            let inspected = self.inspect_value_inner(value, visiting).await?;
            out.push(inspected.value);
            pending.extend(inspected.pending);
            errors.extend(inspected.errors);
        }
        Ok((out, pending, errors))
    }

    pub(super) async fn inspect_tree(
        &self,
        tree: TreeValue,
        visiting: &mut Vec<NodeId>,
    ) -> Result<InspectedValue, PoisonError> {
        match tree {
            TreeValue::Inline { entries, .. } => {
                let mut out = Vec::with_capacity(entries.len());
                let mut pending = BTreeSet::new();
                let mut errors = Vec::new();
                for (name, value) in entries.iter() {
                    let value = self.inspect_value_inner(value.clone(), visiting).await?;
                    pending.extend(value.pending);
                    errors.extend(value.errors);
                    out.push((name.clone(), value.value));
                }
                Ok(InspectedValue {
                    value: Value::Tree(TreeValue::inline(Arc::from(out))),
                    pending,
                    errors,
                })
            }
            TreeValue::FsDir(path) => Ok(InspectedValue::resolved(Value::Tree(TreeValue::FsDir(
                path,
            )))),
            TreeValue::Store(root) => Ok(InspectedValue::resolved(Value::Tree(TreeValue::Store(
                root,
            )))),
        }
    }

    pub(super) async fn inspect_builtin(
        &self,
        builtin: BuiltinValue,
        visiting: &mut Vec<NodeId>,
    ) -> Result<(BuiltinValue, BTreeSet<NodeId>, Vec<PoisonError>), PoisonError> {
        macro_rules! inspect_receiver {
            ($receiver:expr, $visiting:expr) => {{
                let inspected = self.inspect_value_inner(*$receiver, $visiting).await?;
                (inspected.value, inspected.pending, inspected.errors)
            }};
        }

        match builtin {
            BuiltinValue::Namespace(namespace) => Ok((
                BuiltinValue::Namespace(namespace),
                BTreeSet::new(),
                Vec::new(),
            )),
            BuiltinValue::Function(function) => Ok((
                BuiltinValue::Function(function),
                BTreeSet::new(),
                Vec::new(),
            )),
            BuiltinValue::Method(method) => match method {
                BuiltinMethod::StringStartsWith { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::StringStartsWith {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::StringContains { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::StringContains {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::StringSplit { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::StringSplit {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::StringLines { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::StringLines {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::StringTrim { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::StringTrim {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::StringToLower { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::StringToLower {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::BlobFromString => Ok((
                    BuiltinValue::Method(BuiltinMethod::BlobFromString),
                    BTreeSet::new(),
                    Vec::new(),
                )),
                BuiltinMethod::BytesFromString => Ok((
                    BuiltinValue::Method(BuiltinMethod::BytesFromString),
                    BTreeSet::new(),
                    Vec::new(),
                )),
                BuiltinMethod::TreeOf => Ok((
                    BuiltinValue::Method(BuiltinMethod::TreeOf),
                    BTreeSet::new(),
                    Vec::new(),
                )),
                BuiltinMethod::TreeEmpty => Ok((
                    BuiltinValue::Method(BuiltinMethod::TreeEmpty),
                    BTreeSet::new(),
                    Vec::new(),
                )),
                BuiltinMethod::TreeFromPath => Ok((
                    BuiltinValue::Method(BuiltinMethod::TreeFromPath),
                    BTreeSet::new(),
                    Vec::new(),
                )),
                BuiltinMethod::ArrayMap { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::ArrayMap {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::ArrayToMap { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::ArrayToMap {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::ArrayFlatten { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::ArrayFlatten {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::ArrayMerge { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::ArrayMerge {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::MapMapValues { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::MapMapValues {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::MapHas { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::MapHas {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::MapGet { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::MapGet {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::BlobToString { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::BlobToString {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::BlobExecutable { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::BlobExecutable {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::BlobUtf8 { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::BlobUtf8 {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::BlobUtf8Lossy { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::BlobUtf8Lossy {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::BytesUtf8 { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::BytesUtf8 {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::BytesUtf8Lossy { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::BytesUtf8Lossy {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::BytesBlob { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::BytesBlob {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::StreamCollect { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::StreamCollect {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::TreeFile { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::TreeFile {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::TreeMerge { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::TreeMerge {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::TreeSubtree { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::TreeSubtree {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::OptionUnwrapOr { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::OptionUnwrapOr {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::FallibleCall { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::FallibleCall {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::HostRead { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::HostRead {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::HostHash { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::HostHash {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::HostWhich { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::HostWhich {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::HostOs { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::HostOs {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
                BuiltinMethod::HostArch { receiver } => {
                    let (value, pending, errors) = inspect_receiver!(receiver, visiting);
                    Ok((
                        BuiltinValue::Method(BuiltinMethod::HostArch {
                            receiver: Box::new(value),
                        }),
                        pending,
                        errors,
                    ))
                }
            },
        }
    }

    pub(super) async fn materialize_value_deep(
        &self,
        value: Value,
        context: &str,
    ) -> Result<Value, PoisonError> {
        let inspected = self.inspect_value(value).await?;
        if !inspected.errors.is_empty() {
            return Err(self.aggregate_poison_errors_for_stack(context, &[], inspected.errors));
        }
        if !inspected.pending.is_empty() {
            return Err(self.internal_error(&format!(
                "{context} still had unresolved pending values: {}",
                inspected
                    .pending
                    .iter()
                    .map(|node_id| node_id.0.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        Ok(inspected.value)
    }

    pub(super) async fn materialize_many_deep(
        &self,
        values: Vec<Value>,
        context: &str,
    ) -> Result<Vec<Value>, PoisonError> {
        let mut materialized = Vec::with_capacity(values.len());
        let mut pending = BTreeSet::new();
        let mut errors = Vec::new();
        for value in values {
            let inspected = self.inspect_value(value).await?;
            materialized.push(inspected.value);
            pending.extend(inspected.pending);
            errors.extend(inspected.errors);
        }
        if !errors.is_empty() {
            return Err(self.aggregate_poison_errors_for_stack(context, &[], errors));
        }
        if !pending.is_empty() {
            return Err(self.internal_error(&format!(
                "{context} still had unresolved pending values: {}",
                pending
                    .iter()
                    .map(|node_id| node_id.0.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        Ok(materialized)
    }

    pub(super) fn materialize_value_shallow<'a>(
        &'a self,
        value: Value,
        context: &'a str,
    ) -> BoxFuture<'a, Result<Value, PoisonError>> {
        Box::pin(async move {
            match value {
                Value::Pending(node_id) => match self.node_state(node_id)? {
                    SchedulerNodeState::Pending | SchedulerNodeState::Evaluating => Err(self
                        .internal_error(&format!(
                            "{context} still had unresolved pending node {}",
                            node_id.0
                        ))),
                    SchedulerNodeState::Resolved(value) => {
                        self.materialize_value_shallow(value, context).await
                    }
                    SchedulerNodeState::Poisoned(error) => Err(error),
                },
                Value::Discovered(discovered) => Ok(self.attach_obligations(
                    self.materialize_value_shallow(*discovered.value, context)
                        .await?,
                    discovered.obligations.iter().cloned(),
                )),
                other => Ok(other),
            }
        })
    }

    pub(super) async fn materialize_template_parts(
        &self,
        parts: &[TemplatePartValue],
    ) -> Result<Vec<TemplatePartValue>, PoisonError> {
        let values = parts
            .iter()
            .filter_map(|part| match part {
                TemplatePartValue::Text(_) => None,
                TemplatePartValue::Expr(value) => Some(value.clone()),
            })
            .collect::<Vec<_>>();
        let forced = self
            .materialize_many_deep(values, "forcing dependencies")
            .await?;
        let mut forced_iter = forced.into_iter();
        Ok(parts
            .iter()
            .map(|part| match part {
                TemplatePartValue::Text(text) => TemplatePartValue::Text(text.clone()),
                TemplatePartValue::Expr(_) => TemplatePartValue::Expr(
                    forced_iter.next().expect("materialized value for template"),
                ),
            })
            .collect())
    }

    pub(super) async fn execute_node(&self, node: &Arc<Node>) -> Result<Value, PoisonError> {
        match &node.job {
            NodeJob::Binary { op, left, right } => {
                let (left, right) = if matches!(op, BinOp::Concat) {
                    (
                        self.demand_value_shallow(left.clone()).await?,
                        self.demand_value_shallow(right.clone()).await?,
                    )
                } else {
                    let mut values = self
                        .materialize_many_deep(
                            vec![left.clone(), right.clone()],
                            "forcing dependencies",
                        )
                        .await?;
                    let right = values.pop().expect("pair has two values");
                    let left = values.pop().expect("pair has two values");
                    (left, right)
                };
                self.compute_binary(*op, &left, &right).await
            }
            NodeJob::Unary { op, operand } => {
                let operand = self
                    .materialize_value_deep(operand.clone(), "forcing dependency")
                    .await?;
                self.compute_unary(*op, &operand)
            }
            NodeJob::Template { kind, parts } => {
                let values = self.materialize_template_parts(parts).await?;
                match kind {
                    TemplateKind::String => self.interpolate_string_parts(&values).await,
                    TemplateKind::Path => self.interpolate_path_parts(&values).await,
                }
            }
            NodeJob::If {
                cond,
                then_branch,
                else_branch,
                bindings,
                scope_module_id,
                scope_identity,
            } => {
                let cond = self
                    .materialize_value_deep(cond.clone(), "forcing dependency")
                    .await?;
                let (cond, obligations) = self.split_obligations(cond);
                match cond {
                    Value::Bool(true) => self
                        .finish_obligated_build_result(
                            self.eval_block(
                                then_branch,
                                scope_module_id,
                                bindings.as_ref(),
                                scope_identity.as_ref(),
                            )
                            .await,
                            obligations,
                        )
                        .into_value(),
                    Value::Bool(false) => self
                        .finish_obligated_build_result(
                            self.eval_block(
                                else_branch,
                                scope_module_id,
                                bindings.as_ref(),
                                scope_identity.as_ref(),
                            )
                            .await,
                            obligations,
                        )
                        .into_value(),
                    other => Err(self.type_error(&format!(
                        "if condition must be Bool, got {}",
                        other.type_name()
                    ))),
                }
            }
            NodeJob::Fallible { value } => {
                match self
                    .materialize_value_shallow(value.clone(), "forcing fallible operand")
                    .await
                {
                    Ok(value) => Ok(self.build_fallible_value(value).await),
                    Err(error) => Ok(self.fallible_none_from_error(&error)),
                }
            }
            NodeJob::Demand { demands, result } => {
                let _ = self.force_many(demands.iter().cloned().collect()).await?;
                result.clone()
            }
            NodeJob::Call {
                function,
                args,
                kwargs,
                kwargs_expr,
            } => {
                self.execute_call(
                    function.clone(),
                    args.clone(),
                    kwargs.clone(),
                    kwargs_expr.clone(),
                )
                .await
            }
            NodeJob::Invoke { function, args } => {
                self.invoke_function(function, args.clone(), Arc::from([]))
                    .await
            }
            NodeJob::Field {
                receiver,
                field,
                scope_module_id,
            } => {
                let receiver = self.demand_value_shallow(receiver.clone()).await?;
                self.select_field_in_module(&receiver, field, scope_module_id)
                    .await
            }
            NodeJob::Fail { message } => {
                let message = self
                    .materialize_value_deep(message.clone(), "forcing dependency")
                    .await?;
                Err(PoisonError::new(
                    PoisonKind::UserFail,
                    message.to_string(),
                    self.current_demand_chain(None),
                ))
            }
            #[cfg(test)]
            NodeJob::Sleep { millis, value } => {
                tokio::time::sleep(Duration::from_millis(*millis)).await;
                Ok(value.clone())
            }
            #[cfg(test)]
            NodeJob::Poison(error) => Err(error.clone()),
        }
    }

    pub(super) fn execute_call<'a>(
        &'a self,
        function: Value,
        args: Arc<[Value]>,
        kwargs: Arc<[(Arc<str>, Value)]>,
        kwargs_expr: Option<Value>,
    ) -> BoxFuture<'a, Result<Value, PoisonError>> {
        Box::pin(async move {
            let (function, function_obligations) = self.split_obligations(
                self.materialize_value_shallow(function, "forcing callable surface")
                    .await?,
            );
            match function {
                Value::Function(function) => {
                    self.finish_execute_call(
                        function,
                        args,
                        kwargs,
                        kwargs_expr,
                        function_obligations,
                    )
                    .await
                }
                Value::Partial(partial) => {
                    let mut merged_args = partial.args.iter().cloned().collect::<Vec<_>>();
                    merged_args.extend(args.iter().cloned());
                    let mut merged_kwargs = partial.kwargs.iter().cloned().collect::<Vec<_>>();
                    merged_kwargs.extend(kwargs.iter().cloned());
                    let merged_kwargs_expr = match (partial.kwargs_expr.as_ref(), kwargs_expr) {
                        (Some(left), Some(right)) => {
                            Some(self.merge_keyword_records((**left).clone(), right).await?)
                        }
                        (Some(value), None) => Some((**value).clone()),
                        (None, value) => value,
                    };
                    self.finish_obligated_result(
                        self.execute_call(
                            partial.function.clone(),
                            Arc::from(merged_args),
                            Arc::from(merged_kwargs),
                            merged_kwargs_expr,
                        )
                        .await,
                        function_obligations,
                    )
                }
                Value::Builtin(builtin) => self.finish_obligated_result(
                    self.invoke_builtin(&builtin, &args, &kwargs, kwargs_expr)
                        .await,
                    function_obligations,
                ),
                other => Err(self.type_error(&format!("cannot call {}", other.type_name()))),
            }
        })
    }

    pub(super) async fn finish_execute_call(
        &self,
        function: FunctionValue,
        args: Arc<[Value]>,
        kwargs: Arc<[(Arc<str>, Value)]>,
        kwargs_expr: Option<Value>,
        function_obligations: Vec<HostObservation>,
    ) -> Result<Value, PoisonError> {
        let kwargs = self
            .materialize_kwargs(
                &kwargs,
                kwargs_expr,
                &format!("function '{}'", function.def.name),
            )
            .await?;

        if kwargs.is_empty()
            && function.key().is_some()
            && args.iter().all(|value| value.key().is_some())
        {
            let node_id = self.memoized_invoke_node(function, args.iter().cloned().collect());
            return Ok(self.attach_obligations(Value::Pending(node_id), function_obligations));
        }

        self.finish_obligated_result(
            self.invoke_function(&function, args, Arc::from(kwargs))
                .await,
            function_obligations,
        )
    }

    // r[impl eval.enter-functions]
    pub(super) async fn invoke_function(
        &self,
        function: &FunctionValue,
        args: Arc<[Value]>,
        kwargs: Arc<[(Arc<str>, Value)]>,
    ) -> Result<Value, PoisonError> {
        let module = self.module_state(&function.module_id)?;
        let mut bindings = HashMap::new();

        for (name, value) in function.captures.iter() {
            bindings.insert(name.to_string(), value.clone());
        }

        if args.len() != function.def.params.len() {
            return Err(self.type_error_at(
                &format!(
                    "function '{}' expects {} positional argument(s), got {}",
                    function.def.name,
                    function.def.params.len(),
                    args.len()
                ),
                function.def.provenance.clone(),
            ));
        }

        for (param, arg) in function.def.params.iter().zip(args.iter()) {
            bindings.insert(param.name.clone(), arg.clone());
        }

        for (name, overloads) in &module.functions {
            if !bindings.contains_key(name) {
                bindings.insert(
                    name.clone(),
                    Value::Function(
                        self.global_function_value(&function.module_id, overloads[0].clone()),
                    ),
                );
            }
        }

        let mut kwarg_values = kwargs
            .iter()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect::<HashMap<_, _>>();

        for param in &function.def.keyword_params {
            if let Some(value) = kwarg_values.remove(&param.name) {
                bindings.insert(param.name.clone(), value);
                continue;
            }

            let Some(default) = &param.default else {
                return Err(self.type_error(&format!(
                    "function '{}' requires keyword argument '{}'",
                    function.def.name, param.name
                )));
            };

            let value = self
                .eval_expr(
                    default,
                    &function.module_id,
                    &bindings,
                    function.code_identity.as_ref(),
                )
                .await
                .into_value()?;
            bindings.insert(param.name.clone(), value);
        }

        if let Some(name) = kwarg_values.into_keys().next() {
            return Err(self.type_error(&format!(
                "function '{}' does not accept keyword argument '{}'",
                function.def.name, name
            )));
        }

        self.eval_block(
            &function.def.body,
            &function.module_id,
            &bindings,
            function.code_identity.as_ref(),
        )
        .await
        .into_value()
    }

    pub(super) async fn resolve_module_function(
        &self,
        module_id: &ModuleId,
        name: &str,
    ) -> Result<FunctionValue, PoisonError> {
        let mut bindings = (*self.eval_module_scope(module_id).await?).clone();
        match bindings.remove(name) {
            Some(Value::Function(function)) => Ok(function),
            Some(other) => Err(self.type_error(&format!(
                "top-level name '{name}' is {}, not a function",
                other.type_name()
            ))),
            None => Err(self.unresolved_function(name)),
        }
    }

    pub(super) async fn eval_module_scope(
        &self,
        module_id: &ModuleId,
    ) -> Result<Arc<HashMap<String, Value>>, PoisonError> {
        let module = self.module_state(module_id)?;
        {
            let state = module
                .scope_state
                .lock()
                .expect("module scope state mutex poisoned");
            match &*state {
                ModuleScopeState::Ready(bindings) => return Ok(Arc::clone(bindings)),
                ModuleScopeState::Poisoned(error) => return Err(error.clone()),
                ModuleScopeState::Evaluating => {
                    return Err(
                        self.type_error(&format!("cyclic module import involving {}", module_id))
                    );
                }
                ModuleScopeState::Uninitialized => {}
            }
        }
        {
            let mut state = module
                .scope_state
                .lock()
                .expect("module scope state mutex poisoned");
            *state = ModuleScopeState::Evaluating;
        }

        let mut bindings = HashMap::new();
        let mut demanded_values = Vec::new();

        for (name, overloads) in &module.functions {
            bindings.insert(
                name.clone(),
                Value::Function(self.global_function_value(module_id, overloads[0].clone())),
            );
        }

        let outcome = async {
            for stmt in &module.module.top_level.stmts {
                match stmt {
                    instr::Stmt::Let { name, value } | instr::Stmt::Assign { name, value } => {
                        let value = self
                            .eval_expr(value, module_id, &bindings, module_id.as_ref())
                            .await
                            .into_value()?;
                        bindings.insert(name.clone(), value);
                    }
                    instr::Stmt::LetPattern { pattern, value } => {
                        let value = self
                            .eval_expr(value, module_id, &bindings, module_id.as_ref())
                            .await
                            .into_value()?;
                        self.bind_pattern_into(pattern, value, &mut bindings)
                            .await?;
                    }
                    instr::Stmt::Return(_) => {
                        return Err(self.internal_error("return is not allowed at file scope"));
                    }
                    instr::Stmt::FnDecl(def) => {
                        bindings.insert(
                            def.name.clone(),
                            Value::Function(self.local_function_value(
                                module_id,
                                def.clone(),
                                capture_referenced_bindings(
                                    &bindings,
                                    &def.params,
                                    &def.keyword_params,
                                    &def.body,
                                ),
                            )),
                        );
                    }
                    instr::Stmt::StructDecl(decl) => {
                        bindings.insert(decl.name.clone(), self.struct_constructor_value(decl));
                    }
                    instr::Stmt::Expr(expr) => {
                        let value = self
                            .eval_expr(expr, module_id, &bindings, module_id.as_ref())
                            .await
                            .into_value()?;
                        demanded_values.push(value);
                    }
                }
            }

            self.resolve_demanded_values(mem::take(&mut demanded_values))
                .await?;
            Ok(Arc::new(bindings))
        }
        .await;

        let mut state = module
            .scope_state
            .lock()
            .expect("module scope state mutex poisoned");
        match &outcome {
            Ok(bindings) => {
                *state = ModuleScopeState::Ready(Arc::clone(bindings));
            }
            Err(error) => {
                *state = ModuleScopeState::Poisoned(error.clone());
            }
        }
        outcome
    }

    pub(super) async fn eval_block(
        &self,
        block: &instr::Block,
        current_module_id: &ModuleId,
        bindings: &HashMap<String, Value>,
        current_code_identity: &str,
    ) -> BuildResult {
        let mut local_bindings = bindings.clone();
        let mut demanded_values = Vec::new();

        for (index, stmt) in block.stmts.iter().enumerate() {
            let is_last = index + 1 == block.stmts.len();
            match stmt {
                instr::Stmt::Let { name, value } | instr::Stmt::Assign { name, value } => {
                    match self
                        .eval_expr(
                            value,
                            current_module_id,
                            &local_bindings,
                            current_code_identity,
                        )
                        .await
                    {
                        BuildResult::Value(value) => {
                            local_bindings.insert(name.clone(), value);
                        }
                        other => {
                            return self
                                .finish_demanded_result(other, &mut demanded_values)
                                .await;
                        }
                    }
                }
                instr::Stmt::LetPattern { pattern, value } => {
                    match self
                        .eval_expr(
                            value,
                            current_module_id,
                            &local_bindings,
                            current_code_identity,
                        )
                        .await
                    {
                        BuildResult::Value(value) => {
                            if let Err(error) = self
                                .bind_pattern_into(pattern, value, &mut local_bindings)
                                .await
                            {
                                return self
                                    .finish_demanded_result(
                                        BuildResult::Poisoned(error),
                                        &mut demanded_values,
                                    )
                                    .await;
                            }
                        }
                        other => {
                            return self
                                .finish_demanded_result(other, &mut demanded_values)
                                .await;
                        }
                    }
                }
                instr::Stmt::Return(expr) => {
                    match self
                        .eval_expr(
                            expr,
                            current_module_id,
                            &local_bindings,
                            current_code_identity,
                        )
                        .await
                    {
                        BuildResult::Value(value) | BuildResult::Returned(value) => {
                            return self
                                .finish_demanded_result(
                                    BuildResult::Returned(value),
                                    &mut demanded_values,
                                )
                                .await;
                        }
                        BuildResult::Poisoned(error) => {
                            return self
                                .finish_demanded_result(
                                    BuildResult::Poisoned(error),
                                    &mut demanded_values,
                                )
                                .await;
                        }
                    }
                }
                instr::Stmt::FnDecl(def) => {
                    local_bindings.insert(
                        def.name.clone(),
                        Value::Function(self.local_function_value(
                            current_module_id,
                            def.clone(),
                            capture_referenced_bindings(
                                &local_bindings,
                                &def.params,
                                &def.keyword_params,
                                &def.body,
                            ),
                        )),
                    );
                }
                instr::Stmt::StructDecl(decl) => {
                    local_bindings.insert(decl.name.clone(), self.struct_constructor_value(decl));
                }
                instr::Stmt::Expr(expr) => {
                    match self
                        .eval_expr(
                            expr,
                            current_module_id,
                            &local_bindings,
                            current_code_identity,
                        )
                        .await
                    {
                        BuildResult::Value(value) => {
                            if is_last {
                                return self
                                    .finish_demanded_result(
                                        BuildResult::Value(value),
                                        &mut demanded_values,
                                    )
                                    .await;
                            }
                            demanded_values.push(value);
                        }
                        other => {
                            return self
                                .finish_demanded_result(other, &mut demanded_values)
                                .await;
                        }
                    }
                }
            }
        }

        self.finish_demanded_result(BuildResult::Value(Value::Unit), &mut demanded_values)
            .await
    }

    pub(super) async fn finish_demanded_result(
        &self,
        result: BuildResult,
        demanded_values: &mut Vec<Value>,
    ) -> BuildResult {
        let demanded_values = mem::take(demanded_values);
        match result {
            BuildResult::Value(value) => {
                BuildResult::Value(self.wrap_demanded_value(demanded_values, Ok(value)).await)
            }
            BuildResult::Returned(value) => {
                BuildResult::Returned(self.wrap_demanded_value(demanded_values, Ok(value)).await)
            }
            BuildResult::Poisoned(error) => {
                if let Err(demand_error) = self.resolve_demanded_values(demanded_values).await {
                    return BuildResult::Poisoned(demand_error);
                }
                BuildResult::Poisoned(error)
            }
        }
    }

    pub(super) async fn wrap_demanded_value(
        &self,
        demanded_values: Vec<Value>,
        result: Result<Value, PoisonError>,
    ) -> Value {
        if demanded_values.is_empty() {
            return result.expect("demand-free result must be a value");
        }

        self.pending_value(
            NodeJob::Demand {
                demands: Arc::from(demanded_values),
                result,
            },
            "demand(..)".into(),
            None,
        )
    }

    pub(super) async fn resolve_demanded_values(
        &self,
        demanded_values: Vec<Value>,
    ) -> Result<(), PoisonError> {
        if demanded_values.is_empty() {
            return Ok(());
        }

        let _ = self
            .resolve_boundary_value(Value::Array(Arc::from(demanded_values)))
            .await?;
        Ok(())
    }

    pub(super) async fn build_fallible_value(&self, value: Value) -> Value {
        let (value, obligations) = self.split_obligations(value);
        match value {
            Value::Function(_) | Value::Builtin(_) | Value::Partial(_) => {
                let value = self.attach_obligations(value, obligations);
                self.pending_value(
                    NodeJob::Fallible {
                        value: self.build_call(value, vec![], None),
                    },
                    "fallible(..)".into(),
                    None,
                )
            }
            Value::Pending(_) => self.pending_value(
                NodeJob::Fallible {
                    value: self.attach_obligations(value, obligations),
                },
                "fallible(..)".into(),
                None,
            ),
            other => self.attach_obligations(option_some(other), obligations),
        }
    }

    // r[impl eval.forcing]
    pub(super) fn eval_expr<'a>(
        &'a self,
        expr: &'a instr::Expr,
        current_module_id: &'a ModuleId,
        bindings: &'a HashMap<String, Value>,
        current_code_identity: &'a str,
    ) -> BoxFuture<'a, BuildResult> {
        Box::pin(async move {
            match expr {
                instr::Expr::LitInt(n) => BuildResult::Value(Value::U64(*n)),
                instr::Expr::LitFloat(n) => BuildResult::Value(Value::F64(*n)),
                instr::Expr::LitBool(b) => BuildResult::Value(Value::Bool(*b)),
                instr::Expr::LitString(s) => {
                    BuildResult::Value(Value::String(Arc::from(s.as_str())))
                }
                instr::Expr::StringTemplate { parts, provenance } => {
                    self.eval_template_expr(
                        TemplateKind::String,
                        parts,
                        provenance.as_ref(),
                        current_module_id,
                        bindings,
                        current_code_identity,
                    )
                    .await
                }
                instr::Expr::PathTemplate { parts, provenance } => {
                    self.eval_template_expr(
                        TemplateKind::Path,
                        parts,
                        provenance.as_ref(),
                        current_module_id,
                        bindings,
                        current_code_identity,
                    )
                    .await
                }
                instr::Expr::Import { path, provenance } => {
                    BuildResult::from_result(self.import_module(path, provenance.as_ref()).await)
                }
                instr::Expr::Unit => BuildResult::Value(Value::Unit),
                instr::Expr::Var(name) => {
                    if let Some(value) = bindings.get(name) {
                        BuildResult::Value(value.clone())
                    } else if name == "None" {
                        BuildResult::Value(option_none())
                    } else if let Some(overloads) = self
                        .module_state(current_module_id)
                        .ok()
                        .and_then(|module| module.functions.get(name).cloned())
                    {
                        BuildResult::Value(Value::Function(
                            self.global_function_value(current_module_id, overloads[0].clone()),
                        ))
                    } else if let Some(builtin) = builtin_namespace(name) {
                        BuildResult::Value(Value::Builtin(builtin))
                    } else {
                        BuildResult::Poisoned(self.unresolved_name(name))
                    }
                }
                instr::Expr::Binary {
                    op,
                    left,
                    right,
                    provenance,
                } => {
                    let left = match self
                        .eval_expr(left, current_module_id, bindings, current_code_identity)
                        .await
                    {
                        BuildResult::Value(value) => value,
                        other => return other,
                    };
                    let right = match self
                        .eval_expr(right, current_module_id, bindings, current_code_identity)
                        .await
                    {
                        BuildResult::Value(value) => value,
                        other => return other,
                    };

                    let concat_ready = if matches!(op, BinOp::Concat) {
                        let mut deps = Vec::new();
                        left.collect_pending_shallow(&mut deps);
                        right.collect_pending_shallow(&mut deps);
                        deps.is_empty()
                    } else {
                        false
                    };

                    if concat_ready || (left.key().is_some() && right.key().is_some()) {
                        BuildResult::from_result(self.compute_binary(*op, &left, &right).await)
                    } else {
                        BuildResult::Value(self.pending_value(
                            NodeJob::Binary {
                                op: *op,
                                left,
                                right,
                            },
                            format!("binary({op:?})"),
                            provenance.clone(),
                        ))
                    }
                }
                instr::Expr::Unary {
                    op,
                    operand,
                    provenance,
                } => {
                    let operand = match self
                        .eval_expr(operand, current_module_id, bindings, current_code_identity)
                        .await
                    {
                        BuildResult::Value(value) => value,
                        other => return other,
                    };

                    if operand.key().is_some() {
                        BuildResult::from_result(self.compute_unary(*op, &operand))
                    } else {
                        BuildResult::Value(self.pending_value(
                            NodeJob::Unary { op: *op, operand },
                            format!("unary({op:?})"),
                            provenance.clone(),
                        ))
                    }
                }
                instr::Expr::Block(block) => {
                    self.eval_block(block, current_module_id, bindings, current_code_identity)
                        .await
                }
                // r[impl eval.control-demand]
                instr::Expr::If {
                    cond,
                    then_branch,
                    else_branch,
                    provenance,
                } => {
                    let cond = match self
                        .eval_expr(cond, current_module_id, bindings, current_code_identity)
                        .await
                    {
                        BuildResult::Value(value) => value,
                        other => return other,
                    };
                    let (cond, obligations) = self.split_obligations(cond);

                    match cond {
                        Value::Bool(true) => self.finish_obligated_build_result(
                            self.eval_block(
                                then_branch,
                                current_module_id,
                                bindings,
                                current_code_identity,
                            )
                            .await,
                            obligations,
                        ),
                        Value::Bool(false) => self.finish_obligated_build_result(
                            self.eval_block(
                                else_branch,
                                current_module_id,
                                bindings,
                                current_code_identity,
                            )
                            .await,
                            obligations,
                        ),
                        Value::Pending(_) => BuildResult::Value(self.pending_value(
                            NodeJob::If {
                                cond,
                                then_branch: then_branch.clone(),
                                else_branch: else_branch.clone(),
                                bindings: Arc::new(bindings.clone()),
                                scope_module_id: Arc::clone(current_module_id),
                                scope_identity: Arc::from(current_code_identity),
                            },
                            "if(..)".into(),
                            provenance.clone(),
                        )),
                        other => BuildResult::Poisoned(self.type_error(&format!(
                            "if condition must be Bool, got {}",
                            other.type_name()
                        ))),
                    }
                }
                // r[impl fn.closure]
                // r[impl fn.identity.closure]
                instr::Expr::Closure { closure_id, def } => {
                    BuildResult::Value(Value::Function(self.closure_function_value(
                        current_module_id,
                        def.clone(),
                        Arc::from(format!("{current_code_identity}::closure#{closure_id}")),
                        capture_referenced_bindings(
                            bindings,
                            &def.params,
                            &def.keyword_params,
                            &def.body,
                        ),
                    )))
                }
                instr::Expr::Call {
                    func,
                    args,
                    kwargs,
                    kwargs_expr,
                    provenance,
                } => {
                    let function = match self
                        .eval_expr(func, current_module_id, bindings, current_code_identity)
                        .await
                    {
                        BuildResult::Value(value) => value,
                        other => return other,
                    };
                    let mut arg_values = Vec::with_capacity(args.len());
                    for arg in args {
                        match self
                            .eval_expr(arg, current_module_id, bindings, current_code_identity)
                            .await
                        {
                            BuildResult::Value(value) => arg_values.push(value),
                            other => return other,
                        }
                    }
                    let mut kwarg_values = Vec::with_capacity(kwargs.len());
                    for (name, arg) in kwargs {
                        match self
                            .eval_expr(arg, current_module_id, bindings, current_code_identity)
                            .await
                        {
                            BuildResult::Value(value) => {
                                kwarg_values.push((Arc::from(name.as_str()), value));
                            }
                            other => return other,
                        }
                    }
                    let kwargs_expr_value = match kwargs_expr {
                        Some(expr) => match self
                            .eval_expr(expr, current_module_id, bindings, current_code_identity)
                            .await
                        {
                            BuildResult::Value(value) => Some(value),
                            other => return other,
                        },
                        None => None,
                    };
                    BuildResult::Value(self.build_call_with_kwargs(
                        function,
                        arg_values,
                        kwarg_values,
                        kwargs_expr_value,
                        provenance.clone(),
                    ))
                }
                instr::Expr::PartialCall {
                    func,
                    args,
                    kwargs,
                    kwargs_expr,
                    provenance: _,
                } => {
                    let function = match self
                        .eval_expr(func, current_module_id, bindings, current_code_identity)
                        .await
                    {
                        BuildResult::Value(value) => value,
                        other => return other,
                    };
                    let mut arg_values = Vec::with_capacity(args.len());
                    for arg in args {
                        match self
                            .eval_expr(arg, current_module_id, bindings, current_code_identity)
                            .await
                        {
                            BuildResult::Value(value) => arg_values.push(value),
                            other => return other,
                        }
                    }
                    let mut kwarg_values = Vec::with_capacity(kwargs.len());
                    for (name, arg) in kwargs {
                        match self
                            .eval_expr(arg, current_module_id, bindings, current_code_identity)
                            .await
                        {
                            BuildResult::Value(value) => {
                                kwarg_values.push((Arc::from(name.as_str()), value));
                            }
                            other => return other,
                        }
                    }
                    let kwargs_expr_value = match kwargs_expr {
                        Some(expr) => match self
                            .eval_expr(expr, current_module_id, bindings, current_code_identity)
                            .await
                        {
                            BuildResult::Value(value) => Some(value),
                            other => return other,
                        },
                        None => None,
                    };
                    BuildResult::Value(Value::Partial(Arc::new(PartialValue {
                        function,
                        args: Arc::from(arg_values),
                        kwargs: Arc::from(kwarg_values),
                        kwargs_expr: kwargs_expr_value.map(Box::new),
                    })))
                }
                instr::Expr::StructLit { name, fields } => {
                    let mut values = Vec::with_capacity(fields.len());
                    for (field_name, field_expr) in fields {
                        match self
                            .eval_expr(
                                field_expr,
                                current_module_id,
                                bindings,
                                current_code_identity,
                            )
                            .await
                        {
                            BuildResult::Value(value) => {
                                values.push((Arc::from(field_name.as_str()), value));
                            }
                            other => return other,
                        }
                    }
                    BuildResult::Value(Value::Struct(StructValue {
                        type_name: Arc::from(name.as_str()),
                        fields: Arc::from(values),
                    }))
                }
                instr::Expr::Record(items) => {
                    let mut entries = Vec::new();
                    for item in items {
                        match item {
                            instr::RecordItem::Field { name, value } => {
                                let value = match self
                                    .eval_expr(
                                        value,
                                        current_module_id,
                                        bindings,
                                        current_code_identity,
                                    )
                                    .await
                                {
                                    BuildResult::Value(value) => value,
                                    other => return other,
                                };
                                entries.push((Value::String(Arc::from(name.as_str())), value));
                            }
                            instr::RecordItem::Spread(spread_expr) => {
                                let spread_value = match self
                                    .eval_expr(
                                        spread_expr,
                                        current_module_id,
                                        bindings,
                                        current_code_identity,
                                    )
                                    .await
                                {
                                    BuildResult::Value(value) => value,
                                    other => return other,
                                };
                                let spread_value =
                                    match self.demand_value_shallow(spread_value).await {
                                        Ok(value) => value,
                                        Err(error) => return BuildResult::Poisoned(error),
                                    };
                                match self
                                    .extend_record_with_spread(&mut entries, spread_value)
                                    .await
                                {
                                    Ok(()) => {}
                                    Err(error) => return BuildResult::Poisoned(error),
                                }
                            }
                        }
                    }
                    BuildResult::Value(Value::Map(MapValue {
                        entries: Arc::from(entries),
                    }))
                }
                instr::Expr::Array(items) => {
                    let mut values = Vec::with_capacity(items.len());
                    for item in items {
                        match item {
                            instr::ArrayItem::Expr(item_expr) => {
                                match self
                                    .eval_expr(
                                        item_expr,
                                        current_module_id,
                                        bindings,
                                        current_code_identity,
                                    )
                                    .await
                                {
                                    BuildResult::Value(value) => values.push(value),
                                    other => return other,
                                }
                            }
                            instr::ArrayItem::Spread(spread_expr) => {
                                let spread_value = match self
                                    .eval_expr(
                                        spread_expr,
                                        current_module_id,
                                        bindings,
                                        current_code_identity,
                                    )
                                    .await
                                {
                                    BuildResult::Value(value) => value,
                                    other => return other,
                                };
                                let spread_value =
                                    match self.demand_value_shallow(spread_value).await {
                                        Ok(value) => value,
                                        Err(error) => return BuildResult::Poisoned(error),
                                    };
                                match self
                                    .extend_array_with_spread(&mut values, spread_value)
                                    .await
                                {
                                    Ok(()) => {}
                                    Err(error) => return BuildResult::Poisoned(error),
                                }
                            }
                        }
                    }
                    BuildResult::Value(Value::Array(Arc::from(values)))
                }
                instr::Expr::Map(entries) => {
                    let mut values = Vec::with_capacity(entries.len());
                    for entry in entries {
                        match entry {
                            instr::MapItem::Entry { key, value } => {
                                let key = match self
                                    .eval_expr(
                                        key,
                                        current_module_id,
                                        bindings,
                                        current_code_identity,
                                    )
                                    .await
                                {
                                    BuildResult::Value(value) => value,
                                    other => return other,
                                };
                                let value = match self
                                    .eval_expr(
                                        value,
                                        current_module_id,
                                        bindings,
                                        current_code_identity,
                                    )
                                    .await
                                {
                                    BuildResult::Value(value) => value,
                                    other => return other,
                                };
                                values.push((key, value));
                            }
                            instr::MapItem::Spread(spread_expr) => {
                                let spread_value = match self
                                    .eval_expr(
                                        spread_expr,
                                        current_module_id,
                                        bindings,
                                        current_code_identity,
                                    )
                                    .await
                                {
                                    BuildResult::Value(value) => value,
                                    other => return other,
                                };
                                let spread_value =
                                    match self.demand_value_shallow(spread_value).await {
                                        Ok(value) => value,
                                        Err(error) => return BuildResult::Poisoned(error),
                                    };
                                match self.extend_map_with_spread(&mut values, spread_value).await {
                                    Ok(()) => {}
                                    Err(error) => return BuildResult::Poisoned(error),
                                }
                            }
                        }
                    }
                    BuildResult::Value(Value::Map(MapValue {
                        entries: Arc::from(values),
                    }))
                }
                instr::Expr::Field {
                    receiver,
                    field,
                    provenance,
                } => {
                    let receiver = match self
                        .eval_expr(receiver, current_module_id, bindings, current_code_identity)
                        .await
                    {
                        BuildResult::Value(value) => value,
                        other => return other,
                    };

                    if matches!(receiver, Value::Pending(_)) {
                        BuildResult::Value(self.pending_value(
                            NodeJob::Field {
                                receiver,
                                field: Arc::from(field.as_str()),
                                scope_module_id: Arc::clone(current_module_id),
                            },
                            format!("field(.{field})"),
                            provenance.clone(),
                        ))
                    } else {
                        BuildResult::from_result(
                            self.select_field_in_module(&receiver, field, current_module_id)
                                .await,
                        )
                    }
                }
                // r[impl error.fail]
                instr::Expr::Fail {
                    message,
                    provenance,
                } => {
                    let message = match self
                        .eval_expr(message, current_module_id, bindings, current_code_identity)
                        .await
                    {
                        BuildResult::Value(value) => value,
                        other => return other,
                    };
                    BuildResult::Value(self.pending_value(
                        NodeJob::Fail { message },
                        "fail(..)".into(),
                        provenance.clone(),
                    ))
                }
                instr::Expr::Fallible(expr) => {
                    match self
                        .eval_expr(expr, current_module_id, bindings, current_code_identity)
                        .await
                    {
                        BuildResult::Value(value) => {
                            BuildResult::Value(self.build_fallible_value(value).await)
                        }
                        BuildResult::Poisoned(error) => {
                            BuildResult::Value(self.fallible_none_from_error(&error))
                        }
                        other => other,
                    }
                }
                instr::Expr::Tuple(elems) => {
                    let mut values = Vec::with_capacity(elems.len());
                    for elem in elems {
                        match self
                            .eval_expr(elem, current_module_id, bindings, current_code_identity)
                            .await
                        {
                            BuildResult::Value(value) => values.push(value),
                            other => return other,
                        }
                    }
                    BuildResult::Value(Value::Tuple(Arc::from(values)))
                }
            }
        })
    }

    pub(super) async fn eval_template_expr(
        &self,
        kind: TemplateKind,
        parts: &[instr::TemplatePart],
        provenance: Option<&instr::SourceProvenance>,
        current_module_id: &ModuleId,
        bindings: &HashMap<String, Value>,
        current_code_identity: &str,
    ) -> BuildResult {
        let mut values = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                instr::TemplatePart::Text(text) => {
                    values.push(TemplatePartValue::Text(Arc::from(text.as_str())));
                }
                instr::TemplatePart::Expr(expr) => {
                    let value = match self
                        .eval_expr(expr, current_module_id, bindings, current_code_identity)
                        .await
                    {
                        BuildResult::Value(value) => value,
                        other => return other,
                    };
                    values.push(TemplatePartValue::Expr(value));
                }
            }
        }

        let needs_node = values.iter().any(|part| match part {
            TemplatePartValue::Text(_) => false,
            TemplatePartValue::Expr(value) => value.key().is_none(),
        });
        if needs_node {
            return BuildResult::Value(self.pending_value(
                NodeJob::Template {
                    kind,
                    parts: Arc::from(values),
                },
                format!("template({kind:?})"),
                provenance.cloned(),
            ));
        }

        let result = match kind {
            TemplateKind::String => self.interpolate_string_parts(&values).await,
            TemplateKind::Path => self.interpolate_path_parts(&values).await,
        };
        BuildResult::from_result(result)
    }

    pub(super) async fn interpolate_string_parts(
        &self,
        parts: &[TemplatePartValue],
    ) -> Result<Value, PoisonError> {
        let mut value = String::new();
        let mut obligations = BTreeMap::new();
        for part in parts {
            match part {
                TemplatePartValue::Text(text) => value.push_str(text),
                TemplatePartValue::Expr(value_part) => {
                    let (value_part, value_obligations) = self.split_obligations_ref(value_part);
                    Self::add_obligations(&mut obligations, value_obligations.iter().cloned());
                    value.push_str(&self.stringify_interpolated_value(value_part).await?);
                }
            }
        }
        self.finish_obligated_result(
            Ok(Value::String(Arc::from(value))),
            obligations.into_values(),
        )
    }

    // r[exec.arg.interpolation]
    pub(super) async fn interpolate_path_parts(
        &self,
        parts: &[TemplatePartValue],
    ) -> Result<Value, PoisonError> {
        let mut plain = String::new();
        let mut mounted_tree: Option<(TreeValue, String)> = None;
        let mut mounted_blob: Option<(BlobValue, String)> = None;
        let mut obligations = BTreeMap::new();

        for part in parts {
            match part {
                TemplatePartValue::Text(text) => plain.push_str(text),
                TemplatePartValue::Expr(value) => {
                    let (value, value_obligations) = self.split_obligations_ref(value);
                    Self::add_obligations(&mut obligations, value_obligations.iter().cloned());
                    match value {
                        Value::String(text) => plain.push_str(text),
                        Value::Path(path) => plain.push_str(path.as_str()),
                        Value::Tree(tree) => {
                            if mounted_tree.is_some() || mounted_blob.is_some() {
                                return Err(self.type_error(
                                "path literal cannot interpolate more than one mount-bearing value",
                            ));
                            }
                            mounted_tree = Some((tree.clone(), plain.clone()));
                            plain.clear();
                        }
                        Value::Blob(blob) => {
                            if mounted_tree.is_some() || mounted_blob.is_some() {
                                return Err(self.type_error(
                                "path literal cannot interpolate more than one mount-bearing value",
                            ));
                            }
                            mounted_blob = Some((blob.clone(), plain.clone()));
                            plain.clear();
                        }
                        Value::ExecPath(exec_path) => match exec_path {
                            ExecPathValue::InTree { tree, suffix } => {
                                if mounted_tree.is_some() || mounted_blob.is_some() {
                                    return Err(self.type_error(
                                    "path literal cannot interpolate more than one mount-bearing value",
                                ));
                                }
                                plain.push_str(suffix.as_str());
                                mounted_tree = Some((tree.clone(), plain.clone()));
                                plain.clear();
                            }
                            ExecPathValue::InBlob { blob, leaf } => {
                                if mounted_tree.is_some() || mounted_blob.is_some() {
                                    return Err(self.type_error(
                                    "path literal cannot interpolate more than one mount-bearing value",
                                ));
                                }
                                plain.push_str(leaf.as_str());
                                mounted_blob = Some((blob.clone(), plain.clone()));
                                plain.clear();
                            }
                        },
                        other => {
                            return Err(self.type_error(&format!(
                            "path interpolation requires String, Path, Tree, Blob, or ExecPath, got {}",
                            other.type_name()
                        )));
                        }
                    }
                }
            }
        }

        if let Some((tree, prefix)) = mounted_tree {
            let suffix = PathValue::new(format!("{prefix}{plain}").trim_start_matches('/'));
            return self.finish_obligated_result(
                Ok(Value::ExecPath(ExecPathValue::InTree { tree, suffix })),
                obligations.into_values(),
            );
        }

        if let Some((blob, leaf)) = mounted_blob {
            let leaf = format!("{leaf}{plain}");
            let leaf = leaf.trim_start_matches('/').to_string();
            let normalized = PathValue::new(&leaf);
            if normalized.as_str().contains('/') {
                return Err(self
                    .type_error("path literal cannot append path segments to a blob-backed path"));
            }
            return self.finish_obligated_result(
                Ok(Value::ExecPath(ExecPathValue::InBlob {
                    blob,
                    leaf: normalized,
                })),
                obligations.into_values(),
            );
        }

        self.finish_obligated_result(
            Ok(Value::Path(PathValue::new(plain))),
            obligations.into_values(),
        )
    }

    pub(super) async fn stringify_interpolated_value(
        &self,
        value: &Value,
    ) -> Result<String, PoisonError> {
        match value {
            Value::String(text) => Ok(text.to_string()),
            Value::Path(path) => Ok(path.as_str().to_string()),
            Value::Bool(_) | Value::U64(_) | Value::I64(_) | Value::F64(_) | Value::Unit => {
                Ok(value.to_string())
            }
            other => Err(self.type_error(&format!(
                "cannot interpolate {} into String",
                other.type_name()
            ))),
        }
    }

    pub(super) async fn select_field(
        &self,
        receiver: &Value,
        field: &str,
    ) -> Result<Value, PoisonError> {
        self.select_field_in_module(receiver, field, &self.root_module_id)
            .await
    }

    pub(super) async fn select_field_in_module(
        &self,
        receiver: &Value,
        field: &str,
        current_module_id: &ModuleId,
    ) -> Result<Value, PoisonError> {
        let (receiver, obligations) = self.split_obligations_ref(receiver);
        let result = match receiver {
            Value::Struct(struct_value)
                if field == "unwrap_or"
                    && matches!(struct_value.type_name.as_ref(), "Some" | "None") =>
            {
                Ok(Value::Builtin(BuiltinValue::Method(
                    BuiltinMethod::OptionUnwrapOr {
                        receiver: Box::new(receiver.clone()),
                    },
                )))
            }
            Value::Struct(struct_value) => {
                let found = struct_value
                    .fields
                    .iter()
                    .find(|(name, _)| name.as_ref() == field)
                    .map(|(_, value)| value.clone());
                let found = match found {
                    Some(v) => Some(v),
                    None => {
                        self.user_method_value_in_module(
                            receiver,
                            struct_value.type_name.as_ref(),
                            field,
                            current_module_id,
                        )
                        .await
                    }
                };
                found.ok_or_else(|| {
                    self.type_error(&format!(
                        "no field '{}' on struct {}",
                        field, struct_value.type_name
                    ))
                })
            }
            Value::Tuple(elems) => {
                if let Some(index) = field.strip_prefix('_')
                    && let Ok(index) = index.parse::<usize>()
                    && let Some(value) = elems.get(index)
                {
                    Ok(value.clone())
                } else {
                    Err(self.type_error(&format!("invalid tuple field '{}'", field)))
                }
            }
            Value::String(_) if field == "starts_with" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::StringStartsWith {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::String(_) if field == "contains" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::StringContains {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::String(_) if field == "split" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::StringSplit {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::String(_) if field == "lines" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::StringLines {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::String(_) if field == "trim" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::StringTrim {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::String(_) if field == "to_lower" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::StringToLower {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Array(_) if field == "map" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::ArrayMap {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Array(_) if field == "to_map" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::ArrayToMap {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Array(_) if field == "flatten" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::ArrayFlatten {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Array(_) if field == "merge" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::ArrayMerge {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Map(map) => {
                if let Some((_, value)) = map.entries.iter().find(|(key, _)| match key {
                    Value::String(name) => name.as_ref() == field,
                    _ => false,
                }) {
                    return Ok(self.attach_obligations(value.clone(), obligations.iter().cloned()));
                }

                if field == "map_values" {
                    Ok(Value::Builtin(BuiltinValue::Method(
                        BuiltinMethod::MapMapValues {
                            receiver: Box::new(receiver.clone()),
                        },
                    )))
                } else if field == "has" {
                    Ok(Value::Builtin(BuiltinValue::Method(
                        BuiltinMethod::MapHas {
                            receiver: Box::new(receiver.clone()),
                        },
                    )))
                } else if field == "get" {
                    Ok(Value::Builtin(BuiltinValue::Method(
                        BuiltinMethod::MapGet {
                            receiver: Box::new(receiver.clone()),
                        },
                    )))
                } else {
                    Err(self.type_error(&format!("no field '{}' on Map", field)))
                }
            }
            Value::Blob(_) if field == "to_string" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::BlobToString {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Blob(_) if field == "executable" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::BlobExecutable {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Blob(_) if field == "utf8" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::BlobUtf8 {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Blob(_) if field == "utf8_lossy" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::BlobUtf8Lossy {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Bytes(_) if field == "collect" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::StreamCollect {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Bytes(_) if field == "utf8" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::BytesUtf8 {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Bytes(_) if field == "utf8_lossy" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::BytesUtf8Lossy {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Bytes(_) if field == "blob" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::BytesBlob {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Tree(_) if field == "file" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::TreeFile {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Tree(_) if field == "merge" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::TreeMerge {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Tree(_) if field == "subtree" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::TreeSubtree {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Host(_) if field == "read" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::HostRead {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Host(_) if field == "os" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::HostOs {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Host(_) if field == "arch" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::HostArch {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Host(_) if field == "hash" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::HostHash {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Host(_) if field == "which" => Ok(Value::Builtin(BuiltinValue::Method(
                BuiltinMethod::HostWhich {
                    receiver: Box::new(receiver.clone()),
                },
            ))),
            Value::Builtin(BuiltinValue::Namespace(BuiltinNamespace::Blob))
                if field == "from_string" =>
            {
                Ok(Value::Builtin(BuiltinValue::Method(
                    BuiltinMethod::BlobFromString,
                )))
            }
            Value::Builtin(BuiltinValue::Namespace(BuiltinNamespace::Bytes)) => Ok(Value::Builtin(
                BuiltinValue::Method(BuiltinMethod::BytesFromString),
            )),
            Value::Builtin(BuiltinValue::Namespace(BuiltinNamespace::Tree)) if field == "of" => {
                Ok(Value::Builtin(BuiltinValue::Method(BuiltinMethod::TreeOf)))
            }
            Value::Builtin(BuiltinValue::Namespace(BuiltinNamespace::Tree)) if field == "empty" => {
                Ok(Value::Builtin(BuiltinValue::Method(
                    BuiltinMethod::TreeEmpty,
                )))
            }
            Value::Builtin(BuiltinValue::Namespace(BuiltinNamespace::Tree))
                if field == "from_path" =>
            {
                Ok(Value::Builtin(BuiltinValue::Method(
                    BuiltinMethod::TreeFromPath,
                )))
            }
            other => Err(self.type_error(&format!(
                "cannot access field '{}' on {}",
                field,
                other.type_name()
            ))),
        };
        self.finish_obligated_result(result, obligations.iter().cloned())
    }

    pub(super) async fn compute_binary(
        &self,
        op: BinOp,
        left: &Value,
        right: &Value,
    ) -> Result<Value, PoisonError> {
        let (left, left_obligations) = self.split_obligations_ref(left);
        let (right, right_obligations) = self.split_obligations_ref(right);
        let result = match (op, left, right) {
            (BinOp::Add, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a.wrapping_add(*b))),
            (BinOp::Concat, Value::Array(a), Value::Array(b)) => {
                let mut values = Vec::with_capacity(a.len() + b.len());
                values.extend(a.iter().cloned());
                values.extend(b.iter().cloned());
                Ok(Value::Array(Arc::from(values)))
            }
            (BinOp::Sub, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a.wrapping_sub(*b))),
            (BinOp::Mul, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a.wrapping_mul(*b))),
            (BinOp::Div, Value::U64(_), Value::U64(0)) => {
                Err(self.arithmetic_error("division by zero"))
            }
            (BinOp::Div, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a / b)),
            (BinOp::Mod, Value::U64(_), Value::U64(0)) => {
                Err(self.arithmetic_error("modulo by zero"))
            }
            (BinOp::Mod, Value::U64(a), Value::U64(b)) => Ok(Value::U64(a % b)),

            (BinOp::Add, Value::I64(a), Value::I64(b)) => Ok(Value::I64(a.wrapping_add(*b))),
            (BinOp::Sub, Value::I64(a), Value::I64(b)) => Ok(Value::I64(a.wrapping_sub(*b))),
            (BinOp::Mul, Value::I64(a), Value::I64(b)) => Ok(Value::I64(a.wrapping_mul(*b))),
            (BinOp::Div, Value::I64(_), Value::I64(0)) => {
                Err(self.arithmetic_error("division by zero"))
            }
            (BinOp::Div, Value::I64(a), Value::I64(b)) => Ok(Value::I64(a / b)),

            (BinOp::Div, Value::Path(base), Value::String(child)) => {
                Ok(Value::Path(base.join(child.as_ref())))
            }
            (BinOp::Div, Value::Path(base), Value::Path(child)) => {
                Ok(Value::Path(base.join(child.as_str())))
            }

            (BinOp::Add, Value::F64(a), Value::F64(b)) => Ok(Value::F64(a + b)),
            (BinOp::Sub, Value::F64(a), Value::F64(b)) => Ok(Value::F64(a - b)),
            (BinOp::Mul, Value::F64(a), Value::F64(b)) => Ok(Value::F64(a * b)),
            (BinOp::Div, Value::F64(a), Value::F64(b)) => Ok(Value::F64(a / b)),

            (BinOp::Eq, a, b) => Ok(Value::Bool(a == b)),
            (BinOp::Ne, a, b) => Ok(Value::Bool(a != b)),

            (BinOp::Lt, Value::U64(a), Value::U64(b)) => Ok(Value::Bool(a < b)),
            (BinOp::Le, Value::U64(a), Value::U64(b)) => Ok(Value::Bool(a <= b)),
            (BinOp::Gt, Value::U64(a), Value::U64(b)) => Ok(Value::Bool(a > b)),
            (BinOp::Ge, Value::U64(a), Value::U64(b)) => Ok(Value::Bool(a >= b)),

            (BinOp::Lt, Value::I64(a), Value::I64(b)) => Ok(Value::Bool(a < b)),
            (BinOp::Le, Value::I64(a), Value::I64(b)) => Ok(Value::Bool(a <= b)),
            (BinOp::Gt, Value::I64(a), Value::I64(b)) => Ok(Value::Bool(a > b)),
            (BinOp::Ge, Value::I64(a), Value::I64(b)) => Ok(Value::Bool(a >= b)),

            (BinOp::Lt, Value::F64(a), Value::F64(b)) => Ok(Value::Bool(a < b)),
            (BinOp::Le, Value::F64(a), Value::F64(b)) => Ok(Value::Bool(a <= b)),
            (BinOp::Gt, Value::F64(a), Value::F64(b)) => Ok(Value::Bool(a > b)),
            (BinOp::Ge, Value::F64(a), Value::F64(b)) => Ok(Value::Bool(a >= b)),

            (BinOp::And, Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a && *b)),
            (BinOp::Or, Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a || *b)),

            (BinOp::Add, Value::String(a), Value::String(b)) => {
                let mut value = String::with_capacity(a.len() + b.len());
                value.push_str(a);
                value.push_str(b);
                Ok(Value::String(Arc::from(value)))
            }

            _ => Err(self.type_error(&format!(
                "cannot apply {:?} to {} and {}",
                op,
                left.type_name(),
                right.type_name()
            ))),
        };
        self.finish_obligated_result(
            result,
            left_obligations
                .iter()
                .chain(right_obligations.iter())
                .cloned(),
        )
    }

    pub(super) fn compute_unary(&self, op: UnaryOp, value: &Value) -> Result<Value, PoisonError> {
        let (value, obligations) = self.split_obligations_ref(value);
        let result = match (op, value) {
            (UnaryOp::Neg, Value::I64(n)) => Ok(Value::I64(-n)),
            (UnaryOp::Neg, Value::F64(n)) => Ok(Value::F64(-n)),
            (UnaryOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
            _ => Err(self.type_error(&format!("cannot apply {:?} to {}", op, value.type_name()))),
        };
        self.finish_obligated_result(result, obligations.iter().cloned())
    }
}
