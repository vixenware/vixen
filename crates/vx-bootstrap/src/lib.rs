use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::{Cursor, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::UNIX_EPOCH;

use camino::{Utf8Path, Utf8PathBuf};
use eyre::{Context, Result};
use facet::Facet;
use flate2::read::GzDecoder;
use tar::Archive;

pub const BOOTSTRAP_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Facet)]
pub struct HostFacts {
    pub protocol_version: u32,
    pub os: String,
    pub arch: String,
    pub family: String,
    pub exe: Option<String>,
    pub cache_root_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct SourceFileEntry {
    pub path: String,
    pub blake3_hex: String,
    pub executable: bool,
    pub size: u64,
    pub modified_unix_millis: Option<u64>,
}

#[derive(Debug, Clone, Facet)]
pub struct GetCachedSourceManifestRequest {
    pub checkout_dir: String,
}

#[derive(Debug, Clone, Facet)]
pub struct GetCachedSourceManifestResponse {
    pub files: Vec<SourceFileEntry>,
}

#[derive(Debug, Clone, Facet)]
pub struct PlanSourceSyncRequest {
    pub checkout_dir: String,
    pub files: Vec<SourceFileEntry>,
}

#[derive(Debug, Clone, Facet)]
pub struct PlanSourceSyncResponse {
    pub missing_paths: Vec<String>,
    pub deleted_paths: Vec<String>,
}

#[derive(Debug, Clone, Facet)]
pub struct BeginSourceArchiveRequest {
    pub checkout_dir: String,
    pub files: Vec<SourceFileEntry>,
}

#[derive(Debug, Clone, Facet)]
pub struct SourceArchiveChunkRequest {
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Facet)]
pub struct SourceArchiveAppliedResponse {
    pub applied_files: u64,
}

#[derive(Debug, Clone, Facet)]
pub struct BuildSourceTreeRequest {
    pub checkout_dir: String,
    pub build_manifest_relpath: String,
    pub build_packages: Vec<String>,
    pub scrub_files: Vec<String>,
    pub scrub_line_contains: Vec<String>,
}

#[derive(Debug, Clone, Facet)]
pub struct BuildSourceTreeResponse {
    pub checkout_dir: String,
}

#[derive(Debug, Clone, Facet)]
pub struct LaunchRunnerRequest {
    pub checkout_dir: String,
    pub vx_home: String,
    pub store_root: String,
    pub store_endpoint: String,
    pub vfsd_endpoint: String,
    pub runner_endpoint: String,
}

#[derive(Debug, Clone, Facet)]
pub struct BootstrapError {
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Facet)]
pub struct BootstrapProgress {
    pub phase: String,
    pub message: String,
    pub current: Option<u64>,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum BootstrapRequest {
    Hello,
    GetCachedSourceManifest(GetCachedSourceManifestRequest),
    PlanSourceSync(PlanSourceSyncRequest),
    BeginSourceArchive(BeginSourceArchiveRequest),
    SourceArchiveChunk(SourceArchiveChunkRequest),
    FinishSourceArchive,
    BuildSourceTree(BuildSourceTreeRequest),
    LaunchRunner(LaunchRunnerRequest),
    Shutdown,
}

#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum BootstrapResponse {
    Progress(BootstrapProgress),
    Hello(HostFacts),
    GetCachedSourceManifest(GetCachedSourceManifestResponse),
    PlanSourceSync(PlanSourceSyncResponse),
    BeginSourceArchiveReady,
    SourceArchiveApplied(SourceArchiveAppliedResponse),
    BuildSourceTree(BuildSourceTreeResponse),
    LaunchRunnerReady,
    ShuttingDown,
    Error(BootstrapError),
}

struct PendingSourceArchive {
    sender: mpsc::Sender<Option<Vec<u8>>>,
    join_handle: JoinHandle<Result<SourceArchiveAppliedResponse, String>>,
}

#[derive(Default)]
struct BootstrapServerState {
    pending_source_archive: Option<PendingSourceArchive>,
}

pub fn read_frame<R: Read>(reader: &mut R) -> Result<Option<BootstrapRequest>> {
    let mut len_buf = [0_u8; 4];
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error).wrap_err("failed to read bootstrap frame length"),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0_u8; len];
    reader
        .read_exact(&mut payload)
        .wrap_err("failed to read bootstrap frame payload")?;
    let request =
        facet_postcard::from_slice(&payload).wrap_err("failed to decode bootstrap request")?;
    Ok(Some(request))
}

pub fn read_response_frame<R: Read>(reader: &mut R) -> Result<Option<BootstrapResponse>> {
    let mut len_buf = [0_u8; 4];
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error).wrap_err("failed to read bootstrap frame length"),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0_u8; len];
    reader
        .read_exact(&mut payload)
        .wrap_err("failed to read bootstrap frame payload")?;
    let response =
        facet_postcard::from_slice(&payload).wrap_err("failed to decode bootstrap response")?;
    Ok(Some(response))
}

