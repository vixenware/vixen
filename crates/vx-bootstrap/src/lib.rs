use std::env;
use std::fs;
use std::io::{Read, Write};
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

#[derive(Debug, Clone, Facet)]
pub struct PrepareSourceBuildRequest {
    pub bare_repo_dir: String,
    pub checkout_dir: String,
    pub commit: String,
    pub fetch_ref: String,
    pub build_manifest_relpath: String,
    pub build_packages: Vec<String>,
    pub restore_paths: Vec<String>,
    pub scrub_files: Vec<String>,
    pub scrub_line_contains: Vec<String>,
}

#[derive(Debug, Clone, Facet)]
pub struct PrepareSourceBuildResponse {
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
    PrepareSourceBuild(PrepareSourceBuildRequest),
    LaunchRunner(LaunchRunnerRequest),
    Shutdown,
}

#[derive(Debug, Clone, Facet)]
#[repr(u8)]
pub enum BootstrapResponse {
    Hello(HostFacts),
    PrepareSourceBuild(PrepareSourceBuildResponse),
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
        BootstrapRequest::PrepareSourceBuild(request) => match prepare_source_build(&request) {
            Ok(response) => BootstrapResponse::PrepareSourceBuild(response),
            Err(error) => BootstrapResponse::Error(BootstrapError {
                kind: "prepare_source_build".to_string(),
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

pub fn prepare_source_build(
    request: &PrepareSourceBuildRequest,
) -> Result<PrepareSourceBuildResponse> {
    let bare_repo_dir = Utf8Path::new(&request.bare_repo_dir);
    let checkout_dir = Utf8Path::new(&request.checkout_dir);
    let bare_repo_parent = bare_repo_dir
        .parent()
        .ok_or_else(|| eyre::eyre!("bare_repo_dir has no parent: {}", bare_repo_dir))?;
    let checkout_parent = checkout_dir
        .parent()
        .ok_or_else(|| eyre::eyre!("checkout_dir has no parent: {}", checkout_dir))?;

    fs::create_dir_all(bare_repo_parent)
        .wrap_err_with(|| format!("failed to create {}", bare_repo_parent))?;
    fs::create_dir_all(checkout_parent)
        .wrap_err_with(|| format!("failed to create {}", checkout_parent))?;

    if !bare_repo_dir.join("HEAD").exists() {
        if bare_repo_dir.exists() {
            fs::remove_dir_all(bare_repo_dir)
                .wrap_err_with(|| format!("failed to remove {}", bare_repo_dir))?;
        }
        run_command("git", &["init", "--bare", bare_repo_dir.as_str()], None)?;
    }

    if !checkout_dir.join(".git").exists() {
        if checkout_dir.exists() {
            fs::remove_dir_all(checkout_dir)
                .wrap_err_with(|| format!("failed to remove {}", checkout_dir))?;
        }
        run_command(
            "git",
            &["clone", bare_repo_dir.as_str(), checkout_dir.as_str()],
            None,
        )?;
    }

    run_command(
        "git",
        &[
            "-C",
            checkout_dir.as_str(),
            "remote",
            "set-url",
            "origin",
            bare_repo_dir.as_str(),
        ],
        None,
    )?;

    restore_paths(checkout_dir, &request.restore_paths)?;

    run_command(
        "git",
        &[
            "-C",
            checkout_dir.as_str(),
            "fetch",
            "origin",
            &request.fetch_ref,
        ],
        None,
    )?;
    run_command(
        "git",
        &[
            "-C",
            checkout_dir.as_str(),
            "checkout",
            "--detach",
            &request.commit,
        ],
        None,
    )?;

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
    run_command("cargo", &cargo_args, Some(checkout_dir))?;

    restore_paths(checkout_dir, &request.restore_paths)?;

    Ok(PrepareSourceBuildResponse {
        checkout_dir: checkout_dir.to_string(),
    })
}

fn restore_paths(checkout_dir: &Utf8Path, restore_paths: &[String]) -> Result<()> {
    if restore_paths.is_empty() {
        return Ok(());
    }
    let mut args = vec![
        "-C".to_string(),
        checkout_dir.as_str().to_string(),
        "checkout".to_string(),
        "--".to_string(),
    ];
    args.extend(restore_paths.iter().cloned());
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    run_command("git", &args, None)?;
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
        let path = checkout_dir.join(relpath);
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
        let request = BootstrapRequest::PrepareSourceBuild(PrepareSourceBuildRequest {
            bare_repo_dir: "/tmp/repo.git".to_string(),
            checkout_dir: "/tmp/repo".to_string(),
            commit: "deadbeef".to_string(),
            fetch_ref: "refs/vixen-bootstrap/deadbeef".to_string(),
            build_manifest_relpath: "Cargo.toml".to_string(),
            build_packages: vec!["vx-runner".to_string()],
            restore_paths: vec!["Cargo.lock".to_string()],
            scrub_files: vec!["crates/vx/Cargo.toml".to_string()],
            scrub_line_contains: vec!["tree-sitter-vixen".to_string()],
        });
        let payload = facet_postcard::to_vec(&request).expect("encode request");
        let mut framed = Vec::new();
        framed.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        framed.extend_from_slice(&payload);
        let decoded = read_frame(&mut framed.as_slice())
            .expect("read frame")
            .expect("request");
        match decoded {
            BootstrapRequest::PrepareSourceBuild(decoded) => {
                assert_eq!(decoded.commit, "deadbeef");
                assert_eq!(decoded.build_packages, vec!["vx-runner"]);
            }
            other => panic!("unexpected request: {other:?}"),
        }
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
