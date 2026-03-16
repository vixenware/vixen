use super::*;

impl Engine {
    pub(super) fn exec_blob_leaf_name(&self, blob: &BlobValue) -> String {
        match blob {
            BlobValue::FsFile { path, .. } => Path::new(path.as_str())
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| self.synthetic_blob_leaf(blob)),
            BlobValue::Inline { .. } | BlobValue::Store { .. } => self.synthetic_blob_leaf(blob),
        }
    }

    pub(super) fn synthetic_blob_leaf(&self, blob: &BlobValue) -> String {
        let digest = match blob {
            BlobValue::Inline { bytes, executable } => {
                let mut keyed = Vec::with_capacity(bytes.len() + 1);
                keyed.push(u8::from(*executable));
                keyed.extend_from_slice(bytes);
                blake3::hash(&keyed).to_hex().to_string()
            }
            BlobValue::FsFile { path, executable } => {
                blake3::hash(format!("{executable}:{}", path.as_str()).as_bytes())
                    .to_hex()
                    .to_string()
            }
            BlobValue::Store {
                blob_ref,
                executable,
            } => blake3::hash(format!("{executable}:{blob_ref:?}").as_bytes())
                .to_hex()
                .to_string(),
        };
        format!("blob-{}", &digest[..12])
    }

    pub(super) fn render_exec_arg(
        &self,
        value: &ExecArgValue,
        sandbox_root: &PathValue,
        _context: &str,
    ) -> Result<String, PoisonError> {
        match value {
            ExecArgValue::Literal(text) => Ok(text.clone()),
            ExecArgValue::Path(path) => Ok(sandbox_root.join(path.as_str()).as_str().to_string()),
        }
    }

    pub(super) fn materialize_mount_value(
        &self,
        sandbox_root: &PathValue,
        mount_path: &PathValue,
        value: &Value,
    ) -> Result<(), PoisonError> {
        let destination = sandbox_root.join(mount_path.as_str());
        self.materialize_value_to_fs(value, &destination)
    }

    pub(super) fn materialize_value_to_fs(
        &self,
        value: &Value,
        destination: &PathValue,
    ) -> Result<(), PoisonError> {
        let value = self.force_value_shallow(value.clone())?;
        match value {
            Value::Blob(blob) => self.write_blob_to_path(&blob, destination),
            Value::Tree(tree) => self.write_tree_to_path(&tree, destination),
            Value::ExecPath(_) => {
                Err(self.type_error("cannot materialize ExecPath into exec sandbox"))
            }
            other => Err(self.type_error(&format!(
                "cannot materialize {} into exec sandbox",
                other.type_name()
            ))),
        }
    }

    pub(super) fn write_blob_to_path(
        &self,
        blob: &BlobValue,
        destination: &PathValue,
    ) -> Result<(), PoisonError> {
        if let Some(parent) = Path::new(destination.as_str()).parent() {
            fs::create_dir_all(parent).map_err(|error| {
                self.internal_error(&format!("failed to create {}: {error}", parent.display()))
            })?;
        }

        match blob {
            BlobValue::Inline { bytes, executable } => {
                fs::write(destination.as_str(), bytes.as_ref()).map_err(|error| {
                    self.internal_error(&format!(
                        "failed to write {}: {error}",
                        destination.as_str()
                    ))
                })?;
                set_executable_if_needed(destination.as_str(), *executable).map_err(|error| {
                    self.internal_error(&format!(
                        "failed to set executable bit on {}: {error}",
                        destination.as_str()
                    ))
                })
            }
            BlobValue::FsFile { path, executable } => {
                fs::copy(path.as_str(), destination.as_str()).map_err(|error| {
                    self.internal_error(&format!(
                        "failed to copy {} to {}: {error}",
                        path.as_str(),
                        destination.as_str()
                    ))
                })?;
                set_executable_if_needed(destination.as_str(), *executable).map_err(|error| {
                    self.internal_error(&format!(
                        "failed to set executable bit on {}: {error}",
                        destination.as_str()
                    ))
                })
            }
            BlobValue::Store { .. } => {
                let blob = self.hydrate_store_blob(blob, "materializing store-backed blob")?;
                self.write_blob_to_path(&blob, destination)
            }
        }
    }

    pub(super) fn write_tree_to_path(
        &self,
        tree: &TreeValue,
        destination: &PathValue,
    ) -> Result<(), PoisonError> {
        match tree {
            TreeValue::Inline(entries) => {
                self.ensure_dir(destination)?;
                for (name, value) in entries.iter() {
                    self.materialize_value_to_fs(value, &destination.join(name.as_ref()))?;
                }
                Ok(())
            }
            TreeValue::FsDir(path) => self.copy_dir_recursive(path, destination),
            TreeValue::Store(_) => {
                let tree = self.hydrate_store_tree(tree, "materializing store-backed tree")?;
                self.write_tree_to_path(&tree, destination)
            }
        }
    }

    pub(super) fn copy_dir_recursive(
        &self,
        source: &PathValue,
        destination: &PathValue,
    ) -> Result<(), PoisonError> {
        self.ensure_dir(destination)?;
        let mut entries = fs::read_dir(source.as_str()).map_err(|error| {
            self.internal_error(&format!("failed to read {}: {error}", source.as_str()))
        })?;
        let mut names = Vec::new();
        while let Some(entry) = entries.next().transpose().map_err(|error| {
            self.internal_error(&format!("failed to read {}: {error}", source.as_str()))
        })? {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("._") {
                continue;
            }
            names.push(name);
        }
        names.sort();

        for name in names {
            let source_path = source.join(&name);
            let destination_path = destination.join(&name);
            let metadata = fs::metadata(source_path.as_str()).map_err(|error| {
                self.internal_error(&format!("failed to stat {}: {error}", source_path.as_str()))
            })?;
            if metadata.is_dir() {
                self.copy_dir_recursive(&source_path, &destination_path)?;
            } else {
                self.write_blob_to_path(
                    &BlobValue::FsFile {
                        path: source_path.clone(),
                        executable: host_path_is_executable(Path::new(source_path.as_str())),
                    },
                    &destination_path,
                )?;
            }
        }
        Ok(())
    }

    pub(super) fn ensure_dir(&self, path: &PathValue) -> Result<(), PoisonError> {
        fs::create_dir_all(path.as_str()).map_err(|error| {
            self.internal_error(&format!("failed to create {}: {error}", path.as_str()))
        })
    }

    pub(super) fn expect_host_path(
        &self,
        value: &Value,
        context: &str,
    ) -> Result<String, PoisonError> {
        let (value, _) = self.split_obligations_ref(value);
        match value {
            Value::String(path) => Ok(path.to_string()),
            other => Err(self.type_error(&format!(
                "{context} must be String, got {}",
                other.type_name()
            ))),
        }
    }

    pub(super) fn expect_executor_selector(
        &self,
        value: &Value,
        context: &str,
    ) -> Result<ExecutorSelector, PoisonError> {
        let (value, _) = self.split_obligations_ref(value);
        match value {
            Value::Executor(selector) => Ok(selector.clone()),
            Value::String(label) => Ok(ExecutorSelector::class(label.as_ref())),
            other => Err(self.type_error(&format!(
                "{context} must be ExecutorClass, got {}",
                other.type_name()
            ))),
        }
    }

    pub(super) fn host_read(&self, path: &str) -> Result<Value, PoisonError> {
        let metadata = fs::metadata(path).map_err(|error| {
            PoisonError::new(
                PoisonKind::UserFail,
                format!("failed to read host path {path}: {error}"),
                self.current_demand_chain(None),
            )
        })?;
        if metadata.is_dir() {
            return Err(PoisonError::new(
                PoisonKind::UserFail,
                format!("host.read expects a file, got directory {path}"),
                self.current_demand_chain(None),
            ));
        }
        let content_hash = fs::read(path)
            .map(|bytes| blake3::hash(&bytes).to_hex().to_string())
            .map_err(|error| {
                PoisonError::new(
                    PoisonKind::UserFail,
                    format!("failed to read host path {path}: {error}"),
                    self.current_demand_chain(None),
                )
            })?;
        let observation = HostObservation::Read {
            path: path.to_string(),
            content_hash,
        };
        Ok(self.attach_obligations(
            Value::Blob(BlobValue::FsFile {
                path: PathValue::new(path),
                executable: host_path_is_executable(Path::new(path)),
            }),
            [observation],
        ))
    }

    pub(super) fn host_hash(&self, path: &str) -> Result<Value, PoisonError> {
        let bytes = fs::read(path).map_err(|error| {
            PoisonError::new(
                PoisonKind::UserFail,
                format!("failed to hash host path {path}: {error}"),
                self.current_demand_chain(None),
            )
        })?;
        let content_hash = blake3::hash(&bytes).to_hex().to_string();
        let observation = HostObservation::Hash {
            path: path.to_string(),
            content_hash: content_hash.clone(),
        };
        Ok(self.attach_obligations(
            Value::Blob(BlobValue::from_string(content_hash)),
            [observation],
        ))
    }

    pub(super) fn host_which(&self, name: &str) -> Result<Value, PoisonError> {
        if host_lookup_candidate(name).is_some_and(host_path_is_executable) {
            let observation = HostObservation::Which {
                name: name.to_string(),
                expected_path: Some(name.to_string()),
            };
            return Ok(self.attach_obligations(Value::String(Arc::from(name)), [observation]));
        }

        let Some(path) = env::var_os("PATH") else {
            let observation = HostObservation::Which {
                name: name.to_string(),
                expected_path: None,
            };
            return Err(PoisonError::new(
                PoisonKind::UserFail,
                format!("host.which could not find {name}"),
                self.current_demand_chain(None),
            )
            .with_observations([observation]));
        };
        for dir in env::split_paths(&path) {
            let candidate = dir.join(name);
            if host_path_is_executable(&candidate) {
                let candidate = candidate.to_string_lossy().to_string();
                let observation = HostObservation::Which {
                    name: name.to_string(),
                    expected_path: Some(candidate.clone()),
                };
                return Ok(
                    self.attach_obligations(Value::String(Arc::from(candidate)), [observation])
                );
            }
        }
        let observation = HostObservation::Which {
            name: name.to_string(),
            expected_path: None,
        };
        Err(PoisonError::new(
            PoisonKind::UserFail,
            format!("host.which could not find {name}"),
            self.current_demand_chain(None),
        )
        .with_observations([observation]))
    }

    pub(super) fn host_os(&self) -> Value {
        let os = canonical_host_os();
        self.attach_obligations(
            Value::String(Arc::from(os.clone())),
            [HostObservation::Os { value: os }],
        )
    }

    pub(super) fn host_arch(&self) -> Value {
        let arch = env::consts::ARCH.to_string();
        self.attach_obligations(
            Value::String(Arc::from(arch.clone())),
            [HostObservation::Arch { value: arch }],
        )
    }

    pub(super) fn exec_process_value(&self, output: &std::process::Output) -> Value {
        let exit_code = output.status.code().unwrap_or(1).max(0) as u64;
        Value::Struct(StructValue {
            type_name: Arc::from("Process"),
            fields: Arc::from([
                (Arc::from("exit_code"), Value::U64(exit_code)),
                (
                    Arc::from("stdout"),
                    Value::Bytes(Arc::from(output.stdout.clone())),
                ),
                (
                    Arc::from("stderr"),
                    Value::Bytes(Arc::from(output.stderr.clone())),
                ),
            ]),
        })
    }

    pub(super) fn snapshot_value(&self, value: Value) -> Result<Value, PoisonError> {
        let value = self.force_value_shallow(value)?;
        match value {
            Value::Pending(node_id) => {
                self.snapshot_value(self.force_value(Value::Pending(node_id))?)
            }
            Value::Host(_) => {
                Err(self
                    .internal_error("Host values cannot cross the local exec observer boundary"))
            }
            Value::Discovered(discovered) => {
                let value = self.snapshot_value(*discovered.value)?;
                Ok(self.attach_obligations(value, discovered.obligations.iter().cloned()))
            }
            Value::ExecPath(_) => Err(self
                .internal_error("ExecPath values cannot cross the local exec observer boundary")),
            Value::Blob(BlobValue::Inline { .. })
            | Value::Blob(BlobValue::Store { .. })
            | Value::Bytes(_)
            | Value::Path(_)
            | Value::Executor(_)
            | Value::U64(_)
            | Value::I64(_)
            | Value::F64(_)
            | Value::Bool(_)
            | Value::String(_)
            | Value::Unit
            | Value::Function(_)
            | Value::Partial(_)
            | Value::Builtin(_) => Ok(value),
            Value::Blob(BlobValue::FsFile { path, executable }) => {
                let bytes = fs::read(path.as_str()).map_err(|error| {
                    self.internal_error(&format!("failed to read {}: {error}", path.as_str()))
                })?;
                Ok(Value::Blob(BlobValue::Inline {
                    bytes: Arc::from(bytes),
                    executable,
                }))
            }
            Value::Tree(TreeValue::Inline(entries)) => {
                let mut snapshotted = Vec::with_capacity(entries.len());
                for (name, value) in entries.iter() {
                    snapshotted.push((name.clone(), self.snapshot_value(value.clone())?));
                }
                Ok(Value::Tree(TreeValue::Inline(Arc::from(snapshotted))))
            }
            Value::Tree(TreeValue::FsDir(path)) => self.snapshot_fs_tree(&path),
            Value::Tree(TreeValue::Store(root)) => Ok(Value::Tree(TreeValue::Store(root))),
            Value::Tuple(values) => {
                let mut snapshotted = Vec::with_capacity(values.len());
                for value in values.iter() {
                    snapshotted.push(self.snapshot_value(value.clone())?);
                }
                Ok(Value::Tuple(Arc::from(snapshotted)))
            }
            Value::Array(values) => {
                let mut snapshotted = Vec::with_capacity(values.len());
                for value in values.iter() {
                    snapshotted.push(self.snapshot_value(value.clone())?);
                }
                Ok(Value::Array(Arc::from(snapshotted)))
            }
            Value::Map(map) => {
                let mut snapshotted = Vec::with_capacity(map.entries.len());
                for (key, value) in map.entries.iter() {
                    snapshotted.push((
                        self.snapshot_value(key.clone())?,
                        self.snapshot_value(value.clone())?,
                    ));
                }
                Ok(Value::Map(MapValue {
                    entries: Arc::from(snapshotted),
                }))
            }
            Value::Struct(struct_value) => {
                let mut fields = Vec::with_capacity(struct_value.fields.len());
                for (name, value) in struct_value.fields.iter() {
                    fields.push((name.clone(), self.snapshot_value(value.clone())?));
                }
                Ok(Value::Struct(StructValue {
                    type_name: struct_value.type_name,
                    fields: Arc::from(fields),
                }))
            }
        }
    }

    pub(super) fn snapshot_exec_mount_value(&self, value: Value) -> Result<Value, PoisonError> {
        let value = self.force_value_shallow(value)?;
        match value {
            Value::Pending(node_id) => {
                self.snapshot_exec_mount_value(self.force_value(Value::Pending(node_id))?)
            }
            Value::Host(_) => {
                Err(self.internal_error("Host values cannot cross the local exec boundary"))
            }
            Value::Discovered(discovered) => {
                let value = self.snapshot_exec_mount_value(*discovered.value)?;
                Ok(self.attach_obligations(value, discovered.obligations.iter().cloned()))
            }
            Value::ExecPath(_) => {
                Err(self.internal_error("ExecPath values cannot cross the local exec boundary"))
            }
            Value::Blob(BlobValue::Inline { .. })
            | Value::Blob(BlobValue::FsFile { .. })
            | Value::Blob(BlobValue::Store { .. })
            | Value::Bytes(_)
            | Value::Path(_)
            | Value::Executor(_)
            | Value::U64(_)
            | Value::I64(_)
            | Value::F64(_)
            | Value::Bool(_)
            | Value::String(_)
            | Value::Unit
            | Value::Function(_)
            | Value::Partial(_)
            | Value::Builtin(_) => Ok(value),
            Value::Tree(TreeValue::Inline(entries)) => {
                let mut snapshotted = Vec::with_capacity(entries.len());
                for (name, value) in entries.iter() {
                    snapshotted
                        .push((name.clone(), self.snapshot_exec_mount_value(value.clone())?));
                }
                Ok(Value::Tree(TreeValue::Inline(Arc::from(snapshotted))))
            }
            Value::Tree(TreeValue::FsDir(path)) => Ok(Value::Tree(TreeValue::FsDir(path))),
            Value::Tree(TreeValue::Store(root)) => Ok(Value::Tree(TreeValue::Store(root))),
            Value::Tuple(values) => {
                let mut snapshotted = Vec::with_capacity(values.len());
                for value in values.iter() {
                    snapshotted.push(self.snapshot_exec_mount_value(value.clone())?);
                }
                Ok(Value::Tuple(Arc::from(snapshotted)))
            }
            Value::Array(values) => {
                let mut snapshotted = Vec::with_capacity(values.len());
                for value in values.iter() {
                    snapshotted.push(self.snapshot_exec_mount_value(value.clone())?);
                }
                Ok(Value::Array(Arc::from(snapshotted)))
            }
            Value::Map(map) => {
                let mut snapshotted = Vec::with_capacity(map.entries.len());
                for (key, value) in map.entries.iter() {
                    snapshotted.push((
                        self.snapshot_exec_mount_value(key.clone())?,
                        self.snapshot_exec_mount_value(value.clone())?,
                    ));
                }
                Ok(Value::Map(MapValue {
                    entries: Arc::from(snapshotted),
                }))
            }
            Value::Struct(struct_value) => {
                let mut fields = Vec::with_capacity(struct_value.fields.len());
                for (name, value) in struct_value.fields.iter() {
                    fields.push((name.clone(), self.snapshot_exec_mount_value(value.clone())?));
                }
                Ok(Value::Struct(StructValue {
                    type_name: struct_value.type_name,
                    fields: Arc::from(fields),
                }))
            }
        }
    }

    pub(super) fn snapshot_fs_tree(&self, root: &PathValue) -> Result<Value, PoisonError> {
        let mut read_dir = fs::read_dir(root.as_str()).map_err(|error| {
            self.internal_error(&format!("failed to read {}: {error}", root.as_str()))
        })?;
        let mut names = Vec::new();
        while let Some(entry) = read_dir.next().transpose().map_err(|error| {
            self.internal_error(&format!("failed to read {}: {error}", root.as_str()))
        })? {
            let name = entry.file_name().to_string_lossy().to_string();
            // Skip macOS AppleDouble resource fork files
            if name.starts_with("._") {
                continue;
            }
            names.push(name);
        }
        names.sort();

        let mut entries = Vec::with_capacity(names.len());
        for name in names {
            let child = root.join(&name);
            let metadata = fs::metadata(child.as_str()).map_err(|error| {
                self.internal_error(&format!("failed to stat {}: {error}", child.as_str()))
            })?;
            let value = if metadata.is_dir() {
                self.snapshot_fs_tree(&child)?
            } else {
                let bytes = fs::read(child.as_str()).map_err(|error| {
                    self.internal_error(&format!("failed to read {}: {error}", child.as_str()))
                })?;
                Value::Blob(BlobValue::Inline {
                    bytes: Arc::from(bytes),
                    executable: host_path_is_executable(Path::new(child.as_str())),
                })
            };
            entries.push((Arc::from(name), value));
        }

        Ok(Value::Tree(TreeValue::Inline(Arc::from(entries))))
    }

    // r[impl store.tree.file]
    pub(super) fn tree_file(
        &self,
        tree: &TreeValue,
        path: PathValue,
    ) -> Result<Value, PoisonError> {
        match tree {
            TreeValue::Inline(entries) => self.tree_file_inline(entries, &path),
            TreeValue::FsDir(root) => {
                let full_path = root.join(path.as_str());
                let metadata = fs::metadata(full_path.as_str()).map_err(|error| {
                    PoisonError::new(
                        PoisonKind::UserFail,
                        format!("missing file {}: {error}", full_path.as_str()),
                        self.current_demand_chain(None),
                    )
                })?;
                if metadata.is_dir() {
                    return Err(PoisonError::new(
                        PoisonKind::UserFail,
                        format!("{} is a directory", full_path.as_str()),
                        self.current_demand_chain(None),
                    ));
                }
                Ok(Value::Blob(BlobValue::FsFile {
                    executable: host_path_is_executable(Path::new(full_path.as_str())),
                    path: full_path,
                }))
            }
            TreeValue::Store(_) => {
                let tree = self.hydrate_store_tree(tree, "reading store-backed tree file")?;
                self.tree_file(&tree, path)
            }
        }
    }

    pub(super) fn tree_file_inline(
        &self,
        entries: &[(Arc<str>, Value)],
        path: &PathValue,
    ) -> Result<Value, PoisonError> {
        let segments = path
            .as_str()
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != ".")
            .collect::<Vec<_>>();
        if segments.is_empty() {
            return Err(PoisonError::new(
                PoisonKind::UserFail,
                "tree.file path must not be empty",
                self.current_demand_chain(None),
            ));
        }
        self.tree_file_inline_segments(entries, &segments, path)
    }

    pub(super) fn tree_subtree(
        &self,
        tree: &TreeValue,
        path: PathValue,
    ) -> Result<Value, PoisonError> {
        match tree {
            TreeValue::Inline(entries) => self.tree_subtree_inline(entries, &path),
            TreeValue::FsDir(root) => {
                if path.as_str().is_empty() {
                    return Ok(Value::Tree(TreeValue::FsDir(root.clone())));
                }
                let full_path = root.join(path.as_str());
                let metadata = fs::metadata(full_path.as_str()).map_err(|error| {
                    PoisonError::new(
                        PoisonKind::UserFail,
                        format!("missing subtree {}: {error}", full_path.as_str()),
                        self.current_demand_chain(None),
                    )
                })?;
                if !metadata.is_dir() {
                    return Err(PoisonError::new(
                        PoisonKind::UserFail,
                        format!("{} is not a directory", full_path.as_str()),
                        self.current_demand_chain(None),
                    ));
                }
                Ok(Value::Tree(TreeValue::FsDir(full_path)))
            }
            TreeValue::Store(_) => {
                let tree = self.hydrate_store_tree(tree, "reading store-backed subtree")?;
                self.tree_subtree(&tree, path)
            }
        }
    }

    pub(super) fn tree_subtree_inline(
        &self,
        entries: &[(Arc<str>, Value)],
        path: &PathValue,
    ) -> Result<Value, PoisonError> {
        let segments = path
            .as_str()
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != ".")
            .collect::<Vec<_>>();
        if segments.is_empty() {
            return Ok(Value::Tree(TreeValue::Inline(Arc::from(entries.to_vec()))));
        }
        self.tree_subtree_inline_segments(entries, &segments, path)
    }

    pub(super) fn tree_subtree_inline_segments(
        &self,
        entries: &[(Arc<str>, Value)],
        segments: &[&str],
        original_path: &PathValue,
    ) -> Result<Value, PoisonError> {
        let segment = segments[0];
        let (_, value) = entries
            .iter()
            .find(|(name, _)| name.as_ref() == segment)
            .ok_or_else(|| {
                PoisonError::new(
                    PoisonKind::UserFail,
                    format!("missing subtree {}", original_path.as_str()),
                    self.current_demand_chain(None),
                )
            })?;
        let value = self.force_value_shallow(value.clone())?;

        if segments.len() == 1 {
            return match value {
                Value::Tree(tree) => Ok(Value::Tree(tree)),
                Value::Blob(_) => Err(PoisonError::new(
                    PoisonKind::UserFail,
                    format!("{} is not a directory", original_path.as_str()),
                    self.current_demand_chain(None),
                )),
                other => Err(self.type_error(&format!(
                    "tree entry must be Blob or Tree, got {}",
                    other.type_name()
                ))),
            };
        }

        match value {
            Value::Tree(TreeValue::Inline(entries)) => {
                self.tree_subtree_inline_segments(entries.as_ref(), &segments[1..], original_path)
            }
            Value::Tree(TreeValue::FsDir(root)) => self.tree_subtree(
                &TreeValue::FsDir(root),
                PathValue::new(segments[1..].join("/")),
            ),
            Value::Tree(TreeValue::Store(root)) => self.tree_subtree(
                &TreeValue::Store(root),
                PathValue::new(segments[1..].join("/")),
            ),
            Value::Blob(_) => Err(PoisonError::new(
                PoisonKind::UserFail,
                format!("{} points inside a file", original_path.as_str()),
                self.current_demand_chain(None),
            )),
            other => Err(self.type_error(&format!(
                "tree entry must be Blob or Tree, got {}",
                other.type_name()
            ))),
        }
    }

    pub(super) fn tree_merge(
        &self,
        left: &TreeValue,
        right: &TreeValue,
    ) -> Result<Value, PoisonError> {
        let mut merged = BTreeMap::<Arc<str>, Value>::new();
        for (name, value) in self.tree_entries(left)? {
            merged.insert(name, value);
        }
        for (name, value) in self.tree_entries(right)? {
            match (merged.get(&name).cloned(), &value) {
                (Some(Value::Tree(left_tree)), Value::Tree(right_tree)) => {
                    merged.insert(name, self.tree_merge(&left_tree, right_tree)?);
                }
                _ => {
                    merged.insert(name, value);
                }
            }
        }
        Ok(Value::Tree(TreeValue::Inline(Arc::from(
            merged.into_iter().collect::<Vec<_>>(),
        ))))
    }

    pub(super) fn tree_entries(
        &self,
        tree: &TreeValue,
    ) -> Result<Vec<(Arc<str>, Value)>, PoisonError> {
        match tree {
            TreeValue::Inline(entries) => entries
                .iter()
                .map(|(name, value)| Ok((name.clone(), self.force_value_shallow(value.clone())?)))
                .collect(),
            TreeValue::FsDir(root) => {
                let mut names = Vec::new();
                for entry in fs::read_dir(root.as_str()).map_err(|error| {
                    self.internal_error(&format!(
                        "failed to list {} for tree merge: {error}",
                        root.as_str()
                    ))
                })? {
                    let entry = entry.map_err(|error| {
                        self.internal_error(&format!(
                            "failed to read {} for tree merge: {error}",
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

                let mut entries = Vec::with_capacity(names.len());
                for name in names {
                    let child = root.join(&name);
                    let metadata = fs::metadata(child.as_str()).map_err(|error| {
                        self.internal_error(&format!(
                            "failed to stat {} for tree merge: {error}",
                            child.as_str()
                        ))
                    })?;
                    let value = if metadata.is_dir() {
                        Value::Tree(TreeValue::FsDir(child))
                    } else {
                        Value::Blob(BlobValue::FsFile {
                            executable: host_path_is_executable(Path::new(child.as_str())),
                            path: child,
                        })
                    };
                    entries.push((Arc::from(name), value));
                }
                Ok(entries)
            }
            TreeValue::Store(_) => {
                let tree = self.hydrate_store_tree(tree, "merging store-backed tree")?;
                self.tree_entries(&tree)
            }
        }
    }

    pub(super) fn tree_file_inline_segments(
        &self,
        entries: &[(Arc<str>, Value)],
        segments: &[&str],
        original_path: &PathValue,
    ) -> Result<Value, PoisonError> {
        let segment = segments[0];
        let (_, value) = entries
            .iter()
            .find(|(name, _)| name.as_ref() == segment)
            .ok_or_else(|| {
                PoisonError::new(
                    PoisonKind::UserFail,
                    format!("missing file {}", original_path.as_str()),
                    self.current_demand_chain(None),
                )
            })?;
        let value = self.force_value(value.clone())?;

        if segments.len() == 1 {
            return match value {
                Value::Blob(blob) => Ok(Value::Blob(blob)),
                Value::Tree(_) => Err(PoisonError::new(
                    PoisonKind::UserFail,
                    format!("{} points to a directory", original_path.as_str()),
                    self.current_demand_chain(None),
                )),
                other => Err(self.type_error(&format!(
                    "tree entry must be Blob or Tree, got {}",
                    other.type_name()
                ))),
            };
        }

        match value {
            Value::Tree(TreeValue::Inline(entries)) => {
                self.tree_file_inline_segments(entries.as_ref(), &segments[1..], original_path)
            }
            Value::Tree(TreeValue::FsDir(root)) => self.tree_file(
                &TreeValue::FsDir(root),
                PathValue::new(segments[1..].join("/")),
            ),
            Value::Tree(TreeValue::Store(root)) => self.tree_file(
                &TreeValue::Store(root),
                PathValue::new(segments[1..].join("/")),
            ),
            Value::Blob(_) => Err(PoisonError::new(
                PoisonKind::UserFail,
                format!("{} points inside a file", original_path.as_str()),
                self.current_demand_chain(None),
            )),
            other => Err(self.type_error(&format!(
                "tree entry must be Blob or Tree, got {}",
                other.type_name()
            ))),
        }
    }

    pub(super) fn validate_tree_entries(&self, values: &[Value]) -> Result<(), PoisonError> {
        for value in values {
            match value {
                Value::Blob(_) | Value::Tree(_) | Value::Pending(_) => {}
                other => {
                    return Err(self.type_error(&format!(
                        "tree entries must be Blob or Tree, got {}",
                        other.type_name()
                    )));
                }
            }
        }
        Ok(())
    }
}
