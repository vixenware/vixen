//! Private sandboxed executor implementation for the public runner protocol.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Output;

use vx_services::{
    Blake3Hash, ContentHash, DiscoverProbeRequest, DiscoverResult, ExecArg, ExecMount,
    ExecObservationScope, ExecOutcome, ExecPlan, ExecPrefixId, ExecReadObservation, ExecReadSet,
    ExecRequest, ExecResult, ExecRole, ExecSubfileCompletion, ExecTree, ExecTreeEvent,
    ExecTreeFinalization, NodeHash, Runner, exec_role_embedded_paths, exec_role_env_paths,
    exec_role_input_paths, exec_role_output_dir_paths, exec_role_output_paths,
    exec_role_search_dir_paths, exec_role_stdout_paths,
};

const ENV_ALLOWLIST: &[&str] = &["PATH", "HOME", "TMPDIR", "TEMP", "TMP", "SystemRoot"];

#[derive(Debug, Clone, Default)]
pub struct SandboxedExecutor;

impl SandboxedExecutor {
    pub fn new() -> Self {
        Self
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
        self.exec_blocking(request)
    }

    async fn discover_probe(&self, _request: DiscoverProbeRequest) -> DiscoverResult {
        DiscoverResult {}
    }
}

fn run_request(request: ExecRequest) -> Result<ExecResult, String> {
    let temp = tempfile::Builder::new()
        .prefix("vx-sandboxed-exec-")
        .tempdir()
        .map_err(|err| format!("sandboxed exec tempdir: {err}"))?;
    let root = temp.path();
    fs::create_dir_all(root.join(".vx-exec-cas"))
        .map_err(|err| format!("sandboxed exec cas dir: {err}"))?;
    fs::create_dir_all(root.join("tmp")).map_err(|err| format!("sandboxed exec tmp dir: {err}"))?;
    let scope_id = ExecPrefixId(
        root.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("vx-sandboxed-exec")
            .to_string(),
    );

    let world = MountedWorld::new(&request.mounts);
    let mut observed = ObservedWorld::new(&world);
    stage_declared_inputs(&request.plan, &mut observed, root)?;
    let mut write_roots = prepare_output_dirs(&request.plan, root)?;
    write_roots.push(root.join("tmp"));

    let env = command_env(&request.plan, root)?;
    let (program, argv) = command_program_and_argv(&request, &env, root)?;
    let output = run_sandboxed_process(
        &program,
        &argv,
        root,
        &env,
        root,
        &write_roots,
        &request.toolchain_roots,
    )?;

    let exit_code = status_code(&output);
    if !output.status.success() {
        let stderr = output.stderr;
        let stdout = output.stdout;
        let error = format!(
            "sandboxed exec {} exited with {}: {}",
            program.display(),
            exit_code,
            String::from_utf8_lossy(&stderr)
        );
        return Ok(ExecResult {
            exit_code,
            stdout,
            stderr,
            outcome: None,
            error: Some(error),
        });
    }

    let harvested = harvest_outputs(&request.plan, root, &output.stdout)?;
    verify_subfile_completions_at_exit(root, &harvested.tree, &harvested.subfiles)?;
    let tree_events = tree_events_from_subfiles(harvested.subfiles.clone());
    let outcome = ExecOutcome {
        outputs: harvested.tree,
        read_set: observed.into_read_set(),
        observation_scopes: observation_scopes(&scope_id, &request.mounts),
        tree_events,
    };
    Ok(ExecResult {
        exit_code,
        stdout: output.stdout,
        stderr: output.stderr,
        outcome: Some(outcome),
        error: None,
    })
}

fn failed_without_process(error: String) -> ExecResult {
    ExecResult {
        exit_code: -1,
        stdout: Vec::new(),
        stderr: Vec::new(),
        outcome: None,
        error: Some(error),
    }
}

