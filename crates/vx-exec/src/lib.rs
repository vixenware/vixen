//! Private sandboxed executor implementation for the public runner protocol.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Output;
use std::sync::Arc;

use camino::Utf8PathBuf;
use vx_services::{
    BlobRef, ByteLen, CapabilityProbeRequest, CapabilityProbeResult, DirectoryEntryKind, ExecArg,
    ExecBlake3Hash, ExecCompletion, ExecContentHash as ContentHash, ExecDiagnostic, ExecError,
    ExecErrorKind, ExecExitCode, ExecMount, ExecNodeHash, ExecObservationScope, ExecOutcome,
    ExecPath, ExecPlan, ExecPrefixId, ExecReadObservation, ExecReadSet, ExecRequest, ExecResult,
    ExecRole, ExecSubfileCompletion, ExecTree, ExecTreeEvent, ExecTreeFinalization, MerkleTreeFile,
    NodeHash, PlatformArch, PlatformName, RUNNER_PROTOCOL_VERSION, Runner, RunnerCapabilities,
    RunnerPlatform, RunnerTransport, SandboxCapability, StoreClient, StoreDispatcher,
    ToolchainCapability, ToolchainKind, build_merkle_tree,
    chunking::{prepare_blob, store_prepared_blob},
    exec_role_embedded_paths, exec_role_env_paths, exec_role_input_paths,
    exec_role_output_dir_paths, exec_role_output_paths, exec_role_search_dir_paths,
    exec_role_stdout_paths,
};
use vx_vfsd::protocol::{VfsTrackedObservation, Vfsd};

const ENV_ALLOWLIST: &[&str] = &["PATH", "HOME", "TMPDIR", "TEMP", "TMP", "SystemRoot"];

#[derive(Debug, Clone)]
pub struct SandboxedExecutor {
    backend: ExecutorBackend,
}

#[derive(Debug, Clone)]
enum ExecutorBackend {
    Staging,
    NativeVfs { mount_path: Utf8PathBuf },
}

impl Default for SandboxedExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl SandboxedExecutor {
    pub fn new() -> Self {
        Self {
            backend: ExecutorBackend::Staging,
        }
    }

    pub fn native_vfs(mount_path: impl Into<Utf8PathBuf>) -> Self {
        Self {
            backend: ExecutorBackend::NativeVfs {
                mount_path: mount_path.into(),
            },
        }
    }

    pub fn exec_blocking(&self, request: ExecRequest) -> ExecResult {
        match run_request(request) {
            Ok(result) => result,
            Err(error) => failed_without_process(error),
        }
    }
}

impl Runner for SandboxedExecutor {
    async fn exec(&self, request: ExecRequest) -> ExecResult {
        match &self.backend {
            ExecutorBackend::Staging => self.exec_blocking(request),
            ExecutorBackend::NativeVfs { mount_path } => {
                match run_request_native_vfs(request, mount_path.clone()).await {
                    Ok(result) => result,
                    Err(error) => failed_without_process(error),
                }
            }
        }
    }

    async fn capabilities(&self, _request: CapabilityProbeRequest) -> CapabilityProbeResult {
        CapabilityProbeResult {
            capabilities: detect_capabilities(),
        }
    }
}

