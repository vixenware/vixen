//! Private sandboxed executor implementation for the public runner protocol.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::process::Output;
use std::sync::Arc;
use std::time::Duration;

use vx_services::{
    BlobRef, ByteLen, CapabilityProbeRequest, CapabilityProbeResult, DirectoryEntryKind, ExecArg,
    ExecBlake3Hash as Blake3Hash, ExecCompletion, ExecContentHash as ContentHash, ExecDiagnostic,
    ExecError, ExecErrorKind, ExecExitCode, ExecMount, ExecNodeHash as NodeHash,
    ExecObservationScope, ExecOutcome, ExecPath, ExecPlan, ExecPrefixId, ExecReadObservation,
    ExecReadSet, ExecRequest, ExecResult, ExecRole, ExecSubfileCompletion, ExecTree, ExecTreeEvent,
    ExecTreeFinalization, NodeHash as StoreNodeHash, PlatformArch, PlatformName,
    RUNNER_PROTOCOL_VERSION, Runner, RunnerCapabilities, RunnerPlatform, RunnerTransport,
    SandboxCapability, StoreClient, ToolchainCapability, ToolchainKind,
    cas_tree::{MerkleTreeFile, build_merkle_tree},
    chunking::{prepare_blob, store_prepared_blob},
    exec_role_embedded_paths, exec_role_env_paths, exec_role_input_paths,
    exec_role_output_dir_paths, exec_role_output_paths, exec_role_search_dir_paths,
    exec_role_stdout_paths,
};
use vx_vfsd::protocol::{VfsTrackedObservation, VfsdClient};

const ENV_ALLOWLIST: &[&str] = &["PATH", "HOME", "TMPDIR", "TEMP", "TMP", "SystemRoot"];

pub type ExecObservationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<ExecReadSet>, ExecError>> + Send + 'a>>;

pub trait ExecObservationFeed: Send + Sync {
    fn read_set<'a>(
        &'a self,
        prefix_id: &'a ExecPrefixId,
        roots: &'a [ExecPath],
    ) -> ExecObservationFuture<'a>;
}

#[derive(Debug, Clone)]
pub struct FskitObservationFeed {
    vfsd_addr: String,
    connect_timeout: Duration,
    call_timeout: Duration,
}

impl FskitObservationFeed {
    pub fn new(vfsd_addr: impl Into<String>) -> Self {
        Self {
            vfsd_addr: vfsd_addr.into(),
            connect_timeout: Duration::from_secs(2),
            call_timeout: Duration::from_secs(5),
        }
    }

    pub fn with_timeouts(mut self, connect_timeout: Duration, call_timeout: Duration) -> Self {
        self.connect_timeout = connect_timeout;
        self.call_timeout = call_timeout;
        self
    }

    async fn client(&self) -> Result<VfsdClient, ExecError> {
        vox::connect_lane::<VfsdClient>(&self.vfsd_addr)
            .connect_timeout(self.connect_timeout)
            .wait_for_service(self.connect_timeout)
            .await
            .map_err(|err| {
                exec_error(
                    ExecErrorKind::Protocol,
                    format!("fskit observation feed connect {}: {err}", self.vfsd_addr),
                )
            })
    }

    async fn read_set_async(
        &self,
        prefix_id: &ExecPrefixId,
        roots: &[ExecPath],
    ) -> Result<Option<ExecReadSet>, ExecError> {
        let roots = exec_roots_to_vfs_roots(roots).map_err(|err| {
            exec_error(
                ExecErrorKind::Protocol,
                format!("fskit observation feed roots: {err}"),
            )
        })?;
        let client = self.client().await?;
        let observations = tokio::time::timeout(
            self.call_timeout,
            client.tracked_observations(prefix_id.0.clone(), roots),
        )
        .await
        .map_err(|_| {
            exec_error(
                ExecErrorKind::Protocol,
                format!(
                    "fskit observation feed tracked_observations timed out after {:?}",
                    self.call_timeout
                ),
            )
        })?
        .map_err(|err| {
            exec_error(
                ExecErrorKind::Protocol,
                format!("fskit observation feed tracked_observations: {err}"),
            )
        })?;
        Ok(Some(vfs_tracked_observations_to_exec_read_set(
            observations,
        )))
    }
}

impl ExecObservationFeed for FskitObservationFeed {
    fn read_set<'a>(
        &'a self,
        prefix_id: &'a ExecPrefixId,
        roots: &'a [ExecPath],
    ) -> ExecObservationFuture<'a> {
        Box::pin(async move { self.read_set_async(prefix_id, roots).await })
    }
}

#[derive(Debug, Clone)]
pub struct FskitVfsExecutor {
    vfsd_addr: String,
    store_addr: String,
    access_path: PathBuf,
    observation_feed: FskitObservationFeed,
    connect_timeout: Duration,
    call_timeout: Duration,
}

impl FskitVfsExecutor {
    pub fn new(
        vfsd_addr: impl Into<String>,
        store_addr: impl Into<String>,
        access_path: impl Into<PathBuf>,
    ) -> Self {
        let vfsd_addr = vfsd_addr.into();
        Self {
            observation_feed: FskitObservationFeed::new(vfsd_addr.clone()),
            vfsd_addr,
            store_addr: store_addr.into(),
            access_path: access_path.into(),
            connect_timeout: Duration::from_secs(2),
            call_timeout: Duration::from_secs(5),
        }
    }

    pub fn with_timeouts(mut self, connect_timeout: Duration, call_timeout: Duration) -> Self {
        self.connect_timeout = connect_timeout;
        self.call_timeout = call_timeout;
        self.observation_feed = self
            .observation_feed
            .with_timeouts(connect_timeout, call_timeout);
        self
    }

    async fn vfsd_client(&self) -> Result<VfsdClient, ExecError> {
        vox::connect_lane::<VfsdClient>(&self.vfsd_addr)
            .connect_timeout(self.connect_timeout)
            .wait_for_service(self.connect_timeout)
            .await
            .map_err(|err| {
                exec_error(
                    ExecErrorKind::Protocol,
                    format!("fskit vfs executor connect vfsd {}: {err}", self.vfsd_addr),
                )
            })
    }

    async fn store_client(&self) -> Result<StoreClient, ExecError> {
        vox::connect_lane::<StoreClient>(&self.store_addr)
            .connect_timeout(self.connect_timeout)
            .wait_for_service(self.connect_timeout)
            .await
            .map_err(|err| {
                exec_error(
                    ExecErrorKind::Protocol,
                    format!(
                        "fskit vfs executor connect store {}: {err}",
                        self.store_addr
                    ),
                )
            })
    }
}

#[derive(Clone)]
pub struct SandboxedExecutor {
    observation_feed: Arc<dyn ExecObservationFeed>,
}

impl SandboxedExecutor {
    pub fn new() -> Self {
        Self {
            observation_feed: Arc::new(NoExecObservationFeed),
        }
    }