fn stage_declared_inputs(
    plan: &ExecPlan,
    world: &mut ObservedWorld<'_>,
    root: &Path,
) -> Result<(), String> {
    for ExecArg { value, role } in &plan.argv {
        match role {
            ExecRole::Executable | ExecRole::Input | ExecRole::InputFlag => {
                for input in exec_role_input_paths(value, *role) {
                    stage_declared_input(&input, world, root)?;
                }
            }
            ExecRole::SearchDir | ExecRole::SearchDirFlag => {
                for dir in exec_role_search_dir_paths(value, *role) {
                    let physical = physical_path(&dir, root)?;
                    fs::create_dir_all(&physical).map_err(|err| {
                        format!("sandboxed exec create search dir `{dir}`: {err}")
                    })?;
                    if let Some(names) = world.list(&dir) {
                        for name in names {
                            let logical = listed_logical_path(&dir, &name, world);
                            if let Some(bytes) = world.peek_bytes(&logical) {
                                stage_file(root, &logical, &bytes)?;
                            }
                        }
                    }
                }
            }
            ExecRole::Env => {
                for path in exec_role_env_paths(value, *role) {
                    stage_env_path(root, &path, world)?;
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
        for output in exec_role_output_paths(value, *role) {
            let path = physical_path(&output, root)?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|err| format!("sandboxed exec create output dir `{output}`: {err}"))?;
                write_roots.push(parent.to_path_buf());
            }
        }
        for output in exec_role_output_dir_paths(value, *role) {
            let path = physical_path(&output, root)?;
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
        for output in exec_role_output_paths(value, *role) {
            let path = physical_path(&output, root)?;
            let bytes = fs::read(&path).map_err(|err| {
                format!("sandboxed exec output `{output}` was not produced: {err}")
            })?;
            subfiles.push(subfile_completion(output.clone(), &bytes));
            tree.insert_bytes(output, bytes);
        }
        for stdout_path in exec_role_stdout_paths(value, *role) {
            subfiles.push(subfile_completion(stdout_path.clone(), stdout));
            tree.insert_bytes(stdout_path, stdout.to_vec());
        }
        for output_dir in exec_role_output_dir_paths(value, *role) {
            harvest_output_dir(root, &output_dir, &mut tree, &mut subfiles)?;
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
                subfiles.push(subfile_completion(logical.clone(), &bytes));
                tree.insert_bytes(logical, bytes);
            }
        }
    }
    Ok(())
}

fn subfile_completion(path: String, bytes: &[u8]) -> ExecSubfileCompletion {
    ExecSubfileCompletion {
        path,
        content_hash: ContentHash(Blake3Hash::from_bytes(bytes)),
        size: bytes.len() as u64,
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
        let path = physical_path(&subfile.path, root)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => outputs.bytes(&subfile.path).ok_or_else(|| {
                format!(
                    "sandboxed exec completed output `{}` vanished before exit verification: {err}",
                    subfile.path
                )
            })?,
        };
        let exit_hash = ContentHash(Blake3Hash::from_bytes(&bytes));
        if exit_hash != subfile.content_hash || bytes.len() as u64 != subfile.size {
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
    if request.program == "build_script" {
        let executable = request
            .plan
            .argv
            .iter()
            .find(|arg| arg.role == ExecRole::Executable)
            .map(|arg| physical_path(&arg.value, root))
            .transpose()?
            .ok_or_else(|| "build_script command missing executable role".to_string())?;
        make_executable(&executable)?;
        return Ok((executable, process_argv(&request.plan, root)?));
    }

    Ok((
        resolve_program(&request.program, env)?,
        process_argv(&request.plan, root)?,
    ))
}

fn process_argv(plan: &ExecPlan, root: &Path) -> Result<Vec<OsString>, String> {
    plan.argv
        .iter()
        .filter(|arg| !is_control_arg(&arg.value, arg.role))
        .map(|arg| map_arg(&arg.value, arg.role, root))
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
            paths.sort_by_key(|path| std::cmp::Reverse(path.len()));
            for path in paths {
                let physical = physical_path(&path, root)?;
                mapped = mapped.replace(&path, physical.to_string_lossy().as_ref());
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
            .split_once('=')
            .ok_or_else(|| format!("env role expected KEY=VALUE, got `{value}`"))?;
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
    toolchain_roots: &[String],
) -> Result<Output, String> {
    let mut plan = seatbelt_exec::SeatbeltPlan::new().read_root(read_root);
    for root in write_roots {
        plan = plan.write_root(root);
    }
    for root in toolchain_roots {
        plan = plan.toolchain_root(root);
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

#[cfg(not(target_os = "macos"))]
fn run_sandboxed_process(
    _program: &Path,
    _argv: &[OsString],
    _current_dir: &Path,
    _env: &[(OsString, OsString)],
    _read_root: &Path,
    _write_roots: &[PathBuf],
    _toolchain_roots: &[String],
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
            if let Some(rest) = strip_mount(path, &mount.at) {
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
            let Some(rest) = strip_mount(dir, &mount.at) else {
                continue;
            };
            let prefix = rest.trim_matches('/');
            let mut names = BTreeSet::new();
            for key in mount.tree.entries.keys().chain(mount.tree.blobs.keys()) {
                if prefix.is_empty() {
                    names.insert(key.clone());
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
            .any(|key| key.starts_with(&prefix))
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
                    path.to_string(),
                    ExecReadObservation::File {
                        content_hash: ContentHash(Blake3Hash::from_bytes(&contents)),
                        blob_node: None,
                        size: contents.len() as u64,
                    },
                );
                Some(contents)
            }
            MountLookup::LookupMiss {
                parent_path,
                directory_node,
            } => {
                self.read_set.entries.insert(
                    path.to_string(),
                    ExecReadObservation::LookupMiss {
                        parent_path,
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
                dir.to_string(),
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
        let rest = if prefix.is_empty() {
            key.as_str()
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
        let completion = subfile_completion("/m/out/artifact.txt".to_string(), b"first");

        fs::write(&path, b"second").unwrap();
        let error =
            verify_subfile_completions_at_exit(temp.path(), &ExecTree::default(), &[completion])
                .expect_err("changed output must fail close-is-provisional verification");
        assert!(error.contains("changed after close observation"));
    }
}