pub fn write_request_frame<W: Write>(writer: &mut W, request: &BootstrapRequest) -> Result<()> {
    let payload = facet_postcard::to_vec(request).wrap_err("failed to encode bootstrap request")?;
    writer
        .write_all(&(payload.len() as u32).to_le_bytes())
        .wrap_err("failed to write bootstrap frame length")?;
    writer
        .write_all(&payload)
        .wrap_err("failed to write bootstrap frame payload")?;
    writer.flush().wrap_err("failed to flush bootstrap frame")?;
    Ok(())
}

pub fn write_frame<W: Write>(writer: &mut W, response: &BootstrapResponse) -> Result<()> {
    let payload =
        facet_postcard::to_vec(response).wrap_err("failed to encode bootstrap response")?;
    writer
        .write_all(&(payload.len() as u32).to_le_bytes())
        .wrap_err("failed to write bootstrap frame length")?;
    writer
        .write_all(&payload)
        .wrap_err("failed to write bootstrap frame payload")?;
    writer.flush().wrap_err("failed to flush bootstrap frame")?;
    Ok(())
}

pub fn detect_host_facts() -> HostFacts {
    HostFacts {
        protocol_version: BOOTSTRAP_PROTOCOL_VERSION,
        os: env::consts::OS.to_string(),
        arch: env::consts::ARCH.to_string(),
        family: env::consts::FAMILY.to_string(),
        exe: env::current_exe()
            .ok()
            .and_then(|path| Utf8PathBuf::from_path_buf(path).ok())
            .map(|path| path.to_string()),
        cache_root_hint: default_cache_root_hint(),
    }
}

fn default_cache_root_hint() -> Option<String> {
    if cfg!(windows) {
        for key in ["CARGO_HOME", "RUSTUP_HOME", "TMP", "TEMP"] {
            if let Some(value) = env::var_os(key) {
                let value = value.to_string_lossy();
                if value.len() >= 2 && value.as_bytes().get(1) == Some(&b':') {
                    let drive = &value[..2];
                    return Some(format!(r"{drive}\vixen"));
                }
            }
        }
        env::var("USERPROFILE")
            .ok()
            .map(|home| format!(r"{home}\.cache\vixen"))
    } else {
        env::var("HOME")
            .ok()
            .map(|home| format!("{home}/.cache/vixen"))
    }
}

pub fn serve_stdio() -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    let mut state = BootstrapServerState::default();
    while let Some(request) = read_frame(&mut reader)? {
        let should_exit = matches!(request, BootstrapRequest::Shutdown);
        handle_request(&mut writer, &mut state, request)?;
        if should_exit {
            break;
        }
    }
    Ok(())
}

fn handle_request<W: Write>(
    writer: &mut W,
    state: &mut BootstrapServerState,
    request: BootstrapRequest,
) -> Result<()> {
    let response = match request {
        BootstrapRequest::Hello => BootstrapResponse::Hello(detect_host_facts()),
        BootstrapRequest::GetCachedSourceManifest(request) => {
            match get_cached_source_manifest_with_progress(&request, writer) {
                Ok(response) => BootstrapResponse::GetCachedSourceManifest(response),
                Err(error) => BootstrapResponse::Error(BootstrapError {
                    kind: "get_cached_source_manifest".to_string(),
                    message: format!("{error:#}"),
                }),
            }
        }
        BootstrapRequest::PlanSourceSync(request) => {
            match plan_source_sync_with_progress(&request, writer) {
                Ok(response) => BootstrapResponse::PlanSourceSync(response),
                Err(error) => BootstrapResponse::Error(BootstrapError {
                    kind: "plan_source_sync".to_string(),
                    message: format!("{error:#}"),
                }),
            }
        }
        BootstrapRequest::BeginSourceArchive(request) => {
            match begin_source_archive_with_progress(state, &request, writer) {
                Ok(()) => BootstrapResponse::BeginSourceArchiveReady,
                Err(error) => BootstrapResponse::Error(BootstrapError {
                    kind: "begin_source_archive".to_string(),
                    message: format!("{error:#}"),
                }),
            }
        }
        BootstrapRequest::SourceArchiveChunk(request) => {
            match push_source_archive_chunk(state, request) {
                Ok(()) => return Ok(()),
                Err(error) => BootstrapResponse::Error(BootstrapError {
                    kind: "source_archive_chunk".to_string(),
                    message: format!("{error:#}"),
                }),
            }
        }
        BootstrapRequest::FinishSourceArchive => {
            match finish_source_archive_with_progress(state, writer) {
                Ok(response) => BootstrapResponse::SourceArchiveApplied(response),
                Err(error) => BootstrapResponse::Error(BootstrapError {
                    kind: "finish_source_archive".to_string(),
                    message: format!("{error:#}"),
                }),
            }
        }
        BootstrapRequest::BuildSourceTree(request) => {
            match build_source_tree_with_progress(&request, writer) {
                Ok(response) => BootstrapResponse::BuildSourceTree(response),
                Err(error) => BootstrapResponse::Error(BootstrapError {
                    kind: "build_source_tree".to_string(),
                    message: format!("{error:#}"),
                }),
            }
        }
        BootstrapRequest::LaunchRunner(_request) => BootstrapResponse::Error(BootstrapError {
            kind: "launch_runner".to_string(),
            message: "launch_runner is not implemented yet".to_string(),
        }),
        BootstrapRequest::Shutdown => BootstrapResponse::ShuttingDown,
    };
    write_frame(writer, &response)
}