    pub fn with_observation_feed(feed: impl ExecObservationFeed + 'static) -> Self {
        Self {
            observation_feed: Arc::new(feed),
        }
    }

    pub fn exec_blocking(&self, request: ExecRequest) -> ExecResult {
        match run_request_blocking(request) {
            Ok(result) => result,
            Err(error) => failed_without_process(error),
        }
    }
}

impl Default for SandboxedExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl Runner for SandboxedExecutor {
    async fn exec(&self, request: ExecRequest) -> ExecResult {
        match run_request(request, self.observation_feed.as_ref()).await {
            Ok(result) => result,
            Err(error) => failed_without_process(error),
        }
    }

    async fn capabilities(&self, _request: CapabilityProbeRequest) -> CapabilityProbeResult {
        CapabilityProbeResult {
            capabilities: detect_capabilities(),
        }
    }
}

impl Runner for FskitVfsExecutor {
    async fn exec(&self, request: ExecRequest) -> ExecResult {
        match run_fskit_vfs_request(request, self).await {
            Ok(result) => result,
            Err(error) => failed_without_process(error),
        }
    }

    async fn capabilities(&self, _request: CapabilityProbeRequest) -> CapabilityProbeResult {
        CapabilityProbeResult {
            capabilities: detect_capabilities(),
        }
    }
}

struct NoExecObservationFeed;

impl ExecObservationFeed for NoExecObservationFeed {
    fn read_set<'a>(
        &'a self,
        _prefix_id: &'a ExecPrefixId,
        _roots: &'a [ExecPath],
    ) -> ExecObservationFuture<'a> {
        Box::pin(async { Ok(None) })
    }
}

async fn run_request(
    request: ExecRequest,
    observation_feed: &dyn ExecObservationFeed,
) -> Result<ExecResult, ExecError> {
    let prepared = match run_request_to_harvest(request)? {
        PreparedExecCompletion::Succeeded(prepared) => prepared,
        PreparedExecCompletion::Failed(result) => return Ok(result),
    };
    let read_set = match observation_feed
        .read_set(&prepared.scope_id, &prepared.observation_roots)
        .await
        .map_err(observation_error)?
    {
        Some(read_set) => read_set,
        None => prepared.staged_read_set.clone(),
    };
    Ok(prepared.into_result(read_set))
}

fn run_request_blocking(request: ExecRequest) -> Result<ExecResult, ExecError> {
    match run_request_to_harvest(request)? {
        PreparedExecCompletion::Succeeded(prepared) => {
            let read_set = prepared.staged_read_set.clone();
            Ok(prepared.into_result(read_set))
        }
        PreparedExecCompletion::Failed(result) => Ok(result),
    }
}

async fn run_fskit_vfs_request(
    request: ExecRequest,
    executor: &FskitVfsExecutor,
) -> Result<ExecResult, ExecError> {
    let vfsd = executor.vfsd_client().await?;
    let store = executor.store_client().await?;
    let scope_id = ExecPrefixId(fskit_prefix_id());

    materialize_vfs_mounts(&vfsd, &store, &scope_id, &request, executor.call_timeout).await?;
    prepare_vfs_output_dirs(&vfsd, &scope_id, &request.plan, executor.call_timeout).await?;

    let prefix_root = executor.access_path.join(&scope_id.0);
    let env = command_env(&request.plan, &prefix_root).map_err(stage_error)?;
    let (program, argv) =
        command_program_and_argv(&request, &env, &prefix_root).map_err(stage_error)?;
    let write_roots = vfs_write_roots(&request.plan, &prefix_root).map_err(stage_error)?;
    let output = tokio::task::spawn_blocking({
        let program = program.clone();
        let argv = argv.clone();
        let env = env.clone();
        let prefix_root = prefix_root.clone();
        let toolchain_roots = request.toolchain_roots.clone();
        move || {
            run_sandboxed_process_without_canonicalizing_profile(
                &program,
                &argv,
                Path::new("/"),
                &env,
                &prefix_root,
                &write_roots,
                &toolchain_roots,
            )
        }
    })
    .await
    .map_err(|err| exec_error(ExecErrorKind::Spawn, format!("fskit vfs exec task: {err}")))?
    .map_err(spawn_or_unsupported_error)?;

    let exit_code = status_code(&output);
    if !output.status.success() {
        let stderr = output.stderr;
        let stdout = output.stdout;
        let _ = timeout_vfs_call(executor.call_timeout, vfsd.rm_rf(prefix_path(&scope_id))).await;
        let error = exec_error(
            ExecErrorKind::ProcessExit,
            format!(
                "fskit vfs exec {} exited with {}: {}",
                program.display(),
                exit_code,
                String::from_utf8_lossy(&stderr)
            ),
        );
        return Ok(ExecResult {
            exit_code: ExecExitCode(exit_code),
            stdout,
            stderr,
            completion: ExecCompletion::Failed(error),
        });
    }

    let harvested =
        harvest_outputs(&request.plan, &prefix_root, &output.stdout).map_err(harvest_error)?;
    verify_subfile_completions_at_exit(&prefix_root, &harvested.tree, &harvested.subfiles)
        .map_err(verification_error)?;
    let read_set = executor
        .observation_feed
        .read_set_async(&scope_id, &observation_roots(&request.mounts))
        .await?
        .ok_or_else(|| {
            exec_error(
                ExecErrorKind::Protocol,
                "fskit observation feed returned no read set",
            )
        })?;
    let tree_events = tree_events_from_subfiles(harvested.subfiles.clone());
    let _ = timeout_vfs_call(executor.call_timeout, vfsd.rm_rf(prefix_path(&scope_id))).await;

    Ok(ExecResult {
        exit_code: ExecExitCode(exit_code),
        stdout: output.stdout,
        stderr: output.stderr,
        completion: ExecCompletion::Succeeded(ExecOutcome {
            outputs: harvested.tree,
            read_set,
            observation_scopes: observation_scopes(&scope_id, &request.mounts),
            tree_events,
        }),
    })
}

async fn materialize_vfs_mounts(
    vfsd: &VfsdClient,
    store: &StoreClient,
    scope_id: &ExecPrefixId,
    request: &ExecRequest,
    call_timeout: Duration,
) -> Result<(), ExecError> {
    for mount in &request.mounts {
        let executable_paths =
            executable_paths_under_mount(&request.plan, &mount.at).map_err(stage_error)?;
        let root = exec_tree_to_cas(store, &mount.tree, &executable_paths)
            .await
            .map_err(stage_error)?;
        let rel = exec_path_to_vfs_path(mount.at.as_str()).map_err(stage_error)?;
        let path = prefix_child_path(scope_id, &rel);
        timeout_vfs_call(call_timeout, vfsd.mount(path, root.0))
            .await
            .map_err(stage_error)?;
    }
    Ok(())
}

