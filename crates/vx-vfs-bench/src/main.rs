#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("vx-vfs-bench currently supports macOS only");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
mod macos_bench {
    use camino::Utf8PathBuf;
    use std::collections::HashMap;
    use std::fs::File;
    use std::os::unix::fs::{FileExt, PermissionsExt};
    use std::path::Path;
    use std::process::Command;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    use vixenfs_proto::{
        CreateResult, DirEntry, GetAttributesResult, ItemAttributes, ItemId, ItemType,
        LookupResult, ReadDirResult, ReadResult, SetAttributesParams, Vfs, VfsResult, WriteResult,
        errno, mode,
    };
    use vx_vfs::{VfsConfig, VfsStart};

    const ROOT_ID: ItemId = 1;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum BenchMode {
        RandomRead,
        Exec,
        ToolchainBuild,
    }

    impl BenchMode {
        fn as_str(self) -> &'static str {
            match self {
                Self::RandomRead => "random-read",
                Self::Exec => "exec",
                Self::ToolchainBuild => "toolchain-build",
            }
        }
    }

    fn parse_mode(value: &str) -> Result<BenchMode, String> {
        match value {
            "random-read" => Ok(BenchMode::RandomRead),
            "exec" => Ok(BenchMode::Exec),
            "toolchain-build" => Ok(BenchMode::ToolchainBuild),
            _ => Err(format!(
                "Invalid --mode value: {value} (expected random-read, exec, toolchain-build)"
            )),
        }
    }

    #[derive(Debug, Clone)]
    struct BenchConfig {
        mode: BenchMode,
        mount_path: Utf8PathBuf,
        reads: u64,
        min_read_bytes: u64,
        max_read_bytes: u64,
        files_mb: Vec<u64>,
        seed: u64,
        exec_host_file: Utf8PathBuf,
        exec_mounted_name: String,
        exec_args: Vec<String>,
        exec_repeat: u32,
        toolchain_root: Utf8PathBuf,
        build_workspace: Utf8PathBuf,
        build_repeat: u32,
        build_clean_between_runs: bool,
        json_out: Option<Utf8PathBuf>,
    }

    impl BenchConfig {
        fn parse() -> Result<Self, String> {
            let pid = std::process::id();
            let mut config = Self {
                mode: BenchMode::RandomRead,
                mount_path: Utf8PathBuf::from(format!("/tmp/vx-vfs-bench-{pid}")),
                reads: 10_000,
                min_read_bytes: 4 * 1024,
                max_read_bytes: 256 * 1024,
                files_mb: vec![180, 200],
                seed: 0x5EED_CAFE_u64,
                exec_host_file: Utf8PathBuf::from(
                    "/Users/amos/.rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc",
                ),
                exec_mounted_name: "rustc".to_string(),
                exec_args: vec!["--version".to_string()],
                exec_repeat: 1,
                toolchain_root: Utf8PathBuf::from(
                    "/Users/amos/.rustup/toolchains/stable-aarch64-apple-darwin",
                ),
                build_workspace: Utf8PathBuf::from(format!("/tmp/vx-vfs-bench-hello-{pid}")),
                build_repeat: 2,
                build_clean_between_runs: true,
                json_out: None,
            };

            let mut args = std::env::args().skip(1);
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--mode" => {
                        let value = next_value(&mut args, "--mode")?;
                        config.mode = parse_mode(&value)?;
                    }
                    "--mount-path" => {
                        let value = next_value(&mut args, "--mount-path")?;
                        config.mount_path = Utf8PathBuf::from(value);
                    }
                    "--reads" => {
                        let value = next_value(&mut args, "--reads")?;
                        config.reads = parse_u64("--reads", &value)?;
                    }
                    "--min-read-bytes" => {
                        let value = next_value(&mut args, "--min-read-bytes")?;
                        config.min_read_bytes = parse_u64("--min-read-bytes", &value)?;
                    }
                    "--max-read-bytes" => {
                        let value = next_value(&mut args, "--max-read-bytes")?;
                        config.max_read_bytes = parse_u64("--max-read-bytes", &value)?;
                    }
                    "--files-mb" => {
                        let value = next_value(&mut args, "--files-mb")?;
                        config.files_mb = parse_files_mb(&value)?;
                    }
                    "--seed" => {
                        let value = next_value(&mut args, "--seed")?;
                        config.seed = parse_u64("--seed", &value)?;
                    }
                    "--exec-host-file" => {
                        let value = next_value(&mut args, "--exec-host-file")?;
                        config.exec_host_file = Utf8PathBuf::from(value);
                    }
                    "--exec-mounted-name" => {
                        let value = next_value(&mut args, "--exec-mounted-name")?;
                        if value.contains('/') {
                            return Err(
                                "--exec-mounted-name must be a single file name".to_string()
                            );
                        }
                        config.exec_mounted_name = value;
                    }
                    "--exec-arg" => {
                        let value = next_value(&mut args, "--exec-arg")?;
                        config.exec_args.push(value);
                    }
                    "--exec-repeat" => {
                        let value = next_value(&mut args, "--exec-repeat")?;
                        config.exec_repeat = parse_u32("--exec-repeat", &value)?;
                    }
                    "--toolchain-root" => {
                        let value = next_value(&mut args, "--toolchain-root")?;
                        config.toolchain_root = Utf8PathBuf::from(value);
                    }
                    "--build-workspace" => {
                        let value = next_value(&mut args, "--build-workspace")?;
                        config.build_workspace = Utf8PathBuf::from(value);
                    }
                    "--build-repeat" => {
                        let value = next_value(&mut args, "--build-repeat")?;
                        config.build_repeat = parse_u32("--build-repeat", &value)?;
                    }
                    "--no-build-clean-between-runs" => {
                        config.build_clean_between_runs = false;
                    }
                    "--json-out" => {
                        let value = next_value(&mut args, "--json-out")?;
                        config.json_out = Some(Utf8PathBuf::from(value));
                    }
                    "--help" | "-h" => {
                        print_usage();
                        std::process::exit(0);
                    }
                    _ => return Err(format!("Unknown argument: {flag}")),
                }
            }

            if config.exec_args.is_empty() {
                config.exec_args.push("--version".to_string());
            }

            match config.mode {
                BenchMode::RandomRead => {
                    if config.reads == 0 {
                        return Err("--reads must be > 0".to_string());
                    }
                    if config.min_read_bytes == 0 {
                        return Err("--min-read-bytes must be > 0".to_string());
                    }
                    if config.max_read_bytes < config.min_read_bytes {
                        return Err("--max-read-bytes must be >= --min-read-bytes".to_string());
                    }
                    if config.files_mb.is_empty() {
                        return Err("--files-mb cannot be empty".to_string());
                    }
                }
                BenchMode::Exec => {
                    if config.exec_repeat == 0 {
                        return Err("--exec-repeat must be > 0".to_string());
                    }
                }
                BenchMode::ToolchainBuild => {
                    if config.build_repeat == 0 {
                        return Err("--build-repeat must be > 0".to_string());
                    }
                }
            }

            Ok(config)
        }
    }

    fn print_usage() {
        println!(
            "Usage: vx-vfs-bench [OPTIONS]\n\
             \n\
             Options:\n\
             --mode <random-read|exec|toolchain-build>\n\
             --mount-path <path>\n\
             --reads <n>\n\
             --min-read-bytes <n>\n\
             --max-read-bytes <n>\n\
             --files-mb <a,b,c>\n\
             --seed <n>\n\
             --exec-host-file <path>\n\
             --exec-mounted-name <name>\n\
             --exec-arg <arg>\n\
             --exec-repeat <n>\n\
             --toolchain-root <path>\n\
             --build-workspace <path>\n\
             --build-repeat <n>\n\
             --no-build-clean-between-runs\n\
             --json-out <path>\n"
        );
    }

    fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
        args.next()
            .ok_or_else(|| format!("Missing value for {flag}"))
    }

    fn parse_u64(name: &str, value: &str) -> Result<u64, String> {
        value
            .parse::<u64>()
            .map_err(|e| format!("Invalid value for {name}: {value} ({e})"))
    }

    fn parse_u32(name: &str, value: &str) -> Result<u32, String> {
        value
            .parse::<u32>()
            .map_err(|e| format!("Invalid value for {name}: {value} ({e})"))
    }

    fn parse_files_mb(value: &str) -> Result<Vec<u64>, String> {
        let mut out = Vec::new();
        for part in value.split(',') {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                continue;
            }
            let mb = parse_u64("--files-mb", trimmed)?;
            if mb == 0 {
                return Err("--files-mb values must be > 0".to_string());
            }
            out.push(mb);
        }
        if out.is_empty() {
            return Err("--files-mb must contain at least one value".to_string());
        }
        Ok(out)
    }

    #[derive(Debug)]
    struct FileSpec {
        path: String,
        data: Arc<[u8]>,
        mode: u32,
    }

    #[derive(Debug, Clone)]
    struct BenchNode {
        id: ItemId,
        name: String,
        item_type: ItemType,
        mode: u32,
        data: Option<Arc<[u8]>>,
        size: u64,
        modified_time: u64,
        created_time: u64,
        children: Vec<ItemId>,
    }

    #[derive(Clone)]
    struct SyntheticVfs {
        nodes: Arc<HashMap<ItemId, BenchNode>>,
        child_lookup: Arc<HashMap<(ItemId, String), ItemId>>,
        read_stats: Arc<ReadStats>,
    }

    impl SyntheticVfs {
        fn from_file_specs(files: Vec<FileSpec>) -> Result<Self, String> {
            let mut nodes = HashMap::new();
            let mut child_lookup = HashMap::new();
            let now = now_secs();

            nodes.insert(
                ROOT_ID,
                BenchNode {
                    id: ROOT_ID,
                    name: String::new(),
                    item_type: ItemType::Directory,
                    mode: mode::DIRECTORY,
                    data: None,
                    size: 0,
                    modified_time: now,
                    created_time: now,
                    children: Vec::new(),
                },
            );

            let mut next_id: ItemId = 2;

            for spec in files {
                let components = split_vfs_path(&spec.path)?;
                if components.is_empty() {
                    return Err("file path cannot be empty".to_string());
                }

                let mut parent = ROOT_ID;
                for dir_name in &components[0..components.len() - 1] {
                    parent = ensure_dir(
                        &mut nodes,
                        &mut child_lookup,
                        &mut next_id,
                        parent,
                        dir_name,
                        now,
                    )?;
                }

                let file_name = components
                    .last()
                    .expect("components should not be empty")
                    .to_string();
                let key = (parent, file_name.clone());
                if child_lookup.contains_key(&key) {
                    return Err(format!("duplicate VFS path: {}", spec.path));
                }

                let file_id = next_id;
                next_id = next_id.saturating_add(1);

                nodes.insert(
                    file_id,
                    BenchNode {
                        id: file_id,
                        name: file_name.clone(),
                        item_type: ItemType::File,
                        mode: spec.mode,
                        size: spec.data.len() as u64,
                        data: Some(spec.data),
                        modified_time: now,
                        created_time: now,
                        children: Vec::new(),
                    },
                );
                child_lookup.insert(key, file_id);

                let parent_node = nodes
                    .get_mut(&parent)
                    .ok_or_else(|| format!("missing parent node {parent}"))?;
                parent_node.children.push(file_id);
            }

            Ok(Self {
                nodes: Arc::new(nodes),
                child_lookup: Arc::new(child_lookup),
                read_stats: Arc::new(ReadStats::new()),
            })
        }

        fn synthetic(files_mb: &[u64], seed: u64) -> Result<Self, String> {
            let mut specs = Vec::new();
            for (idx, mb) in files_mb.iter().enumerate() {
                let bytes = mb.saturating_mul(1024).saturating_mul(1024) as usize;
                let data_seed = seed ^ ((idx as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
                specs.push(FileSpec {
                    path: format!("file-{}mb.bin", mb),
                    data: generate_synthetic_bytes(bytes, data_seed),
                    mode: mode::FILE_REGULAR,
                });
            }
            Self::from_file_specs(specs)
        }

        fn backend_stats(&self) -> StatsSnapshot {
            self.read_stats.snapshot()
        }
    }

    fn split_vfs_path(path: &str) -> Result<Vec<String>, String> {
        let mut parts = Vec::new();
        for part in path.split('/') {
            if part.is_empty() {
                continue;
            }
            if part == "." || part == ".." {
                return Err(format!("invalid path segment in {path}: {part}"));
            }
            parts.push(part.to_string());
        }
        Ok(parts)
    }

    fn ensure_dir(
        nodes: &mut HashMap<ItemId, BenchNode>,
        child_lookup: &mut HashMap<(ItemId, String), ItemId>,
        next_id: &mut ItemId,
        parent: ItemId,
        dir_name: &str,
        now: u64,
    ) -> Result<ItemId, String> {
        let key = (parent, dir_name.to_string());
        if let Some(existing) = child_lookup.get(&key).copied() {
            let node = nodes
                .get(&existing)
                .ok_or_else(|| format!("missing existing node {existing}"))?;
            if node.item_type != ItemType::Directory {
                return Err(format!("{} exists as non-directory", dir_name));
            }
            return Ok(existing);
        }

        let id = *next_id;
        *next_id = next_id.saturating_add(1);

        nodes.insert(
            id,
            BenchNode {
                id,
                name: dir_name.to_string(),
                item_type: ItemType::Directory,
                mode: mode::DIRECTORY,
                data: None,
                size: 0,
                modified_time: now,
                created_time: now,
                children: Vec::new(),
            },
        );
        child_lookup.insert(key, id);

        let parent_node = nodes
            .get_mut(&parent)
            .ok_or_else(|| format!("missing parent node {parent}"))?;
        parent_node.children.push(id);

        Ok(id)
    }

    fn generate_synthetic_bytes(size: usize, seed: u64) -> Arc<[u8]> {
        let mut out = Vec::with_capacity(size);
        let mut rng = SplitMix64::new(seed);
        for _ in 0..size {
            out.push((rng.next_u64() >> 56) as u8);
        }
        Arc::from(out)
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    impl Vfs for SyntheticVfs {
        async fn lookup(
            &self,
            _cx: &roam::Context,
            parent_id: ItemId,
            name: String,
        ) -> LookupResult {
            match self.child_lookup.get(&(parent_id, name)).copied() {
                Some(id) => {
                    let item_type = self
                        .nodes
                        .get(&id)
                        .map(|n| n.item_type)
                        .unwrap_or(ItemType::File);
                    LookupResult {
                        item_id: id,
                        item_type,
                        error: errno::OK,
                    }
                }
                None => LookupResult {
                    item_id: 0,
                    item_type: ItemType::File,
                    error: errno::ENOENT,
                },
            }
        }

        async fn get_attributes(
            &self,
            _cx: &roam::Context,
            item_id: ItemId,
        ) -> GetAttributesResult {
            match self.nodes.get(&item_id) {
                Some(node) => GetAttributesResult {
                    attrs: ItemAttributes {
                        item_id: node.id,
                        item_type: node.item_type,
                        size: node.size,
                        modified_time: node.modified_time,
                        created_time: node.created_time,
                        mode: node.mode,
                    },
                    error: errno::OK,
                },
                None => GetAttributesResult {
                    attrs: ItemAttributes {
                        item_id,
                        item_type: ItemType::File,
                        size: 0,
                        modified_time: 0,
                        created_time: 0,
                        mode: mode::FILE_REGULAR,
                    },
                    error: errno::ENOENT,
                },
            }
        }

        async fn read_dir(
            &self,
            _cx: &roam::Context,
            item_id: ItemId,
            cursor: u64,
        ) -> ReadDirResult {
            let Some(node) = self.nodes.get(&item_id) else {
                return ReadDirResult {
                    entries: Vec::new(),
                    next_cursor: 0,
                    error: errno::ENOENT,
                };
            };

            if node.item_type != ItemType::Directory {
                return ReadDirResult {
                    entries: Vec::new(),
                    next_cursor: 0,
                    error: errno::ENOTDIR,
                };
            }

            let mut children = node.children.clone();
            children.sort_by_key(|id| {
                self.nodes
                    .get(id)
                    .map(|n| n.name.clone())
                    .unwrap_or_default()
            });

            let start = cursor as usize;
            if start >= children.len() {
                return ReadDirResult {
                    entries: Vec::new(),
                    next_cursor: 0,
                    error: errno::OK,
                };
            }

            let mut entries = Vec::new();
            for child_id in children.iter().skip(start) {
                if let Some(child) = self.nodes.get(child_id) {
                    entries.push(DirEntry {
                        name: child.name.clone(),
                        item_id: child.id,
                        item_type: child.item_type,
                    });
                }
            }

            ReadDirResult {
                entries,
                next_cursor: 0,
                error: errno::OK,
            }
        }

        async fn read(
            &self,
            _cx: &roam::Context,
            item_id: ItemId,
            offset: u64,
            len: u64,
        ) -> ReadResult {
            let start = Instant::now();

            let result = match self.nodes.get(&item_id) {
                Some(node) if node.item_type != ItemType::File => ReadResult {
                    data: Vec::new(),
                    error: errno::EISDIR,
                },
                Some(node) => {
                    let data = node.data.as_ref().expect("file node should have data");
                    let file_len = data.len() as u64;
                    let start_idx = offset.min(file_len) as usize;
                    let end_idx = offset.saturating_add(len).min(file_len) as usize;
                    ReadResult {
                        data: data[start_idx..end_idx].to_vec(),
                        error: errno::OK,
                    }
                }
                None => ReadResult {
                    data: Vec::new(),
                    error: errno::ENOENT,
                },
            };

            self.read_stats.record(
                len,
                result.data.len() as u64,
                start.elapsed(),
                result.error != errno::OK,
            );

            result
        }

        async fn write(
            &self,
            _cx: &roam::Context,
            _item_id: ItemId,
            _offset: u64,
            _data: Vec<u8>,
        ) -> WriteResult {
            WriteResult {
                bytes_written: 0,
                error: errno::EACCES,
            }
        }

        async fn create(
            &self,
            _cx: &roam::Context,
            _parent_id: ItemId,
            _name: String,
            _item_type: ItemType,
        ) -> CreateResult {
            CreateResult {
                item_id: 0,
                error: errno::EACCES,
            }
        }

        async fn delete(&self, _cx: &roam::Context, _item_id: ItemId) -> VfsResult {
            VfsResult {
                error: errno::EACCES,
            }
        }

        async fn rename(
            &self,
            _cx: &roam::Context,
            _item_id: ItemId,
            _new_parent_id: ItemId,
            _new_name: String,
        ) -> VfsResult {
            VfsResult {
                error: errno::EACCES,
            }
        }

        async fn set_attributes(
            &self,
            _cx: &roam::Context,
            _item_id: ItemId,
            _params: SetAttributesParams,
        ) -> VfsResult {
            VfsResult {
                error: errno::EACCES,
            }
        }

        async fn ping(&self, _cx: &roam::Context) -> String {
            "pong from vx-vfs-bench".to_string()
        }
    }

    #[derive(Default)]
    struct ReadStatsInner {
        count: u64,
        error_count: u64,
        total_requested_bytes: u128,
        total_returned_bytes: u128,
        total_latency_ns: u128,
        min_requested_bytes: u64,
        max_requested_bytes: u64,
        min_returned_bytes: u64,
        max_returned_bytes: u64,
        min_latency_ns: u64,
        max_latency_ns: u64,
        latencies_ns: Vec<u64>,
    }

    struct ReadStats {
        inner: Mutex<ReadStatsInner>,
    }

    impl ReadStats {
        fn new() -> Self {
            Self {
                inner: Mutex::new(ReadStatsInner {
                    min_requested_bytes: u64::MAX,
                    min_returned_bytes: u64::MAX,
                    min_latency_ns: u64::MAX,
                    ..ReadStatsInner::default()
                }),
            }
        }

        fn record(&self, requested: u64, returned: u64, latency: Duration, is_error: bool) {
            let mut inner = self.inner.lock().expect("read stats mutex poisoned");
            inner.count = inner.count.saturating_add(1);
            if is_error {
                inner.error_count = inner.error_count.saturating_add(1);
            }
            inner.total_requested_bytes = inner
                .total_requested_bytes
                .saturating_add(requested as u128);
            inner.total_returned_bytes =
                inner.total_returned_bytes.saturating_add(returned as u128);
            inner.min_requested_bytes = inner.min_requested_bytes.min(requested);
            inner.max_requested_bytes = inner.max_requested_bytes.max(requested);
            inner.min_returned_bytes = inner.min_returned_bytes.min(returned);
            inner.max_returned_bytes = inner.max_returned_bytes.max(returned);

            let ns = latency.as_nanos() as u64;
            inner.total_latency_ns = inner.total_latency_ns.saturating_add(ns as u128);
            inner.min_latency_ns = inner.min_latency_ns.min(ns);
            inner.max_latency_ns = inner.max_latency_ns.max(ns);
            inner.latencies_ns.push(ns);
        }

        fn snapshot(&self) -> StatsSnapshot {
            let inner = self.inner.lock().expect("read stats mutex poisoned");
            StatsSnapshot::from_inner(&inner)
        }
    }

    #[derive(Debug, Clone)]
    struct StatsSnapshot {
        count: u64,
        error_count: u64,
        total_requested_bytes: u128,
        total_returned_bytes: u128,
        total_latency_ns: u128,
        min_requested_bytes: u64,
        max_requested_bytes: u64,
        avg_requested_bytes: f64,
        min_returned_bytes: u64,
        max_returned_bytes: u64,
        avg_returned_bytes: f64,
        min_latency_ns: u64,
        max_latency_ns: u64,
        avg_latency_ns: f64,
        p50_latency_ns: u64,
        p95_latency_ns: u64,
        p99_latency_ns: u64,
    }

    impl StatsSnapshot {
        fn from_inner(inner: &ReadStatsInner) -> Self {
            if inner.count == 0 {
                return Self {
                    count: 0,
                    error_count: 0,
                    total_requested_bytes: 0,
                    total_returned_bytes: 0,
                    total_latency_ns: 0,
                    min_requested_bytes: 0,
                    max_requested_bytes: 0,
                    avg_requested_bytes: 0.0,
                    min_returned_bytes: 0,
                    max_returned_bytes: 0,
                    avg_returned_bytes: 0.0,
                    min_latency_ns: 0,
                    max_latency_ns: 0,
                    avg_latency_ns: 0.0,
                    p50_latency_ns: 0,
                    p95_latency_ns: 0,
                    p99_latency_ns: 0,
                };
            }

            let mut sorted = inner.latencies_ns.clone();
            sorted.sort_unstable();

            Self {
                count: inner.count,
                error_count: inner.error_count,
                total_requested_bytes: inner.total_requested_bytes,
                total_returned_bytes: inner.total_returned_bytes,
                total_latency_ns: inner.total_latency_ns,
                min_requested_bytes: inner.min_requested_bytes,
                max_requested_bytes: inner.max_requested_bytes,
                avg_requested_bytes: inner.total_requested_bytes as f64 / inner.count as f64,
                min_returned_bytes: inner.min_returned_bytes,
                max_returned_bytes: inner.max_returned_bytes,
                avg_returned_bytes: inner.total_returned_bytes as f64 / inner.count as f64,
                min_latency_ns: inner.min_latency_ns,
                max_latency_ns: inner.max_latency_ns,
                avg_latency_ns: inner.total_latency_ns as f64 / inner.count as f64,
                p50_latency_ns: percentile_ns(&sorted, 0.50),
                p95_latency_ns: percentile_ns(&sorted, 0.95),
                p99_latency_ns: percentile_ns(&sorted, 0.99),
            }
        }

        fn to_json_object(&self) -> String {
            format!(
                "{{\"reads\":{},\"errors\":{},\"requested_bytes\":{{\"total\":{},\"min\":{},\"max\":{},\"avg\":{}}},\"returned_bytes\":{{\"total\":{},\"min\":{},\"max\":{},\"avg\":{}}},\"latency_ns\":{{\"total\":{},\"min\":{},\"p50\":{},\"p95\":{},\"p99\":{},\"max\":{},\"avg\":{}}}}}",
                self.count,
                self.error_count,
                self.total_requested_bytes,
                self.min_requested_bytes,
                self.max_requested_bytes,
                self.avg_requested_bytes,
                self.total_returned_bytes,
                self.min_returned_bytes,
                self.max_returned_bytes,
                self.avg_returned_bytes,
                self.total_latency_ns,
                self.min_latency_ns,
                self.p50_latency_ns,
                self.p95_latency_ns,
                self.p99_latency_ns,
                self.max_latency_ns,
                self.avg_latency_ns,
            )
        }
    }

    #[derive(Debug, Clone)]
    struct StatsDelta {
        reads: u64,
        errors: u64,
        requested_bytes: u128,
        returned_bytes: u128,
        total_latency_ns: u128,
    }

    impl StatsDelta {
        fn between(previous: &StatsSnapshot, current: &StatsSnapshot) -> Self {
            Self {
                reads: current.count.saturating_sub(previous.count),
                errors: current.error_count.saturating_sub(previous.error_count),
                requested_bytes: current
                    .total_requested_bytes
                    .saturating_sub(previous.total_requested_bytes),
                returned_bytes: current
                    .total_returned_bytes
                    .saturating_sub(previous.total_returned_bytes),
                total_latency_ns: current
                    .total_latency_ns
                    .saturating_sub(previous.total_latency_ns),
            }
        }

        fn avg_latency_ns(&self) -> f64 {
            if self.reads == 0 {
                0.0
            } else {
                self.total_latency_ns as f64 / self.reads as f64
            }
        }

        fn to_json_object(&self) -> String {
            format!(
                "{{\"reads\":{},\"errors\":{},\"requested_bytes\":{},\"returned_bytes\":{},\"total_latency_ns\":{},\"avg_latency_ns\":{}}}",
                self.reads,
                self.errors,
                self.requested_bytes,
                self.returned_bytes,
                self.total_latency_ns,
                self.avg_latency_ns()
            )
        }
    }

    #[derive(Debug, Clone)]
    struct ProcessRunSummary {
        run_index: u32,
        elapsed: Duration,
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
        backend_total: StatsSnapshot,
        backend_delta: StatsDelta,
    }

    #[derive(Debug, Clone)]
    enum BenchSummary {
        RandomRead {
            elapsed: Duration,
            checksum: u64,
            client_stats: StatsSnapshot,
            backend_stats: StatsSnapshot,
        },
        Exec {
            preload_elapsed: Duration,
            mount_elapsed: Duration,
            exec_total_elapsed: Duration,
            total_elapsed: Duration,
            mounted_exec_path: Utf8PathBuf,
            runs: Vec<ProcessRunSummary>,
            backend_stats_total: StatsSnapshot,
        },
        ToolchainBuild {
            preload_elapsed: Duration,
            mount_elapsed: Duration,
            build_total_elapsed: Duration,
            total_elapsed: Duration,
            mounted_cargo_path: Utf8PathBuf,
            mounted_rustc_path: Utf8PathBuf,
            workspace_path: Utf8PathBuf,
            loaded_files: u64,
            loaded_bytes: u128,
            runs: Vec<ProcessRunSummary>,
            backend_stats_total: StatsSnapshot,
        },
    }

    pub(crate) fn percentile_ns(sorted_values: &[u64], percentile: f64) -> u64 {
        if sorted_values.is_empty() {
            return 0;
        }
        let rank = (percentile.clamp(0.0, 1.0) * sorted_values.len() as f64).ceil() as usize;
        let idx = rank.saturating_sub(1).min(sorted_values.len() - 1);
        sorted_values[idx]
    }

    async fn mount_with_vfs(
        mount_path: &Utf8PathBuf,
        vfs: SyntheticVfs,
    ) -> Result<Utf8PathBuf, Box<dyn std::error::Error>> {
        tokio::fs::create_dir_all(mount_path.as_std_path()).await?;
        let result = vx_vfs::mount(
            VfsConfig {
                mount_path: mount_path.clone(),
            },
            vfs,
        )
        .await?;

        match result {
            VfsStart::Mounted { mount_path } => Ok(mount_path),
            VfsStart::Unsupported { platform } => {
                Err(format!("VFS mount unsupported on platform {platform}").into())
            }
        }
    }

    async fn run_random_read_benchmark(
        config: &BenchConfig,
    ) -> Result<BenchSummary, Box<dyn std::error::Error>> {
        let vfs = SyntheticVfs::synthetic(&config.files_mb, config.seed)
            .map_err(|e| format!("failed to create synthetic vfs: {e}"))?;
        let mount_path = mount_with_vfs(&config.mount_path, vfs.clone()).await?;

        let mut open_files = Vec::new();
        for mb in &config.files_mb {
            let name = format!("file-{}mb.bin", mb);
            let path = mount_path.join(&name);
            let file = wait_for_file_open(&path, Duration::from_secs(5))?;
            open_files.push((
                name,
                mb.saturating_mul(1024).saturating_mul(1024),
                Arc::new(file),
            ));
        }

        let client_stats = Arc::new(ReadStats::new());
        let mut buffer = vec![0_u8; config.max_read_bytes as usize];
        let mut rng = SplitMix64::new(config.seed ^ 0xD00D_F00D_1234_5678);
        let mut checksum: u64 = 0;

        let start = Instant::now();
        for _ in 0..config.reads {
            let idx = (rng.next_u64() % open_files.len() as u64) as usize;
            let (_name, file_size, file) = &open_files[idx];

            let requested =
                random_range_inclusive(&mut rng, config.min_read_bytes, config.max_read_bytes)
                    .min((*file_size).max(1));
            let max_offset = file_size.saturating_sub(requested);
            let offset = if max_offset == 0 {
                0
            } else {
                rng.next_u64() % (max_offset + 1)
            };

            let read_start = Instant::now();
            let bytes_read = file.read_at(&mut buffer[..requested as usize], offset)?;
            let elapsed = read_start.elapsed();

            if bytes_read > 0 {
                checksum = checksum.wrapping_add(buffer[0] as u64);
            }

            client_stats.record(
                requested,
                bytes_read as u64,
                elapsed,
                bytes_read as u64 != requested,
            );
        }

        let elapsed = start.elapsed();
        let client = client_stats.snapshot();
        let backend = vfs.backend_stats();

        println!("mode: {}", config.mode.as_str());
        println!("mount path: {}", mount_path);
        println!("elapsed: {:.6}s", elapsed.as_secs_f64());
        println!("checksum: {}", checksum);
        println!(
            "throughput (returned): {:.2} MB/s",
            (client.total_returned_bytes as f64 / (1024.0 * 1024.0))
                / elapsed.as_secs_f64().max(1e-9)
        );

        Ok(BenchSummary::RandomRead {
            elapsed,
            checksum,
            client_stats: client,
            backend_stats: backend,
        })
    }

    async fn run_exec_benchmark(
        config: &BenchConfig,
    ) -> Result<BenchSummary, Box<dyn std::error::Error>> {
        let total_start = Instant::now();

        let preload_start = Instant::now();
        let host_bytes = std::fs::read(config.exec_host_file.as_std_path())?;
        let preload_elapsed = preload_start.elapsed();

        let vfs = SyntheticVfs::from_file_specs(vec![FileSpec {
            path: config.exec_mounted_name.clone(),
            data: Arc::from(host_bytes),
            mode: mode::FILE_EXECUTABLE,
        }])
        .map_err(|e| format!("failed to create exec vfs: {e}"))?;

        let mount_start = Instant::now();
        let mount_path = mount_with_vfs(&config.mount_path, vfs.clone()).await?;
        let mount_elapsed = mount_start.elapsed();

        let mounted_exec_path = mount_path.join(&config.exec_mounted_name);
        let _opened = wait_for_file_open(&mounted_exec_path, Duration::from_secs(5))?;

        let mut runs = Vec::new();
        let exec_total_start = Instant::now();
        let mut previous = vfs.backend_stats();
        for run_index in 1..=config.exec_repeat {
            let run_start = Instant::now();
            let output = Command::new(mounted_exec_path.as_str())
                .args(&config.exec_args)
                .output()?;
            let elapsed = run_start.elapsed();

            let total = vfs.backend_stats();
            let delta = StatsDelta::between(&previous, &total);
            previous = total.clone();

            runs.push(ProcessRunSummary {
                run_index,
                elapsed,
                exit_code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                backend_total: total,
                backend_delta: delta,
            });
        }
        let exec_total_elapsed = exec_total_start.elapsed();

        let total_elapsed = total_start.elapsed();
        let backend_stats_total = vfs.backend_stats();

        println!("mode: {}", config.mode.as_str());
        println!("mount path: {}", mount_path);
        println!("mounted executable: {}", mounted_exec_path);
        println!("exec repeat: {}", config.exec_repeat);
        println!(
            "timing: preload={:.6}s mount={:.6}s exec_total={:.6}s total={:.6}s",
            preload_elapsed.as_secs_f64(),
            mount_elapsed.as_secs_f64(),
            exec_total_elapsed.as_secs_f64(),
            total_elapsed.as_secs_f64()
        );
        for run in &runs {
            println!(
                "run {}: exec={:.6}s exit={:?} backend_reads_delta={} backend_bytes_delta={}",
                run.run_index,
                run.elapsed.as_secs_f64(),
                run.exit_code,
                run.backend_delta.reads,
                run.backend_delta.returned_bytes
            );
            println!("  stdout: {}", run.stdout.trim_end());
        }

        Ok(BenchSummary::Exec {
            preload_elapsed,
            mount_elapsed,
            exec_total_elapsed,
            total_elapsed,
            mounted_exec_path,
            runs,
            backend_stats_total,
        })
    }

    fn load_toolchain_file_specs(
        toolchain_root: &Utf8PathBuf,
    ) -> Result<(Vec<FileSpec>, u64, u128), Box<dyn std::error::Error>> {
        let mut files = Vec::new();
        let mut loaded_files: u64 = 0;
        let mut loaded_bytes: u128 = 0;
        walk_toolchain_dir(
            toolchain_root.as_std_path(),
            Path::new(""),
            &mut files,
            &mut loaded_files,
            &mut loaded_bytes,
        )?;
        Ok((files, loaded_files, loaded_bytes))
    }

    fn walk_toolchain_dir(
        abs_dir: &Path,
        rel_dir: &Path,
        out: &mut Vec<FileSpec>,
        loaded_files: &mut u64,
        loaded_bytes: &mut u128,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for entry in std::fs::read_dir(abs_dir)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let rel_path = rel_dir.join(&file_name);
            let abs_path = entry.path();
            let file_type = entry.file_type()?;

            if file_type.is_dir() {
                walk_toolchain_dir(&abs_path, &rel_path, out, loaded_files, loaded_bytes)?;
                continue;
            }

            let metadata = std::fs::metadata(&abs_path)?;
            if metadata.is_dir() {
                walk_toolchain_dir(&abs_path, &rel_path, out, loaded_files, loaded_bytes)?;
                continue;
            }
            if !metadata.is_file() {
                continue;
            }

            let rel_str = normalize_relative_path(&rel_path)?;
            let data = std::fs::read(&abs_path)?;
            let bytes = data.len() as u128;
            let mode_bits = metadata.permissions().mode() as u32 & 0o777;
            let entry_mode = if mode_bits & 0o111 != 0 {
                mode::FILE_EXECUTABLE
            } else {
                mode::FILE_REGULAR
            };

            out.push(FileSpec {
                path: rel_str,
                data: Arc::from(data),
                mode: entry_mode,
            });
            *loaded_files = loaded_files.saturating_add(1);
            *loaded_bytes = loaded_bytes.saturating_add(bytes);
        }

        Ok(())
    }

    fn normalize_relative_path(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
        let mut parts = Vec::new();
        for component in path.components() {
            let s = component.as_os_str().to_str().ok_or_else(|| {
                format!("encountered non-utf8 path component in {}", path.display())
            })?;
            if !s.is_empty() {
                parts.push(s.to_string());
            }
        }
        Ok(parts.join("/"))
    }

    fn prepare_hello_workspace(path: &Utf8PathBuf) -> Result<(), Box<dyn std::error::Error>> {
        if path.as_std_path().exists() {
            std::fs::remove_dir_all(path.as_std_path())?;
        }

        std::fs::create_dir_all(path.join("src").as_std_path())?;
        std::fs::write(
            path.join("Cargo.toml").as_std_path(),
            b"[package]\nname = \"hello_vfs_bench\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )?;
        std::fs::write(
            path.join("src/main.rs").as_std_path(),
            b"fn main() { println!(\"hello from mounted toolchain\"); }\n",
        )?;

        Ok(())
    }

    async fn run_toolchain_build_benchmark(
        config: &BenchConfig,
    ) -> Result<BenchSummary, Box<dyn std::error::Error>> {
        let total_start = Instant::now();

        let preload_start = Instant::now();
        let (file_specs, loaded_files, loaded_bytes) =
            load_toolchain_file_specs(&config.toolchain_root)?;
        let preload_elapsed = preload_start.elapsed();

        let vfs = SyntheticVfs::from_file_specs(file_specs)
            .map_err(|e| format!("failed to build toolchain vfs: {e}"))?;

        let mount_start = Instant::now();
        let mount_path = mount_with_vfs(&config.mount_path, vfs.clone()).await?;
        let mount_elapsed = mount_start.elapsed();

        let mounted_cargo = mount_path.join("bin/cargo");
        let mounted_rustc = mount_path.join("bin/rustc");
        let _cargo_open = wait_for_file_open(&mounted_cargo, Duration::from_secs(5))?;
        let _rustc_open = wait_for_file_open(&mounted_rustc, Duration::from_secs(5))?;

        prepare_hello_workspace(&config.build_workspace)?;

        let target_dir = config.build_workspace.join("target");

        let mut runs = Vec::new();
        let mut previous = vfs.backend_stats();
        let build_total_start = Instant::now();
        for run_index in 1..=config.build_repeat {
            if run_index > 1 && config.build_clean_between_runs && target_dir.as_std_path().exists()
            {
                std::fs::remove_dir_all(target_dir.as_std_path())?;
            }

            let run_start = Instant::now();
            let output = Command::new(mounted_cargo.as_str())
                .current_dir(config.build_workspace.as_std_path())
                .arg("build")
                .arg("--offline")
                .env("CARGO_TARGET_DIR", target_dir.as_str())
                .env("RUSTC", mounted_rustc.as_str())
                .env_remove("RUSTUP_TOOLCHAIN")
                .output()?;
            let elapsed = run_start.elapsed();

            let total = vfs.backend_stats();
            let delta = StatsDelta::between(&previous, &total);
            previous = total.clone();

            if !output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!(
                    "mounted cargo build failed (run {} status {:?})\nstdout:\n{}\nstderr:\n{}",
                    run_index,
                    output.status.code(),
                    stdout,
                    stderr
                )
                .into());
            }

            runs.push(ProcessRunSummary {
                run_index,
                elapsed,
                exit_code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                backend_total: total,
                backend_delta: delta,
            });
        }
        let build_total_elapsed = build_total_start.elapsed();

        let total_elapsed = total_start.elapsed();
        let backend_stats_total = vfs.backend_stats();

        println!("mode: {}", config.mode.as_str());
        println!("mount path: {}", mount_path);
        println!("toolchain root: {}", config.toolchain_root);
        println!("toolchain files loaded: {}", loaded_files);
        println!("toolchain bytes loaded: {}", loaded_bytes);
        println!("mounted cargo: {}", mounted_cargo);
        println!("mounted rustc: {}", mounted_rustc);
        println!("workspace: {}", config.build_workspace);
        println!(
            "timing: preload={:.6}s mount={:.6}s build_total={:.6}s total={:.6}s",
            preload_elapsed.as_secs_f64(),
            mount_elapsed.as_secs_f64(),
            build_total_elapsed.as_secs_f64(),
            total_elapsed.as_secs_f64()
        );
        for run in &runs {
            println!(
                "run {}: build={:.6}s exit={:?} backend_reads_delta={} backend_bytes_delta={} backend_avg_latency_delta_ns={:.1}",
                run.run_index,
                run.elapsed.as_secs_f64(),
                run.exit_code,
                run.backend_delta.reads,
                run.backend_delta.returned_bytes,
                run.backend_delta.avg_latency_ns(),
            );
        }

        Ok(BenchSummary::ToolchainBuild {
            preload_elapsed,
            mount_elapsed,
            build_total_elapsed,
            total_elapsed,
            mounted_cargo_path: mounted_cargo,
            mounted_rustc_path: mounted_rustc,
            workspace_path: config.build_workspace.clone(),
            loaded_files,
            loaded_bytes,
            runs,
            backend_stats_total,
        })
    }

    async fn run_benchmark(
        config: &BenchConfig,
    ) -> Result<BenchSummary, Box<dyn std::error::Error>> {
        match config.mode {
            BenchMode::RandomRead => run_random_read_benchmark(config).await,
            BenchMode::Exec => run_exec_benchmark(config).await,
            BenchMode::ToolchainBuild => run_toolchain_build_benchmark(config).await,
        }
    }

    fn wait_for_file_open(
        path: &Utf8PathBuf,
        timeout: Duration,
    ) -> Result<File, Box<dyn std::error::Error>> {
        let start = Instant::now();
        loop {
            match File::open(path.as_std_path()) {
                Ok(file) => return Ok(file),
                Err(err) => {
                    if start.elapsed() >= timeout {
                        return Err(format!("failed to open {}: {}", path, err).into());
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }

    fn random_range_inclusive(rng: &mut SplitMix64, min: u64, max: u64) -> u64 {
        if min >= max {
            return min;
        }
        min + (rng.next_u64() % (max - min + 1))
    }

    fn escape_json(input: &str) -> String {
        let mut out = String::with_capacity(input.len() + 8);
        for ch in input.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out
    }

    fn run_json(run: &ProcessRunSummary) -> String {
        format!(
            "{{\"run_index\":{},\"elapsed_seconds\":{},\"exit_code\":{},\"stdout\":\"{}\",\"stderr\":\"{}\",\"backend_total\":{},\"backend_delta\":{}}}",
            run.run_index,
            run.elapsed.as_secs_f64(),
            run.exit_code
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".to_string()),
            escape_json(&run.stdout),
            escape_json(&run.stderr),
            run.backend_total.to_json_object(),
            run.backend_delta.to_json_object(),
        )
    }

    fn render_json_report(config: &BenchConfig, summary: &BenchSummary) -> String {
        let config_json = format!(
            "{{\"mode\":\"{}\",\"mount_path\":\"{}\",\"reads\":{},\"min_read_bytes\":{},\"max_read_bytes\":{},\"files_mb\":[{}],\"seed\":{},\"exec_host_file\":\"{}\",\"exec_mounted_name\":\"{}\",\"exec_repeat\":{},\"toolchain_root\":\"{}\",\"build_workspace\":\"{}\",\"build_repeat\":{},\"build_clean_between_runs\":{}}}",
            config.mode.as_str(),
            escape_json(config.mount_path.as_str()),
            config.reads,
            config.min_read_bytes,
            config.max_read_bytes,
            config
                .files_mb
                .iter()
                .map(|mb| mb.to_string())
                .collect::<Vec<_>>()
                .join(","),
            config.seed,
            escape_json(config.exec_host_file.as_str()),
            escape_json(&config.exec_mounted_name),
            config.exec_repeat,
            escape_json(config.toolchain_root.as_str()),
            escape_json(config.build_workspace.as_str()),
            config.build_repeat,
            config.build_clean_between_runs,
        );

        let summary_json = match summary {
            BenchSummary::RandomRead {
                elapsed,
                checksum,
                client_stats,
                backend_stats,
            } => format!(
                "{{\"kind\":\"random-read\",\"elapsed_seconds\":{},\"checksum\":{},\"client_stats\":{},\"backend_stats\":{}}}",
                elapsed.as_secs_f64(),
                checksum,
                client_stats.to_json_object(),
                backend_stats.to_json_object()
            ),
            BenchSummary::Exec {
                preload_elapsed,
                mount_elapsed,
                exec_total_elapsed,
                total_elapsed,
                mounted_exec_path,
                runs,
                backend_stats_total,
            } => format!(
                "{{\"kind\":\"exec\",\"preload_seconds\":{},\"mount_seconds\":{},\"exec_total_seconds\":{},\"total_seconds\":{},\"mounted_exec_path\":\"{}\",\"runs\":[{}],\"backend_stats_total\":{}}}",
                preload_elapsed.as_secs_f64(),
                mount_elapsed.as_secs_f64(),
                exec_total_elapsed.as_secs_f64(),
                total_elapsed.as_secs_f64(),
                escape_json(mounted_exec_path.as_str()),
                runs.iter().map(run_json).collect::<Vec<_>>().join(","),
                backend_stats_total.to_json_object(),
            ),
            BenchSummary::ToolchainBuild {
                preload_elapsed,
                mount_elapsed,
                build_total_elapsed,
                total_elapsed,
                mounted_cargo_path,
                mounted_rustc_path,
                workspace_path,
                loaded_files,
                loaded_bytes,
                runs,
                backend_stats_total,
            } => format!(
                "{{\"kind\":\"toolchain-build\",\"preload_seconds\":{},\"mount_seconds\":{},\"build_total_seconds\":{},\"total_seconds\":{},\"mounted_cargo_path\":\"{}\",\"mounted_rustc_path\":\"{}\",\"workspace_path\":\"{}\",\"loaded_files\":{},\"loaded_bytes\":{},\"runs\":[{}],\"backend_stats_total\":{}}}",
                preload_elapsed.as_secs_f64(),
                mount_elapsed.as_secs_f64(),
                build_total_elapsed.as_secs_f64(),
                total_elapsed.as_secs_f64(),
                escape_json(mounted_cargo_path.as_str()),
                escape_json(mounted_rustc_path.as_str()),
                escape_json(workspace_path.as_str()),
                loaded_files,
                loaded_bytes,
                runs.iter().map(run_json).collect::<Vec<_>>().join(","),
                backend_stats_total.to_json_object(),
            ),
        };

        format!(
            "{{\"config\":{},\"summary\":{}}}",
            config_json, summary_json
        )
    }

    #[derive(Clone)]
    struct SplitMix64 {
        state: u64,
    }

    impl SplitMix64 {
        fn new(seed: u64) -> Self {
            Self { state: seed }
        }

        fn next_u64(&mut self) -> u64 {
            self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
    }

    #[tokio::main]
    pub async fn main() -> Result<(), Box<dyn std::error::Error>> {
        let config = BenchConfig::parse().map_err(|e| format!("Argument parsing failed: {e}"))?;
        let summary = run_benchmark(&config).await?;

        if let Some(path) = &config.json_out {
            let json = render_json_report(&config, &summary);
            std::fs::write(path.as_std_path(), json.as_bytes())?;
            println!("json report written to {}", path);
        }

        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    macos_bench::main()
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::macos_bench::percentile_ns;

    #[test]
    fn percentile_calculation_matches_expected_indices() {
        let values = vec![10, 20, 30, 40, 50];
        assert_eq!(percentile_ns(&values, 0.50), 30);
        assert_eq!(percentile_ns(&values, 0.95), 50);
        assert_eq!(percentile_ns(&values, 0.99), 50);
    }
}