async fn run_request_native_vfs(
    request: ExecRequest,
    mount_path: Utf8PathBuf,
) -> Result<ExecResult, ExecError> {
    let temp = tempfile::Builder::new()
        .prefix("vx-native-exec-")
        .tempdir_in("/tmp")
        .map_err(|err| {
            exec_error(
                ExecErrorKind::Staging,
                format!("native vfs exec tempdir: {err}"),
            )
        })?;
    let root = temp.path();
    let store_root = Utf8PathBuf::from_path_buf(root.join("store")).map_err(|path| {
        exec_error(
            ExecErrorKind::Staging,
            format!(
                "native vfs exec store path is not UTF-8: {}",
                path.display()
            ),
        )
    })?;
    let store = vx_store::open(store_root).await.map_err(|err| {
        exec_error(
            ExecErrorKind::Staging,
            format!("native vfs exec open store: {err:#}"),
        )
    })?;
    let store_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|err| {
            exec_error(
                ExecErrorKind::Staging,
                format!("native vfs exec bind store listener: {err}"),
            )
        })?;
    let store_addr = format!(
        "tcp://{}",
        store_listener.local_addr().map_err(|err| {
            exec_error(
                ExecErrorKind::Staging,
                format!("native vfs exec store listener addr: {err}"),
            )
        })?
    );
    let store_server = tokio::spawn(async move {
        vox::serve_listener(store_listener, StoreDispatcher::new(store))
            .await
            .map_err(|error| error.to_string())
    });

    let cas: StoreClient = vox::connect_lane(&store_addr)
        .connect_timeout(std::time::Duration::from_millis(100))
        .wait_for_service(std::time::Duration::from_secs(5))
        .await
        .map_err(|err| {
            exec_error(
                ExecErrorKind::Staging,
                format!("native vfs exec connect store at {store_addr}: {err}"),
            )
        })?;
    let service = vx_vfsd::boot_with_store(Arc::new(cas.clone()), &mount_path)
        .await
        .map_err(|err| {
            exec_error(
                ExecErrorKind::Staging,
                format!("native vfs exec boot vfsd: {err:#}"),
            )
        })?;

    let prefix_id = native_prefix_id();
    materialize_mounts(&service, &cas, &prefix_id, &request.plan, &request.mounts).await?;
    prepare_native_output_dirs(&service, &prefix_id, &request.plan).await?;

    let access_path = Vfsd::access_path(&service).await.map_err(stage_error)?;
    let prefix_root = PathBuf::from(access_path).join(&prefix_id);
    let env = command_env_native(&request.plan, &prefix_root).map_err(stage_error)?;
    let (program, argv) =
        command_program_and_argv_native(&request, &env, &prefix_root).map_err(stage_error)?;
    let write_roots = native_write_roots(&request.plan, &prefix_root).map_err(stage_error)?;
    let output = tokio::task::spawn_blocking({
        let program = program.clone();
        let argv = argv.clone();
        let env = env.clone();
        let prefix_root = prefix_root.clone();
        let toolchain_roots = request.toolchain_roots.clone();
        move || {
            run_sandboxed_process(
                &program,
                &argv,
                &prefix_root,
                &env,
                &prefix_root,
                &write_roots,
                &toolchain_roots,
            )
        }
    })
    .await
    .map_err(|err| exec_error(ExecErrorKind::Spawn, format!("native vfs exec task: {err}")))?
    .map_err(spawn_or_unsupported_error)?;

    let exit_code = status_code(&output);
    if !output.status.success() {
        let stderr = output.stderr;
        let stdout = output.stdout;
        let _ = Vfsd::rm_rf(&service, format!("/{prefix_id}")).await;
        store_server.abort();
        let error = exec_error(
            ExecErrorKind::ProcessExit,
            format!(
                "native vfs exec {} exited with {}: {}",
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
    let tracked_roots = tracked_roots(&request.mounts);
    let tracked = Vfsd::tracked_observations(&service, prefix_id.clone(), tracked_roots)
        .await
        .map_err(stage_error)?;
    let read_set = tracked_observations_to_read_set(tracked);
    let tree_events = tree_events_from_subfiles(harvested.subfiles.clone());
    let outcome = ExecOutcome {
        outputs: harvested.tree,
        read_set,
        observation_scopes: observation_scopes(&ExecPrefixId(prefix_id.clone()), &request.mounts),
        tree_events,
    };
    let _ = Vfsd::rm_rf(&service, format!("/{prefix_id}")).await;
    store_server.abort();
    Ok(ExecResult {
        exit_code: ExecExitCode(exit_code),
        stdout: output.stdout,
        stderr: output.stderr,
        completion: ExecCompletion::Succeeded(outcome),
    })
}

fn run_request(request: ExecRequest) -> Result<ExecResult, ExecError> {
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
        return Ok(ExecResult {
            exit_code: ExecExitCode(exit_code),
            stdout,
            stderr,
            completion: ExecCompletion::Failed(error),
        });
    }

    let harvested = harvest_outputs(&request.plan, root, &output.stdout).map_err(harvest_error)?;
    verify_subfile_completions_at_exit(root, &harvested.tree, &harvested.subfiles)
        .map_err(verification_error)?;
    let tree_events = tree_events_from_subfiles(harvested.subfiles.clone());
    let outcome = ExecOutcome {
        outputs: harvested.tree,
        read_set: observed.into_read_set(),
        observation_scopes: observation_scopes(&scope_id, &request.mounts),
        tree_events,
    };
    Ok(ExecResult {
        exit_code: ExecExitCode(exit_code),
        stdout: output.stdout,
        stderr: output.stderr,
        completion: ExecCompletion::Succeeded(outcome),
    })
}

async fn materialize_mounts(
    service: &vx_vfsd::VfsdService,
    cas: &StoreClient,
    prefix_id: &str,
    plan: &ExecPlan,
    mounts: &[ExecMount],
) -> Result<(), ExecError> {
    for mount in mounts {
        let executable_paths =
            executable_paths_under_mount(plan, &mount.at).map_err(stage_error)?;
        let root = exec_tree_to_cas(cas, &mount.tree, &executable_paths)
            .await
            .map_err(stage_error)?;
        let rel = logical_to_prefix_relative(mount.at.as_str()).map_err(stage_error)?;
        let path = if rel.is_empty() {
            format!("/{prefix_id}")
        } else {
            format!("/{prefix_id}/{rel}")
        };
        Vfsd::mount(service, path, root.0)
            .await
            .map_err(stage_error)?;
    }
    Ok(())
}

fn executable_paths_under_mount(
    plan: &ExecPlan,
    mount_at: &ExecPath,
) -> Result<BTreeSet<String>, String> {
    let mut executable_paths = BTreeSet::new();
    let mount_rel = logical_to_tree_relative(mount_at.as_str())?;
    let mount_prefix = if mount_rel.is_empty() {
        String::new()
    } else {
        format!("{mount_rel}/")
    };

    for ExecArg { value, role } in &plan.argv {
        if *role != ExecRole::Executable {
            continue;
        }

        let executable_rel = logical_to_tree_relative(value.as_str())?;
        if mount_rel.is_empty() {
            executable_paths.insert(executable_rel);
        } else if let Some(relative) = executable_rel.strip_prefix(&mount_prefix) {
            executable_paths.insert(relative.to_string());
        }
    }

    Ok(executable_paths)
}

async fn exec_tree_to_cas(
    cas: &StoreClient,
    tree: &ExecTree,
    executable_paths: &BTreeSet<String>,
) -> Result<NodeHash, String> {
    let mut files = Vec::with_capacity(tree.entries.len() + tree.blobs.len());
    for (path, text) in &tree.entries {
        let relative = logical_to_tree_relative(path.as_str())?;
        files.push(
            exec_tree_file(
                cas,
                path.as_str(),
                text.0.as_bytes(),
                executable_paths.contains(&relative),
            )
            .await?,
        );
    }
    for (path, bytes) in &tree.blobs {
        let relative = logical_to_tree_relative(path.as_str())?;
        files.push(
            exec_tree_file(
                cas,
                path.as_str(),
                bytes,
                executable_paths.contains(&relative),
            )
            .await?,
        );
    }
    let (root, _total_size) = build_merkle_tree(cas, files).await?;
    Ok(root)
}

async fn exec_tree_file(
    cas: &StoreClient,
    path: &str,
    bytes: &[u8],
    executable: bool,
) -> Result<MerkleTreeFile, String> {
    let relative = logical_to_tree_relative(path)?;
    let prepared = prepare_blob(bytes);
    let blob = store_prepared_blob(cas, &prepared, bytes).await?;
    Ok(MerkleTreeFile {
        path: relative,
        kind: blob_ref_to_dir_entry_kind(&blob, executable),
    })
}

fn blob_ref_to_dir_entry_kind(blob: &BlobRef, executable: bool) -> DirectoryEntryKind {
    blob.to_dir_entry_kind(executable)
}

async fn prepare_native_output_dirs(
    service: &vx_vfsd::VfsdService,
    prefix_id: &str,
    plan: &ExecPlan,
) -> Result<(), ExecError> {
    for ExecArg { value, role } in &plan.argv {
        let value = value.as_str();
        for output in exec_role_output_paths(value, *role) {
            let parent_path = parent_logical_path(output.as_str());
            let Some(parent) = parent_path.strip_prefix('/') else {
                continue;
            };
            if !parent.is_empty() {
                Vfsd::mkdir(service, format!("/{prefix_id}/{parent}"))
                    .await
                    .map_err(stage_error)?;
            }
        }
        for output_dir in exec_role_output_dir_paths(value, *role) {
            let rel = logical_to_prefix_relative(output_dir.as_str()).map_err(stage_error)?;
            Vfsd::mkdir(service, format!("/{prefix_id}/{rel}"))
                .await
                .map_err(stage_error)?;
        }
    }
    Vfsd::mkdir(service, format!("/{prefix_id}/tmp"))
        .await
        .map_err(stage_error)?;
    Ok(())
}

fn command_program_and_argv_native(
    request: &ExecRequest,
    env: &[(OsString, OsString)],
    prefix_root: &Path,
) -> Result<(PathBuf, Vec<OsString>), String> {
    if request.program.as_str() == "build_script" {
        let executable = request
            .plan
            .argv
            .iter()
            .find(|arg| arg.role == ExecRole::Executable)
            .map(|arg| native_physical_path(arg.value.as_str(), prefix_root))
            .transpose()?
            .ok_or_else(|| "build_script command missing executable role".to_string())?;
        return Ok((executable, process_argv_native(&request.plan, prefix_root)?));
    }

    Ok((
        resolve_program(request.program.as_str(), env)?,
        process_argv_native(&request.plan, prefix_root)?,
    ))
}

fn process_argv_native(plan: &ExecPlan, prefix_root: &Path) -> Result<Vec<OsString>, String> {
    plan.argv
        .iter()
        .filter(|arg| !is_control_arg(arg.value.as_str(), arg.role))
        .map(|arg| map_arg_native(arg.value.as_str(), arg.role, prefix_root))
        .collect()
}

fn command_env_native(
    plan: &ExecPlan,
    prefix_root: &Path,
) -> Result<Vec<(OsString, OsString)>, String> {
    let mut env = scrubbed_env(prefix_root);
    for ExecArg { value, role } in &plan.argv {
        if *role != ExecRole::Env {
            continue;
        }
        let (key, value) = value
            .as_str()
            .split_once('=')
            .ok_or_else(|| format!("env role expected KEY=VALUE, got `{}`", value.as_str()))?;
        let value = if value.starts_with('/') {
            native_physical_path(value, prefix_root)?.into_os_string()
        } else {
            OsString::from(value)
        };
        env.retain(|(existing, _)| existing.to_str() != Some(key));
        env.push((OsString::from(key), value));
    }
    Ok(env)
}

fn native_write_roots(plan: &ExecPlan, prefix_root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut write_roots = Vec::new();
    for ExecArg { value, role } in &plan.argv {
        let value = value.as_str();
        for output in exec_role_output_paths(value, *role) {
            let path = native_physical_path(output.as_str(), prefix_root)?;
            if let Some(parent) = path.parent() {
                write_roots.push(parent.to_path_buf());
            }
        }
        for output in exec_role_output_dir_paths(value, *role) {
            write_roots.push(native_physical_path(output.as_str(), prefix_root)?);
        }
    }
    write_roots.push(prefix_root.join("tmp"));
    Ok(dedup_paths(write_roots))
}

fn map_arg_native(arg: &str, role: ExecRole, prefix_root: &Path) -> Result<OsString, String> {
    match role {
        ExecRole::Executable
        | ExecRole::Input
        | ExecRole::Output
        | ExecRole::OutputDir
        | ExecRole::Stdout
        | ExecRole::SearchDir => Ok(native_physical_path(arg, prefix_root)?.into_os_string()),
        ExecRole::InputFlag | ExecRole::OutputFlag | ExecRole::Env | ExecRole::SearchDirFlag => {
            let mut mapped = arg.to_string();
            let mut paths = exec_role_embedded_paths(arg, role);
            paths.sort_by_key(|path| std::cmp::Reverse(path.as_str().len()));
            for path in paths {
                let physical = native_physical_path(path.as_str(), prefix_root)?;
                mapped = mapped.replace(path.as_str(), physical.to_string_lossy().as_ref());
            }
            Ok(OsString::from(mapped))
        }
        ExecRole::Flag => Ok(OsString::from(arg)),
    }
}

fn native_physical_path(logical: &str, prefix_root: &Path) -> Result<PathBuf, String> {
    physical_path(logical, prefix_root)
}

fn tracked_roots(mounts: &[ExecMount]) -> Vec<String> {
    mounts
        .iter()
        .filter_map(|mount| logical_to_prefix_relative(mount.at.as_str()).ok())
        .collect()
}

fn tracked_observations_to_read_set(observations: Vec<VfsTrackedObservation>) -> ExecReadSet {
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
                    tracked_exec_path(&path),
                    ExecReadObservation::File {
                        content_hash: store_content_hash_to_exec(content_hash),
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
                    tracked_exec_path(&path),
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
                    tracked_exec_path(&path),
                    ExecReadObservation::LookupMiss {
                        parent_path: tracked_exec_path(&parent_path),
                        directory_node: store_node_hash_to_exec(directory_node),
                    },
                );
            }
        }
    }
    read_set
}