struct ChannelChunkReader {
    receiver: mpsc::Receiver<Option<Vec<u8>>>,
    current: Cursor<Vec<u8>>,
    finished: bool,
}

impl ChannelChunkReader {
    fn new(receiver: mpsc::Receiver<Option<Vec<u8>>>) -> Self {
        Self {
            receiver,
            current: Cursor::new(Vec::new()),
            finished: false,
        }
    }
}

impl Read for ChannelChunkReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let read = self.current.read(buf)?;
            if read > 0 {
                return Ok(read);
            }
            if self.finished {
                return Ok(0);
            }
            match self.receiver.recv() {
                Ok(Some(bytes)) => {
                    self.current = Cursor::new(bytes);
                }
                Ok(None) => {
                    self.finished = true;
                }
                Err(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "source archive stream closed unexpectedly",
                    ));
                }
            }
        }
    }
}

fn begin_source_archive(
    state: &mut BootstrapServerState,
    request: &BeginSourceArchiveRequest,
) -> Result<()> {
    if state.pending_source_archive.is_some() {
        eyre::bail!("source archive upload is already in progress");
    }
    let checkout_dir = request.checkout_dir.clone();
    let files = request.files.clone();
    let (sender, receiver) = mpsc::channel::<Option<Vec<u8>>>();
    let join_handle =
        thread::spawn(move || extract_source_archive_stream(&checkout_dir, files, receiver));
    state.pending_source_archive = Some(PendingSourceArchive {
        sender,
        join_handle,
    });
    Ok(())
}

fn begin_source_archive_with_progress<W: Write>(
    state: &mut BootstrapServerState,
    request: &BeginSourceArchiveRequest,
    writer: &mut W,
) -> Result<()> {
    emit_progress(
        writer,
        "begin_source_archive",
        format!(
            "ready to receive source archive for {} files into {}",
            request.files.len(),
            request.checkout_dir
        ),
        Some(0),
        Some(request.files.len() as u64),
    )?;
    begin_source_archive(state, request)
}

fn push_source_archive_chunk(
    state: &mut BootstrapServerState,
    request: SourceArchiveChunkRequest,
) -> Result<()> {
    let pending = state
        .pending_source_archive
        .as_mut()
        .ok_or_else(|| eyre::eyre!("source archive upload was not started"))?;
    pending
        .sender
        .send(Some(request.bytes))
        .map_err(|_| eyre::eyre!("source archive receiver exited before upload finished"))?;
    Ok(())
}

fn finish_source_archive(state: &mut BootstrapServerState) -> Result<SourceArchiveAppliedResponse> {
    let pending = state
        .pending_source_archive
        .take()
        .ok_or_else(|| eyre::eyre!("source archive upload was not started"))?;
    pending
        .sender
        .send(None)
        .map_err(|_| eyre::eyre!("source archive receiver exited before finish"))?;
    pending
        .join_handle
        .join()
        .map_err(|_| eyre::eyre!("source archive worker panicked"))?
        .map_err(|message| eyre::eyre!(message))
}

fn finish_source_archive_with_progress<W: Write>(
    state: &mut BootstrapServerState,
    writer: &mut W,
) -> Result<SourceArchiveAppliedResponse> {
    emit_progress(
        writer,
        "finish_source_archive",
        "finalizing streamed source archive",
        None,
        None,
    )?;
    let response = finish_source_archive(state)?;
    emit_progress(
        writer,
        "finish_source_archive",
        format!(
            "applied {} files from streamed source archive",
            response.applied_files
        ),
        Some(response.applied_files),
        Some(response.applied_files),
    )?;
    Ok(response)
}

fn extract_source_archive_stream(
    checkout_dir: &str,
    files: Vec<SourceFileEntry>,
    receiver: mpsc::Receiver<Option<Vec<u8>>>,
) -> Result<SourceArchiveAppliedResponse, String> {
    extract_source_archive_stream_inner(checkout_dir, files, receiver)
        .map_err(|error| format!("{error:#}"))
}