async fn prepare_vfs_output_dirs(
    vfsd: &VfsdClient,
    scope_id: &ExecPrefixId,
    plan: &ExecPlan,
    call_timeout: Duration,
) -> Result<(), ExecError> {
    for ExecArg { value, role } in &plan.argv {
        let value = value.as_str();
        for output in exec_role_output_paths(value, *role) {
            let parent = parent_logical_path(output.as_str());
            let rel = exec_path_to_vfs_path(&parent).map_err(stage_error)?;
            if !rel.is_empty() {
                timeout_vfs_call(call_timeout, vfsd.mkdir(prefix_child_path(scope_id, &rel)))
                    .await
                    .map_err(stage_error)?;
            }
        }
        for output_dir in exec_role_output_dir_paths(value, *role) {
            let rel = exec_path_to_vfs_path(output_dir.as_str()).map_err(stage_error)?;
            timeout_vfs_call(call_timeout, vfsd.mkdir(prefix_child_path(scope_id, &rel)))
                .await
                .map_err(stage_error)?;
        }
    }
    Ok(())
}

async fn timeout_vfs_call<T, E>(
    timeout: Duration,
    call: impl Future<Output = Result<T, E>>,
) -> Result<T, String>
where
    E: std::fmt::Debug,
{
    tokio::time::timeout(timeout, call)
        .await
        .map_err(|_| format!("vx-vfsd call timed out after {timeout:?}"))?
        .map_err(|err| format!("{err:?}"))
}

fn vfs_write_roots(plan: &ExecPlan, prefix_root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut roots = Vec::new();
    for ExecArg { value, role } in &plan.argv {
        let value = value.as_str();
        for output in exec_role_output_paths(value, *role) {
            let path = physical_path(output.as_str(), prefix_root)?;
            if let Some(parent) = path.parent() {
                roots.push(parent.to_path_buf());
            }
        }
        for output in exec_role_output_dir_paths(value, *role) {
            roots.push(physical_path(output.as_str(), prefix_root)?);
        }
    }
    roots.push(prefix_root.join("tmp"));
    Ok(dedup_paths(roots))
}

fn executable_paths_under_mount(
    plan: &ExecPlan,
    mount_at: &ExecPath,
) -> Result<BTreeSet<String>, String> {
    let mut paths = BTreeSet::new();
    let mount_rel = exec_path_to_vfs_path(mount_at.as_str())?;
    let mount_prefix = if mount_rel.is_empty() {
        String::new()
    } else {
        format!("{mount_rel}/")
    };
    for ExecArg { value, role } in &plan.argv {
        if *role != ExecRole::Executable {
            continue;
        }
        let executable = exec_path_to_vfs_path(value.as_str())?;
        if mount_rel.is_empty() {
            paths.insert(executable);
        } else if let Some(relative) = executable.strip_prefix(&mount_prefix) {
            paths.insert(relative.to_string());
        }
    }
    Ok(paths)
}

async fn exec_tree_to_cas(
    store: &StoreClient,
    tree: &ExecTree,
    executable_paths: &BTreeSet<String>,
) -> Result<StoreNodeHash, String> {
    let mut files = Vec::with_capacity(tree.entries.len() + tree.blobs.len());
    for (path, text) in &tree.entries {
        files.push(
            exec_tree_file(
                store,
                path.as_str(),
                text.0.as_bytes(),
                executable_paths.contains(path.as_str()),
            )
            .await?,
        );
    }
    for (path, bytes) in &tree.blobs {
        files.push(
            exec_tree_file(
                store,
                path.as_str(),
                bytes,
                executable_paths.contains(path.as_str()),
            )
            .await?,
        );
    }
    let (root, _total_size) = build_merkle_tree(store, files).await?;
    Ok(root)
}

async fn exec_tree_file(
    store: &StoreClient,
    path: &str,
    bytes: &[u8],
    executable: bool,
) -> Result<MerkleTreeFile, String> {
    let relative = exec_path_to_vfs_path(path)?;
    let prepared = prepare_blob(bytes);
    let blob = store_prepared_blob(store, &prepared, bytes).await?;
    Ok(MerkleTreeFile {
        path: relative,
        kind: blob_to_directory_entry_kind(&blob, executable),
    })
}

fn blob_to_directory_entry_kind(blob: &BlobRef, executable: bool) -> DirectoryEntryKind {
    blob.to_dir_entry_kind(executable)
}

enum PreparedExecCompletion {
    Succeeded(PreparedExecResult),
    Failed(ExecResult),
}

struct PreparedExecResult {
    exit_code: ExecExitCode,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    outputs: ExecTree,
    staged_read_set: ExecReadSet,
    observation_roots: Vec<ExecPath>,
    scope_id: ExecPrefixId,
    tree_events: Vec<ExecTreeEvent>,
}

impl PreparedExecResult {
    fn into_result(self, read_set: ExecReadSet) -> ExecResult {
        ExecResult {
            exit_code: self.exit_code,
            stdout: self.stdout,
            stderr: self.stderr,
            completion: ExecCompletion::Succeeded(ExecOutcome {
                outputs: self.outputs,
                read_set,
                observation_scopes: vec![ExecObservationScope {
                    prefix_id: self.scope_id,
                    roots: self.observation_roots,
                }],
                tree_events: self.tree_events,
            }),
        }
    }
}

fn run_request_to_harvest(request: ExecRequest) -> Result<PreparedExecCompletion, ExecError> {
    let temp = tempfile::Builder::new()
        .prefix("vx-sandboxed-exec-")
        .tempdir()
        .map_err(|err| {
            exec_error(
                ExecErrorKind::Staging,
                format!("sandboxed exec tempdir: {err}"),
            )
        })?;
    let root = temp.path();
    fs::create_dir_all(root.join(".vx-exec-cas")).map_err(|err| {
        exec_error(
            ExecErrorKind::Staging,
            format!("sandboxed exec cas dir: {err}"),
        )
    })?;
    fs::create_dir_all(root.join("tmp")).map_err(|err| {
        exec_error(
            ExecErrorKind::Staging,
            format!("sandboxed exec tmp dir: {err}"),
        )
    })?;
    let scope_id = ExecPrefixId(
        root.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("vx-sandboxed-exec")
            .to_string(),
    );

    let world = MountedWorld::new(&request.mounts);
    let mut observed = ObservedWorld::new(&world);
    stage_declared_inputs(&request.plan, &mut observed, root).map_err(stage_error)?;
    let mut write_roots = prepare_output_dirs(&request.plan, root).map_err(stage_error)?;
    write_roots.push(root.join("tmp"));

    let env = command_env(&request.plan, root).map_err(stage_error)?;
    let (program, argv) = command_program_and_argv(&request, &env, root).map_err(stage_error)?;
    let output = run_sandboxed_process(
        &program,
        &argv,
        root,
        &env,
        root,
        &write_roots,
        &request.toolchain_roots,
    )
    .map_err(spawn_or_unsupported_error)?;

    let exit_code = status_code(&output);
    if !output.status.success() {
        let stderr = output.stderr;
        let stdout = output.stdout;
        let error = exec_error(
            ExecErrorKind::ProcessExit,
            format!(
                "sandboxed exec {} exited with {}: {}",
                program.display(),
                exit_code,
                String::from_utf8_lossy(&stderr)
            ),
        );
        return Ok(PreparedExecCompletion::Failed(ExecResult {
            exit_code: ExecExitCode(exit_code),
            stdout,
            stderr,
            completion: ExecCompletion::Failed(error),
        }));
    }

    let harvested = harvest_outputs(&request.plan, root, &output.stdout).map_err(harvest_error)?;
    verify_subfile_completions_at_exit(root, &harvested.tree, &harvested.subfiles)
        .map_err(verification_error)?;
    let observation_roots = observation_roots(&request.mounts);
    Ok(PreparedExecCompletion::Succeeded(PreparedExecResult {
        exit_code: ExecExitCode(exit_code),
        stdout: output.stdout,
        stderr: output.stderr,
        outputs: harvested.tree,
        staged_read_set: observed.into_read_set(),
        observation_roots,
        scope_id,
        tree_events: tree_events_from_subfiles(harvested.subfiles.clone()),
    }))
}

