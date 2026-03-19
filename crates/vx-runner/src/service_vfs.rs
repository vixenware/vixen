//! VFS-based action execution
//!
//! This module implements compilation using the VFS instead of disk materialization.
//! Files are served lazily from CAS via the Vixen mount.

use camino::{Utf8Path, Utf8PathBuf};
use facet_postcard::to_vec as postcard_to_vec;
use moire::task::FutureExt;
use std::collections::BTreeSet;
use std::future::Future;
use std::time::Instant;
use tokio::task::spawn_blocking;
use tracing::info;
use vx_eval::{
    DemandChain, HostObservation, PathValue, PoisonError, PoisonKind, TransportPoisonError, Value,
    verify_host_obligations,
};
use vx_runner_proto::{
    DiscoverRequest, DiscoverResult, RunArg, RunFailureKind, RunFileMount, RunMountMode,
    RunRequest, RunResult, RunTreeMount, TransportValue,
};
use vx_store_proto::{
    CasNode, NodeHash, OutputBlobRef, RemoteTracePhase, RemoteTraceSpan, StoreProgress,
};

use crate::RunnerService;
use crate::eval_boundary::{
    commit_value_to_transport, evaluate_shipped_discover, evaluate_shipped_verification,
    invoke_shipped_observer,
};
use crate::sandbox::{ActionKind, CommandError, send_progress_reliably};

async fn emit_runner_progress_trace(progress: &roam::Tx<StoreProgress>, span: RemoteTraceSpan) {
    let _ = send_progress_reliably(
        progress,
        StoreProgress::trace(span, RemoteTracePhase::Begin),
    )
    .await;
}

async fn emit_runner_progress_trace_labeled(
    progress: &roam::Tx<StoreProgress>,
    span: RemoteTraceSpan,
    label: impl Into<String>,
) {
    let _ = send_progress_reliably(
        progress,
        StoreProgress::trace_labeled(span, RemoteTracePhase::Begin, label),
    )
    .await;
}

async fn end_runner_progress_trace(progress: &roam::Tx<StoreProgress>, span: RemoteTraceSpan) {
    let _ =
        send_progress_reliably(progress, StoreProgress::trace(span, RemoteTracePhase::End)).await;
}

async fn with_runner_progress_trace<T>(
    progress: &roam::Tx<StoreProgress>,
    span: RemoteTraceSpan,
    future: impl Future<Output = T>,
) -> T {
    emit_runner_progress_trace(progress, span).await;
    let result = future.await;
    end_runner_progress_trace(progress, span).await;
    result
}

fn is_relative_vfs_path(path: &str) -> bool {
    !path.is_empty() && !path.starts_with('/') && !path.split('/').any(|segment| segment == "..")
}

fn is_tree_mount_path(path: &str) -> bool {
    path.is_empty() || is_relative_vfs_path(path)
}

fn covers_path(prefix: &str, path: &str) -> bool {
    prefix.is_empty()
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn paths_overlap(left: &str, right: &str) -> bool {
    covers_path(left, right) || covers_path(right, left)
}

fn path_is_covered(
    path: &str,
    tree_mounts: &[RunTreeMount],
    file_mounts: &[RunFileMount],
    writable_roots: &[String],
) -> bool {
    tree_mounts
        .iter()
        .any(|mount| covers_path(&mount.mount, path))
        || file_mounts.iter().any(|mount| mount.path == path)
        || writable_roots.iter().any(|root| covers_path(root, path))
}

fn parent_dir(path: &str) -> Option<&str> {
    let (parent, _) = path.rsplit_once('/')?;
    (!parent.is_empty()).then_some(parent)
}

fn parent_dirs_for_request(request: &RunRequest) -> Vec<String> {
    let mut dirs = BTreeSet::new();
    let mut record = |path: &str| {
        if let Some(parent) = parent_dir(path)
            && is_relative_vfs_path(parent)
        {
            dirs.insert(parent.to_string());
        }
    };

    if let RunArg::Path(path) = &request.program {
        record(path);
    }
    for arg in &request.args {
        if let RunArg::Path(path) = arg {
            record(path);
        }
    }
    for (_, value) in &request.env {
        if let RunArg::Path(path) = value {
            record(path);
        }
    }
    if let Some(cwd) = &request.cwd {
        record(cwd);
    }

    dirs.into_iter().collect()
}

fn parent_dirs_to_materialize(request: &RunRequest) -> Vec<String> {
    parent_dirs_for_request(request)
        .into_iter()
        .filter(|dir| {
            let within_writable_root = request
                .writable_roots
                .iter()
                .any(|root| covers_path(root, dir));

            if within_writable_root {
                return !request.writable_roots.iter().any(|root| root == dir);
            }

            !request
                .tree_mounts
                .iter()
                .any(|mount| covers_path(&mount.mount, dir))
                && !request
                    .file_mounts
                    .iter()
                    .any(|mount| covers_path(dir, &mount.path))
        })
        .collect()
}

fn explicit_dirs_to_materialize(request: &RunRequest) -> Vec<String> {
    let mut dirs = request.create_dirs.clone();
    dirs.sort();
    dirs.dedup();
    dirs
}

fn resolve_program_path(program: &RunArg, expanded: String, cwd: &Utf8Path) -> String {
    match program {
        RunArg::Path(_) => {
            let path = Utf8Path::new(&expanded);
            if path.is_absolute() {
                expanded
            } else {
                cwd.join(path).as_str().to_string()
            }
        }
        RunArg::Literal(_) => expanded,
    }
}

fn host_path_string(path: &Utf8Path) -> String {
    if std::path::MAIN_SEPARATOR == '/' {
        path.as_str().to_string()
    } else {
        path.as_str()
            .replace('/', &std::path::MAIN_SEPARATOR.to_string())
    }
}

fn opaque_tree_prefix_id(mount: &RunTreeMount) -> String {
    format!("opaque-tree-{}", mount.manifest)
}

fn opaque_file_prefix_id(mount: &RunFileMount) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&mount.manifest.0);
    hasher.update(&[0]);
    hasher.update(mount.logical.as_bytes());
    format!("opaque-file-{}", hasher.finalize().to_hex())
}

fn opaque_file_visible_name(mount: &RunFileMount) -> &str {
    Utf8Path::new(&mount.path).file_name().unwrap_or("file")
}

fn resolve_opaque_visible_path(
    mount_base: &Utf8Path,
    path: &str,
    tree_mounts: &[RunTreeMount],
    file_mounts: &[RunFileMount],
) -> Option<Utf8PathBuf> {
    if let Some(mount) = file_mounts
        .iter()
        .find(|mount| mount.mode == RunMountMode::Opaque && mount.path == path)
    {
        return Some(
            mount_base
                .join(opaque_file_prefix_id(mount))
                .join(opaque_file_visible_name(mount)),
        );
    }

    let mount = tree_mounts
        .iter()
        .filter(|mount| mount.mode == RunMountMode::Opaque && covers_path(&mount.mount, path))
        .max_by_key(|mount| mount.mount.len())?;

    let suffix = if mount.mount.is_empty() {
        path
    } else {
        path.strip_prefix(&mount.mount)
            .expect("covered opaque tree mount must strip prefix")
            .trim_start_matches('/')
    };

    let mut visible = mount_base.join(opaque_tree_prefix_id(mount));
    if !suffix.is_empty() {
        visible.push(suffix);
    }
    Some(visible)
}

fn render_run_arg_for_host(
    arg: &RunArg,
    mount_base: &Utf8Path,
    tree_mounts: &[RunTreeMount],
    file_mounts: &[RunFileMount],
) -> String {
    match arg {
        RunArg::Literal(value) => value.clone(),
        RunArg::Path(path) => {
            if Utf8Path::new(path).is_absolute() {
                arg.render_for_host()
            } else if let Some(visible) =
                resolve_opaque_visible_path(mount_base, path, tree_mounts, file_mounts)
            {
                host_path_string(&visible)
            } else {
                arg.render_for_host()
            }
        }
    }
}