fn extract_source_archive_stream_inner(
    checkout_dir: &str,
    files: Vec<SourceFileEntry>,
    receiver: mpsc::Receiver<Option<Vec<u8>>>,
) -> Result<SourceArchiveAppliedResponse> {
    let checkout_dir = Utf8Path::new(checkout_dir);
    if let Some(parent) = checkout_dir.parent() {
        fs::create_dir_all(parent).wrap_err_with(|| format!("failed to create {}", parent))?;
    }
    fs::create_dir_all(checkout_dir)
        .wrap_err_with(|| format!("failed to create {}", checkout_dir))?;

    let files_by_path = files
        .iter()
        .map(|entry| normalize_relative_path(&entry.path).map(|path| (path, entry.clone())))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let reader = ChannelChunkReader::new(receiver);
    let decoder = GzDecoder::new(reader);
    let mut archive = Archive::new(decoder);
    let mut extracted_paths = BTreeSet::new();

    for entry in archive
        .entries()
        .wrap_err("failed to read source archive entries")?
    {
        let mut entry = entry.wrap_err("failed to read source archive entry")?;
        if entry.header().entry_type().is_dir() {
            continue;
        }
        let path = entry
            .path()
            .wrap_err("failed to read source archive entry path")?
            .into_owned();
        let path = Utf8PathBuf::from_path_buf(path)
            .map_err(|path| eyre::eyre!("non-utf8 path in source archive: {}", path.display()))?;
        let relpath = normalize_relative_path(path.as_str())?;
        if !files_by_path.contains_key(&relpath) {
            eyre::bail!("source archive contained unexpected path {relpath}");
        }
        let full_path = checkout_dir.join(&relpath);
        let parent = full_path
            .parent()
            .ok_or_else(|| eyre::eyre!("path has no parent: {}", full_path))?;
        fs::create_dir_all(parent).wrap_err_with(|| format!("failed to create {}", parent))?;
        entry
            .unpack_in(checkout_dir)
            .wrap_err_with(|| format!("failed to unpack {} into {}", relpath, checkout_dir))?;
        extracted_paths.insert(relpath);
    }

    let expected_paths = files_by_path.keys().cloned().collect::<BTreeSet<_>>();
    if extracted_paths != expected_paths {
        let missing = expected_paths
            .difference(&extracted_paths)
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            eyre::bail!(
                "source archive did not contain {} expected paths: {}",
                missing.len(),
                missing.join(", ")
            );
        }
    }

    let mut cached_manifest = load_cached_source_manifest(checkout_dir)?;
    for relpath in extracted_paths {
        let entry = files_by_path
            .get(&relpath)
            .ok_or_else(|| eyre::eyre!("missing manifest entry for extracted path {relpath}"))?;
        let full_path = checkout_dir.join(&relpath);
        set_file_executable(&full_path, entry.executable)?;
        cached_manifest.insert(relpath, entry.clone());
    }
    save_cached_source_manifest(checkout_dir, &cached_manifest)?;
    Ok(SourceArchiveAppliedResponse {
        applied_files: files.len() as u64,
    })
}

fn emit_progress<W: Write>(
    writer: &mut W,
    phase: &str,
    message: impl Into<String>,
    current: Option<u64>,
    total: Option<u64>,
) -> Result<()> {
    write_frame(
        writer,
        &BootstrapResponse::Progress(BootstrapProgress {
            phase: phase.to_string(),
            message: message.into(),
            current,
            total,
        }),
    )
}

pub fn get_cached_source_manifest(
    request: &GetCachedSourceManifestRequest,
) -> Result<GetCachedSourceManifestResponse> {
    let checkout_dir = Utf8Path::new(&request.checkout_dir);
    let files = load_cached_source_manifest(checkout_dir)?
        .into_values()
        .collect::<Vec<_>>();
    Ok(GetCachedSourceManifestResponse { files })
}

fn get_cached_source_manifest_with_progress<W: Write>(
    request: &GetCachedSourceManifestRequest,
    writer: &mut W,
) -> Result<GetCachedSourceManifestResponse> {
    emit_progress(
        writer,
        "get_cached_source_manifest",
        format!(
            "loading cached source manifest from {}",
            request.checkout_dir
        ),
        None,
        None,
    )?;
    let response = get_cached_source_manifest(request)?;
    emit_progress(
        writer,
        "get_cached_source_manifest",
        format!("loaded {} cached source entries", response.files.len()),
        Some(response.files.len() as u64),
        Some(response.files.len() as u64),
    )?;
    Ok(response)
}

