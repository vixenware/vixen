use super::*;

impl Engine {
    pub(super) async fn parse_exec_request(
        &self,
        args: &[Value],
        kwargs: &[(Arc<str>, Value)],
    ) -> Result<ParsedExecRequest, PoisonError> {
        self.check_arity("exec", args, 1)?;

        let mut env_value = None;
        let mut mounts_value = None;
        let mut observer_value = None;
        let mut executor_value = None;
        let mut writable_value = None;
        let mut create_dirs_value = None;

        for (name, value) in kwargs {
            match name.as_ref() {
                "env" => env_value = Some(value.clone()),
                "mounts" => mounts_value = Some(value.clone()),
                "observer" => observer_value = Some(value.clone()),
                "executor" => executor_value = Some(value.clone()),
                "writable" => writable_value = Some(value.clone()),
                "create_dirs" => create_dirs_value = Some(value.clone()),
                other => {
                    return Err(self
                        .type_error(&format!("exec does not accept keyword argument '{other}'")));
                }
            }
        }

        let args_value = self
            .materialize_value_shallow(args[0].clone(), "forcing exec args")
            .await?;
        let (args_value, top_level_arg_obligations) = self.split_obligations(args_value);
        let Value::Array(exec_args_raw) = args_value else {
            return Err(self.type_error(&format!(
                "exec expects Array[String | Path | Blob | Tree] for args, got {}",
                args_value.type_name()
            )));
        };

        let observer =
            observer_value.ok_or_else(|| self.type_error("exec requires an observer function"))?;
        let observer = self
            .materialize_value_deep(observer, "forcing exec observer")
            .await?;
        match self.split_obligations_ref(&observer).0 {
            Value::Function(_) | Value::Builtin(_) => {}
            other => {
                return Err(self.type_error(&format!(
                    "exec observer must be callable, got {}",
                    other.type_name()
                )));
            }
        }

        let executor = match executor_value {
            Some(executor) => {
                let executor = self
                    .materialize_value_deep(executor, "forcing exec executor")
                    .await?;
                self.expect_executor_selector(&executor, "exec executor")
                    .await?
            }
            None => ExecutorSelector::class("default"),
        };

        let mut verification_obligations = BTreeMap::new();
        Self::add_obligations(&mut verification_obligations, top_level_arg_obligations);
        self.collect_value_obligations(
            &Value::Array(exec_args_raw.clone()),
            &mut verification_obligations,
        );
        if let Some(env_value) = &env_value {
            self.collect_value_obligations(env_value, &mut verification_obligations);
        }
        if let Some(mounts_value) = &mounts_value {
            self.collect_value_obligations(mounts_value, &mut verification_obligations);
        }
        if let Some(writable_value) = &writable_value {
            self.collect_value_obligations(writable_value, &mut verification_obligations);
        }
        if let Some(create_dirs_value) = &create_dirs_value {
            self.collect_value_obligations(create_dirs_value, &mut verification_obligations);
        }
        self.collect_value_obligations(&observer, &mut verification_obligations);

        let mut mounts = self.exec_mounts_from_value(mounts_value).await?;
        let explicit_writable = self.exec_writable_from_value(writable_value).await?;
        let env = self
            .exec_env_from_value(env_value, &mut mounts, &explicit_writable)
            .await?;
        let mut exec_args = Vec::with_capacity(exec_args_raw.len());
        for value in exec_args_raw.iter() {
            exec_args.push(
                self.parse_exec_arg(value, &mut mounts, &explicit_writable, "exec arg")
                    .await?,
            );
        }
        if exec_args.is_empty() {
            return Err(self.type_error("exec requires at least one program argument"));
        }
        let writable_roots = if explicit_writable.is_empty() {
            self.infer_exec_writable_roots(&mounts, &exec_args, &env)
                .await
        } else {
            explicit_writable
        };
        let create_dirs = self
            .exec_create_dirs_from_value(create_dirs_value, &writable_roots)
            .await?;

        Ok(ParsedExecRequest {
            args: exec_args,
            env,
            mounts,
            writable_roots,
            create_dirs,
            executor,
            observer,
            verification_obligations: verification_obligations.into_values().collect(),
        })
    }