fn resolve_cwd_path(request: &RunRequest, action: &ActionContext) -> Utf8PathBuf {
    request
        .cwd
        .as_deref()
        .map(|path| {
            resolve_opaque_visible_path(
                &action.mount_base,
                path,
                &request.tree_mounts,
                &request.file_mounts,
            )
            .unwrap_or_else(|| action.prefix_path.join(path))
        })
        .unwrap_or_else(|| action.prefix_path.clone())
}

fn opaque_mount_overlap(
    path: &str,
    tree_mounts: &[RunTreeMount],
    file_mounts: &[RunFileMount],
) -> bool {
    tree_mounts
        .iter()
        .any(|mount| mount.mode == RunMountMode::Opaque && paths_overlap(&mount.mount, path))
        || file_mounts
            .iter()
            .any(|mount| mount.mode == RunMountMode::Opaque && paths_overlap(&mount.path, path))
}

#[cfg(windows)]
fn summarize_disk_dir(path: &Utf8Path) -> String {
    const SAMPLE_LIMIT: usize = 8;

    let read_dir = match std::fs::read_dir(path) {
        Ok(read_dir) => read_dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return "<missing>".to_string();
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotADirectory => {
            return "<not-a-directory>".to_string();
        }
        Err(error) => return format!("<error: {error}>"),
    };

    let mut names = read_dir
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    names.sort();

    if names.len() > SAMPLE_LIMIT {
        let remaining = names.len() - SAMPLE_LIMIT;
        names.truncate(SAMPLE_LIMIT);
        names.push(format!("... (+{remaining} more)"));
    }

    if names.is_empty() {
        "-".to_string()
    } else {
        names.join(", ")
    }
}

#[cfg(windows)]
fn summarize_prefix_disk_view(prefix_path: &Utf8Path) -> String {
    let src_path = prefix_path.join("src");
    let out_path = prefix_path.join("out");

    format!(
        "root_children=[{}]; src_children=[{}]; out_children=[{}]",
        summarize_disk_dir(prefix_path),
        summarize_disk_dir(&src_path),
        summarize_disk_dir(&out_path),
    )
}