pub fn plan_source_sync(request: &PlanSourceSyncRequest) -> Result<PlanSourceSyncResponse> {
    let checkout_dir = Utf8Path::new(&request.checkout_dir);
    if let Some(parent) = checkout_dir.parent() {
        fs::create_dir_all(parent).wrap_err_with(|| format!("failed to create {}", parent))?;
    }
    fs::create_dir_all(checkout_dir)
        .wrap_err_with(|| format!("failed to create {}", checkout_dir))?;

    let manifest = request
        .files
        .iter()
        .map(|entry| {
            normalize_relative_path(&entry.path).map(|path| {
                (
                    path,
                    SourceFileEntry {
                        path: entry.path.clone(),
                        blake3_hex: entry.blake3_hex.clone(),
                        executable: entry.executable,
                        size: entry.size,
                        modified_unix_millis: entry.modified_unix_millis,
                    },
                )
            })
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut cached_manifest = load_cached_source_manifest(checkout_dir)?;

    let mut deleted_paths = Vec::new();
    for cached_path in cached_manifest.keys().cloned().collect::<Vec<_>>() {
        if !manifest.contains_key(&cached_path) {
            let full_path = checkout_dir.join(&cached_path);
            match fs::remove_file(&full_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).wrap_err_with(|| format!("failed to remove {}", full_path));
                }
            }
            deleted_paths.push(cached_path.clone());
            cached_manifest.remove(&cached_path);
        }
    }
    save_cached_source_manifest(checkout_dir, &cached_manifest)?;

    let mut missing_paths = Vec::new();
    for (path, entry) in &manifest {
        let Some(cached_entry) = cached_manifest.get(path) else {
            missing_paths.push(path.clone());
            continue;
        };
        if cached_entry != entry {
            missing_paths.push(path.clone());
            continue;
        }
        let full_path = checkout_dir.join(path);
        if !full_path.is_file() {
            missing_paths.push(path.clone());
            continue;
        }
        let metadata =
            fs::metadata(&full_path).wrap_err_with(|| format!("failed to stat {}", full_path))?;
        if metadata.len() != entry.size || file_is_executable(&metadata) != entry.executable {
            missing_paths.push(path.clone());
        }
    }

    Ok(PlanSourceSyncResponse {
        missing_paths,
        deleted_paths,
    })
}

fn plan_source_sync_with_progress<W: Write>(
    request: &PlanSourceSyncRequest,
    writer: &mut W,
) -> Result<PlanSourceSyncResponse> {
    emit_progress(
        writer,
        "plan_source_sync",
        format!("planning source sync for {} files", request.files.len()),
        Some(0),
        Some(request.files.len() as u64),
    )?;
    let response = plan_source_sync(request)?;
    emit_progress(
        writer,
        "plan_source_sync",
        format!(
            "planned source sync: {} missing, {} deleted",
            response.missing_paths.len(),
            response.deleted_paths.len()
        ),
        Some(request.files.len() as u64),
        Some(request.files.len() as u64),
    )?;
    Ok(response)
}

pub fn build_source_tree(request: &BuildSourceTreeRequest) -> Result<BuildSourceTreeResponse> {
    let checkout_dir = Utf8Path::new(&request.checkout_dir);
    let scrub_backups = snapshot_scrubbed_files(
        checkout_dir,
        &request.scrub_files,
        &request.scrub_line_contains,
    )?;
    let build_result = (|| {
        stop_prior_bootstrap_processes()?;
        scrub_files(
            checkout_dir,
            &request.scrub_files,
            &request.scrub_line_contains,
        )?;

        let manifest_path = checkout_dir.join(&request.build_manifest_relpath);
        let mut cargo_args = vec![
            "build".to_string(),
            "--release".to_string(),
            "--manifest-path".to_string(),
            manifest_path.as_str().to_string(),
        ];
        for package in &request.build_packages {
            cargo_args.push("-p".to_string());
            cargo_args.push(package.clone());
        }
        let cargo_args = cargo_args.iter().map(String::as_str).collect::<Vec<_>>();
        run_command("cargo", &cargo_args, Some(checkout_dir))
    })();
    restore_scrubbed_files(checkout_dir, scrub_backups)?;
    build_result?;

    Ok(BuildSourceTreeResponse {
        checkout_dir: checkout_dir.to_string(),
    })
}

fn build_source_tree_with_progress<W: Write>(
    request: &BuildSourceTreeRequest,
    writer: &mut W,
) -> Result<BuildSourceTreeResponse> {
    emit_progress(
        writer,
        "build_source_tree",
        format!(
            "building {} packages from {}",
            request.build_packages.len(),
            request.checkout_dir
        ),
        Some(0),
        Some(request.build_packages.len() as u64),
    )?;
    let response = build_source_tree(request)?;
    emit_progress(
        writer,
        "build_source_tree",
        format!("cargo build finished for {}", request.checkout_dir),
        Some(request.build_packages.len() as u64),
        Some(request.build_packages.len() as u64),
    )?;
    Ok(response)
}