fn store_content_hash_to_exec(hash: vx_services::Blake3Hash) -> ContentHash {
    ContentHash(ExecBlake3Hash(hash.0))
}

fn store_node_hash_to_exec(hash: NodeHash) -> ExecNodeHash {
    ExecNodeHash(ExecBlake3Hash(hash.0.0))
}

fn tracked_exec_path(path: &str) -> ExecPath {
    if path.is_empty() {
        ExecPath::from("/")
    } else {
        ExecPath::from(format!("/{path}"))
    }
}

fn logical_to_prefix_relative(path: &str) -> Result<String, String> {
    logical_to_tree_relative(path)
}

fn logical_to_tree_relative(path: &str) -> Result<String, String> {
    let relative = path.trim_start_matches('/');
    let path = Path::new(relative);
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        return Err(format!(
            "native vfs refuses non-relative path `{}`",
            path.display()
        ));
    }
    Ok(relative.to_string())
}

fn native_prefix_id() -> String {
    format!(
        "exec-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    )
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
                .map(|bytes| ContentHash(ExecBlake3Hash::from_bytes(&bytes))),
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
        content_hash: ContentHash(ExecBlake3Hash::from_bytes(bytes)),
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
        let exit_hash = ContentHash(ExecBlake3Hash::from_bytes(&bytes));
        if exit_hash != subfile.content_hash || ByteLen(bytes.len() as u64) != subfile.size {
            return Err(format!(
                "sandboxed exec output `{}` changed after close observation",
                subfile.path
            ));
        }
    }
    Ok(())
}