fn failed_without_process(error: ExecError) -> ExecResult {
    ExecResult {
        exit_code: ExecExitCode(-1),
        stdout: Vec::new(),
        stderr: Vec::new(),
        completion: ExecCompletion::Failed(error),
    }
}

fn exec_error(kind: ExecErrorKind, diagnostic: impl Into<String>) -> ExecError {
    ExecError {
        kind,
        diagnostic: ExecDiagnostic(diagnostic.into()),
    }
}

fn stage_error(diagnostic: String) -> ExecError {
    exec_error(ExecErrorKind::Staging, diagnostic)
}

fn harvest_error(diagnostic: String) -> ExecError {
    exec_error(ExecErrorKind::Harvest, diagnostic)
}

fn verification_error(diagnostic: String) -> ExecError {
    exec_error(ExecErrorKind::Verification, diagnostic)
}

fn observation_error(error: ExecError) -> ExecError {
    exec_error(
        ExecErrorKind::Verification,
        format!("sandboxed exec observation feed: {}", error.diagnostic.0),
    )
}

fn spawn_or_unsupported_error(diagnostic: String) -> ExecError {
    let kind = if diagnostic.contains("implemented for macOS") {
        ExecErrorKind::UnsupportedPlatform
    } else {
        ExecErrorKind::Spawn
    };
    exec_error(kind, diagnostic)
}

fn detect_capabilities() -> RunnerCapabilities {
    RunnerCapabilities {
        protocol_version: RUNNER_PROTOCOL_VERSION,
        platform: RunnerPlatform {
            os: PlatformName(std::env::consts::OS.to_string()),
            arch: PlatformArch(std::env::consts::ARCH.to_string()),
        },
        transports: vec![
            RunnerTransport::VoxWebsocket,
            RunnerTransport::VoxLocal,
            RunnerTransport::VoxTcp,
        ],
        sandboxes: detected_sandboxes(),
        toolchains: detected_toolchains(),
    }
}

fn detected_sandboxes() -> Vec<SandboxCapability> {
    let mut sandboxes = Vec::new();
    if cfg!(target_os = "macos") && Path::new("/usr/bin/sandbox-exec").exists() {
        sandboxes.push(SandboxCapability::Seatbelt);
    }
    if cfg!(target_os = "linux") {
        sandboxes.push(SandboxCapability::Landlock);
    }
    sandboxes
}

fn detected_toolchains() -> Vec<ToolchainCapability> {
    [
        (ToolchainKind::Shell, "/bin/sh"),
        (ToolchainKind::CCompiler, "/usr/bin/cc"),
        (ToolchainKind::CCompiler, "/usr/bin/clang"),
        (ToolchainKind::Rustc, "/usr/bin/rustc"),
        (ToolchainKind::SandboxExec, "/usr/bin/sandbox-exec"),
    ]
    .into_iter()
    .filter_map(|(kind, path)| {
        let path_ref = Path::new(path);
        if !path_ref.exists() {
            return None;
        }
        Some(ToolchainCapability {
            kind,
            executable: ExecPath::from(path),
            content_hash: fs::read(path_ref)
                .ok()
                .map(|bytes| ContentHash(Blake3Hash::from_bytes(&bytes))),
        })
    })
    .collect()
}

fn stage_declared_inputs(
    plan: &ExecPlan,
    world: &mut ObservedWorld<'_>,
    root: &Path,
) -> Result<(), String> {
    for ExecArg { value, role } in &plan.argv {
        let value = value.as_str();
        match role {
            ExecRole::Executable | ExecRole::Input | ExecRole::InputFlag => {
                for input in exec_role_input_paths(value, *role) {
                    stage_declared_input(input.as_str(), world, root)?;
                }
            }
            ExecRole::SearchDir | ExecRole::SearchDirFlag => {
                for dir in exec_role_search_dir_paths(value, *role) {
                    let physical = physical_path(dir.as_str(), root)?;
                    fs::create_dir_all(&physical).map_err(|err| {
                        format!("sandboxed exec create search dir `{dir}`: {err}")
                    })?;
                    if let Some(names) = world.list(dir.as_str()) {
                        for name in names {
                            let logical = listed_logical_path(dir.as_str(), &name, world);
                            if let Some(bytes) = world.peek_bytes(&logical) {
                                stage_file(root, &logical, &bytes)?;
                            }
                        }
                    }
                }
            }
            ExecRole::Env => {
                for path in exec_role_env_paths(value, *role) {
                    stage_env_path(root, path.as_str(), world)?;
                }
            }
            ExecRole::Output
            | ExecRole::OutputFlag
            | ExecRole::OutputDir
            | ExecRole::Stdout
            | ExecRole::Flag => {}
        }
    }
    Ok(())
}

fn stage_env_path(root: &Path, path: &str, world: &mut ObservedWorld<'_>) -> Result<(), String> {
    if let Some(bytes) = world.read_bytes(path) {
        return stage_file(root, path, &bytes);
    }
    if let Some(names) = world.list(path) {
        for name in names {
            let logical = listed_logical_path(path, &name, world);
            if let Some(bytes) = world.peek_bytes(&logical) {
                stage_file(root, &logical, &bytes)?;
            }
        }
        return Ok(());
    }
    if world.saw_lookup_miss(path) {
        let parent = parent_logical_path(path);
        let physical_parent = physical_path(&parent, root)?;
        fs::create_dir_all(&physical_parent)
            .map_err(|err| format!("sandboxed exec create lookup-miss parent `{parent}`: {err}"))?;
    }
    Ok(())
}