fn normalize_relative_path(path: &str) -> Result<String> {
    if path.is_empty() {
        eyre::bail!("path must not be empty");
    }
    if path.starts_with('/') || path.contains('\\') {
        eyre::bail!("path must be a normalized relative path: {path}");
    }
    let parts = path.split('/').collect::<Vec<_>>();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        eyre::bail!("path must be a normalized relative path: {path}");
    }
    Ok(path.to_string())
}

fn cached_source_manifest_path(checkout_dir: &Utf8Path) -> Utf8PathBuf {
    checkout_dir
        .join(".vx-bootstrap")
        .join("source-manifest.postcard")
}

#[derive(Debug, Clone, Facet)]
struct LegacySourceFileEntry {
    path: String,
    blake3_hex: String,
    executable: bool,
    size: u64,
}

fn load_cached_source_manifest(
    checkout_dir: &Utf8Path,
) -> Result<BTreeMap<String, SourceFileEntry>> {
    let path = cached_source_manifest_path(checkout_dir);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let bytes = fs::read(&path).wrap_err_with(|| format!("failed to read {}", path))?;
    let entries = match facet_postcard::from_slice::<Vec<SourceFileEntry>>(&bytes) {
        Ok(entries) => entries,
        Err(_) => facet_postcard::from_slice::<Vec<LegacySourceFileEntry>>(&bytes)
            .wrap_err_with(|| format!("failed to decode cached source manifest {}", path))?
            .into_iter()
            .map(|entry| SourceFileEntry {
                path: entry.path,
                blake3_hex: entry.blake3_hex,
                executable: entry.executable,
                size: entry.size,
                modified_unix_millis: None,
            })
            .collect(),
    };
    Ok(entries
        .into_iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect())
}

fn save_cached_source_manifest(
    checkout_dir: &Utf8Path,
    manifest: &BTreeMap<String, SourceFileEntry>,
) -> Result<()> {
    let path = cached_source_manifest_path(checkout_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).wrap_err_with(|| format!("failed to create {}", parent))?;
    }
    let entries = manifest.values().cloned().collect::<Vec<_>>();
    let bytes =
        facet_postcard::to_vec(&entries).wrap_err("failed to encode cached source manifest")?;
    fs::write(&path, bytes).wrap_err_with(|| format!("failed to write {}", path))?;
    Ok(())
}

pub fn file_modified_unix_millis(metadata: &fs::Metadata) -> Option<u64> {
    let modified = metadata.modified().ok()?;
    let duration = modified.duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(duration.as_millis()).ok()
}

fn snapshot_scrubbed_files(
    checkout_dir: &Utf8Path,
    scrub_files: &[String],
    scrub_needles: &[String],
) -> Result<Vec<(String, Vec<u8>)>> {
    if scrub_files.is_empty() || scrub_needles.is_empty() {
        return Ok(Vec::new());
    }
    let mut backups = Vec::new();
    for relpath in scrub_files {
        let relpath = normalize_relative_path(relpath)?;
        let path = checkout_dir.join(&relpath);
        if !path.exists() {
            continue;
        }
        backups.push((
            relpath,
            fs::read(&path).wrap_err_with(|| format!("failed to read {}", path))?,
        ));
    }
    Ok(backups)
}

fn restore_scrubbed_files(checkout_dir: &Utf8Path, backups: Vec<(String, Vec<u8>)>) -> Result<()> {
    for (relpath, contents) in backups {
        let path = checkout_dir.join(&relpath);
        fs::write(&path, contents).wrap_err_with(|| format!("failed to restore {}", path))?;
    }
    Ok(())
}

