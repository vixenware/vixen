use super::*;

impl Engine {
    pub(super) async fn user_method_value_in_module(
        &self,
        receiver: &Value,
        type_name: &str,
        field: &str,
        current_module_id: &ModuleId,
    ) -> Option<Value> {
        let qualified = format!("{type_name}.{field}");
        let module = self.module_state(current_module_id).ok()?;
        let overloads = module.functions.get(&qualified)?;
        let function =
            Value::Function(self.global_function_value(current_module_id, overloads[0].clone()));
        Some(Value::Partial(Arc::new(PartialValue {
            function,
            args: Arc::from([receiver.clone()]),
            kwargs: Arc::from([]),
            kwargs_expr: None,
        })))
    }

    pub(super) async fn invoke_builtin(
        &self,
        builtin: &BuiltinValue,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
    ) -> Result<Value, PoisonError> {
        match builtin {
            BuiltinValue::Namespace(BuiltinNamespace::Blob) => {
                self.require_no_kwargs("Blob", kwargs)?;
                self.check_arity("Blob", args, 1)?;
                let value = self
                    .materialize_value_deep(args[0].clone(), "forcing Blob value")
                    .await?;
                let text = self.stringify_scalar(&value)?;
                Ok(Value::Blob(BlobValue::from_string(text)))
            }
            BuiltinValue::Namespace(BuiltinNamespace::Bytes) => {
                self.require_no_kwargs("Bytes", kwargs)?;
                self.check_arity("Bytes", args, 1)?;
                let value = self
                    .materialize_value_deep(args[0].clone(), "forcing Bytes value")
                    .await?;
                let text = self.stringify_scalar(&value)?;
                Ok(Value::Bytes(Arc::from(text.as_bytes().to_vec())))
            }
            BuiltinValue::Namespace(BuiltinNamespace::Tree) => {
                self.require_no_kwargs("Tree", kwargs)?;
                self.check_arity("Tree", args, 1)?;
                let (value, _) = self.split_obligations(
                    self.materialize_value_shallow(args[0].clone(), "forcing Tree input")
                        .await?,
                );
                match value {
                    Value::Map(map) => self.tree_from_map(&map).await,
                    Value::Array(_) => self.array_merge(&value).await,
                    Value::Path(path) => Ok(Value::Tree(TreeValue::FsDir(path))),
                    Value::String(path) => {
                        Ok(Value::Tree(TreeValue::FsDir(PathValue::new(path.as_ref()))))
                    }
                    other => Err(self.type_error(&format!(
                        "Tree expects Map, Array, or Path/String, got {}",
                        other.type_name()
                    ))),
                }
            }
            BuiltinValue::Function(BuiltinFunction::Acquire) => {
                self.acquire_builtin(args, kwargs, kwargs_expr).await
            }
            BuiltinValue::Function(BuiltinFunction::Stabilize) => {
                self.stabilize_builtin(args, kwargs, kwargs_expr).await
            }
            BuiltinValue::Function(BuiltinFunction::Render) => {
                self.render_builtin(args, kwargs, kwargs_expr).await
            }
            BuiltinValue::Function(BuiltinFunction::Exec) => {
                self.exec_builtin(args, kwargs, kwargs_expr).await
            }
            BuiltinValue::Function(BuiltinFunction::Discover) => {
                self.discover_builtin(args, kwargs, kwargs_expr).await
            }
            BuiltinValue::Function(BuiltinFunction::ExecutorClass) => {
                self.executor_class_builtin(args, kwargs, kwargs_expr).await
            }
            BuiltinValue::Function(BuiltinFunction::StructConstructor { type_name, fields }) => {
                self.require_no_kwargs(type_name.as_ref(), kwargs)?;
                self.check_arity(type_name.as_ref(), args, 1)?;
                let (value, _) = self.split_obligations(
                    self.materialize_value_shallow(
                        args[0].clone(),
                        &format!("forcing {type_name} payload"),
                    )
                    .await?,
                );
                let Value::Map(map) = value else {
                    return Err(self.type_error(&format!(
                        "{} expects a record/map payload, got {}",
                        type_name,
                        value.type_name()
                    )));
                };
                let mut values = Vec::with_capacity(fields.len());
                for field in fields.iter() {
                    let field_value = map
                        .entries
                        .iter()
                        .find_map(|(key, value)| match key {
                            Value::String(name) if name.as_ref() == field.as_ref() => {
                                Some(value.clone())
                            }
                            _ => None,
                        })
                        .ok_or_else(|| {
                            self.type_error(&format!("{} requires field '{}'", type_name, field))
                        })?;
                    values.push((Arc::<str>::clone(field), field_value));
                }
                Ok(Value::Struct(StructValue {
                    type_name: Arc::clone(type_name),
                    fields: Arc::from(values),
                }))
            }
            BuiltinValue::Method(BuiltinMethod::BlobFromString) => {
                self.require_no_kwargs("Blob.from_string", kwargs)?;
                self.check_arity("Blob.from_string", args, 1)?;
                let value = self
                    .materialize_value_deep(args[0].clone(), "forcing Blob.from_string value")
                    .await?;
                let text = self.stringify_scalar(&value)?;
                Ok(Value::Blob(BlobValue::from_string(text)))
            }
            BuiltinValue::Method(BuiltinMethod::BytesFromString) => {
                self.require_no_kwargs("Bytes", kwargs)?;
                self.check_arity("Bytes", args, 1)?;
                let value = self
                    .materialize_value_deep(args[0].clone(), "forcing Bytes.from_string value")
                    .await?;
                let text = self.stringify_scalar(&value)?;
                Ok(Value::Bytes(Arc::from(text.as_bytes().to_vec())))
            }
            BuiltinValue::Method(BuiltinMethod::StringStartsWith { receiver }) => {
                self.require_no_kwargs("string.starts_with", kwargs)?;
                self.check_arity("string.starts_with", args, 1)?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing string.starts_with receiver",
                    )
                    .await?,
                );
                let Value::String(receiver) = receiver else {
                    return Err(self.type_error("string.starts_with receiver must be String"));
                };
                let prefix = self
                    .materialize_value_deep(args[0].clone(), "forcing string.starts_with prefix")
                    .await?;
                let prefix = self.stringify_scalar(&prefix)?;
                self.finish_obligated_result(
                    Ok(Value::Bool(receiver.starts_with(&prefix))),
                    obligations,
                )
            }
            BuiltinValue::Method(BuiltinMethod::StringContains { receiver }) => {
                self.require_no_kwargs("string.contains", kwargs)?;
                self.check_arity("string.contains", args, 1)?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing string.contains receiver",
                    )
                    .await?,
                );
                let Value::String(receiver) = receiver else {
                    return Err(self.type_error("string.contains receiver must be String"));
                };
                let needle = self
                    .materialize_value_deep(args[0].clone(), "forcing string.contains needle")
                    .await?;
                let needle = self.stringify_scalar(&needle)?;
                self.finish_obligated_result(
                    Ok(Value::Bool(receiver.contains(&needle))),
                    obligations,
                )
            }
            BuiltinValue::Method(BuiltinMethod::StringSplit { receiver }) => {
                self.require_no_kwargs("string.split", kwargs)?;
                self.check_arity("string.split", args, 1)?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing string.split receiver",
                    )
                    .await?,
                );
                let Value::String(receiver) = receiver else {
                    return Err(self.type_error("string.split receiver must be String"));
                };
                let sep = self
                    .materialize_value_deep(args[0].clone(), "forcing string.split separator")
                    .await?;
                let sep = self.stringify_scalar(&sep)?;
                let items = receiver
                    .split(&sep)
                    .map(|item| Value::String(Arc::from(item)))
                    .collect::<Vec<_>>();
                self.finish_obligated_result(Ok(Value::Array(Arc::from(items))), obligations)
            }
            BuiltinValue::Method(BuiltinMethod::StringLines { receiver }) => {
                self.require_no_kwargs("string.lines", kwargs)?;
                self.check_arity("string.lines", args, 0)?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing string.lines receiver",
                    )
                    .await?,
                );
                let Value::String(receiver) = receiver else {
                    return Err(self.type_error("string.lines receiver must be String"));
                };
                let items = receiver
                    .lines()
                    .map(|line| Value::String(Arc::from(line)))
                    .collect::<Vec<_>>();
                self.finish_obligated_result(Ok(Value::Array(Arc::from(items))), obligations)
            }
            BuiltinValue::Method(BuiltinMethod::StringTrim { receiver }) => {
                self.require_no_kwargs("string.trim", kwargs)?;
                self.check_arity("string.trim", args, 0)?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing string.trim receiver",
                    )
                    .await?,
                );
                let Value::String(receiver) = receiver else {
                    return Err(self.type_error("string.trim receiver must be String"));
                };
                self.finish_obligated_result(
                    Ok(Value::String(Arc::from(receiver.trim()))),
                    obligations,
                )
            }
            BuiltinValue::Method(BuiltinMethod::StringToLower { receiver }) => {
                self.require_no_kwargs("string.to_lower", kwargs)?;
                self.check_arity("string.to_lower", args, 0)?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing string.to_lower receiver",
                    )
                    .await?,
                );
                let Value::String(receiver) = receiver else {
                    return Err(self.type_error("string.to_lower receiver must be String"));
                };
                self.finish_obligated_result(
                    Ok(Value::String(Arc::from(receiver.to_lowercase()))),
                    obligations,
                )
            }
            BuiltinValue::Method(BuiltinMethod::TreeOf) => {
                self.require_no_kwargs("Tree.of", kwargs)?;
                self.check_arity("Tree.of", args, 1)?;
                let (value, _) = self.split_obligations(
                    self.materialize_value_shallow(args[0].clone(), "forcing Tree.of input")
                        .await?,
                );
                let Value::Map(map) = value else {
                    return Err(
                        self.type_error(&format!("Tree.of expects Map, got {}", value.type_name()))
                    );
                };
                self.tree_from_map(&map).await
            }
            BuiltinValue::Method(BuiltinMethod::TreeEmpty) => {
                self.require_no_kwargs("Tree.empty", kwargs)?;
                self.check_arity("Tree.empty", args, 0)?;
                Ok(Value::Tree(TreeValue::Inline(Arc::from([]))))
            }
            BuiltinValue::Method(BuiltinMethod::TreeFromPath) => {
                self.require_no_kwargs("Tree.from_path", kwargs)?;
                self.check_arity("Tree.from_path", args, 1)?;
                let path = self
                    .materialize_value_deep(args[0].clone(), "forcing Tree.from_path")
                    .await?;
                let path = self.expect_path_value(&path, "Tree.from_path path")?;
                Ok(Value::Tree(TreeValue::FsDir(path)))
            }
            BuiltinValue::Method(BuiltinMethod::ArrayMap { receiver }) => {
                self.require_no_kwargs("array.map", kwargs)?;
                self.check_arity("array.map", args, 1)?;
                self.array_map(receiver, args[0].clone()).await
            }
            BuiltinValue::Method(BuiltinMethod::ArrayToMap { receiver }) => {
                self.require_no_kwargs("array.to_map", kwargs)?;
                self.check_arity("array.to_map", args, 1)?;
                self.array_to_map(receiver, args[0].clone()).await
            }
            BuiltinValue::Method(BuiltinMethod::MapMapValues { receiver }) => {
                self.require_no_kwargs("map.map_values", kwargs)?;
                self.check_arity("map.map_values", args, 1)?;
                self.map_map_values(receiver, args[0].clone()).await
            }
            BuiltinValue::Method(BuiltinMethod::MapHas { receiver }) => {
                self.require_no_kwargs("map.has", kwargs)?;
                self.check_arity("map.has", args, 1)?;
                let (receiver, _obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing map.has receiver",
                    )
                    .await?,
                );
                let Value::Map(map) = receiver else {
                    return Err(self.type_error("map.has receiver must be Map"));
                };
                let key = self
                    .materialize_value_shallow(args[0].clone(), "forcing map.has key")
                    .await?;
                let found = map.entries.iter().any(|(k, _)| *k == key);
                Ok(Value::Bool(found))
            }
            BuiltinValue::Method(BuiltinMethod::MapGet { receiver }) => {
                self.require_no_kwargs("map.get", kwargs)?;
                self.check_arity("map.get", args, 1)?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing map.get receiver",
                    )
                    .await?,
                );
                let Value::Map(map) = receiver else {
                    return Err(self.type_error("map.get receiver must be Map"));
                };
                let key = self
                    .materialize_value_shallow(args[0].clone(), "forcing map.get key")
                    .await?;
                match map.entries.iter().find(|(k, _)| *k == key) {
                    Some((_, value)) => {
                        Ok(self.attach_obligations(value.clone(), obligations.iter().cloned()))
                    }
                    None => {
                        Err(self
                            .type_error(&format!("map.get: key not found: {}", key.type_name())))
                    }
                }
            }
            BuiltinValue::Method(BuiltinMethod::ArrayFlatten { receiver }) => {
                self.require_no_kwargs("array.flatten", kwargs)?;
                self.check_arity("array.flatten", args, 0)?;
                self.array_flatten(receiver).await
            }
            BuiltinValue::Method(BuiltinMethod::ArrayMerge { receiver }) => {
                self.require_no_kwargs("array.merge", kwargs)?;
                self.check_arity("array.merge", args, 0)?;
                self.array_merge(receiver).await
            }
            BuiltinValue::Method(BuiltinMethod::BlobToString { receiver }) => {
                self.require_no_kwargs("blob.to_string", kwargs)?;
                self.check_arity("blob.to_string", args, 0)?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing blob.to_string receiver",
                    )
                    .await?,
                );
                let Value::Blob(blob) = receiver else {
                    return Err(self.type_error("blob.to_string receiver must be Blob"));
                };
                self.finish_obligated_result(
                    blob.to_utf8_string().map(Value::String).map_err(|message| {
                        PoisonError::new(
                            PoisonKind::TypeError,
                            message,
                            self.current_demand_chain(None),
                        )
                    }),
                    obligations,
                )
            }
            BuiltinValue::Method(BuiltinMethod::BlobExecutable { receiver }) => {
                self.require_no_kwargs("blob.executable", kwargs)?;
                self.check_arity("blob.executable", args, 0)?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing blob.executable receiver",
                    )
                    .await?,
                );
                let Value::Blob(blob) = receiver else {
                    return Err(self.type_error("blob.executable receiver must be Blob"));
                };
                let blob = match blob {
                    BlobValue::Inline { bytes, .. } => BlobValue::Inline {
                        bytes,
                        executable: true,
                    },
                    BlobValue::FsFile { path, .. } => BlobValue::FsFile {
                        path,
                        executable: true,
                    },
                    BlobValue::Store { blob_ref, .. } => BlobValue::Store {
                        blob_ref,
                        executable: true,
                    },
                };
                self.finish_obligated_result(Ok(Value::Blob(blob)), obligations)
            }
            BuiltinValue::Method(BuiltinMethod::BlobUtf8 { receiver }) => {
                self.require_no_kwargs("blob.utf8", kwargs)?;
                self.check_arity("blob.utf8", args, 0)?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing blob.utf8 receiver",
                    )
                    .await?,
                );
                let Value::Blob(blob) = receiver else {
                    return Err(self.type_error("blob.utf8 receiver must be Blob"));
                };
                self.finish_obligated_result(
                    blob.to_utf8_string().map(Value::String).map_err(|message| {
                        PoisonError::new(
                            PoisonKind::TypeError,
                            message,
                            self.current_demand_chain(None),
                        )
                    }),
                    obligations,
                )
            }
            BuiltinValue::Method(BuiltinMethod::BlobUtf8Lossy { receiver }) => {
                self.require_no_kwargs("blob.utf8_lossy", kwargs)?;
                self.check_arity("blob.utf8_lossy", args, 0)?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing blob.utf8_lossy receiver",
                    )
                    .await?,
                );
                let Value::Blob(blob) = receiver else {
                    return Err(self.type_error("blob.utf8_lossy receiver must be Blob"));
                };
                let result = match blob {
                    BlobValue::Inline { bytes, .. } => Ok(Value::String(Arc::from(
                        String::from_utf8_lossy(bytes.as_ref()).into_owned(),
                    ))),
                    BlobValue::FsFile { path, .. } => std::fs::read(path.as_str())
                        .map(|bytes| {
                            Value::String(Arc::from(String::from_utf8_lossy(&bytes).into_owned()))
                        })
                        .map_err(|error| {
                            self.type_error(&format!("failed to read {}: {error}", path.as_str()))
                        }),
                    BlobValue::Store { blob_ref, .. } => Err(self.type_error(&format!(
                        "store-backed blob {:?} requires an external store resolver",
                        blob_ref
                    ))),
                };
                self.finish_obligated_result(result, obligations)
            }
            BuiltinValue::Method(BuiltinMethod::BytesUtf8 { receiver }) => {
                self.require_no_kwargs("bytes.utf8", kwargs)?;
                self.check_arity("bytes.utf8", args, 0)?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing bytes.utf8 receiver",
                    )
                    .await?,
                );
                let Value::Bytes(bytes) = receiver else {
                    return Err(self.type_error("bytes.utf8 receiver must be Bytes"));
                };
                self.finish_obligated_result(
                    String::from_utf8(bytes.as_ref().to_vec())
                        .map(|text| Value::String(Arc::from(text)))
                        .map_err(|error| {
                            PoisonError::new(
                                PoisonKind::TypeError,
                                format!("bytes are not UTF-8: {error}"),
                                self.current_demand_chain(None),
                            )
                        }),
                    obligations,
                )
            }
            BuiltinValue::Method(BuiltinMethod::BytesUtf8Lossy { receiver }) => {
                self.require_no_kwargs("bytes.utf8_lossy", kwargs)?;
                self.check_arity("bytes.utf8_lossy", args, 0)?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing bytes.utf8_lossy receiver",
                    )
                    .await?,
                );
                let Value::Bytes(bytes) = receiver else {
                    return Err(self.type_error("bytes.utf8_lossy receiver must be Bytes"));
                };
                let text = String::from_utf8_lossy(bytes.as_ref()).into_owned();
                self.finish_obligated_result(Ok(Value::String(Arc::from(text))), obligations)
            }
            BuiltinValue::Method(BuiltinMethod::BytesBlob { receiver }) => {
                self.require_no_kwargs("bytes.blob", kwargs)?;
                self.check_arity("bytes.blob", args, 0)?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing bytes.blob receiver",
                    )
                    .await?,
                );
                let Value::Bytes(bytes) = receiver else {
                    return Err(self.type_error("bytes.blob receiver must be Bytes"));
                };
                self.finish_obligated_result(
                    Ok(Value::Blob(BlobValue::Inline {
                        bytes,
                        executable: false,
                    })),
                    obligations,
                )
            }
            BuiltinValue::Method(BuiltinMethod::StreamCollect { receiver }) => {
                self.require_no_kwargs("stream.collect", kwargs)?;
                self.check_arity("stream.collect", args, 0)?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_deep(
                        receiver.as_ref().clone(),
                        "forcing stream.collect receiver",
                    )
                    .await?,
                );
                self.finish_obligated_result(Ok(receiver), obligations)
            }
            BuiltinValue::Method(BuiltinMethod::TreeFile { receiver }) => {
                self.require_no_kwargs("tree.file", kwargs)?;
                self.check_arity("tree.file", args, 1)?;
                let (path, path_obligations) = self.split_obligations(
                    self.materialize_value_deep(args[0].clone(), "forcing tree.file path")
                        .await?,
                );
                let path = self.expect_relative_path(&path, "tree.file path")?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing tree.file receiver",
                    )
                    .await?,
                );
                let Value::Tree(tree) = receiver else {
                    return Err(self.type_error("tree.file receiver must be Tree"));
                };
                self.finish_obligated_result(
                    self.tree_file(&tree, path).await,
                    obligations.into_iter().chain(path_obligations),
                )
            }
            BuiltinValue::Method(BuiltinMethod::TreeMerge { receiver }) => {
                self.require_no_kwargs("tree.merge", kwargs)?;
                self.check_arity("tree.merge", args, 1)?;
                let (other, other_obligations) = self.split_obligations(
                    self.materialize_value_shallow(args[0].clone(), "forcing tree.merge other")
                        .await?,
                );
                let Value::Tree(other) = other else {
                    return Err(self.type_error("tree.merge expects Tree"));
                };
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing tree.merge receiver",
                    )
                    .await?,
                );
                let Value::Tree(receiver) = receiver else {
                    return Err(self.type_error("tree.merge receiver must be Tree"));
                };
                self.finish_obligated_result(
                    self.tree_merge(&receiver, &other).await,
                    obligations.into_iter().chain(other_obligations),
                )
            }
            BuiltinValue::Method(BuiltinMethod::TreeSubtree { receiver }) => {
                self.require_no_kwargs("tree.subtree", kwargs)?;
                self.check_arity("tree.subtree", args, 1)?;
                let (path, path_obligations) = self.split_obligations(
                    self.materialize_value_deep(args[0].clone(), "forcing tree.subtree path")
                        .await?,
                );
                let path = self.expect_relative_path(&path, "tree.subtree path")?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing tree.subtree receiver",
                    )
                    .await?,
                );
                let Value::Tree(tree) = receiver else {
                    return Err(self.type_error("tree.subtree receiver must be Tree"));
                };
                self.finish_obligated_result(
                    self.tree_subtree(&tree, path).await,
                    obligations.into_iter().chain(path_obligations),
                )
            }
            BuiltinValue::Method(BuiltinMethod::OptionUnwrapOr { receiver }) => {
                self.require_no_kwargs("option.unwrap_or", kwargs)?;
                self.check_arity("option.unwrap_or", args, 1)?;
                let (receiver, obligations) = self.split_obligations(
                    self.materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing option.unwrap_or receiver",
                    )
                    .await?,
                );
                let Value::Struct(option) = receiver else {
                    return Err(self.type_error("option.unwrap_or receiver must be Option"));
                };
                let result = match option.type_name.as_ref() {
                    "Some" => option
                        .fields
                        .iter()
                        .find(|(name, _)| name.as_ref() == "value")
                        .map(|(_, value)| value.clone())
                        .ok_or_else(|| self.type_error("Some.unwrap_or requires a value field")),
                    "None" => Ok(args[0].clone()),
                    _ => Err(self.type_error("option.unwrap_or receiver must be Option")),
                };
                self.finish_obligated_result(result, obligations)
            }
            BuiltinValue::Method(BuiltinMethod::FallibleCall { receiver }) => {
                self.require_no_kwargs("fallible", kwargs)?;
                self.check_arity("fallible", args, 0)?;
                Ok(self.build_fallible_value(receiver.as_ref().clone()).await)
            }
            BuiltinValue::Method(BuiltinMethod::HostOs { receiver }) => {
                self.require_no_kwargs("host.os", kwargs)?;
                self.check_arity("host.os", args, 0)?;
                let receiver = self
                    .materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing host.os receiver",
                    )
                    .await?;
                let Value::Host(_) = receiver else {
                    return Err(self.type_error("host.os receiver must be Host"));
                };
                Ok(self.host_os())
            }
            BuiltinValue::Method(BuiltinMethod::HostArch { receiver }) => {
                self.require_no_kwargs("host.arch", kwargs)?;
                self.check_arity("host.arch", args, 0)?;
                let receiver = self
                    .materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing host.arch receiver",
                    )
                    .await?;
                let Value::Host(_) = receiver else {
                    return Err(self.type_error("host.arch receiver must be Host"));
                };
                Ok(self.host_arch())
            }
            BuiltinValue::Method(BuiltinMethod::HostRead { receiver }) => {
                self.require_no_kwargs("host.read", kwargs)?;
                self.check_arity("host.read", args, 1)?;
                let (path, obligations) = self.split_obligations(
                    self.materialize_value_deep(args[0].clone(), "forcing host.read")
                        .await?,
                );
                let path = self.expect_host_path(&path, "host.read path").await?;
                let receiver = self
                    .materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing host.read receiver",
                    )
                    .await?;
                let Value::Host(_) = receiver else {
                    return Err(self.type_error("host.read receiver must be Host"));
                };
                self.finish_obligated_result(self.host_read(&path).await, obligations)
            }
            BuiltinValue::Method(BuiltinMethod::HostHash { receiver }) => {
                self.require_no_kwargs("host.hash", kwargs)?;
                self.check_arity("host.hash", args, 1)?;
                let (path, obligations) = self.split_obligations(
                    self.materialize_value_deep(args[0].clone(), "forcing host.hash")
                        .await?,
                );
                let path = self.expect_host_path(&path, "host.hash path").await?;
                let receiver = self
                    .materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing host.hash receiver",
                    )
                    .await?;
                let Value::Host(_) = receiver else {
                    return Err(self.type_error("host.hash receiver must be Host"));
                };
                self.finish_obligated_result(self.host_hash(&path).await, obligations)
            }
            BuiltinValue::Method(BuiltinMethod::HostWhich { receiver }) => {
                self.require_no_kwargs("host.which", kwargs)?;
                self.check_arity("host.which", args, 1)?;
                let (name, obligations) = self.split_obligations(
                    self.materialize_value_deep(args[0].clone(), "forcing host.which")
                        .await?,
                );
                let name = self.expect_host_path(&name, "host.which name").await?;
                let receiver = self
                    .materialize_value_shallow(
                        receiver.as_ref().clone(),
                        "forcing host.which receiver",
                    )
                    .await?;
                let Value::Host(_) = receiver else {
                    return Err(self.type_error("host.which receiver must be Host"));
                };
                self.finish_obligated_result(self.host_which(&name).await, obligations)
            }
        }
    }

    pub(super) async fn tree_from_map(&self, map: &MapValue) -> Result<Value, PoisonError> {
        fn insert_tree_path(
            engine: &Engine,
            entries: &mut BTreeMap<Arc<str>, Value>,
            segments: &[&str],
            value: Value,
        ) -> Result<(), PoisonError> {
            let Some((head, tail)) = segments.split_first() else {
                return Err(engine.type_error("Tree entry path cannot be empty"));
            };
            if tail.is_empty() {
                if entries.contains_key(*head) {
                    return Err(engine.type_error(&format!(
                        "Tree contains duplicate entry '{}'",
                        segments.join("/")
                    )));
                }
                entries.insert(Arc::from(*head), value);
                return Ok(());
            }

            let child = entries
                .entry(Arc::from(*head))
                .or_insert_with(|| Value::Tree(TreeValue::Inline(Arc::from([]))));
            match child {
                Value::Tree(TreeValue::Inline(existing)) => {
                    let mut nested = existing
                        .iter()
                        .cloned()
                        .collect::<BTreeMap<Arc<str>, Value>>();
                    insert_tree_path(engine, &mut nested, tail, value)?;
                    *child = Value::Tree(TreeValue::Inline(Arc::from(
                        nested.into_iter().collect::<Vec<_>>(),
                    )));
                    Ok(())
                }
                Value::Tree(TreeValue::FsDir(_))
                | Value::Tree(TreeValue::Store(_))
                | Value::Blob(_)
                | Value::Pending(_) => Err(engine.type_error(&format!(
                    "Tree entry path '{}' collides with an existing non-inline tree entry",
                    segments.join("/")
                ))),
                other => Err(engine.type_error(&format!(
                    "tree entries must be Blob or Tree, got {}",
                    other.type_name()
                ))),
            }
        }

        let mut nested = BTreeMap::<Arc<str>, Value>::new();
        for (key, value) in map.entries.iter() {
            let key = self
                .materialize_value_deep(key.clone(), "forcing Tree entry path")
                .await?;
            let path = self.expect_relative_path(&key, "Tree entry path")?;
            let raw = path.as_str();
            if raw.is_empty() || raw == "." {
                return Err(self.type_error("Tree entry path cannot be empty"));
            }
            let segments = raw.split('/').collect::<Vec<_>>();
            insert_tree_path(self, &mut nested, &segments, value.clone())?;
        }
        self.validate_tree_entries(&nested.values().cloned().collect::<Vec<_>>())?;
        Ok(Value::Tree(TreeValue::Inline(Arc::from(
            nested.into_iter().collect::<Vec<_>>(),
        ))))
    }

    pub(super) async fn array_map(
        &self,
        receiver: &Value,
        callback: Value,
    ) -> Result<Value, PoisonError> {
        let (receiver, obligations) = self.split_obligations(
            self.materialize_value_shallow(receiver.clone(), "forcing array.map receiver")
                .await?,
        );
        let Value::Array(items) = receiver else {
            return Err(self.type_error("array.map receiver must be Array"));
        };

        let out = items
            .iter()
            .map(|item| self.build_call(callback.clone(), vec![item.clone()], None))
            .collect::<Vec<_>>();

        Ok(self.attach_obligations(Value::Array(Arc::from(out)), obligations))
    }

    pub(super) async fn array_to_map(
        &self,
        receiver: &Value,
        callback: Value,
    ) -> Result<Value, PoisonError> {
        let (receiver, obligations) = self.split_obligations(
            self.materialize_value_shallow(receiver.clone(), "forcing array.to_map receiver")
                .await?,
        );
        let Value::Array(items) = receiver else {
            return Err(self.type_error("array.to_map receiver must be Array"));
        };

        let entries = items
            .iter()
            .map(|item| {
                (
                    item.clone(),
                    self.build_call(callback.clone(), vec![item.clone()], None),
                )
            })
            .collect::<Vec<_>>();

        Ok(self.attach_obligations(
            Value::Map(MapValue {
                entries: Arc::from(entries),
            }),
            obligations,
        ))
    }

    pub(super) async fn array_flatten(&self, receiver: &Value) -> Result<Value, PoisonError> {
        let (receiver, obligations) = self.split_obligations(
            self.materialize_value_shallow(receiver.clone(), "forcing array.flatten receiver")
                .await?,
        );
        let Value::Array(items) = receiver else {
            return Err(self.type_error("array.flatten receiver must be Array"));
        };

        let mut out = Vec::new();
        for item in items.iter() {
            let (item, item_obligations) = self.split_obligations(
                self.materialize_value_shallow(item.clone(), "forcing array.flatten item")
                    .await?,
            );
            let Value::Array(inner) = item else {
                return Err(self.type_error("array.flatten expects Array items"));
            };
            for value in inner.iter() {
                out.push(self.attach_obligations(value.clone(), item_obligations.iter().cloned()));
            }
        }

        Ok(self.attach_obligations(Value::Array(Arc::from(out)), obligations))
    }

    pub(super) async fn array_merge(&self, receiver: &Value) -> Result<Value, PoisonError> {
        let (receiver, receiver_obligations) = self.split_obligations(
            self.materialize_value_shallow(receiver.clone(), "forcing array.merge receiver")
                .await?,
        );
        let Value::Array(items) = receiver else {
            return Err(self.type_error("array.merge receiver must be Array"));
        };

        if items.is_empty() {
            return Ok(self.attach_obligations(
                Value::Tree(TreeValue::Inline(Arc::from([]))),
                receiver_obligations,
            ));
        }

        let mut materialized = self
            .materialize_many_deep(items.iter().cloned().collect(), "forcing array.merge items")
            .await?;
        let first = materialized.remove(0);
        let (first, mut obligations) = self.split_obligations(first);
        let Value::Tree(mut merged) = first else {
            return Err(self.type_error("array.merge expects Tree items"));
        };
        obligations.extend(receiver_obligations);

        for item in materialized {
            let (item, item_obligations) = self.split_obligations(item);
            let Value::Tree(tree) = item else {
                return Err(self.type_error("array.merge expects Tree items"));
            };
            merged = match self.tree_merge(&merged, &tree).await? {
                Value::Tree(tree) => tree,
                other => {
                    return Err(self.type_error(&format!(
                        "array.merge expected Tree result, got {}",
                        other.type_name()
                    )));
                }
            };
            obligations.extend(item_obligations);
        }

        Ok(self.attach_obligations(Value::Tree(merged), obligations))
    }

    pub(super) async fn map_map_values(
        &self,
        receiver: &Value,
        callback: Value,
    ) -> Result<Value, PoisonError> {
        let (receiver, obligations) = self.split_obligations(
            self.materialize_value_shallow(receiver.clone(), "forcing map.map_values receiver")
                .await?,
        );
        let Value::Map(map) = receiver else {
            return Err(self.type_error("map.map_values receiver must be Map"));
        };

        let entries = map
            .entries
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    self.build_call(callback.clone(), vec![value.clone()], None),
                )
            })
            .collect::<Vec<_>>();

        Ok(self.attach_obligations(
            Value::Map(MapValue {
                entries: Arc::from(entries),
            }),
            obligations,
        ))
    }

    pub(super) async fn acquire_builtin(
        &self,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
    ) -> Result<Value, PoisonError> {
        let kwargs = self
            .materialize_kwargs(kwargs, kwargs_expr, "acquire")
            .await?;
        self.check_arity("acquire", args, 1)?;

        let url = self
            .materialize_value_deep(args[0].clone(), "forcing acquire url")
            .await?;
        let url = self.expect_host_path(&url, "acquire url").await?;

        let mut digest = None;
        let mut extract = None;
        for (name, value) in &kwargs {
            match name.as_ref() {
                "digest" => digest = Some(value.clone()),
                "extract" => extract = Some(value.clone()),
                other => {
                    return Err(self.type_error(&format!(
                        "acquire does not accept keyword argument '{other}'"
                    )));
                }
            }
        }

        let digest =
            digest.ok_or_else(|| self.type_error("acquire requires keyword argument 'digest'"))?;
        let digest = self
            .materialize_value_deep(digest, "forcing acquire digest")
            .await?;
        let digest = self.expect_host_path(&digest, "acquire digest").await?;

        let extract = match extract {
            Some(value) => {
                let value = self
                    .materialize_value_deep(value, "forcing acquire extract")
                    .await?;
                Some(self.expect_host_path(&value, "acquire extract").await?)
            }
            None => None,
        };

        let Some(runtime) = &self.runtime else {
            return Err(self.type_error("acquire requires an acquire handler"));
        };

        runtime
            .acquire(AcquireRequest {
                url,
                digest,
                extract,
                demand_chain: self.current_demand_chain(None),
            })
            .await
    }

    pub(super) async fn import_module(
        &self,
        path: &str,
        provenance: Option<&instr::SourceProvenance>,
    ) -> Result<Value, PoisonError> {
        let Some(runtime) = &self.runtime else {
            return Err(self.type_error("import requires an import handler"));
        };
        let from_source_id = provenance
            .map(|provenance| provenance.source_id.to_string())
            .ok_or_else(|| self.internal_error("import is missing source provenance"))?;
        let module = runtime
            .import_module(ImportRequest {
                path: path.to_string(),
                from_source_id,
                demand_chain: self.current_demand_chain(None),
            })
            .await?;
        let module_id = Arc::<str>::from(module.source_id.as_ref());
        match self.modules.entry(Arc::clone(&module_id)) {
            Entry::Occupied(_) => {}
            Entry::Vacant(entry) => {
                entry.insert(Arc::new(Self::build_module_state(module)));
            }
        }
        self.module_namespace_for(&module_id).await
    }

    pub(super) async fn render_builtin(
        &self,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
    ) -> Result<Value, PoisonError> {
        let kwargs = self
            .materialize_kwargs(kwargs, kwargs_expr, "render")
            .await?;
        self.check_arity("render", args, 1)?;

        let template = self
            .materialize_value_deep(args[0].clone(), "forcing render template")
            .await?;
        let mut obligations = BTreeMap::new();
        self.collect_value_obligations(&template, &mut obligations);
        let template = self.expect_path_value(&template, "render template")?;

        let mut bindings = None;
        for (name, value) in &kwargs {
            match name.as_ref() {
                "bindings" => bindings = Some(value.clone()),
                other => {
                    return Err(self.type_error(&format!(
                        "render does not accept keyword argument '{other}'"
                    )));
                }
            }
        }

        let bindings = bindings
            .ok_or_else(|| self.type_error("render requires keyword argument 'bindings'"))?;
        let bindings = self
            .materialize_value_deep(bindings, "forcing render bindings")
            .await?;
        self.collect_value_obligations(&bindings, &mut obligations);

        let Some(runtime) = &self.runtime else {
            return Err(self.type_error("render requires a render handler"));
        };

        self.finish_obligated_result(
            runtime
                .render(RenderRequest {
                    template: template.as_str().to_string(),
                    bindings,
                    demand_chain: self.current_demand_chain(None),
                })
                .await,
            obligations.into_values(),
        )
    }

    pub(super) async fn stabilize_builtin(
        &self,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
    ) -> Result<Value, PoisonError> {
        self.require_no_kwargs("stabilize", kwargs)?;
        if kwargs_expr.is_some() {
            return Err(self.type_error("stabilize does not accept keyword arguments"));
        }
        self.check_arity("stabilize", args, 2)?;

        let witness = self
            .materialize_value_deep(args[0].clone(), "forcing stabilize witness")
            .await?;
        let witness_value = self
            .resolve_boundary_value(self.build_call(witness, vec![], None))
            .await?;
        let witness_value = self.snapshot_value(witness_value).await?;

        let resolve = self
            .materialize_value_deep(args[1].clone(), "forcing stabilize resolve")
            .await?;
        let resolve = self.ship_function(resolve).await?;

        let Some(runtime) = &self.runtime else {
            return Err(self.type_error("stabilize requires a stabilize handler"));
        };

        runtime
            .stabilize(StabilizeRequest {
                witness: witness_value,
                module: self.transport_module(),
                resolve,
                demand_chain: self.current_demand_chain(None),
            })
            .await
    }

    pub(super) async fn exec_builtin(
        &self,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
    ) -> Result<Value, PoisonError> {
        let _permit = match &self.exec_semaphore {
            Some(sem) => Some(sem.acquire().await.expect("exec semaphore closed")),
            None => None,
        };
        let kwargs = self.materialize_kwargs(kwargs, kwargs_expr, "exec").await?;
        let request = self.parse_exec_request(args, &kwargs).await?;
        if let Some(runtime) = &self.runtime
            && runtime.has_exec()
        {
            let request = self.ship_exec_request(&request).await?;
            return runtime.exec(request).await;
        }
        self.exec_builtin_local(request).await
    }

    pub(super) async fn discover_builtin(
        &self,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
    ) -> Result<Value, PoisonError> {
        if kwargs_expr.is_some() {
            return Err(self.type_error("discover does not accept keyword records"));
        }
        self.require_no_kwargs("discover", kwargs)?;
        self.check_arity("discover", args, 2)?;
        let executor = self
            .materialize_value_deep(args[0].clone(), "forcing discover executor")
            .await?;
        let executor = self
            .expect_executor_selector(&executor, "discover executor")
            .await?;
        let probe = self
            .materialize_value_deep(args[1].clone(), "forcing discover probe")
            .await?;

        if let Some(runtime) = &self.runtime
            && runtime.has_discover()
        {
            let request = DiscoverRequest {
                executor,
                demand_chain: self.current_demand_chain(None),
                module: self.transport_module(),
                probe: self.ship_function(probe).await?,
            };
            return runtime.discover(request).await;
        }

        self.invoke_discover_probe(probe).await
    }

    pub(super) async fn executor_class_builtin(
        &self,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
    ) -> Result<Value, PoisonError> {
        if kwargs_expr.is_some() {
            return Err(self.type_error("executor_class does not accept keyword records"));
        }
        self.require_no_kwargs("executor_class", kwargs)?;
        self.check_arity("executor_class", args, 1)?;
        let name = self
            .materialize_value_deep(args[0].clone(), "forcing executor_class name")
            .await?;
        let name = self.expect_host_path(&name, "executor_class name").await?;
        Ok(Value::Executor(ExecutorSelector::class(name)))
    }

    pub(super) async fn materialize_kwargs(
        &self,
        kwargs: &[(Arc<str>, Value)],
        kwargs_expr: Option<Value>,
        context: &str,
    ) -> Result<Vec<(Arc<str>, Value)>, PoisonError> {
        let mut out = kwargs.to_vec();
        let Some(kwargs_expr) = kwargs_expr else {
            return Ok(out);
        };

        let kwargs_expr = self
            .materialize_value_shallow(kwargs_expr, &format!("forcing {context} keyword record"))
            .await?;
        let (value, obligations) = self.split_obligations(kwargs_expr);
        let Value::Map(map) = value else {
            return Err(self.type_error(&format!(
                "{context} keyword record must be a record/map, got {}",
                value.type_name()
            )));
        };

        for (key, value) in map.entries.iter() {
            let key = self
                .materialize_value_deep(key.clone(), &format!("forcing {context} keyword key"))
                .await?;
            let Value::String(name) = key else {
                return Err(self.type_error(&format!(
                    "{context} keyword record keys must be String, got {}",
                    key.type_name()
                )));
            };
            out.push((
                Arc::from(name.as_ref()),
                self.attach_obligations(value.clone(), obligations.iter().cloned()),
            ));
        }

        Ok(out)
    }

    pub(super) async fn merge_keyword_records(
        &self,
        left: Value,
        right: Value,
    ) -> Result<Value, PoisonError> {
        let mut entries = Vec::new();
        let left = self
            .materialize_value_shallow(left, "forcing keyword record spread")
            .await?;
        self.extend_record_with_spread(&mut entries, left).await?;
        let right = self
            .materialize_value_shallow(right, "forcing keyword record spread")
            .await?;
        self.extend_record_with_spread(&mut entries, right).await?;
        Ok(Value::Map(MapValue {
            entries: Arc::from(entries),
        }))
    }
}