fn stage_declared_input(
    input: &str,
    world: &mut ObservedWorld<'_>,
    root: &Path,
) -> Result<(), String> {
    if let Some(bytes) = world.read_bytes(input) {
        return stage_file(root, input, &bytes);
    }
    if let Some(names) = world.list(input) {
        for name in names {
            let logical = listed_logical_path(input, &name, world);
            let bytes = world.read_bytes(&logical).ok_or_else(|| {
                format!("sandboxed exec input `{logical}` vanished while staging `{input}`")
            })?;
            stage_file(root, &logical, &bytes)?;
        }
        return Ok(());
    }
    if world.saw_lookup_miss(input) {
        let parent = parent_logical_path(input);
        let physical_parent = physical_path(&parent, root)?;
        fs::create_dir_all(&physical_parent)
            .map_err(|err| format!("sandboxed exec create lookup-miss parent `{parent}`: {err}"))?;
        return Ok(());
    }
    Err(format!(
        "sandboxed exec input `{input}` is outside mounted trees"
    ))
}

fn listed_logical_path(dir: &str, name: &str, world: &ObservedWorld<'_>) -> String {
    let direct = format!("{}/{}", dir.trim_end_matches('/'), name);
    if world.peek_bytes(&direct).is_some() {
        return direct;
    }
    if let Some(root) = mount_root(dir) {
        let rooted = format!("{root}/{name}");
        if world.peek_bytes(&rooted).is_some() {
            return rooted;
        }
    }
    direct
}

fn mount_root(path: &str) -> Option<String> {
    let mut parts = path.trim_start_matches('/').split('/');
    let first = parts.next()?;
    let second = parts.next()?;
    if first == "m" {
        Some(format!("/{first}/{second}"))
    } else {
        None
    }
}

fn prepare_output_dirs(plan: &ExecPlan, root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut write_roots = Vec::new();
    for ExecArg { value, role } in &plan.argv {
        let value = value.as_str();
        for output in exec_role_output_paths(value, *role) {
            let path = physical_path(output.as_str(), root)?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|err| format!("sandboxed exec create output dir `{output}`: {err}"))?;
                write_roots.push(parent.to_path_buf());
            }
        }
        for output in exec_role_output_dir_paths(value, *role) {
            let path = physical_path(output.as_str(), root)?;
            fs::create_dir_all(&path)
                .map_err(|err| format!("sandboxed exec create output dir `{output}`: {err}"))?;
            write_roots.push(path);
        }
    }
    Ok(dedup_paths(write_roots))
}

struct HarvestedOutputs {
    tree: ExecTree,
    subfiles: Vec<ExecSubfileCompletion>,
}

fn harvest_outputs(
    plan: &ExecPlan,
    root: &Path,
    stdout: &[u8],
) -> Result<HarvestedOutputs, String> {
    let mut tree = ExecTree::default();
    let mut subfiles = Vec::new();
    for ExecArg { value, role } in &plan.argv {
        let value = value.as_str();
        for output in exec_role_output_paths(value, *role) {
            let path = physical_path(output.as_str(), root)?;
            let bytes = fs::read(&path).map_err(|err| {
                format!("sandboxed exec output `{output}` was not produced: {err}")
            })?;
            subfiles.push(subfile_completion(output.clone(), &bytes));
            tree.insert_bytes(output.0, bytes);
        }
        for stdout_path in exec_role_stdout_paths(value, *role) {
            subfiles.push(subfile_completion(stdout_path.clone(), stdout));
            tree.insert_bytes(stdout_path.0, stdout.to_vec());
        }
        for output_dir in exec_role_output_dir_paths(value, *role) {
            harvest_output_dir(root, output_dir.as_str(), &mut tree, &mut subfiles)?;
        }
    }
    if tree.entries.is_empty() && tree.blobs.is_empty() {
        return Err("sandboxed exec plan declared no outputs".to_string());
    }
    subfiles.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(HarvestedOutputs { tree, subfiles })
}

fn harvest_output_dir(
    root: &Path,
    logical_dir: &str,
    tree: &mut ExecTree,
    subfiles: &mut Vec<ExecSubfileCompletion>,
) -> Result<(), String> {
    let physical = physical_path(logical_dir, root)?;
    if !physical.exists() {
        return Ok(());
    }
    let mut stack = vec![physical.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)
            .map_err(|err| format!("sandboxed exec read output dir `{}`: {err}", dir.display()))?
        {
            let entry = entry.map_err(|err| {
                format!(
                    "sandboxed exec read output dir entry `{}`: {err}",
                    dir.display()
                )
            })?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.is_file() {
                let relative = path.strip_prefix(&physical).map_err(|err| {
                    format!("sandboxed exec output path escaped output dir: {err}")
                })?;
                let logical = join_logical(logical_dir, relative);
                let bytes = fs::read(&path).map_err(|err| {
                    format!(
                        "sandboxed exec output `{}` was not readable: {err}",
                        path.display()
                    )
                })?;
                subfiles.push(subfile_completion(ExecPath::from(logical.clone()), &bytes));
                tree.insert_bytes(logical, bytes);
            }
        }
    }
    Ok(())
}

fn subfile_completion(path: ExecPath, bytes: &[u8]) -> ExecSubfileCompletion {
    ExecSubfileCompletion {
        path,
        content_hash: ContentHash(Blake3Hash::from_bytes(bytes)),
        size: ByteLen(bytes.len() as u64),
    }
}

fn tree_events_from_subfiles(subfiles: Vec<ExecSubfileCompletion>) -> Vec<ExecTreeEvent> {
    // PIN(progressive-exec-trees): this private staging executor has no daemon
    // close callback. vx-vfsd integration must emit SubfileCompleted from VFS
    // close/release notifications before process exit, then reuse the exit
    // verification below to enforce close-is-provisional.
    let mut events = subfiles
        .iter()
        .cloned()
        .map(ExecTreeEvent::SubfileCompleted)
        .collect::<Vec<_>>();
    events.push(ExecTreeEvent::TreeFinalized(ExecTreeFinalization {
        files: subfiles,
    }));
    events
}

fn verify_subfile_completions_at_exit(
    root: &Path,
    outputs: &ExecTree,
    subfiles: &[ExecSubfileCompletion],
) -> Result<(), String> {
    for subfile in subfiles {
        let path = physical_path(subfile.path.as_str(), root)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => outputs.bytes(subfile.path.as_str()).ok_or_else(|| {
                format!(
                    "sandboxed exec completed output `{}` vanished before exit verification: {err}",
                    subfile.path
                )
            })?,
        };
        let exit_hash = ContentHash(Blake3Hash::from_bytes(&bytes));
        if exit_hash != subfile.content_hash || ByteLen(bytes.len() as u64) != subfile.size {
            return Err(format!(
                "sandboxed exec output `{}` changed after close observation",
                subfile.path
            ));
        }
    }
    Ok(())
}

fn observation_roots(mounts: &[ExecMount]) -> Vec<ExecPath> {
    mounts.iter().map(|mount| mount.at.clone()).collect()
}