fn scrub_files(
    checkout_dir: &Utf8Path,
    scrub_files: &[String],
    scrub_needles: &[String],
) -> Result<()> {
    if scrub_files.is_empty() || scrub_needles.is_empty() {
        return Ok(());
    }
    for relpath in scrub_files {
        let relpath = normalize_relative_path(relpath)?;
        let path = checkout_dir.join(&relpath);
        if !path.exists() {
            continue;
        }
        let text =
            fs::read_to_string(&path).wrap_err_with(|| format!("failed to read {}", path))?;
        let mut changed = false;
        let mut updated = Vec::new();
        for line in text.lines() {
            if scrub_needles.iter().any(|needle| line.contains(needle)) {
                changed = true;
                continue;
            }
            updated.push(line);
        }
        if changed {
            let mut new_text = updated.join("\n");
            if text.ends_with('\n') {
                new_text.push('\n');
            }
            fs::write(&path, new_text).wrap_err_with(|| format!("failed to write {}", path))?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn file_is_executable(metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn file_is_executable(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn set_file_executable(path: &Utf8Path, executable: bool) -> Result<()> {
    let mode = if executable { 0o755 } else { 0o644 };
    let permissions = fs::Permissions::from_mode(mode);
    fs::set_permissions(path, permissions)
        .wrap_err_with(|| format!("failed to set permissions on {}", path))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_executable(_path: &Utf8Path, _executable: bool) -> Result<()> {
    Ok(())
}

fn run_command(program: &str, args: &[&str], current_dir: Option<&Utf8Path>) -> Result<()> {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(current_dir) = current_dir {
        command.current_dir(current_dir);
    }
    command.stdin(Stdio::null());
    let output = command
        .output()
        .wrap_err_with(|| format!("failed to spawn {program}"))?;
    if output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let mut detail = format!("{program} failed with {}", output.status);
    if !stdout.is_empty() {
        detail.push_str(&format!("\nstdout:\n{stdout}"));
    }
    if !stderr.is_empty() {
        detail.push_str(&format!("\nstderr:\n{stderr}"));
    }
    eyre::bail!(detail);
}

#[cfg(not(windows))]
fn stop_prior_bootstrap_processes() -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn stop_prior_bootstrap_processes() -> Result<()> {
    const IMAGE_NAMES: &[&str] = &["vx-store.exe", "vx-runner.exe", "vx-vfsd.exe"];

    let mut kill = Command::new("taskkill");
    kill.args(["/F", "/T"]);
    for image in IMAGE_NAMES {
        kill.args(["/IM", image]);
    }
    kill.stdin(Stdio::null());
    let output = kill
        .output()
        .wrap_err("failed to spawn taskkill for bootstrap-owned processes")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("not found") && !stderr.contains("There are no running tasks") {
            let stdout = String::from_utf8_lossy(&output.stdout);
            eyre::bail!(
                "taskkill failed with {} while stopping prior bootstrap processes\nstdout:\n{}\nstderr:\n{}",
                output.status,
                stdout.trim(),
                stderr.trim()
            );
        }
    }

    for _ in 0..50 {
        if !windows_processes_running(IMAGE_NAMES)? {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    eyre::bail!("timed out waiting for prior bootstrap processes to exit");
}

#[cfg(windows)]
fn windows_processes_running(image_names: &[&str]) -> Result<bool> {
    let output = Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .stdin(Stdio::null())
        .output()
        .wrap_err("failed to spawn tasklist for bootstrap-owned processes")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eyre::bail!(
            "tasklist failed with {} while checking bootstrap-owned processes\nstderr:\n{}",
            output.status,
            stderr.trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    Ok(image_names
        .iter()
        .any(|image| stdout.contains(&format!("\"{}\"", image.to_ascii_lowercase()))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_frames_roundtrip() {
        let request = BootstrapRequest::PlanSourceSync(PlanSourceSyncRequest {
            checkout_dir: "/tmp/repo".to_string(),
            files: vec![SourceFileEntry {
                path: "Cargo.toml".to_string(),
                blake3_hex: "deadbeef".to_string(),
                executable: false,
                size: 17,
                modified_unix_millis: Some(123),
            }],
        });
        let payload = facet_postcard::to_vec(&request).expect("encode request");
        let mut framed = Vec::new();
        framed.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        framed.extend_from_slice(&payload);
        let decoded = read_frame(&mut framed.as_slice())
            .expect("read frame")
            .expect("request");
        match decoded {
            BootstrapRequest::PlanSourceSync(decoded) => {
                assert_eq!(decoded.checkout_dir, "/tmp/repo");
                assert_eq!(decoded.files[0].path, "Cargo.toml");
            }
            other => panic!("unexpected request: {other:?}"),
        }
    }

    #[test]
    fn bootstrap_progress_frames_roundtrip() {
        let response = BootstrapResponse::Progress(BootstrapProgress {
            phase: "plan_source_sync".to_string(),
            message: "planned source sync".to_string(),
            current: Some(10),
            total: Some(12),
        });
        let payload = facet_postcard::to_vec(&response).expect("encode response");
        let mut framed = Vec::new();
        framed.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        framed.extend_from_slice(&payload);
        let decoded = read_response_frame(&mut framed.as_slice())
            .expect("read response frame")
            .expect("response");
        match decoded {
            BootstrapResponse::Progress(decoded) => {
                assert_eq!(decoded.phase, "plan_source_sync");
                assert_eq!(decoded.message, "planned source sync");
                assert_eq!(decoded.current, Some(10));
                assert_eq!(decoded.total, Some(12));
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn plan_source_sync_detects_missing_changed_and_stale_files() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let checkout_dir =
            Utf8PathBuf::from_path_buf(tempdir.path().join("checkout")).expect("utf8");
        fs::create_dir_all(checkout_dir.join("src")).expect("src dir");
        fs::create_dir_all(checkout_dir.join("target")).expect("target dir");
        fs::write(checkout_dir.join("src/lib.rs"), "old").expect("write src");
        fs::write(checkout_dir.join("stale.txt"), "stale").expect("write stale");
        fs::write(checkout_dir.join("target/keep.txt"), "keep").expect("write target");
        save_cached_source_manifest(
            &checkout_dir,
            &BTreeMap::from([
                (
                    "src/lib.rs".to_string(),
                    SourceFileEntry {
                        path: "src/lib.rs".to_string(),
                        blake3_hex: blake3::hash(b"old").to_hex().to_string(),
                        executable: false,
                        size: 3,
                        modified_unix_millis: Some(1),
                    },
                ),
                (
                    "stale.txt".to_string(),
                    SourceFileEntry {
                        path: "stale.txt".to_string(),
                        blake3_hex: blake3::hash(b"stale").to_hex().to_string(),
                        executable: false,
                        size: 5,
                        modified_unix_millis: Some(2),
                    },
                ),
            ]),
        )
        .expect("seed cached manifest");

        let request = PlanSourceSyncRequest {
            checkout_dir: checkout_dir.to_string(),
            files: vec![
                SourceFileEntry {
                    path: "Cargo.toml".to_string(),
                    blake3_hex: blake3::hash(b"[package]\nname = 'vx'\n")
                        .to_hex()
                        .to_string(),
                    executable: false,
                    size: 22,
                    modified_unix_millis: Some(3),
                },
                SourceFileEntry {
                    path: "src/lib.rs".to_string(),
                    blake3_hex: blake3::hash(b"new").to_hex().to_string(),
                    executable: false,
                    size: 3,
                    modified_unix_millis: Some(4),
                },
            ],
        };
        let response = plan_source_sync(&request).expect("plan source sync");
        assert_eq!(
            response.missing_paths,
            vec!["Cargo.toml".to_string(), "src/lib.rs".to_string()]
        );
        assert_eq!(response.deleted_paths, vec!["stale.txt".to_string()]);
        assert!(!checkout_dir.join("stale.txt").exists());
        assert!(checkout_dir.join("target/keep.txt").exists());
    }

    #[test]
    fn streamed_source_archive_writes_contents() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let checkout_dir =
            Utf8PathBuf::from_path_buf(tempdir.path().join("checkout")).expect("utf8");
        fs::create_dir_all(&checkout_dir).expect("checkout dir");
        let file = SourceFileEntry {
            path: "src/main.rs".to_string(),
            blake3_hex: blake3::hash(b"fn main() {}\n").to_hex().to_string(),
            executable: false,
            size: 13,
            modified_unix_millis: Some(123),
        };
        let mut archive_bytes = Vec::new();
        {
            let encoder =
                flate2::write::GzEncoder::new(&mut archive_bytes, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(13);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "src/main.rs", &b"fn main() {}\n"[..])
                .expect("append source file");
            let encoder = builder.into_inner().expect("finish tar");
            encoder.finish().expect("finish gzip");
        }
        let mut state = BootstrapServerState::default();
        begin_source_archive(
            &mut state,
            &BeginSourceArchiveRequest {
                checkout_dir: checkout_dir.to_string(),
                files: vec![file.clone()],
            },
        )
        .expect("begin source archive");
        for chunk in archive_bytes.chunks(5) {
            push_source_archive_chunk(
                &mut state,
                SourceArchiveChunkRequest {
                    bytes: chunk.to_vec(),
                },
            )
            .expect("push chunk");
        }
        let response = finish_source_archive(&mut state).expect("finish source archive");
        assert_eq!(response.applied_files, 1);
        assert_eq!(
            fs::read_to_string(checkout_dir.join("src/main.rs")).expect("read file"),
            "fn main() {}\n"
        );
    }

    #[test]
    fn scrub_files_removes_matching_lines() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let manifest = root.join("Cargo.toml");
        fs::write(
            &manifest,
            "a = 1\ntree-sitter-vixen = { path = \"/tmp/tree-sitter-vixen\" }\nb = 2\n",
        )
        .expect("write manifest");

        scrub_files(
            &root,
            &["Cargo.toml".to_string()],
            &["tree-sitter-vixen".to_string()],
        )
        .expect("scrub manifest");

        let updated = fs::read_to_string(&manifest).expect("read manifest");
        assert_eq!(updated, "a = 1\nb = 2\n");
    }

    #[test]
    fn detect_host_facts_reports_protocol_version() {
        let facts = detect_host_facts();
        assert_eq!(facts.protocol_version, BOOTSTRAP_PROTOCOL_VERSION);
        assert!(!facts.os.is_empty());
        assert!(!facts.arch.is_empty());
    }
}