#[cfg(windows)]
fn summarize_windows_path_inputs(
    cwd: &Utf8Path,
    program: Option<&RunArg>,
    args: &[RunArg],
    env: &[(String, RunArg)],
) -> String {
    let mut candidates = Vec::new();

    if let Some(RunArg::Path(path)) = program {
        candidates.push(("program".to_string(), path.to_string()));
    }

    for (index, arg) in args.iter().enumerate() {
        if let RunArg::Path(path) = arg {
            candidates.push((format!("arg[{index}]"), path.to_string()));
        }
    }

    for (key, value) in env {
        if let RunArg::Path(path) = value {
            candidates.push((format!("env:{key}"), path.to_string()));
        }
    }

    if candidates.is_empty() {
        return "-".to_string();
    }

    candidates
        .into_iter()
        .map(|(label, path)| {
            let absolute = if Utf8Path::new(&path).is_absolute() {
                Utf8PathBuf::from(&path)
            } else {
                cwd.join(&path)
            };

            match std::fs::metadata(absolute.as_std_path()) {
                Ok(metadata) => format!(
                    "{label}={path} -> exists(file={}, dir={}, len={})",
                    metadata.is_file(),
                    metadata.is_dir(),
                    metadata.len()
                ),
                Err(error) => format!("{label}={path} -> missing({error})"),
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn summarize_list(items: &[String], limit: usize) -> String {
    if items.is_empty() {
        return "-".to_string();
    }
    if items.len() <= limit {
        return items.join(", ");
    }

    let mut summary = items[..limit].join(", ");
    summary.push_str(format!(", ... (+{} more)", items.len() - limit).as_str());
    summary
}

fn run_failure_result(
    start: Instant,
    exit_code: i32,
    stdout: String,
    stderr: String,
    error: Option<String>,
) -> RunResult {
    run_failure_result_with_kind(start, exit_code, stdout, stderr, None, error, None)
}

fn run_failure_result_with_kind(
    start: Instant,
    exit_code: i32,
    stdout: String,
    stderr: String,
    error_kind: Option<RunFailureKind>,
    error: Option<String>,
    poison: Option<TransportPoisonError>,
) -> RunResult {
    RunResult {
        success: false,
        exit_code,
        stdout,
        stderr,
        duration_ms: start.elapsed().as_millis() as u64,
        observer_value: None,
        poison,
        error_kind,
        error,
    }
}

fn run_success_result(
    start: Instant,
    exit_code: i32,
    stdout: String,
    stderr: String,
    observer_value: Option<TransportValue>,
) -> RunResult {
    RunResult {
        success: true,
        exit_code,
        stdout,
        stderr,
        duration_ms: start.elapsed().as_millis() as u64,
        observer_value,
        poison: None,
        error_kind: None,
        error: None,
    }
}

fn append_process_output(mut message: String, stdout: &str, stderr: &str) -> String {
    if !stderr.trim().is_empty() {
        message.push_str("\nprocess stderr:\n");
        message.push_str(stderr);
    }
    if !stdout.trim().is_empty() {
        message.push_str("\nprocess stdout:\n");
        message.push_str(stdout);
    }
    message
}

fn append_similar_missing_input_hint(
    mut message: String,
    prefix_path: &Utf8Path,
    stderr: &str,
) -> String {
    let Some(missing_path) = parse_missing_process_input_from_stderr(stderr) else {
        return message;
    };
    let Some(similar_path) = find_similar_sandbox_path(prefix_path, &missing_path) else {
        return message;
    };
    message.push_str("\nsimilar sandbox path: ");
    message.push_str(&similar_path);
    message
}

fn parse_missing_process_input_from_stderr(stderr: &str) -> Option<String> {
    stderr.lines().find_map(parse_missing_process_input_line)
}

fn parse_missing_process_input_line(line: &str) -> Option<String> {
    let (_, value) = line.split_once("no such file or directory: ")?;
    parse_quoted_path(value.trim())
}

fn parse_quoted_path(value: &str) -> Option<String> {
    ['\'', '"'].into_iter().find_map(|quote| {
        let stripped = value.strip_prefix(quote)?;
        let (path, _) = stripped.split_once(quote)?;
        Some(path.to_string())
    })
}

fn find_similar_sandbox_path(prefix_path: &Utf8Path, missing_path: &str) -> Option<String> {
    let missing_segments = path_segments(missing_path);
    let mut best = None;
    let mut stack = vec![prefix_path.to_path_buf()];
    let mut scanned_entries = 0usize;

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            scanned_entries += 1;
            if scanned_entries > 4096 {
                return best.map(|(_, path)| path);
            }

            let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if path.file_name() != Some("out") {
                    stack.push(path);
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }

            let Ok(relative) = path.strip_prefix(prefix_path) else {
                continue;
            };
            let candidate = relative.as_str();
            if candidate == missing_path {
                continue;
            }

            let Some(score) = similarity_score(candidate, &missing_segments) else {
                continue;
            };

            if best.as_ref().is_none_or(|(best_score, best_path)| {
                better_similarity(score, candidate, *best_score, best_path)
            }) {
                best = Some((score, candidate.to_string()));
            }
        }
    }

    best.map(|(_, path)| path)
}

fn path_segments(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .collect()
}

fn similarity_score(
    candidate: &str,
    missing_segments: &[&str],
) -> Option<(bool, usize, bool, usize, usize)> {
    let candidate_segments = path_segments(candidate);
    let suffix_matches = candidate_segments
        .iter()
        .rev()
        .zip(missing_segments.iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let same_filename = candidate_segments.last() == missing_segments.last();
    if suffix_matches == 0 && !same_filename {
        return None;
    }

    let exact_suffix = candidate_segments.len() > missing_segments.len()
        && candidate_segments[candidate_segments.len() - missing_segments.len()..]
            == *missing_segments;
    let extra_segments = candidate_segments
        .len()
        .saturating_sub(missing_segments.len());

    Some((
        exact_suffix,
        suffix_matches,
        same_filename,
        usize::MAX - extra_segments,
        usize::MAX - candidate.len(),
    ))
}

fn better_similarity(
    candidate_score: (bool, usize, bool, usize, usize),
    candidate_path: &str,
    best_score: (bool, usize, bool, usize, usize),
    best_path: &str,
) -> bool {
    candidate_score > best_score || (candidate_score == best_score && candidate_path < best_path)
}

/// Action execution context with all paths and sandbox config.
pub struct ActionContext {
    pub prefix_id: String,
    pub mount_base: Utf8PathBuf,
    pub prefix_path: Utf8PathBuf,
    pub write_path: Utf8PathBuf,
    pub allowed_host_exec_paths: Vec<Utf8PathBuf>,
}

impl ActionContext {
    /// Clear the environment and re-add only what's needed for hermetic builds.
    ///
    /// Sets PATH (linker discovery), HOME, and temp dirs. On Windows the temp
    /// vars are critical — without them `GetTempPathW` falls back to `C:\WINDOWS`.
    pub fn hermetic_env(&self, cmd: &mut moire::process::Command) {
        cmd.env_clear();
        // Passthrough vars needed by compilers/linkers on the host.
        // On Windows, LIB/INCLUDE are required for MSVC link.exe to find
        // kernel32.lib and friends.
        const PASSTHROUGH: &[&str] = &[
            "PATH",
            "HOME",
            // MSVC toolchain
            "LIB",
            "INCLUDE",
            "LIBPATH",
            // Windows SDK
            "WindowsSdkDir",
            "WindowsSDKVersion",
        ];
        for var in PASSTHROUGH {
            if let Ok(val) = std::env::var(var) {
                cmd.env(var, val);
            }
        }
        cmd.env("TMPDIR", self.write_path.as_str());
        cmd.env("TMP", self.write_path.as_str());
        cmd.env("TEMP", self.write_path.as_str());
    }
}

fn common_relative_ancestor(paths: &[String]) -> Option<String> {
    let mut iter = paths.iter();
    let first = iter.next()?;
    let mut common = first.split('/').collect::<Vec<_>>();

    for path in iter {
        let segments = path.split('/').collect::<Vec<_>>();
        let shared_len = common
            .iter()
            .zip(segments.iter())
            .take_while(|(left, right)| left == right)
            .count();
        common.truncate(shared_len);
        if common.is_empty() {
            return None;
        }
    }

    Some(common.join("/"))
}

fn sandbox_write_root(request: &RunRequest) -> Option<String> {
    common_relative_ancestor(&request.writable_roots)
}

fn discover_request_cache_key(request: &DiscoverRequest) -> Result<String, TransportPoisonError> {
    let bytes = postcard_to_vec(request).map_err(|error| {
        TransportPoisonError::from(&PoisonError::new(
            PoisonKind::UserFail,
            format!("discover cache key: {error}"),
            DemandChain::default(),
        ))
    })?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn cached_discover_value_is_valid(value: &TransportValue) -> bool {
    match value {
        TransportValue::Discovered { obligations, .. } => {
            verify_host_obligations(obligations).is_ok()
        }
        _ => true,
    }
}

impl RunnerService {
    pub async fn discover_vfs(&self, request: DiscoverRequest) -> DiscoverResult {
        let cache_key = match discover_request_cache_key(&request) {
            Ok(key) => key,
            Err(poison) => {
                return DiscoverResult {
                    value: None,
                    poison: Some(poison),
                };
            }
        };

        if let Some(cached) = self
            .discover_cache
            .lock()
            .expect("discover cache poisoned")
            .get(&cache_key)
            .cloned()
        {
            if cached_discover_value_is_valid(&cached) {
                return DiscoverResult {
                    value: Some(cached),
                    poison: None,
                };
            }
            self.discover_cache
                .lock()
                .expect("discover cache poisoned")
                .remove(&cache_key);
        }

        match evaluate_shipped_discover(self.cas.as_ref(), &request).await {
            Ok(value) => {
                self.discover_cache
                    .lock()
                    .expect("discover cache poisoned")
                    .insert(cache_key, value.clone());
                DiscoverResult {
                    value: Some(value),
                    poison: None,
                }
            }
            Err(poison) => DiscoverResult {
                value: None,
                poison: Some(poison),
            },
        }
    }

    /// Create action execution context with prefix, paths, and sandbox config.
    async fn create_action_context(&self) -> ActionContext {
        let prefix_id = self.create_action_prefix().await;
        let mount_base = self.vfs_access_path.clone();
        let prefix_path = mount_base.join(&prefix_id);
        let out_path = prefix_path.join("out");

        ActionContext {
            prefix_id,
            mount_base,
            prefix_path,
            write_path: out_path.clone(),
            allowed_host_exec_paths: Vec::new(),
        }
    }

    fn allowed_host_exec_paths(request: &RunRequest) -> Vec<Utf8PathBuf> {
        let mut paths = BTreeSet::new();
        let Some(eval) = &request.eval else {
            return Vec::new();
        };
        for obligation in &eval.verification_obligations {
            if let HostObservation::Which {
                expected_path: Some(path),
                ..
            } = obligation
                && std::path::Path::new(path).is_absolute()
                && let Ok(path) = Utf8PathBuf::from_path_buf(path.into())
            {
                paths.insert(path);
            }
        }
        paths.into_iter().collect()
    }

    /// Run a sandboxed command with declared CAS/VFS inputs.
    pub async fn run_vfs(
        &self,
        request: RunRequest,
        progress: roam::Tx<StoreProgress>,
    ) -> RunResult {
        let start = Instant::now();
        let command_name = request.program.as_str().to_string();

        let mut action = self
            .create_action_context()
            .named("runner.run_vfs.create_action_context")
            .await;
        action.write_path = sandbox_write_root(&request)
            .map(|path| action.prefix_path.join(path))
            .unwrap_or_else(|| action.prefix_path.clone());
        action.allowed_host_exec_paths = Self::allowed_host_exec_paths(&request);

        info!(
            action.prefix_id = %action.prefix_id,
            action.prefix_path = %action.prefix_path,
            command = %command_name,
            cwd = request.cwd.as_deref().unwrap_or("."),
            tree_mounts = request.tree_mounts.len(),
            tree_mount_specs = %summarize_list(
                &request
                    .tree_mounts
                    .iter()
                    .map(|mount| format!("{}@{}", mount.mount, mount.manifest))
                    .collect::<Vec<_>>(),
                6
            ),
            file_mounts = request.file_mounts.len(),
            writable_roots = request.writable_roots.len(),
            "preparing VFS run action"
        );
        struct PreparedRun {
            cwd: Utf8PathBuf,
            program_path: String,
            cmd: moire::process::Command,
            expanded_args: Vec<String>,
            expanded_env: Vec<String>,
            action_desc: String,
            action_kind: ActionKind,
            verification_task: Option<tokio::task::JoinHandle<Result<(), String>>>,
        }

        let prepared_run = with_runner_progress_trace(
            &progress,
            RemoteTraceSpan::RunnerPrepareVfs,
            async {
                let mount_start = Instant::now();

                for mount in &request.tree_mounts {
                    if !is_tree_mount_path(&mount.mount) {
                        self.remove_action_prefix(&action.prefix_id).await;
                        return Err(run_failure_result(
                            start,
                            -1,
                            String::new(),
                            String::new(),
                            Some(format!(
                                "tree mount '{}' must be a relative VFS path",
                                mount.mount
                            )),
                        ));
                    }
                }

                for mount in &request.file_mounts {
                    if !is_relative_vfs_path(&mount.path) {
                        self.remove_action_prefix(&action.prefix_id).await;
                        return Err(run_failure_result(
                            start,
                            -1,
                            String::new(),
                            String::new(),
                            Some(format!(
                                "file mount '{}' must be a relative VFS path",
                                mount.path
                            )),
                        ));
                    }
                }

                for root in &request.writable_roots {
                    if !is_relative_vfs_path(root) {
                        self.remove_action_prefix(&action.prefix_id).await;
                        return Err(run_failure_result(
                            start,
                            -1,
                            String::new(),
                            String::new(),
                            Some(format!(
                                "writable root '{root}' must be a relative VFS path"
                            )),
                        ));
                    }
                }

                for root in &request.writable_roots {
                    if opaque_mount_overlap(root, &request.tree_mounts, &request.file_mounts) {
                        self.remove_action_prefix(&action.prefix_id).await;
                        return Err(run_failure_result(
                            start,
                            -1,
                            String::new(),
                            String::new(),
                            Some(format!(
                                "writable root '{root}' overlaps an opaque mount"
                            )),
                        ));
                    }
                }

                for dir in &request.create_dirs {
                    if !is_relative_vfs_path(dir) {
                        self.remove_action_prefix(&action.prefix_id).await;
                        return Err(run_failure_result(
                            start,
                            -1,
                            String::new(),
                            String::new(),
                            Some(format!("create dir '{dir}' must be a relative VFS path")),
                        ));
                    }
                    if opaque_mount_overlap(dir, &request.tree_mounts, &request.file_mounts) {
                        self.remove_action_prefix(&action.prefix_id).await;
                        return Err(run_failure_result(
                            start,
                            -1,
                            String::new(),
                            String::new(),
                            Some(format!(
                                "create dir '{dir}' overlaps an opaque mount"
                            )),
                        ));
                    }
                }

                for mount in &request.tree_mounts {
                    match self.cas.get_node(NodeHash(mount.manifest)).await {
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            self.remove_action_prefix(&action.prefix_id).await;
                            return Err(run_failure_result(
                                start,
                                -1,
                                String::new(),
                                String::new(),
                                Some(format!(
                                    "tree manifest {} not found in CAS (mount '{}')",
                                    mount.manifest, mount.mount
                                )),
                            ));
                        }
                        Err(e) => {
                            self.remove_action_prefix(&action.prefix_id).await;
                            return Err(run_failure_result(
                                start,
                                -1,
                                String::new(),
                                String::new(),
                                Some(format!("failed to fetch tree manifest: {e:?}")),
                            ));
                        }
                    }

                    let (target_prefix_id, target_mount_path) = match mount.mode {
                        RunMountMode::Tracked => (action.prefix_id.clone(), mount.mount.clone()),
                        RunMountMode::Opaque => {
                            let prefix_id = opaque_tree_prefix_id(mount);
                            self.vfs.create_prefix(prefix_id.clone()).await;
                            (prefix_id, String::new())
                        }
                    };

                    if let Err(e) = self
                        .vfs
                        .mount_tree(&target_prefix_id, &target_mount_path, mount.manifest)
                        .named("runner.run_vfs.mount_tree")
                        .await
                    {
                        self.remove_action_prefix(&action.prefix_id).await;
                        return Err(run_failure_result(
                            start,
                            -1,
                            String::new(),
                            String::new(),
                            Some(format!("failed to mount tree '{}': {e}", mount.mount)),
                        ));
                    }
                }

                if !request.file_mounts.is_empty() {
                    for fm in &request.file_mounts {
                        let manifest = match self.cas.get_node(NodeHash(fm.manifest)).await {
                            Ok(Some(node_data)) => match CasNode::from_bytes(&node_data) {
                                Ok(CasNode::Manifest(m)) => m,
                                Ok(_) => {
                                    self.remove_action_prefix(&action.prefix_id).await;
                                    return Err(run_failure_result(
                                        start,
                                        -1,
                                        String::new(),
                                        String::new(),
                                        Some(format!(
                                            "input manifest {} is not a Manifest node",
                                            fm.manifest
                                        )),
                                    ));
                                }
                                Err(e) => {
                                    self.remove_action_prefix(&action.prefix_id).await;
                                    return Err(run_failure_result(
                                        start,
                                        -1,
                                        String::new(),
                                        String::new(),
                                        Some(format!(
                                            "failed to decode input manifest {}: {}",
                                            fm.manifest, e
                                        )),
                                    ));
                                }
                            },
                            Ok(None) => {
                                self.remove_action_prefix(&action.prefix_id).await;
                                return Err(run_failure_result(
                                    start,
                                    -1,
                                    String::new(),
                                    String::new(),
                                    Some(format!(
                                        "input manifest {} not found in CAS",
                                        fm.manifest
                                    )),
                                ));
                            }
                            Err(e) => {
                                self.remove_action_prefix(&action.prefix_id).await;
                                return Err(run_failure_result(
                                    start,
                                    -1,
                                    String::new(),
                                    String::new(),
                                    Some(format!("failed to fetch input manifest: {e:?}")),
                                ));
                            }
                        };

                        let entry = match manifest.outputs.iter().find(|o| o.logical == fm.logical)
                        {
                            Some(e) => e,
                            None => {
                                self.remove_action_prefix(&action.prefix_id).await;
                                return Err(run_failure_result(
                                    start,
                                    -1,
                                    String::new(),
                                    String::new(),
                                    Some(format!(
                                        "input manifest {} missing logical output '{}'",
                                        fm.manifest, fm.logical
                                    )),
                                ));
                            }
                        };

                        let (blob_hash, blob_size) = match &entry.blob {
                            OutputBlobRef::Small { chunk, size } => (chunk.0, *size),
                            OutputBlobRef::Large {
                                content_hash, size, ..
                            } => (content_hash.0, *size),
                        };
                        let (target_prefix_id, target_path) = match fm.mode {
                            RunMountMode::Tracked => (action.prefix_id.clone(), fm.path.clone()),
                            RunMountMode::Opaque => {
                                let prefix_id = opaque_file_prefix_id(fm);
                                self.vfs.create_prefix(prefix_id.clone()).await;
                                (prefix_id, opaque_file_visible_name(fm).to_string())
                            }
                        };
                        if let Err(e) = self
                            .vfs
                            .add_file_to_prefix(
                                &target_prefix_id,
                                &target_path,
                                blob_hash,
                                blob_size,
                                entry.executable,
                            )
                            .named("runner.run_vfs.add_file_to_prefix")
                            .await
                        {
                            self.remove_action_prefix(&action.prefix_id).await;
                            return Err(run_failure_result(
                                start,
                                -1,
                                String::new(),
                                String::new(),
                                Some(format!(
                                    "failed to materialize file mount '{}': {e}",
                                    fm.path
                                )),
                            ));
                        }
                    }
                }

                for root in &request.writable_roots {
                    if let Err(e) = self.vfs.add_dir_to_prefix(&action.prefix_id, root).await {
                        self.remove_action_prefix(&action.prefix_id).await;
                        return Err(run_failure_result(
                            start,
                            -1,
                            String::new(),
                            String::new(),
                            Some(format!("failed to create writable root '{root}': {e}")),
                        ));
                    }
                }
                let create_dirs = explicit_dirs_to_materialize(&request);
                for dir in &create_dirs {
                    if let Err(e) = self.vfs.add_dir_to_prefix(&action.prefix_id, dir).await {
                        self.remove_action_prefix(&action.prefix_id).await;
                        return Err(run_failure_result(
                            start,
                            -1,
                            String::new(),
                            String::new(),
                            Some(format!("failed to create explicit dir '{dir}': {e}")),
                        ));
                    }
                }
                let parent_dirs = parent_dirs_to_materialize(&request);
                for dir in &parent_dirs {
                    if let Err(e) = self.vfs.add_dir_to_prefix(&action.prefix_id, dir).await {
                        self.remove_action_prefix(&action.prefix_id).await;
                        return Err(run_failure_result(
                            start,
                            -1,
                            String::new(),
                            String::new(),
                            Some(format!("failed to create writable dir '{dir}': {e}")),
                        ));
                    }
                }

                info!(
                    action.prefix_id = %action.prefix_id,
                    tree_mounts = %summarize_list(
                        &request
                            .tree_mounts
                            .iter()
                            .map(|mount| mount.mount.clone())
                            .collect::<Vec<_>>(),
                        6
                    ),
                    file_mounts = %summarize_list(
                        &request
                            .file_mounts
                            .iter()
                            .map(|mount| mount.path.clone())
                            .collect::<Vec<_>>(),
                        6
                    ),
                    writable_roots = %summarize_list(&request.writable_roots, 6),
                    create_dirs = %summarize_list(&create_dirs, 6),
                    parent_dirs = %summarize_list(&parent_dirs, 6),
                    elapsed_secs = mount_start.elapsed().as_secs_f32(),
                    "prepared VFS inputs"
                );

                #[cfg(windows)]
                if !request.tree_mounts.is_empty() {
                    match self.vfs.describe_prefix_state(&action.prefix_id).await {
                        Ok(summary) => {
                            info!(
                                action.prefix_id = %action.prefix_id,
                                prefix_found = summary.prefix_found,
                                total_items = summary.total_items,
                                total_prefixes = summary.total_prefixes,
                                path_mappings_count = summary.path_mappings_count,
                                root_children = %summarize_list(&summary.root_children, 12),
                                prefix_children = %summarize_list(&summary.prefix_children, 12),
                                src_children = %summarize_list(&summary.src_children, 12),
                                toolchain_children = %summarize_list(&summary.toolchain_children, 12),
                                toolchain_bin_children = %summarize_list(&summary.toolchain_bin_children, 12),
                                path_mappings_sample = %summarize_list(&summary.path_mappings_sample, 12),
                                src_path_mappings_sample = %summarize_list(
                                    &summary.src_path_mappings_sample,
                                    16
                                ),
                                "VFS prefix state after preparing inputs"
                            );
                        }
                        Err(error) => {
                            info!(
                                action.prefix_id = %action.prefix_id,
                                %error,
                                "failed to describe VFS state after preparing inputs"
                            );
                        }
                    }
                }

                for arg in &request.args {
                    if let RunArg::Path(path) = arg
                        && !path_is_covered(
                            path,
                            &request.tree_mounts,
                            &request.file_mounts,
                            &request.writable_roots,
                        )
                    {
                        self.remove_action_prefix(&action.prefix_id).await;
                        return Err(run_failure_result(
                            start,
                            -1,
                            String::new(),
                            String::new(),
                            Some(format!(
                                "path arg '{}' is not covered by a mount or writable root",
                                path
                            )),
                        ));
                    }
                }

                for (key, value) in &request.env {
                    if let RunArg::Path(path) = value
                        && !path_is_covered(
                            path,
                            &request.tree_mounts,
                            &request.file_mounts,
                            &request.writable_roots,
                        )
                    {
                        self.remove_action_prefix(&action.prefix_id).await;
                        return Err(run_failure_result(
                            start,
                            -1,
                            String::new(),
                            String::new(),
                            Some(format!(
                                "env path '{}' for '{}' is not covered by a mount or writable root",
                                path, key
                            )),
                        ));
                    }
                }

                if let Some(cwd) = &request.cwd
                    && !path_is_covered(
                        cwd,
                        &request.tree_mounts,
                        &request.file_mounts,
                        &request.writable_roots,
                    )
                {
                    self.remove_action_prefix(&action.prefix_id).await;
                    return Err(run_failure_result(
                        start,
                        -1,
                        String::new(),
                        String::new(),
                        Some(format!(
                            "cwd '{cwd}' is not covered by a mount or writable root"
                        )),
                    ));
                }

                let cwd = resolve_cwd_path(&request, &action);

                let program_path = resolve_program_path(
                    &request.program,
                    render_run_arg_for_host(
                        &request.program,
                        &action.mount_base,
                        &request.tree_mounts,
                        &request.file_mounts,
                    ),
                    &cwd,
                );
                let mut cmd = crate::sandbox::sandboxed_command(&program_path, &action);
                let expanded_args = request
                    .args
                    .iter()
                    .map(|arg| {
                        render_run_arg_for_host(
                            arg,
                            &action.mount_base,
                            &request.tree_mounts,
                            &request.file_mounts,
                        )
                    })
                    .collect::<Vec<_>>();
                for arg in &expanded_args {
                    cmd.arg(arg);
                }
                cmd.current_dir(cwd.as_str());

                if request.clear_env {
                    action.hermetic_env(&mut cmd);
                }

                let expanded_env = request
                    .env
                    .iter()
                    .map(|(key, value)| {
                        format!(
                            "{key}={}",
                            render_run_arg_for_host(
                                value,
                                &action.mount_base,
                                &request.tree_mounts,
                                &request.file_mounts,
                            )
                        )
                    })
                    .collect::<Vec<_>>();
                for (k, v) in &request.env {
                    cmd.env(
                        k,
                        render_run_arg_for_host(
                            v,
                            &action.mount_base,
                            &request.tree_mounts,
                            &request.file_mounts,
                        ),
                    );
                }

                let action_desc = format!(
                    "{} {}",
                    request.program.as_str(),
                    request.args.first().map(RunArg::as_str).unwrap_or_default()
                );

                let action_kind = if request.args.iter().any(|arg| arg.as_str() == "-c") {
                    ActionKind::CcCompile
                } else {
                    ActionKind::CcLink
                };
                let verification_task = request
                    .eval
                    .as_ref()
                    .filter(|eval| !eval.verification_obligations.is_empty())
                    .cloned()
                    .map(|eval| spawn_blocking(move || evaluate_shipped_verification(&eval)));

                Ok(PreparedRun {
                    cwd,
                    program_path,
                    cmd,
                    expanded_args,
                    expanded_env,
                    action_desc,
                    action_kind,
                    verification_task,
                })
            },
        )
        .await;

        let PreparedRun {
            cwd,
            program_path,
            cmd,
            expanded_args,
            expanded_env,
            action_desc,
            action_kind,
            verification_task,
        } = match prepared_run {
            Ok(prepared) => prepared,
            Err(result) => return result,
        };

        info!(
            action.prefix_id = %action.prefix_id,
            action_desc,
            program = %program_path,
            cwd = %cwd,
            argv = %summarize_list(&expanded_args, 12),
            env = %summarize_list(&expanded_env, 8),
            clear_env = request.clear_env,
            "executing sandboxed command"
        );
        #[cfg(windows)]
        info!(
            action.prefix_id = %action.prefix_id,
            action_desc,
            disk_view = %summarize_prefix_disk_view(&action.prefix_path),
            "Windows VFS disk view before command"
        );
        #[cfg(windows)]
        info!(
            action.prefix_id = %action.prefix_id,
            action_desc,
            path_inputs = %summarize_windows_path_inputs(
                &cwd,
                Some(&request.program),
                &request.args,
                &request.env,
            ),
            "Windows VFS path inputs before command"
        );
        let command_start = Instant::now();

        emit_runner_progress_trace_labeled(
            &progress,
            RemoteTraceSpan::RunnerCommand,
            action_desc.clone(),
        )
        .await;
        let (progress, output) =
            crate::sandbox::run_sandboxed_command(cmd, &action_kind, &action_desc, progress, None)
                .named("runner.run_vfs.run_sandboxed_command")
                .await;
        end_runner_progress_trace(&progress, RemoteTraceSpan::RunnerCommand).await;
        let output = match output {
            Ok(o) => o,
            Err(CommandError::SpawnFailed(e)) => {
                info!(
                    action.prefix_id = %action.prefix_id,
                    action_desc,
                    error = %e,
                    elapsed_secs = command_start.elapsed().as_secs_f32(),
                    "sandboxed command failed to start"
                );
                self.remove_action_prefix(&action.prefix_id).await;
                return run_failure_result(
                    start,
                    -1,
                    String::new(),
                    String::new(),
                    Some(format!("failed to execute command: {e}")),
                );
            }
            Err(CommandError::TimedOut {
                action_desc,
                timeout,
                partial_output,
                ..
            }) => {
                info!(
                    action.prefix_id = %action.prefix_id,
                    action_desc,
                    timeout_secs = timeout.as_secs_f32(),
                    partial_output_len = partial_output.len(),
                    elapsed_secs = command_start.elapsed().as_secs_f32(),
                    "sandboxed command timed out"
                );
                self.remove_action_prefix(&action.prefix_id).await;
                return run_failure_result(
                    start,
                    -1,
                    String::new(),
                    partial_output,
                    Some(format!(
                        "action '{}' timed out after {}s (limit: {}s)",
                        action_desc,
                        timeout.as_secs(),
                        timeout.as_secs()
                    )),
                );
            }
            Err(CommandError::Cancelled {
                action_desc,
                partial_output,
            }) => {
                info!(
                    action.prefix_id = %action.prefix_id,
                    action_desc,
                    partial_output_len = partial_output.len(),
                    elapsed_secs = command_start.elapsed().as_secs_f32(),
                    "sandboxed command cancelled"
                );
                self.remove_action_prefix(&action.prefix_id).await;
                return run_failure_result(
                    start,
                    -1,
                    String::new(),
                    partial_output,
                    Some(format!("action '{}' cancelled", action_desc)),
                );
            }
        };
        info!(
            action.prefix_id = %action.prefix_id,
            action_desc,
            exit_code = output.exit_status.code().unwrap_or(-1),
            stdout_bytes = output.stdout.len(),
            stderr_bytes = output.stderr.len(),
            elapsed_secs = command_start.elapsed().as_secs_f32(),
            "sandboxed command finished"
        );

        let exit_code = output.exit_status.code().unwrap_or(-1);
        // r[impl exec.errors]
        info!(
            action.prefix_id = %action.prefix_id,
            action_desc,
            has_eval = request.eval.is_some(),
            "starting shipped observer evaluation"
        );
        let observer_result: Result<Option<TransportValue>, TransportPoisonError> =
            with_runner_progress_trace(&progress, RemoteTraceSpan::RunnerObserver, async {
                match request.eval.as_ref() {
                    Some(eval) => {
                        let observed: Option<Value> = with_runner_progress_trace(
                            &progress,
                            RemoteTraceSpan::RunnerObserverInvoke,
                            invoke_shipped_observer(
                                eval,
                                exit_code,
                                &output.stdout,
                                &output.stderr,
                                &PathValue::new(action.prefix_path.as_str()),
                            ),
                        )
                        .await?;
                        match observed {
                            Some(observed) => with_runner_progress_trace(
                                &progress,
                                RemoteTraceSpan::RunnerObserverCommit,
                                commit_value_to_transport(
                                    self.cas.as_ref(),
                                    observed,
                                    Some(&progress),
                                ),
                            )
                            .await
                            .map(Some),
                            None => Ok(None),
                        }
                    }
                    None => Ok(None),
                }
            })
            .await;
        info!(
            action.prefix_id = %action.prefix_id,
            action_desc,
            observer_ok = observer_result.is_ok(),
            "finished shipped observer evaluation"
        );

        let verification_error = match verification_task {
            Some(task) => match task.await {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(error) => {
                    self.remove_action_prefix(&action.prefix_id).await;
                    return run_failure_result(
                        start,
                        exit_code,
                        output.stdout.clone(),
                        output.stderr.clone(),
                        Some(format!("failed to join exec verification task: {error}")),
                    );
                }
            },
            None => None,
        };

        if let Some(error) = verification_error {
            let mut message = format!("exec verification failed: {error}");
            if let Err(observer_error) = &observer_result {
                message.push_str("\nobserver error:\n");
                message.push_str(&observer_error.to_string());
            }
            let message = append_process_output(message, &output.stdout, &output.stderr);
            self.remove_action_prefix(&action.prefix_id).await;
            return run_failure_result_with_kind(
                start,
                exit_code,
                output.stdout.clone(),
                output.stderr.clone(),
                Some(RunFailureKind::VerificationFailure),
                Some(message),
                None,
            );
        }

        let observer_value = match observer_result {
            Ok(observer_value) => observer_value,
            Err(error) => {
                let message = if output.exit_status.success() {
                    append_process_output(
                        format!("failed to evaluate shipped observer: {error}"),
                        &output.stdout,
                        &output.stderr,
                    )
                } else {
                    append_process_output(
                        format!(
                            "process exited with status {exit_code}; observer reported: {error}"
                        ),
                        &output.stdout,
                        &output.stderr,
                    )
                };
                let message =
                    append_similar_missing_input_hint(message, &action.prefix_path, &output.stderr);
                self.remove_action_prefix(&action.prefix_id).await;
                return run_failure_result_with_kind(
                    start,
                    exit_code,
                    output.stdout.clone(),
                    output.stderr.clone(),
                    Some(RunFailureKind::User),
                    Some(message),
                    Some(error),
                );
            }
        };
        if !output.exit_status.success() && observer_value.is_none() {
            info!(
                action.prefix_id = %action.prefix_id,
                action_desc,
                "skipping output flush because process failed without observer value"
            );
            self.remove_action_prefix(&action.prefix_id).await;
            return run_failure_result(start, exit_code, output.stdout, output.stderr, None);
        }

        info!(
            action.prefix_id = %action.prefix_id,
            action_desc,
            "removing action prefix"
        );
        let cleanup_service = self.clone();
        let cleanup_prefix_id = action.prefix_id.clone();
        let cleanup_action_desc = action_desc.to_string();
        moire::task::spawn(
            async move {
                cleanup_service
                    .remove_action_prefix(&cleanup_prefix_id)
                    .await;
                info!(
                    action.prefix_id = %cleanup_prefix_id,
                    action_desc = cleanup_action_desc,
                    "removed action prefix"
                );
            }
            .named("runner.run_vfs.cleanup_prefix"),
        );

        run_success_result(
            start,
            exit_code,
            output.stdout,
            output.stderr,
            observer_value,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Arc;
    use tempfile::TempDir;
    use vx_eval::{Engine, ExecutorSelector, HostObservation, lower_source_file_with_source_id};
    use vx_parser::SourceFile;
    use vx_store_proto::Blake3Hash;

    #[test]
    // r[verify exec.mount]
    fn path_coverage_uses_mounts_and_writable_roots() {
        let tree_mounts = vec![RunTreeMount {
            mount: "src".to_string(),
            manifest: Blake3Hash([1; 32]),
            mode: RunMountMode::Tracked,
        }];
        let file_mounts = vec![RunFileMount {
            path: "deps/libutil.rlib".to_string(),
            manifest: Blake3Hash([2; 32]),
            logical: "rlib".to_string(),
            mode: RunMountMode::Tracked,
        }];
        let writable_roots = vec!["out".to_string()];

        assert!(path_is_covered(
            "src/main.rs",
            &tree_mounts,
            &file_mounts,
            &writable_roots,
        ));
        assert!(path_is_covered(
            "deps/libutil.rlib",
            &tree_mounts,
            &file_mounts,
            &writable_roots,
        ));
        assert!(path_is_covered(
            "out/app",
            &tree_mounts,
            &file_mounts,
            &writable_roots,
        ));
        assert!(!path_is_covered(
            "tmp/scratch",
            &tree_mounts,
            &file_mounts,
            &writable_roots,
        ));
    }

    #[test]
    // r[verify exec.mount]
    fn vfs_paths_must_be_relative() {
        assert!(is_relative_vfs_path("src"));
        assert!(is_relative_vfs_path("out/app"));
        assert!(!is_relative_vfs_path(""));
        assert!(!is_relative_vfs_path("/src"));
        assert!(!is_relative_vfs_path("../src"));
    }

    #[test]
    fn root_tree_mounts_are_allowed() {
        assert!(is_tree_mount_path(""));
        assert!(is_tree_mount_path("src"));
        assert!(!is_tree_mount_path("/src"));
    }

    #[test]
    fn parent_dirs_are_derived_from_path_inputs() {
        let request = RunRequest {
            executor: ExecutorSelector::class("default"),
            program: RunArg::literal("cc"),
            args: vec![
                RunArg::literal("-c"),
                RunArg::path("src/lua.c"),
                RunArg::literal("-o"),
                RunArg::path("out/obj/lua.c.o"),
            ],
            tree_mounts: Vec::new(),
            file_mounts: Vec::new(),
            writable_roots: vec!["out".to_string()],
            create_dirs: Vec::new(),
            cwd: Some("build/tmp".to_string()),
            env: vec![("LUA_OUTPUT".to_string(), RunArg::path("gen/config.h"))],
            clear_env: false,
            eval: None,
            macos_commandlinetools_version: None,
        };

        assert_eq!(
            parent_dirs_for_request(&request),
            vec![
                "build".to_string(),
                "gen".to_string(),
                "out/obj".to_string(),
                "src".to_string(),
            ],
        );
    }

    #[test]
    fn parent_dirs_skip_paths_already_provided_by_mounts() {
        let request = RunRequest {
            executor: ExecutorSelector::class("default"),
            program: RunArg::literal("cc"),
            args: vec![
                RunArg::literal("-c"),
                RunArg::path("src/lua.c"),
                RunArg::literal("-o"),
                RunArg::path("out/obj/lua.c.o"),
            ],
            tree_mounts: vec![RunTreeMount {
                mount: "src".to_string(),
                manifest: Blake3Hash([1; 32]),
                mode: RunMountMode::Tracked,
            }],
            file_mounts: vec![RunFileMount {
                path: "gen/config.h".to_string(),
                manifest: Blake3Hash([2; 32]),
                logical: "config".to_string(),
                mode: RunMountMode::Tracked,
            }],
            writable_roots: vec!["out".to_string()],
            create_dirs: Vec::new(),
            cwd: Some("build/tmp".to_string()),
            env: vec![("CONFIG".to_string(), RunArg::path("gen/config.h"))],
            clear_env: false,
            eval: None,
            macos_commandlinetools_version: None,
        };

        assert_eq!(
            parent_dirs_to_materialize(&request),
            vec!["build".to_string(), "out/obj".to_string()],
        );
    }

    #[test]
    fn root_tree_mount_does_not_hide_nested_writable_dirs() {
        let request = RunRequest {
            executor: ExecutorSelector::class("default"),
            program: RunArg::literal("cc"),
            args: vec![
                RunArg::literal("-c"),
                RunArg::path("src/hello.c"),
                RunArg::literal("-o"),
                RunArg::path("out/obj/hello.o"),
            ],
            tree_mounts: vec![RunTreeMount {
                mount: String::new(),
                manifest: Blake3Hash([1; 32]),
                mode: RunMountMode::Tracked,
            }],
            file_mounts: Vec::new(),
            writable_roots: vec!["out".to_string()],
            create_dirs: Vec::new(),
            cwd: None,
            env: Vec::new(),
            clear_env: false,
            eval: None,
            macos_commandlinetools_version: None,
        };

        assert_eq!(
            parent_dirs_to_materialize(&request),
            vec!["out/obj".to_string()]
        );
    }

    #[test]
    fn explicit_create_dirs_are_preserved() {
        let request = RunRequest {
            executor: ExecutorSelector::class("default"),
            program: RunArg::literal("cl.exe"),
            args: vec![
                RunArg::literal("/nologo"),
                RunArg::literal("/c"),
                RunArg::path("src/hello.c"),
                RunArg::literal("/Foout\\obj\\hello.obj"),
            ],
            tree_mounts: vec![RunTreeMount {
                mount: "src".to_string(),
                manifest: Blake3Hash([1; 32]),
                mode: RunMountMode::Tracked,
            }],
            file_mounts: Vec::new(),
            writable_roots: vec!["out".to_string()],
            create_dirs: vec![
                "out/obj".to_string(),
                "out/obj".to_string(),
                "out/obj/nested".to_string(),
            ],
            cwd: None,
            env: Vec::new(),
            clear_env: false,
            eval: None,
            macos_commandlinetools_version: None,
        };

        assert_eq!(
            explicit_dirs_to_materialize(&request),
            vec!["out/obj".to_string(), "out/obj/nested".to_string()],
        );
        assert!(parent_dirs_to_materialize(&request).is_empty());
    }

    #[test]
    fn sandbox_write_root_uses_single_writable_root() {
        let request = RunRequest {
            executor: ExecutorSelector::class("default"),
            program: RunArg::path("bin/png-version"),
            args: Vec::new(),
            tree_mounts: Vec::new(),
            file_mounts: Vec::new(),
            writable_roots: vec!["bin".to_string()],
            create_dirs: Vec::new(),
            cwd: None,
            env: Vec::new(),
            clear_env: false,
            eval: None,
            macos_commandlinetools_version: None,
        };

        assert_eq!(sandbox_write_root(&request), Some("bin".to_string()));
    }

    #[test]
    fn sandbox_write_root_uses_common_ancestor_for_multiple_roots() {
        let request = RunRequest {
            executor: ExecutorSelector::class("default"),
            program: RunArg::literal("tool"),
            args: Vec::new(),
            tree_mounts: Vec::new(),
            file_mounts: Vec::new(),
            writable_roots: vec!["out/bin".to_string(), "out/lib".to_string()],
            create_dirs: Vec::new(),
            cwd: None,
            env: Vec::new(),
            clear_env: false,
            eval: None,
            macos_commandlinetools_version: None,
        };

        assert_eq!(sandbox_write_root(&request), Some("out".to_string()));
    }

    #[test]
    fn sandbox_write_root_falls_back_when_roots_do_not_share_prefix() {
        let request = RunRequest {
            executor: ExecutorSelector::class("default"),
            program: RunArg::literal("tool"),
            args: Vec::new(),
            tree_mounts: Vec::new(),
            file_mounts: Vec::new(),
            writable_roots: vec!["out".to_string(), "bin".to_string()],
            create_dirs: Vec::new(),
            cwd: None,
            env: Vec::new(),
            clear_env: false,
            eval: None,
            macos_commandlinetools_version: None,
        };

        assert_eq!(sandbox_write_root(&request), None);
    }

    #[test]
    fn relative_program_paths_resolve_within_sandbox_cwd() {
        let cwd = Utf8Path::new("/sandbox/prefix");
        let program = RunArg::path("bin/lua");

        assert_eq!(
            resolve_program_path(&program, "bin/lua".to_string(), cwd),
            "/sandbox/prefix/bin/lua"
        );
    }

    #[test]
    fn literal_programs_do_not_resolve_within_sandbox_cwd() {
        let cwd = Utf8Path::new("/sandbox/prefix");
        let program = RunArg::literal("cc");

        assert_eq!(resolve_program_path(&program, "cc".to_string(), cwd), "cc");
    }

    #[test]
    fn opaque_tree_paths_resolve_to_shared_visible_root() {
        let mount_base = Utf8Path::new("/tmp/vx/vfs");
        let tree_mounts = vec![RunTreeMount {
            mount: "toolchain".to_string(),
            manifest: Blake3Hash([7; 32]),
            mode: RunMountMode::Opaque,
        }];

        let resolved =
            resolve_opaque_visible_path(mount_base, "toolchain/bin/cc", &tree_mounts, &[])
                .expect("opaque path should resolve");

        assert_eq!(
            resolved,
            mount_base
                .join(opaque_tree_prefix_id(&tree_mounts[0]))
                .join("bin/cc")
        );
    }

    #[test]
    fn opaque_file_paths_resolve_to_shared_visible_file() {
        let mount_base = Utf8Path::new("/tmp/vx/vfs");
        let file_mounts = vec![RunFileMount {
            path: "deps/liblua.a".to_string(),
            manifest: Blake3Hash([8; 32]),
            logical: "archive".to_string(),
            mode: RunMountMode::Opaque,
        }];

        let resolved = resolve_opaque_visible_path(mount_base, "deps/liblua.a", &[], &file_mounts)
            .expect("opaque file should resolve");

        assert_eq!(
            resolved,
            mount_base
                .join(opaque_file_prefix_id(&file_mounts[0]))
                .join("liblua.a")
        );
    }

    #[test]
    fn opaque_args_render_as_shared_absolute_paths() {
        let mount_base = Utf8Path::new("/tmp/vx/vfs");
        let tree_mounts = vec![RunTreeMount {
            mount: "toolchain".to_string(),
            manifest: Blake3Hash([9; 32]),
            mode: RunMountMode::Opaque,
        }];
        let arg = RunArg::path("toolchain/bin/clang");

        let rendered = render_run_arg_for_host(&arg, mount_base, &tree_mounts, &[]);

        assert_eq!(
            rendered,
            host_path_string(
                &mount_base
                    .join(opaque_tree_prefix_id(&tree_mounts[0]))
                    .join("bin/clang")
            )
        );
    }

    #[test]
    fn opaque_cwd_resolves_to_shared_absolute_path() {
        let request = RunRequest {
            executor: ExecutorSelector::class("default"),
            program: RunArg::literal("clang"),
            args: Vec::new(),
            tree_mounts: vec![RunTreeMount {
                mount: "toolchain".to_string(),
                manifest: Blake3Hash([10; 32]),
                mode: RunMountMode::Opaque,
            }],
            file_mounts: Vec::new(),
            writable_roots: vec!["out".to_string()],
            create_dirs: Vec::new(),
            cwd: Some("toolchain/bin".to_string()),
            env: Vec::new(),
            clear_env: false,
            eval: None,
            macos_commandlinetools_version: None,
        };
        let action = ActionContext {
            prefix_id: "build-123".into(),
            mount_base: Utf8PathBuf::from("/tmp/vx/vfs"),
            prefix_path: Utf8PathBuf::from("/tmp/vx/vfs/build-123"),
            write_path: Utf8PathBuf::from("/tmp/vx/vfs/build-123/out"),
            allowed_host_exec_paths: Vec::new(),
        };

        let cwd = resolve_cwd_path(&request, &action);

        assert_eq!(
            cwd,
            action
                .mount_base
                .join(opaque_tree_prefix_id(&request.tree_mounts[0]))
                .join("bin")
        );
    }

    #[test]
    fn writable_roots_must_not_overlap_opaque_mounts() {
        let tree_mounts = vec![RunTreeMount {
            mount: "toolchain".to_string(),
            manifest: Blake3Hash([11; 32]),
            mode: RunMountMode::Opaque,
        }];
        let file_mounts = vec![RunFileMount {
            path: "sdk/lib/libcmt.lib".to_string(),
            manifest: Blake3Hash([12; 32]),
            logical: "archive".to_string(),
            mode: RunMountMode::Opaque,
        }];

        assert!(opaque_mount_overlap(
            "toolchain/out",
            &tree_mounts,
            &file_mounts
        ));
        assert!(opaque_mount_overlap("sdk/lib", &tree_mounts, &file_mounts));
        assert!(!opaque_mount_overlap("out", &tree_mounts, &file_mounts));
    }

    #[test]
    fn cached_discover_values_require_valid_obligations() {
        let tempdir = TempDir::new().expect("tempdir");
        let path = tempdir.path().join("tool.txt");
        fs::write(&path, "toolchain-v1").expect("write host file");
        let expected_hash = blake3::hash(b"toolchain-v1").to_hex().to_string();

        let cached = TransportValue::Discovered {
            value: Box::new(TransportValue::String("ok".into())),
            obligations: vec![HostObservation::Read {
                path: path.to_string_lossy().to_string(),
                content_hash: expected_hash,
            }],
        };

        assert!(cached_discover_value_is_valid(&cached));

        fs::write(&path, "toolchain-v2").expect("rewrite host file");

        assert!(!cached_discover_value_is_valid(&cached));
    }

    #[tokio::test]
    async fn discover_request_cache_key_handles_toolchain_probe_shape() {
        let source = r#"
struct Toolchain {
  os: String,
  arch: String,
  cc: String,
  archive: String,
}

fn probe(host: Host) {
  let os = host.os();
  let arch = host.arch();
  let cc = host.which("cc");
  let archive = host.which("ar");
  Toolchain { os, arch, cc, archive }
}

fn make() { probe }
"#;
        let engine = shipped_function_engine(source);
        let request = DiscoverRequest {
            executor: ExecutorSelector::class("default"),
            module: engine.transport_module(),
            probe: engine
                .ship_function(
                    engine
                        .call("make", vec![])
                        .await
                        .expect("make should evaluate"),
                )
                .await
                .expect("probe should ship"),
        };

        let key = discover_request_cache_key(&request).expect("cache key should serialize");
        assert!(!key.is_empty());
    }

    fn shipped_function_engine(source: &str) -> Arc<Engine> {
        let parse = vx_parser::parse(source);
        assert_eq!(parse.errors(), []);
        let root = SourceFile::cast_root(parse.syntax());
        let module =
            lower_source_file_with_source_id(&root, "<test>").expect("lowering should succeed");
        Engine::from_module(&module)
    }

    #[test]
    fn picks_similar_missing_sandbox_path_with_extra_prefix_segment() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let prefix_path = Utf8Path::from_path(temp_dir.path()).expect("utf8 temp dir");
        std::fs::create_dir_all(prefix_path.join("src/src")).expect("create src tree");
        std::fs::create_dir_all(prefix_path.join("deps/src")).expect("create deps tree");
        std::fs::create_dir_all(prefix_path.join("out/src")).expect("create out tree");
        std::fs::write(prefix_path.join("src/src/lua.c"), "").expect("write candidate");
        std::fs::write(prefix_path.join("deps/src/lua.c"), "").expect("write distractor");
        std::fs::write(prefix_path.join("out/src/lua.c"), "").expect("write output distractor");

        assert_eq!(
            find_similar_sandbox_path(prefix_path, "src/lua.c"),
            Some("src/src/lua.c".to_string())
        );
    }

    #[test]
    fn appends_similar_missing_input_hint_from_stderr() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let prefix_path = Utf8Path::from_path(temp_dir.path()).expect("utf8 temp dir");
        std::fs::create_dir_all(prefix_path.join("src/src")).expect("create src tree");
        std::fs::write(prefix_path.join("src/src/lua.c"), "").expect("write candidate");

        let message = append_similar_missing_input_hint(
            "process exited with status 1".to_string(),
            prefix_path,
            "clang: error: no such file or directory: 'src/lua.c'\nclang: error: no input files\n",
        );

        assert!(
            message.contains("similar sandbox path: src/src/lua.c"),
            "{message}"
        );
    }
}