    pub(super) async fn ship_exec_request(
        &self,
        request: &ParsedExecRequest,
    ) -> Result<ExecRequest, PoisonError> {
        let mut shipped_mounts = Vec::with_capacity(request.mounts.len());
        for (path, value) in request.mounts.iter() {
            shipped_mounts.push(ExecMount {
                path: path.clone(),
                value: self.snapshot_exec_mount_value(value.clone()).await?,
            });
        }
        Ok(ExecRequest {
            args: request.args.clone(),
            env: request.env.clone(),
            mounts: shipped_mounts,
            writable_roots: request.writable_roots.clone(),
            create_dirs: request.create_dirs.clone(),
            executor: request.executor.clone(),
            demand_chain: self.current_demand_chain(None),
            module: self.transport_module(),
            observer: self.ship_function(request.observer.clone()).await?,
            verification_obligations: request.verification_obligations.clone(),
        })
    }

    pub(super) async fn exec_builtin_local(
        &self,
        request: ParsedExecRequest,
    ) -> Result<Value, PoisonError> {
        let sandbox = TempDir::new().map_err(|error| {
            self.internal_error(&format!("failed to create exec sandbox: {error}"))
        })?;
        let sandbox_root = PathValue::new(sandbox.path().to_string_lossy());

        for (mount_path, value) in &request.mounts {
            self.materialize_mount_value(&sandbox_root, mount_path, value)
                .await?;
        }
        for root in &request.writable_roots {
            self.ensure_dir(&sandbox_root.join(root.as_str()))?;
        }
        for dir in &request.create_dirs {
            self.ensure_dir(&sandbox_root.join(dir.as_str()))?;
        }

        let verification_task = if request.verification_obligations.is_empty() {
            None
        } else {
            let verification_obligations = request.verification_obligations.clone();
            let search_path = self.host_search_path.clone();
            Some(thread::spawn(move || {
                verify_host_obligations_with_search_path(
                    &verification_obligations,
                    search_path.as_deref(),
                )
            }))
        };

        let mut rendered_args = Vec::with_capacity(request.args.len());
        for value in request.args.iter() {
            rendered_args.push(
                self.render_exec_arg(value, &sandbox_root, "exec arg")
                    .await?,
            );
        }

        let mut command = Command::new(&rendered_args[0]);
        command.args(&rendered_args[1..]);
        command.current_dir(sandbox.path());
        for (key, value) in &request.env {
            command.env(
                key,
                self.render_exec_arg(value, &sandbox_root, "exec env value")
                    .await?,
            );
        }

        let output = command.output().map_err(|error| {
            PoisonError::new(
                PoisonKind::UserFail,
                format!("failed to execute {}: {error}", rendered_args[0]),
                self.current_demand_chain(None),
            )
        })?;

        let process = self.exec_process_value(&output);
        let tree = Value::Tree(TreeValue::FsDir(sandbox_root.clone()));
        let observed = self
            .resolve_boundary_value(self.build_call(request.observer, vec![process, tree], None))
            .await;

        if let Some(verification_task) = verification_task {
            match verification_task.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    return Err(PoisonError::new(
                        PoisonKind::VerificationFailure,
                        format!("exec verification failed: {error}"),
                        self.current_demand_chain(None),
                    ));
                }
                Err(_) => {
                    return Err(PoisonError::new(
                        PoisonKind::Internal,
                        "exec verification task panicked",
                        self.current_demand_chain(None),
                    ));
                }
            }
        }

        let observed = observed?;
        self.snapshot_value(observed).await
    }

    pub(super) async fn exec_mounts_from_value(
        &self,
        value: Option<Value>,
    ) -> Result<Vec<(PathValue, Value)>, PoisonError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let (value, _) = self.split_obligations(
            self.materialize_value_shallow(value, "forcing exec mounts")
                .await?,
        );
        let Value::Map(map) = value else {
            return Err(self.type_error(&format!(
                "exec mounts must be Map, got {}",
                value.type_name()
            )));
        };

        let mut mounts = Vec::with_capacity(map.entries.len());
        for (key, value) in map.entries.iter() {
            let key = self
                .materialize_value_deep(key.clone(), "forcing exec mount path")
                .await?;
            let path = self.expect_relative_path(&key, "exec mount path")?;
            let (value, _) = self.split_obligations(
                self.materialize_value_shallow(value.clone(), "forcing exec mount value")
                    .await?,
            );
            match value {
                Value::Blob(_) | Value::Tree(_) => mounts.push((path, value)),
                other => {
                    return Err(self.type_error(&format!(
                        "exec mount values must be Blob or Tree, got {}",
                        other.type_name()
                    )));
                }
            }
        }
        Ok(mounts)
    }

    pub(super) async fn exec_writable_from_value(
        &self,
        value: Option<Value>,
    ) -> Result<Vec<PathValue>, PoisonError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let (value, _) = self.split_obligations(
            self.materialize_value_shallow(value, "forcing exec writable")
                .await?,
        );
        let Value::Array(items) = value else {
            return Err(self.type_error(&format!(
                "exec writable must be Array[Path], got {}",
                value.type_name()
            )));
        };

        let mut writable = Vec::with_capacity(items.len());
        for item in items.iter() {
            let item = self
                .materialize_value_deep(item.clone(), "forcing exec writable path")
                .await?;
            writable.push(self.expect_relative_path(&item, "exec writable path")?);
        }
        Ok(writable)
    }

    pub(super) async fn exec_create_dirs_from_value(
        &self,
        value: Option<Value>,
        writable_roots: &[PathValue],
    ) -> Result<Vec<PathValue>, PoisonError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let (value, _) = self.split_obligations(
            self.materialize_value_shallow(value, "forcing exec create_dirs")
                .await?,
        );
        let Value::Array(items) = value else {
            return Err(self.type_error(&format!(
                "exec create_dirs must be Array[Path], got {}",
                value.type_name()
            )));
        };

        let mut create_dirs = Vec::with_capacity(items.len());
        for item in items.iter() {
            let item = self
                .materialize_value_deep(item.clone(), "forcing exec create_dirs path")
                .await?;
            let path = self.expect_relative_path(&item, "exec create_dirs path")?;
            if !writable_roots.iter().any(|root| {
                path == *root
                    || path
                        .as_str()
                        .starts_with(&(root.as_str().to_string() + "/"))
            }) {
                return Err(self.type_error(&format!(
                    "exec create_dirs path '{}' must be within a writable root",
                    path.as_str()
                )));
            }
            create_dirs.push(path);
        }
        Ok(create_dirs)
    }

    pub(super) async fn infer_exec_writable_roots(
        &self,
        mounts: &[(PathValue, Value)],
        args: &[ExecArgValue],
        env: &[(String, ExecArgValue)],
    ) -> Vec<PathValue> {
        let mut roots = BTreeSet::new();
        let mount_paths = mounts
            .iter()
            .map(|(path, _)| path.as_str().to_string())
            .collect::<Vec<_>>();

        let mut add_parent_paths = |path: &PathValue| {
            if mount_paths.iter().any(|mount| {
                path == &PathValue::new(mount.clone())
                    || path.as_str().starts_with(&(mount.to_string() + "/"))
            }) {
                return;
            }
            let mut current = path.as_str();
            while let Some((parent, _)) = current.rsplit_once('/') {
                if parent.is_empty() {
                    break;
                }
                roots.insert(parent.to_string());
                current = parent;
            }
        };

        for arg in args {
            if let ExecArgValue::Path(path) = arg {
                add_parent_paths(path);
            }
        }

        for (_, value) in env {
            if let ExecArgValue::Path(path) = value {
                add_parent_paths(path);
            }
        }

        roots.into_iter().map(PathValue::new).collect()
    }

    pub(super) async fn exec_env_from_value(
        &self,
        value: Option<Value>,
        mounts: &mut Vec<(PathValue, Value)>,
        writable_roots: &[PathValue],
    ) -> Result<Vec<(String, ExecArgValue)>, PoisonError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let (value, _) = self.split_obligations(
            self.materialize_value_shallow(value, "forcing exec env")
                .await?,
        );
        let Value::Map(map) = value else {
            return Err(
                self.type_error(&format!("exec env must be Map, got {}", value.type_name()))
            );
        };

        let mut env = Vec::with_capacity(map.entries.len());
        for (key, value) in map.entries.iter() {
            let key = self
                .materialize_value_deep(key.clone(), "forcing exec env key")
                .await?;
            let key = match key {
                Value::String(key) => key.to_string(),
                other => {
                    return Err(self.type_error(&format!(
                        "exec env keys must be String, got {}",
                        other.type_name()
                    )));
                }
            };
            let value = self
                .parse_exec_arg(value, mounts, writable_roots, "exec env value")
                .await?;
            env.push((key, value));
        }
        Ok(env)
    }

    pub(super) async fn parse_exec_arg(
        &self,
        value: &Value,
        mounts: &mut Vec<(PathValue, Value)>,
        _writable_roots: &[PathValue],
        context: &str,
    ) -> Result<ExecArgValue, PoisonError> {
        let (value, _) = self.split_obligations(
            self.materialize_value_deep(value.clone(), &format!("forcing {context}"))
                .await?,
        );
        match value {
            Value::String(text) => Ok(ExecArgValue::Literal(text.to_string())),
            Value::Path(path) => {
                let path = self.expect_relative_path(&Value::Path(path), context)?;
                Ok(ExecArgValue::Path(path))
            }
            Value::ExecPath(path) => self.exec_arg_from_exec_path(mounts, &path).await,
            Value::Blob(blob) => Ok(ExecArgValue::Path(
                self.ensure_blob_exec_mount(mounts, &blob).await,
            )),
            Value::Tree(tree) => Ok(ExecArgValue::Path(
                self.ensure_tree_exec_mount(mounts, &tree).await,
            )),
            other => {
                if let Some(text) = self.try_stringify_exec_arg(&other).await? {
                    return Ok(ExecArgValue::Literal(text));
                }
                Err(self.type_error(&format!(
                    "{context} must be String, Path, Blob, Tree, ExecPath, or a value with to_string(), got {}",
                    other.type_name()
                )))
            }
        }
    }

    pub(super) async fn try_stringify_exec_arg(
        &self,
        value: &Value,
    ) -> Result<Option<String>, PoisonError> {
        let Some(method) = self.select_field(value, "to_string").await.ok() else {
            return Ok(None);
        };
        let result = self
            .resolve_boundary_value(self.build_call(method, Vec::new(), None))
            .await?;
        let (result, _) = self.split_obligations(result);
        match result {
            Value::String(text) => Ok(Some(text.to_string())),
            _ => Ok(None),
        }
    }

    pub(super) async fn extend_array_with_spread(
        &self,
        values: &mut Vec<Value>,
        spread_value: Value,
    ) -> Result<(), PoisonError> {
        let (spread_value, obligations) = self.split_obligations(spread_value);
        match spread_value {
            Value::Array(items) => {
                values.extend(
                    items
                        .iter()
                        .cloned()
                        .map(|value| self.attach_obligations(value, obligations.clone())),
                );
                Ok(())
            }
            Value::Tree(tree) => {
                values.extend(
                    self.tree_exec_path_values(&tree)
                        .await?
                        .into_iter()
                        .map(|value| self.attach_obligations(value, obligations.clone())),
                );
                Ok(())
            }
            other => Err(self.type_error(&format!(
                "array spread requires Array or Tree, got {}",
                other.type_name()
            ))),
        }
    }

    pub(super) async fn extend_record_with_spread(
        &self,
        entries: &mut Vec<(Value, Value)>,
        spread_value: Value,
    ) -> Result<(), PoisonError> {
        let (spread_value, obligations) = self.split_obligations(spread_value);
        match spread_value {
            Value::Map(map) => {
                entries.extend(map.entries.iter().cloned().map(|(key, value)| {
                    (
                        self.attach_obligations(key, obligations.clone()),
                        self.attach_obligations(value, obligations.clone()),
                    )
                }));
                Ok(())
            }
            Value::Struct(struct_value) => {
                entries.extend(struct_value.fields.iter().cloned().map(|(name, value)| {
                    (
                        self.attach_obligations(Value::String(name), obligations.clone()),
                        self.attach_obligations(value, obligations.clone()),
                    )
                }));
                Ok(())
            }
            other => Err(self.type_error(&format!(
                "record spread requires Map or Struct, got {}",
                other.type_name()
            ))),
        }
    }

    pub(super) async fn extend_map_with_spread(
        &self,
        entries: &mut Vec<(Value, Value)>,
        spread_value: Value,
    ) -> Result<(), PoisonError> {
        let (spread_value, obligations) = self.split_obligations(spread_value);
        let Value::Map(map) = spread_value else {
            return Err(self.type_error(&format!(
                "map spread requires Map, got {}",
                spread_value.type_name()
            )));
        };

        entries.extend(map.entries.iter().cloned().map(|(key, value)| {
            (
                self.attach_obligations(key, obligations.clone()),
                self.attach_obligations(value, obligations.clone()),
            )
        }));
        Ok(())
    }

    pub(super) fn bind_pattern_into<'a>(
        &'a self,
        pattern: &'a instr::Pattern,
        value: Value,
        bindings: &'a mut HashMap<String, Value>,
    ) -> BoxFuture<'a, Result<(), PoisonError>> {
        Box::pin(async move {
            match pattern {
                instr::Pattern::Binding(name) => {
                    bindings.insert(name.clone(), value);
                    Ok(())
                }
                instr::Pattern::Struct(fields) => {
                    let (value, obligations) =
                        self.split_obligations(self.force_value_shallow(value).await?);
                    match value {
                        Value::Map(map) => {
                            for (name, nested) in fields {
                                let field_value = map
                                    .entries
                                    .iter()
                                    .find_map(|(key, value)| match key {
                                        Value::String(key_name) if key_name.as_ref() == name => {
                                            Some(value.clone())
                                        }
                                        _ => None,
                                    })
                                    .ok_or_else(|| {
                                        self.type_error(&format!(
                                            "record pattern requires field '{name}'"
                                        ))
                                    })?;
                                let field_value = self
                                    .attach_obligations(field_value, obligations.iter().cloned());
                                match nested {
                                    Some(nested) => {
                                        self.bind_pattern_into(nested, field_value, bindings)
                                            .await?
                                    }
                                    None => {
                                        bindings.insert(name.clone(), field_value);
                                    }
                                }
                            }
                            Ok(())
                        }
                        Value::Struct(struct_value) => {
                            for (name, nested) in fields {
                                let field_value = struct_value
                                    .fields
                                    .iter()
                                    .find(|(field_name, _)| field_name.as_ref() == name)
                                    .map(|(_, value)| value.clone())
                                    .ok_or_else(|| {
                                        self.type_error(&format!(
                                            "struct pattern requires field '{name}'"
                                        ))
                                    })?;
                                let field_value = self
                                    .attach_obligations(field_value, obligations.iter().cloned());
                                match nested {
                                    Some(nested) => {
                                        self.bind_pattern_into(nested, field_value, bindings)
                                            .await?
                                    }
                                    None => {
                                        bindings.insert(name.clone(), field_value);
                                    }
                                }
                            }
                            Ok(())
                        }
                        other => Err(self.type_error(&format!(
                            "struct pattern requires Map or Struct, got {}",
                            other.type_name()
                        ))),
                    }
                }
            }
        })
    }

    pub(super) async fn exec_arg_from_exec_path(
        &self,
        mounts: &mut Vec<(PathValue, Value)>,
        path: &ExecPathValue,
    ) -> Result<ExecArgValue, PoisonError> {
        match path {
            ExecPathValue::InTree { tree, suffix } => Ok(ExecArgValue::Path(
                self.ensure_tree_exec_mount(mounts, tree)
                    .await
                    .join(suffix.as_str()),
            )),
            ExecPathValue::InBlob { blob, leaf } => Ok(ExecArgValue::Path(
                self.ensure_exec_mount(
                    mounts,
                    Value::Blob(blob.clone()),
                    Some(leaf.as_str().to_string()),
                )
                .await,
            )),
        }
    }

    pub(super) async fn tree_exec_path_values(
        &self,
        tree: &TreeValue,
    ) -> Result<Vec<Value>, PoisonError> {
        let suffixes = self.tree_leaf_suffixes(tree).await?;
        Ok(suffixes
            .into_iter()
            .map(|suffix| {
                Value::ExecPath(ExecPathValue::InTree {
                    tree: tree.clone(),
                    suffix,
                })
            })
            .collect())
    }

    pub(super) async fn tree_leaf_suffixes(
        &self,
        tree: &TreeValue,
    ) -> Result<Vec<PathValue>, PoisonError> {
        let mut suffixes = Vec::new();
        self.collect_tree_leaf_suffixes(tree, &PathValue::new(""), &mut suffixes)
            .await?;
        Ok(suffixes)
    }

    pub(super) fn collect_tree_leaf_suffixes<'a>(
        &'a self,
        tree: &'a TreeValue,
        prefix: &'a PathValue,
        suffixes: &'a mut Vec<PathValue>,
    ) -> BoxFuture<'a, Result<(), PoisonError>> {
        Box::pin(async move {
            match tree {
                TreeValue::Inline(entries) => {
                    let mut sorted = entries.iter().collect::<Vec<_>>();
                    sorted.sort_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
                    for (name, value) in sorted {
                        let child_path = prefix.join(name.as_ref());
                        let value = self.force_value_shallow(value.clone()).await?;
                        match value {
                            Value::Blob(_) => suffixes.push(child_path),
                            Value::Tree(tree) => {
                                self.collect_tree_leaf_suffixes(&tree, &child_path, suffixes)
                                    .await?
                            }
                            other => {
                                return Err(self.type_error(&format!(
                                    "tree spread entries must be Blob or Tree, got {}",
                                    other.type_name()
                                )));
                            }
                        }
                    }
                    Ok(())
                }
                TreeValue::FsDir(root) => {
                    let mut names = Vec::new();
                    for entry in fs::read_dir(root.as_str()).map_err(|error| {
                        self.internal_error(&format!(
                            "failed to list {} for tree spread: {error}",
                            root.as_str()
                        ))
                    })? {
                        let entry = entry.map_err(|error| {
                            self.internal_error(&format!(
                                "failed to read {} for tree spread: {error}",
                                root.as_str()
                            ))
                        })?;
                        let name = entry.file_name().to_string_lossy().to_string();
                        if name.starts_with("._") {
                            continue;
                        }
                        names.push(name);
                    }
                    names.sort();

                    for name in names {
                        let child_root = root.join(&name);
                        let child_path = prefix.join(&name);
                        let metadata = fs::metadata(child_root.as_str()).map_err(|error| {
                            self.internal_error(&format!(
                                "failed to stat {} for tree spread: {error}",
                                child_root.as_str()
                            ))
                        })?;
                        if metadata.is_dir() {
                            self.collect_tree_leaf_suffixes(
                                &TreeValue::FsDir(child_root),
                                &child_path,
                                suffixes,
                            )
                            .await?;
                        } else {
                            suffixes.push(child_path);
                        }
                    }
                    Ok(())
                }
                TreeValue::Store(_) => {
                    let tree = self
                        .hydrate_store_tree(tree, "spreading store-backed tree")
                        .await?;
                    self.collect_tree_leaf_suffixes(&tree, prefix, suffixes)
                        .await
                }
            }
        })
    }

    // r[impl exec.arg.auto-mount]
    pub(super) async fn ensure_tree_exec_mount(
        &self,
        mounts: &mut Vec<(PathValue, Value)>,
        tree: &TreeValue,
    ) -> PathValue {
        let value = Value::Tree(tree.clone());
        self.ensure_exec_mount(mounts, value, None).await
    }

    // r[impl exec.arg.auto-mount]
    pub(super) async fn ensure_blob_exec_mount(
        &self,
        mounts: &mut Vec<(PathValue, Value)>,
        blob: &BlobValue,
    ) -> PathValue {
        let value = Value::Blob(blob.clone());
        let leaf = self.exec_blob_leaf_name(blob);
        self.ensure_exec_mount(mounts, value, Some(leaf)).await
    }

    // r[impl exec.mount]
    pub(super) async fn ensure_exec_mount(
        &self,
        mounts: &mut Vec<(PathValue, Value)>,
        value: Value,
        leaf: Option<String>,
    ) -> PathValue {
        if let Some((path, _)) = mounts.iter().find(|(_, mounted)| *mounted == value) {
            return path.clone();
        }

        let mount_root = self.next_exec_mount_root(mounts);
        let mount_path = match leaf {
            Some(leaf) => mount_root.join(&leaf),
            None => mount_root,
        };
        mounts.push((mount_path.clone(), value));
        mount_path
    }

    pub(super) fn next_exec_mount_root(&self, mounts: &[(PathValue, Value)]) -> PathValue {
        for index in 0usize.. {
            let candidate = PathValue::new(format!("{IMPLICIT_EXEC_MOUNT_PREFIX}/{index}"));
            if mounts
                .iter()
                .all(|(path, _)| !self.exec_paths_overlap(&candidate, path))
            {
                return candidate;
            }
        }
        unreachable!("implicit exec mount path space is infinite")
    }

    pub(super) fn exec_paths_overlap(&self, left: &PathValue, right: &PathValue) -> bool {
        if left == right {
            return true;
        }
        let left = left.as_str();
        let right = right.as_str();
        right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
            || left
                .strip_prefix(right)
                .is_some_and(|suffix| suffix.starts_with('/'))
    }
}
