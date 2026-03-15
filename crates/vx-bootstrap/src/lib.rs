use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

use camino::{Utf8Path, Utf8PathBuf};
use eyre::{Context, Result};
use facet::Facet;

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
}

#[derive(Debug, Clone, Facet)]
pub struct PlanSourceSyncRequest {
    pub checkout_dir: String,
    pub files: Vec<SourceFileEntry>,
    pub preserve_roots: Vec<String>,
}

#[derive(Debug, Clone, Facet)]
pub struct PlanSourceSyncResponse {
    pub missing_paths: Vec<String>,
    pub deleted_paths: Vec<String>,
}

#[derive(Debug, Clone, Facet)]
pub struct PutSourceFileRequest {
    pub checkout_dir: String,
    pub path: String,
    pub executable: bool,
    pub contents: Vec<u8>,
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
#[repr(u8)]
pub enum BootstrapRequest {
    Hello,
    PlanSourceSync(PlanSourceSyncRequest),
    PutSourceFile(PutSourceFileRequest),
    BuildSourceTree(BuildSourceTreeRequest),
    LaunchRunner(LaunchRunnerRequest),
    Shutdown,
}

#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum BootstrapResponse {
    Hello(HostFacts),
    PlanSourceSync(PlanSourceSyncResponse),
    PutSourceFileAck,
    BuildSourceTree(BuildSourceTreeResponse),
    LaunchRunnerReady,
    ShuttingDown,
    Error(BootstrapError),
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
    while let Some(request) = read_frame(&mut reader)? {
        let should_exit = matches!(request, BootstrapRequest::Shutdown);
        let response = handle_request(request);
        write_frame(&mut writer, &response)?;
        if should_exit {
            break;
        }
    }
    Ok(())
}

fn handle_request(request: BootstrapRequest) -> BootstrapResponse {
    match request {
        BootstrapRequest::Hello => BootstrapResponse::Hello(detect_host_facts()),
        BootstrapRequest::PlanSourceSync(request) => match plan_source_sync(&request) {
            Ok(response) => BootstrapResponse::PlanSourceSync(response),
            Err(error) => BootstrapResponse::Error(BootstrapError {
                kind: "plan_source_sync".to_string(),
                message: format!("{error:#}"),
            }),
        },
        BootstrapRequest::PutSourceFile(request) => match put_source_file(&request) {
            Ok(()) => BootstrapResponse::PutSourceFileAck,
            Err(error) => BootstrapResponse::Error(BootstrapError {
                kind: "put_source_file".to_string(),
                message: format!("{error:#}"),
            }),
        },
        BootstrapRequest::BuildSourceTree(request) => match build_source_tree(&request) {
            Ok(response) => BootstrapResponse::BuildSourceTree(response),
            Err(error) => BootstrapResponse::Error(BootstrapError {
                kind: "build_source_tree".to_string(),
                message: format!("{error:#}"),
            }),
        },
        BootstrapRequest::LaunchRunner(_request) => BootstrapResponse::Error(BootstrapError {
            kind: "launch_runner".to_string(),
            message: "launch_runner is not implemented yet".to_string(),
        }),
        BootstrapRequest::Shutdown => BootstrapResponse::ShuttingDown,
    }
}

pub fn plan_source_sync(request: &PlanSourceSyncRequest) -> Result<PlanSourceSyncResponse> {
    let checkout_dir = Utf8Path::new(&request.checkout_dir);
    if let Some(parent) = checkout_dir.parent() {
        fs::create_dir_all(parent).wrap_err_with(|| format!("failed to create {}", parent))?;
    }
    fs::create_dir_all(checkout_dir)
        .wrap_err_with(|| format!("failed to create {}", checkout_dir))?;

    let preserve_roots = request
        .preserve_roots
        .iter()
        .map(|path| normalize_relative_path(path))
        .collect::<Result<BTreeSet<_>>>()?;
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
                    },
                )
            })
        })
        .collect::<Result<BTreeMap<_, _>>>()?;

    let mut deleted_paths = Vec::new();
    let remote_paths = collect_checkout_files(checkout_dir, &preserve_roots)?;
    for remote_path in remote_paths {
        if !manifest.contains_key(&remote_path) {
            let full_path = checkout_dir.join(&remote_path);
            fs::remove_file(&full_path)
                .wrap_err_with(|| format!("failed to remove {}", full_path))?;
            deleted_paths.push(remote_path);
        }
    }

    let mut missing_paths = Vec::new();
    for (path, entry) in &manifest {
        let full_path = checkout_dir.join(path);
        if !full_path.is_file() {
            missing_paths.push(path.clone());
            continue;
        }
        let metadata =
            fs::metadata(&full_path).wrap_err_with(|| format!("failed to stat {}", full_path))?;
        if metadata.len() != entry.size {
            missing_paths.push(path.clone());
            continue;
        }
        let bytes =
            fs::read(&full_path).wrap_err_with(|| format!("failed to read {}", full_path))?;
        let hash = blake3::hash(&bytes).to_hex().to_string();
        if hash != entry.blake3_hex || file_is_executable(&metadata) != entry.executable {
            missing_paths.push(path.clone());
        }
    }

    Ok(PlanSourceSyncResponse {
        missing_paths,
        deleted_paths,
    })
}