fn observation_scopes(scope_id: &ExecPrefixId, mounts: &[ExecMount]) -> Vec<ExecObservationScope> {
    vec![ExecObservationScope {
        prefix_id: scope_id.clone(),
        roots: observation_roots(mounts),
    }]
}

pub fn vfs_tracked_observations_to_exec_read_set(
    observations: Vec<VfsTrackedObservation>,
) -> ExecReadSet {
    let mut read_set = ExecReadSet::default();
    for observation in observations {
        match observation {
            VfsTrackedObservation::File {
                path,
                content_hash,
                blob_node,
                size,
            } => {
                read_set.entries.insert(
                    vfs_observation_to_exec_path(&path),
                    ExecReadObservation::File {
                        content_hash: ContentHash(Blake3Hash(content_hash.0)),
                        blob_node: blob_node.map(store_node_hash_to_exec),
                        size: ByteLen(size),
                    },
                );
            }
            VfsTrackedObservation::Directory {
                path,
                directory_node,
            } => {
                read_set.entries.insert(
                    vfs_observation_to_exec_path(&path),
                    ExecReadObservation::Directory {
                        directory_node: store_node_hash_to_exec(directory_node),
                    },
                );
            }
            VfsTrackedObservation::LookupMiss {
                path,
                parent_path,
                directory_node,
            } => {
                read_set.entries.insert(
                    vfs_observation_to_exec_path(&path),
                    ExecReadObservation::LookupMiss {
                        parent_path: vfs_observation_to_exec_path(&parent_path),
                        directory_node: store_node_hash_to_exec(directory_node),
                    },
                );
            }
        }
    }
    read_set
}

fn store_node_hash_to_exec(hash: StoreNodeHash) -> NodeHash {
    NodeHash(Blake3Hash(hash.0.0))
}

fn exec_roots_to_vfs_roots(roots: &[ExecPath]) -> Result<Vec<String>, String> {
    roots
        .iter()
        .map(|root| exec_path_to_vfs_path(root.as_str()))
        .collect()
}

fn exec_path_to_vfs_path(path: &str) -> Result<String, String> {
    let relative = path.trim_start_matches('/');
    let path = Path::new(relative);
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        return Err(format!(
            "fskit vfs refuses non-relative path `{}`",
            path.display()
        ));
    }
    Ok(relative.to_string())
}

fn vfs_observation_to_exec_path(path: &str) -> ExecPath {
    if path.is_empty() {
        ExecPath::from("/")
    } else {
        ExecPath::from(format!("/{path}"))
    }
}

fn fskit_prefix_id() -> String {
    format!(
        "exec-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    )
}

fn prefix_path(scope_id: &ExecPrefixId) -> String {
    format!("/{}", scope_id.0)
}

fn prefix_child_path(scope_id: &ExecPrefixId, child: &str) -> String {
    if child.is_empty() {
        prefix_path(scope_id)
    } else {
        format!("/{}/{}", scope_id.0, child)
    }
}

fn stage_file(root: &Path, logical: &str, bytes: &[u8]) -> Result<(), String> {
    let physical = physical_path(logical, root)?;
    if let Some(parent) = physical.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("sandboxed exec create staged parent `{logical}`: {err}"))?;
    }

    let hash = Blake3Hash::from_bytes(bytes);
    let cas_path = root.join(".vx-exec-cas").join(hash.to_hex());
    if !cas_path.exists() {
        fs::write(&cas_path, bytes)
            .map_err(|err| format!("sandboxed exec write cas `{logical}`: {err}"))?;
    }
    fs::hard_link(&cas_path, &physical)
        .or_else(|_| fs::copy(&cas_path, &physical).map(|_| ()))
        .map_err(|err| format!("sandboxed exec stage `{logical}`: {err}"))
}

fn command_program_and_argv(
    request: &ExecRequest,
    env: &[(OsString, OsString)],
    root: &Path,
) -> Result<(PathBuf, Vec<OsString>), String> {
    if request.program.as_str() == "build_script" {
        let executable = request
            .plan
            .argv
            .iter()
            .find(|arg| arg.role == ExecRole::Executable)
            .map(|arg| physical_path(arg.value.as_str(), root))
            .transpose()?
            .ok_or_else(|| "build_script command missing executable role".to_string())?;
        make_executable(&executable)?;
        return Ok((executable, process_argv(&request.plan, root)?));
    }

    Ok((
        resolve_program(request.program.as_str(), env)?,
        process_argv(&request.plan, root)?,
    ))
}

fn process_argv(plan: &ExecPlan, root: &Path) -> Result<Vec<OsString>, String> {
    plan.argv
        .iter()
        .filter(|arg| !is_control_arg(arg.value.as_str(), arg.role))
        .map(|arg| map_arg(arg.value.as_str(), arg.role, root))
        .collect()
}

fn is_control_arg(arg: &str, role: ExecRole) -> bool {
    matches!(
        role,
        ExecRole::Executable | ExecRole::OutputDir | ExecRole::Stdout | ExecRole::Env
    ) || matches!(arg, "--executable" | "--stdout" | "--out-dir" | "--env")
}

fn map_arg(arg: &str, role: ExecRole, root: &Path) -> Result<OsString, String> {
    match role {
        ExecRole::Executable
        | ExecRole::Input
        | ExecRole::Output
        | ExecRole::OutputDir
        | ExecRole::Stdout
        | ExecRole::SearchDir => Ok(physical_path(arg, root)?.into_os_string()),
        ExecRole::InputFlag | ExecRole::OutputFlag | ExecRole::Env | ExecRole::SearchDirFlag => {
            let mut mapped = arg.to_string();
            let mut paths = exec_role_embedded_paths(arg, role);
            paths.sort_by_key(|path| std::cmp::Reverse(path.as_str().len()));
            for path in paths {
                let physical = physical_path(path.as_str(), root)?;
                mapped = mapped.replace(path.as_str(), physical.to_string_lossy().as_ref());
            }
            Ok(OsString::from(mapped))
        }
        ExecRole::Flag => Ok(OsString::from(arg)),
    }
}

fn command_env(plan: &ExecPlan, root: &Path) -> Result<Vec<(OsString, OsString)>, String> {
    let mut env = scrubbed_env(root);
    for ExecArg { value, role } in &plan.argv {
        if *role != ExecRole::Env {
            continue;
        }
        let (key, value) = value
            .as_str()
            .split_once('=')
            .ok_or_else(|| format!("env role expected KEY=VALUE, got `{}`", value.as_str()))?;
        let value = if value.starts_with('/') {
            physical_path(value, root)?.into_os_string()
        } else {
            OsString::from(value)
        };
        env.retain(|(existing, _)| existing.to_str() != Some(key));
        env.push((OsString::from(key), value));
    }
    Ok(env)
}