fn observation_scopes(scope_id: &ExecPrefixId, mounts: &[ExecMount]) -> Vec<ExecObservationScope> {
    vec![ExecObservationScope {
        prefix_id: scope_id.clone(),
        roots: mounts.iter().map(|mount| mount.at.clone()).collect(),
    }]
}

fn stage_file(root: &Path, logical: &str, bytes: &[u8]) -> Result<(), String> {
    let physical = physical_path(logical, root)?;
    if let Some(parent) = physical.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("sandboxed exec create staged parent `{logical}`: {err}"))?;
    }

    let hash = ExecBlake3Hash::from_bytes(bytes);
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
        directory_node: ExecNodeHash,
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
                        content_hash: ContentHash(ExecBlake3Hash::from_bytes(&contents)),
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

fn directory_node_hash(names: &[String]) -> ExecNodeHash {
    let mut bytes = Vec::new();
    for name in names {
        bytes.extend_from_slice(&(name.len() as u64).to_le_bytes());
        bytes.extend_from_slice(name.as_bytes());
    }
    ExecNodeHash(ExecBlake3Hash::from_bytes(&bytes))
}

fn directory_node_for_parent(tree: &ExecTree, relative: &str) -> ExecNodeHash {
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

    #[test]
    fn vfs_tracked_observations_convert_to_exec_read_set() {
        let file_hash = vx_services::Blake3Hash::from_bytes(b"alpha\n");
        let blob_node = NodeHash(vx_services::Blake3Hash::from_bytes(b"blob-node"));
        let directory_node = NodeHash(vx_services::Blake3Hash::from_bytes(b"dir-node"));

        let read_set = tracked_observations_to_read_set(vec![
            VfsTrackedObservation::File {
                path: "m/in/a.txt".to_string(),
                content_hash: file_hash,
                blob_node: Some(blob_node),
                size: 6,
            },
            VfsTrackedObservation::Directory {
                path: "m/in".to_string(),
                directory_node,
            },
            VfsTrackedObservation::LookupMiss {
                path: "m/in/missing.txt".to_string(),
                parent_path: "m/in".to_string(),
                directory_node,
            },
        ]);

        assert_eq!(
            read_set.entries.get("/m/in/a.txt"),
            Some(&ExecReadObservation::File {
                content_hash: ContentHash(ExecBlake3Hash(file_hash.0)),
                blob_node: Some(ExecNodeHash(ExecBlake3Hash(blob_node.0.0))),
                size: ByteLen(6),
            })
        );
        assert_eq!(
            read_set.entries.get("/m/in"),
            Some(&ExecReadObservation::Directory {
                directory_node: ExecNodeHash(ExecBlake3Hash(directory_node.0.0)),
            })
        );
        assert_eq!(
            read_set.entries.get("/m/in/missing.txt"),
            Some(&ExecReadObservation::LookupMiss {
                parent_path: ExecPath::from("/m/in"),
                directory_node: ExecNodeHash(ExecBlake3Hash(directory_node.0.0)),
            })
        );
    }

    #[tokio::test]
    async fn native_exec_store_dispatcher_accepts_local_vox_client() {
        let temp = tempfile::Builder::new()
            .prefix("vx-exec-store-smoke-")
            .tempdir_in("/tmp")
            .unwrap();
        let store_root = Utf8PathBuf::from_path_buf(temp.path().join("store")).unwrap();
        let store_addr = format!("local://{}", temp.path().join("store.vox").display());
        let store = vx_store::open(store_root).await.unwrap();

        let server_addr = store_addr.clone();
        let server = tokio::spawn(async move {
            vox::serve(server_addr, StoreDispatcher::new(store))
                .await
                .expect("store dispatcher should serve");
        });

        let client: StoreClient = vox::connect_lane(&store_addr)
            .connect_timeout(std::time::Duration::from_millis(100))
            .wait_for_service(std::time::Duration::from_secs(5))
            .await
            .expect("store client should connect");
        let missing = NodeHash(vx_services::Blake3Hash::from_bytes(b"missing-node"));
        assert!(!client.has_node(missing).await.unwrap());

        server.abort();
    }
}