pub fn put_source_file(request: &PutSourceFileRequest) -> Result<()> {
    let checkout_dir = Utf8Path::new(&request.checkout_dir);
    let relpath = normalize_relative_path(&request.path)?;
    let path = checkout_dir.join(&relpath);
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("path has no parent: {}", path))?;
    fs::create_dir_all(parent).wrap_err_with(|| format!("failed to create {}", parent))?;
    let temp_path = path.with_extension(format!(
        "{}.tmp.{}",
        path.extension().unwrap_or("sync"),
        std::process::id()
    ));
    fs::write(&temp_path, &request.contents)
        .wrap_err_with(|| format!("failed to write {}", temp_path))?;
    set_file_executable(&temp_path, request.executable)?;
    fs::rename(&temp_path, &path)
        .wrap_err_with(|| format!("failed to rename {} to {}", temp_path, path))?;
    Ok(())
}

pub fn build_source_tree(request: &BuildSourceTreeRequest) -> Result<BuildSourceTreeResponse> {
    let checkout_dir = Utf8Path::new(&request.checkout_dir);
    let scrub_backups = snapshot_scrubbed_files(
        checkout_dir,
        &request.scrub_files,
        &request.scrub_line_contains,
    )?;
    let build_result = (|| {
        scrub_files(
            checkout_dir,
            &request.scrub_files,
            &request.scrub_line_contains,
        )?;

        let manifest_path = checkout_dir.join(&request.build_manifest_relpath);
        let mut cargo_args = vec![
            "build".to_string(),
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

fn should_preserve_path(path: &str, preserve_roots: &BTreeSet<String>) -> bool {
    preserve_roots
        .iter()
        .any(|root| path == root || path.starts_with(&format!("{root}/")))
}

fn collect_checkout_files(
    root: &Utf8Path,
    preserve_roots: &BTreeSet<String>,
) -> Result<Vec<String>> {
    let mut files = Vec::new();
    collect_checkout_files_recursive(root, root, preserve_roots, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_checkout_files_recursive(
    root: &Utf8Path,
    current: &Utf8Path,
    preserve_roots: &BTreeSet<String>,
    files: &mut Vec<String>,
) -> Result<()> {
    for entry in fs::read_dir(current).wrap_err_with(|| format!("failed to read {}", current))? {
        let entry = entry.wrap_err_with(|| format!("failed to read entry under {}", current))?;
        let path = Utf8PathBuf::from_path_buf(entry.path())
            .map_err(|path| eyre::eyre!("non-utf8 path in checkout: {}", path.display()))?;
        let relpath = path
            .strip_prefix(root)
            .map_err(|error| eyre::eyre!("failed to strip {} from {}: {error}", root, path))?
            .to_string();
        if should_preserve_path(&relpath, preserve_roots) {
            continue;
        }
        let metadata =
            fs::symlink_metadata(&path).wrap_err_with(|| format!("failed to stat {}", path))?;
        if metadata.file_type().is_symlink() {
            eyre::bail!(
                "symlinks are not supported in synced source trees: {}",
                path
            );
        }
        if metadata.is_dir() {
            collect_checkout_files_recursive(root, &path, preserve_roots, files)?;
        } else if metadata.is_file() {
            files.push(relpath);
        }
    }
    Ok(())
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
            }],
            preserve_roots: vec!["target".to_string()],
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
    fn plan_source_sync_detects_missing_changed_and_stale_files() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let checkout_dir =
            Utf8PathBuf::from_path_buf(tempdir.path().join("checkout")).expect("utf8");
        fs::create_dir_all(checkout_dir.join("src")).expect("src dir");
        fs::create_dir_all(checkout_dir.join("target")).expect("target dir");
        fs::write(checkout_dir.join("src/lib.rs"), "old").expect("write src");
        fs::write(checkout_dir.join("stale.txt"), "stale").expect("write stale");
        fs::write(checkout_dir.join("target/keep.txt"), "keep").expect("write target");

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
                },
                SourceFileEntry {
                    path: "src/lib.rs".to_string(),
                    blake3_hex: blake3::hash(b"new").to_hex().to_string(),
                    executable: false,
                    size: 3,
                },
            ],
            preserve_roots: vec!["target".to_string()],
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
    fn put_source_file_writes_contents() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let checkout_dir =
            Utf8PathBuf::from_path_buf(tempdir.path().join("checkout")).expect("utf8");
        fs::create_dir_all(&checkout_dir).expect("checkout dir");
        put_source_file(&PutSourceFileRequest {
            checkout_dir: checkout_dir.to_string(),
            path: "src/main.rs".to_string(),
            executable: false,
            contents: b"fn main() {}\n".to_vec(),
        })
        .expect("put source file");
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