fn scrubbed_env(root: &Path) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = ENV_ALLOWLIST
        .iter()
        .filter_map(|key| std::env::var_os(key).map(|value| (OsString::from(key), value)))
        .collect();
    let tmp = root.join("tmp");
    let _ = fs::create_dir_all(&tmp);
    let tmp = tmp.into_os_string();
    env.retain(|(key, _)| !matches!(key.to_str(), Some("TMPDIR" | "TEMP" | "TMP")));
    env.push((OsString::from("TMPDIR"), tmp.clone()));
    env.push((OsString::from("TEMP"), tmp.clone()));
    env.push((OsString::from("TMP"), tmp));
    env
}

fn resolve_program(program: &str, env: &[(OsString, OsString)]) -> Result<PathBuf, String> {
    let path = Path::new(program);
    if path.is_absolute() || path.components().count() > 1 {
        return Ok(path.to_path_buf());
    }
    let path_value = env
        .iter()
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| value.clone())
        .or_else(|| std::env::var_os("PATH"))
        .ok_or_else(|| format!("program `{program}` is not absolute and PATH is unset"))?;
    for dir in std::env::split_paths(&path_value) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!("program `{program}` was not found on PATH"))
}

fn physical_path(logical: &str, root: &Path) -> Result<PathBuf, String> {
    let relative = logical.trim_start_matches('/');
    let path = Path::new(relative);
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        return Err(format!(
            "sandboxed exec refuses non-relative staged path `{logical}`"
        ));
    }
    Ok(root.join(path))
}

fn join_logical(logical_dir: &str, relative: &Path) -> String {
    let relative = relative.to_string_lossy().replace('\\', "/");
    if relative.is_empty() {
        logical_dir.to_string()
    } else {
        format!("{}/{}", logical_dir.trim_end_matches('/'), relative)
    }
}

fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for path in paths {
        let key = path.to_string_lossy().to_string();
        if seen.insert(key) {
            out.push(path);
        }
    }
    out
}

#[cfg(target_os = "macos")]
fn run_sandboxed_process(
    program: &Path,
    argv: &[OsString],
    current_dir: &Path,
    env: &[(OsString, OsString)],
    read_root: &Path,
    write_roots: &[PathBuf],
    toolchain_roots: &[ExecPath],
) -> Result<Output, String> {
    let mut plan = seatbelt_exec::SeatbeltPlan::new().read_root(read_root);
    for root in write_roots {
        plan = plan.write_root(root);
    }
    for root in toolchain_roots {
        plan = plan.toolchain_root(toolchain_root_path(root.as_str(), read_root));
    }
    plan = plan.toolchain_root(program);
    if let Some(interpreter) = shebang_interpreter(program) {
        if is_macos_shell_selector_interpreter(&interpreter)
            && Path::new("/private/var/select/sh").exists()
        {
            plan = plan.toolchain_root("/private/var/select");
            plan = plan.toolchain_root("/bin/bash");
        }
        plan = plan.toolchain_root(interpreter);
    }
    let argv: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect();
    let mut command = seatbelt_exec::make_command(program, &argv, plan)
        .map_err(|err| format!("sandboxed exec failed to build seatbelt command: {err}"))?;
    command
        .current_dir(current_dir)
        .env_clear()
        .envs(env.iter().cloned());
    command
        .output()
        .map_err(|err| format!("sandboxed exec failed to spawn sandbox-exec: {err}"))
}

#[cfg(target_os = "macos")]
fn run_sandboxed_process_without_canonicalizing_profile(
    program: &Path,
    argv: &[OsString],
    current_dir: &Path,
    env: &[(OsString, OsString)],
    read_root: &Path,
    write_roots: &[PathBuf],
    toolchain_roots: &[ExecPath],
) -> Result<Output, String> {
    let mut plan = seatbelt_exec::SeatbeltPlan::new().read_root(read_root);
    for root in write_roots {
        plan = plan.write_root(root);
    }
    for root in toolchain_roots {
        plan = plan.toolchain_root(toolchain_root_path(root.as_str(), read_root));
    }
    plan = plan.toolchain_root(program);
    if let Some(interpreter) = shebang_interpreter(program) {
        if is_macos_shell_selector_interpreter(&interpreter)
            && Path::new("/private/var/select/sh").exists()
        {
            plan = plan.toolchain_root("/private/var/select");
            plan = plan.toolchain_root("/bin/bash");
        }
        plan = plan.toolchain_root(interpreter);
    }
    let argv: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect();
    let mut command = plan.compile().command(program, &argv);
    command
        .current_dir(current_dir)
        .env_clear()
        .envs(env.iter().cloned());
    command
        .output()
        .map_err(|err| format!("sandboxed exec failed to spawn sandbox-exec: {err}"))
}

#[cfg(target_os = "macos")]
fn toolchain_root_path(root: &str, sandbox_root: &Path) -> PathBuf {
    if let Ok(staged) = physical_path(root, sandbox_root)
        && staged.exists()
    {
        return staged;
    }
    PathBuf::from(root)
}

#[cfg(target_os = "macos")]
fn shebang_interpreter(program: &Path) -> Option<PathBuf> {
    let bytes = fs::read(program).ok()?;
    let line = bytes
        .strip_prefix(b"#!")?
        .split(|byte| *byte == b'\n')
        .next()?;
    let line = std::str::from_utf8(line).ok()?.trim();
    let interpreter = line.split_ascii_whitespace().next()?;
    interpreter
        .starts_with('/')
        .then(|| PathBuf::from(interpreter))
}

#[cfg(target_os = "macos")]
fn is_macos_shell_selector_interpreter(interpreter: &Path) -> bool {
    interpreter == Path::new("/bin/sh") || interpreter == Path::new("/bin/bash")
}

#[cfg(not(target_os = "macos"))]
fn run_sandboxed_process(
    _program: &Path,
    _argv: &[OsString],
    _current_dir: &Path,
    _env: &[(OsString, OsString)],
    _read_root: &Path,
    _write_roots: &[PathBuf],
    _toolchain_roots: &[ExecPath],
) -> Result<Output, String> {
    Err("sandboxed exec is implemented for macOS seatbelt in stage 1".to_string())
}

#[cfg(not(target_os = "macos"))]
fn run_sandboxed_process_without_canonicalizing_profile(
    _program: &Path,
    _argv: &[OsString],
    _current_dir: &Path,
    _env: &[(OsString, OsString)],
    _read_root: &Path,
    _write_roots: &[PathBuf],
    _toolchain_roots: &[ExecPath],
) -> Result<Output, String> {
    Err("sandboxed exec is implemented for macOS seatbelt in stage 1".to_string())
}

#[cfg(unix)]
fn status_code(output: &Output) -> i32 {
    use std::os::unix::process::ExitStatusExt;

    output
        .status
        .code()
        .or_else(|| output.status.signal().map(|signal| 128 + signal))
        .unwrap_or(1)
}

#[cfg(not(unix))]
fn status_code(output: &Output) -> i32 {
    output.status.code().unwrap_or(1)
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .map_err(|err| {
            format!(
                "sandboxed exec executable metadata `{}`: {err}",
                path.display()
            )
        })?
        .permissions();
    permissions.set_mode(permissions.mode() | 0o700);
    fs::set_permissions(path, permissions).map_err(|err| {
        format!(
            "sandboxed exec chmod executable `{}`: {err}",
            path.display()
        )
    })
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

struct MountedWorld<'a> {
    mounts: &'a [ExecMount],
}

impl<'a> MountedWorld<'a> {
    fn new(mounts: &'a [ExecMount]) -> Self {
        Self { mounts }
    }

    fn read_bytes(&self, path: &str) -> MountLookup {
        for mount in self.mounts {
            if let Some(rest) = strip_mount(path, mount.at.as_str()) {
                let relative = rest.trim_start_matches('/');
                if let Some(bytes) = mount.tree.bytes(relative) {
                    return MountLookup::File(bytes);
                }
                if self.parent_exists(mount, relative) {
                    return MountLookup::LookupMiss {
                        parent_path: parent_logical_path(path),
                        directory_node: directory_node_for_parent(&mount.tree, relative),
                    };
                }
                return MountLookup::Outside;
            }
        }
        MountLookup::Outside
    }

    fn list(&self, dir: &str) -> Option<Vec<String>> {
        for mount in self.mounts {
            let Some(rest) = strip_mount(dir, mount.at.as_str()) else {
                continue;
            };
            let prefix = rest.trim_matches('/');
            let mut names = BTreeSet::new();
            for key in mount.tree.entries.keys().chain(mount.tree.blobs.keys()) {
                let key = key.as_str();
                if prefix.is_empty() {
                    names.insert(key.to_string());
                } else if let Some(child) = key
                    .strip_prefix(prefix)
                    .and_then(|child| child.strip_prefix('/'))
                    && !child.is_empty()
                {
                    names.insert(child.to_string());
                }
            }
            if !names.is_empty() || prefix.is_empty() {
                return Some(names.into_iter().collect());
            }
        }
        None
    }

    fn parent_exists(&self, mount: &ExecMount, relative: &str) -> bool {
        let Some((parent, _leaf)) = relative.rsplit_once('/') else {
            return true;
        };
        let prefix = format!("{}/", parent.trim_matches('/'));
        mount
            .tree
            .entries
            .keys()
            .chain(mount.tree.blobs.keys())
            .any(|key| key.as_str().starts_with(&prefix))
    }
}

enum MountLookup {
    File(Vec<u8>),
    LookupMiss {
        parent_path: String,
        directory_node: NodeHash,
    },
    Outside,
}

fn strip_mount<'a>(path: &'a str, mount_at: &str) -> Option<&'a str> {
    let mount_at = mount_at.trim_end_matches('/');
    if path == mount_at {
        return Some("");
    }
    path.strip_prefix(mount_at)
        .and_then(|rest| rest.strip_prefix('/'))
}

fn parent_logical_path(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(parent, _)| {
            if parent.is_empty() {
                "/".to_string()
            } else {
                parent.to_string()
            }
        })
        .unwrap_or_else(|| "/".to_string())
}

struct ObservedWorld<'a> {
    world: &'a MountedWorld<'a>,
    read_set: ExecReadSet,
}

impl<'a> ObservedWorld<'a> {
    fn new(world: &'a MountedWorld<'a>) -> Self {
        Self {
            world,
            read_set: ExecReadSet::default(),
        }
    }

    fn read_bytes(&mut self, path: &str) -> Option<Vec<u8>> {
        match self.world.read_bytes(path) {
            MountLookup::File(contents) => {
                self.read_set.entries.insert(
                    ExecPath::from(path),
                    ExecReadObservation::File {
                        content_hash: ContentHash(Blake3Hash::from_bytes(&contents)),
                        blob_node: None,
                        size: ByteLen(contents.len() as u64),
                    },
                );
                Some(contents)
            }
            MountLookup::LookupMiss {
                parent_path,
                directory_node,
            } => {
                self.read_set.entries.insert(
                    ExecPath::from(path),
                    ExecReadObservation::LookupMiss {
                        parent_path: ExecPath::from(parent_path),
                        directory_node,
                    },
                );
                None
            }
            MountLookup::Outside => None,
        }
    }

    fn peek_bytes(&self, path: &str) -> Option<Vec<u8>> {
        match self.world.read_bytes(path) {
            MountLookup::File(contents) => Some(contents),
            MountLookup::LookupMiss { .. } | MountLookup::Outside => None,
        }
    }

    fn list(&mut self, dir: &str) -> Option<Vec<String>> {
        let got = self.world.list(dir);
        if let Some(names) = &got {
            self.read_set.entries.insert(
                ExecPath::from(dir),
                ExecReadObservation::Directory {
                    directory_node: directory_node_hash(names),
                },
            );
        }
        got
    }

    fn saw_lookup_miss(&self, path: &str) -> bool {
        matches!(
            self.read_set.entries.get(path),
            Some(ExecReadObservation::LookupMiss { .. })
        )
    }

    fn into_read_set(self) -> ExecReadSet {
        self.read_set
    }
}

fn directory_node_hash(names: &[String]) -> NodeHash {
    let mut bytes = Vec::new();
    for name in names {
        bytes.extend_from_slice(&(name.len() as u64).to_le_bytes());
        bytes.extend_from_slice(name.as_bytes());
    }
    NodeHash(Blake3Hash::from_bytes(&bytes))
}

fn directory_node_for_parent(tree: &ExecTree, relative: &str) -> NodeHash {
    let parent = relative
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("");
    let mut names = BTreeSet::new();
    let prefix = if parent.is_empty() {
        String::new()
    } else {
        format!("{}/", parent.trim_matches('/'))
    };
    for key in tree.entries.keys().chain(tree.blobs.keys()) {
        let key = key.as_str();
        let rest = if prefix.is_empty() {
            key
        } else if let Some(rest) = key.strip_prefix(&prefix) {
            rest
        } else {
            continue;
        };
        if let Some(name) = rest.split('/').next()
            && !name.is_empty()
        {
            names.insert(name.to_string());
        }
    }
    directory_node_hash(&names.into_iter().collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_verification_rejects_changed_subfile() {
        let temp = tempfile::Builder::new()
            .prefix("vx-exec-close-verify-")
            .tempdir()
            .unwrap();
        let path = temp.path().join("m/out/artifact.txt");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"first").unwrap();
        let completion = subfile_completion(ExecPath::from("/m/out/artifact.txt"), b"first");

        fs::write(&path, b"second").unwrap();
        let error =
            verify_subfile_completions_at_exit(temp.path(), &ExecTree::default(), &[completion])
                .expect_err("changed output must fail close-is-provisional verification");
        assert!(error.contains("changed after close observation"));
    }
}
