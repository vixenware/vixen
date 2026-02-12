//! VFS stress test orchestrator.
//!
//! Launches real vxd → vx-store → vx-runner processes, then drives concurrent
//! `vx build` commands to exercise the full VFS code path (FUSE on Linux, FSKit on macOS).
//! Detects hangs and dumps diagnostics.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eyre::{Result, WrapErr, bail};
use tracing::{error, info, warn};

// ── CLI args ──────────────────────────────────────────────────────

struct Args {
    projects: usize,
    concurrency: usize,
    timeout_secs: u64,
    rounds: usize,
    forever: bool,
    skip_warmup: bool,
    cold_cache_each_round: bool,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            projects: 8,
            concurrency: 8,
            timeout_secs: 10,
            rounds: 20,
            forever: false,
            skip_warmup: false,
            cold_cache_each_round: false,
        }
    }
}

fn parse_args() -> Args {
    let mut args = Args::default();
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--projects" => {
                args.projects = iter
                    .next()
                    .expect("--projects value")
                    .parse()
                    .expect("number")
            }
            "--concurrency" => {
                args.concurrency = iter
                    .next()
                    .expect("--concurrency value")
                    .parse()
                    .expect("number")
            }
            "--timeout-secs" => {
                args.timeout_secs = iter
                    .next()
                    .expect("--timeout-secs value")
                    .parse()
                    .expect("number")
            }
            "--rounds" => {
                args.rounds = iter
                    .next()
                    .expect("--rounds value")
                    .parse()
                    .expect("number")
            }
            "--forever" => {
                args.forever = true;
            }
            "--skip-warmup" => {
                args.skip_warmup = true;
            }
            "--cold-cache-each-round" => {
                args.cold_cache_each_round = true;
            }
            other => {
                eprintln!("Unknown argument: {other}");
                eprintln!("Usage: vx-fuse-stress [OPTIONS]");
                eprintln!("  --projects <N>      Number of test projects (default: 8)");
                eprintln!("  --concurrency <N>   Concurrent builds per round (default: 8)");
                eprintln!("  --timeout-secs <N>  Hang detection timeout (default: 10)");
                eprintln!("  --rounds <N>        Number of stress rounds (default: 20)");
                eprintln!("  --forever           Run indefinitely until failure");
                eprintln!("  --skip-warmup       Skip initial warmup build");
                eprintln!(
                    "  --cold-cache-each-round  Clear VX_HOME/cache/blobs/blake3 before each round"
                );
                std::process::exit(1);
            }
        }
    }
    args
}

// ── Binary discovery ─────────────────────────────────────────────

fn find_sibling_binary(name: &str) -> Result<PathBuf> {
    let exe = std::env::current_exe().wrap_err("can't find current executable")?;
    let dir = exe.parent().unwrap();
    let candidate = dir.join(name);
    if candidate.exists() {
        Ok(candidate)
    } else {
        bail!(
            "{name} not found at {}. Build with: cargo build -p {name}",
            candidate.display()
        )
    }
}

// ── Project scaffolding ──────────────────────────────────────────

fn create_test_project(dir: &Path, index: usize) -> Result<()> {
    let src_dir = dir.join("src");
    std::fs::create_dir_all(&src_dir)?;
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            r#"[package]
name = "stress-{index}"
version = "0.1.0"
edition = "2021"
"#
        ),
    )?;
    write_project_source(dir, index, 0)?;
    Ok(())
}

fn write_project_source(dir: &Path, index: usize, round: usize) -> Result<()> {
    let src = format!("fn main() {{ let _project = {index}; let _round = {round}; }}\n");
    std::fs::write(dir.join("src/main.rs"), src)?;
    Ok(())
}

// ── Diagnostics (platform-specific) ──────────────────────────────

fn dump_hang_diagnostics(vx_home: &Path) {
    eprintln!("\n=== HANG DIAGNOSTICS ===\n");

    // Trigger peeps dumps via SIGUSR1 to all vixen processes
    #[cfg(unix)]
    let pids = find_vixen_pids();
    #[cfg(not(unix))]
    let pids: Vec<(String, u32)> = Vec::new();

    #[cfg(unix)]
    for (name, pid) in &pids {
        eprintln!("Sending SIGUSR1 to {name} (PID {pid})...");
        unsafe {
            libc::kill(*pid as i32, libc::SIGUSR1);
        }
    }
    if !pids.is_empty() {
        std::thread::sleep(Duration::from_millis(500));
    }

    // Read peeps dumps
    let dump_dir = Path::new("/tmp/peeps-dumps");
    if dump_dir.exists() {
        eprintln!("\n--- peeps dumps ---");
        if let Ok(entries) = std::fs::read_dir(dump_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json")
                    && let Ok(content) = std::fs::read_to_string(&path)
                {
                    eprintln!("  {}: {} bytes", path.display(), content.len());
                }
            }
        }
    }

    // Platform-specific diagnostics
    #[cfg(target_os = "linux")]
    dump_linux_diagnostics(vx_home, &pids);

    #[cfg(target_os = "macos")]
    dump_macos_diagnostics(vx_home, &pids);

    eprintln!("\n=== END DIAGNOSTICS ===\n");
}

#[cfg(unix)]
fn find_vixen_pids() -> Vec<(String, u32)> {
    let output = std::process::Command::new("pgrep")
        .args(["-la", "(vxd|vx-store|vx-runner|vx-vfsd)"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut pids = Vec::new();
    for line in stdout.lines() {
        let mut parts = line.splitn(2, ' ');
        if let (Some(pid_str), Some(name)) = (parts.next(), parts.next())
            && let Ok(pid) = pid_str.parse::<u32>()
        {
            // Extract just the binary name from the command path
            let binary_name = Path::new(name.split_whitespace().next().unwrap_or(name))
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| name.to_string());
            pids.push((binary_name, pid));
        }
    }
    pids
}

#[cfg(target_os = "linux")]
fn dump_linux_diagnostics(_vx_home: &Path, pids: &[(String, u32)]) {
    for (name, pid) in pids {
        eprintln!("\n--- {name} (PID {pid}) threads ---");
        let task_dir = format!("/proc/{pid}/task");
        let Ok(tasks) = std::fs::read_dir(&task_dir) else {
            eprintln!("  (cannot read {task_dir})");
            continue;
        };
        for task in tasks.flatten() {
            let tid = task.file_name();
            let tid_str = tid.to_string_lossy();
            let comm = std::fs::read_to_string(task.path().join("comm")).unwrap_or_default();
            let comm = comm.trim();
            let status = std::fs::read_to_string(task.path().join("status")).unwrap_or_default();
            let state = status
                .lines()
                .find(|l| l.starts_with("State:"))
                .unwrap_or("State: ?");
            let wchan = std::fs::read_to_string(task.path().join("wchan")).unwrap_or_default();
            let wchan = wchan.trim();
            let wchan_display = if wchan.is_empty() || wchan == "0" {
                "running"
            } else {
                wchan
            };
            eprintln!("  TID {tid_str} ({comm}): {state} | wchan={wchan_display}");
        }
    }

    // Kernel FUSE state
    eprintln!("\n--- Kernel FUSE state ---");
    if let Ok(entries) = std::fs::read_dir("/sys/fs/fuse/connections") {
        for entry in entries.flatten() {
            let waiting_path = entry.path().join("waiting");
            if let Ok(val) = std::fs::read_to_string(&waiting_path) {
                eprintln!("  {}: waiting={}", entry.path().display(), val.trim());
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn dump_macos_diagnostics(vx_home: &Path, pids: &[(String, u32)]) {
    // Sample stuck processes (non-blocking, 1 second sample)
    for (name, pid) in pids {
        eprintln!("\n--- {name} (PID {pid}) sample ---");
        let output = std::process::Command::new("sample")
            .args([&pid.to_string(), "1"])
            .output();
        match output {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                // Print just the call graph summary (last ~50 lines)
                let lines: Vec<&str> = text.lines().collect();
                let start = lines.len().saturating_sub(50);
                for line in &lines[start..] {
                    eprintln!("  {line}");
                }
            }
            Err(e) => eprintln!("  (sample failed: {e})"),
        }
    }

    // Check for orphaned vixenfs-mount / VixenFS processes
    eprintln!("\n--- FSKit processes ---");
    let output = std::process::Command::new("pgrep")
        .args(["-la", "(vixenfs-mount|VixenFS)"])
        .output();
    if let Ok(out) = output {
        let text = String::from_utf8_lossy(&out.stdout);
        if text.is_empty() {
            eprintln!("  (none)");
        } else {
            for line in text.lines() {
                eprintln!("  {line}");
            }
        }
    }

    // Print the vixenfs-mount log file if it exists
    let log_file = vx_home.join("vixen").join("vixenfs-mount.log");
    if log_file.exists() {
        eprintln!("\n--- vixenfs-mount.log ---");
        if let Ok(content) = std::fs::read_to_string(&log_file) {
            for line in content.lines() {
                eprintln!("  {line}");
            }
        }
    }
}

// ── Cleanup (platform-specific) ──────────────────────────────────

fn cleanup_mount(vx_home: &Path) {
    let runner_mount = vx_home.join("vixen").join("runner");
    if !runner_mount.exists() {
        return;
    }

    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("fusermount3")
            .args(["-uz", runner_mount.to_str().unwrap()])
            .status();
    }

    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("diskutil")
            .args(["unmount", "force", runner_mount.to_str().unwrap()])
            .status();
    }
}

// ── Stress loop ──────────────────────────────────────────────────

async fn stress_loop(
    args: &Args,
    vx_bin: &Path,
    vx_home: &Path,
    project_dirs: &[PathBuf],
) -> Result<()> {
    let timeout = Duration::from_secs(args.timeout_secs);
    let mut round = 0usize;
    let l2_cache_dir = vx_home.join("cache").join("blobs").join("blake3");
    let vx_home_str = vx_home.to_string_lossy().to_string();

    loop {
        if !args.forever && round >= args.rounds {
            break;
        }

        info!(round, "starting round");

        if args.cold_cache_each_round {
            if let Err(e) = tokio::fs::remove_dir_all(&l2_cache_dir).await
                && e.kind() != std::io::ErrorKind::NotFound
            {
                warn!(round, cache_dir = %l2_cache_dir.display(), error = %e, "failed to clear L2 cache directory");
            }
            if let Err(e) = tokio::fs::create_dir_all(&l2_cache_dir).await {
                warn!(round, cache_dir = %l2_cache_dir.display(), error = %e, "failed to recreate L2 cache directory");
            } else {
                info!(round, cache_dir = %l2_cache_dir.display(), "cleared L2 cache directory");
            }
        }

        // Mutate each project's source to prevent cache hits
        for (i, project_dir) in project_dirs.iter().enumerate() {
            write_project_source(project_dir, i, round + 1)?;
        }

        // Spawn concurrent builds
        let mut children = Vec::new();
        for i in 0..args.concurrency {
            let project_idx = i % project_dirs.len();
            let project_dir = &project_dirs[project_idx];

            let child = tokio::process::Command::new(vx_bin)
                .args(["build", "--no-spawn", "--no-tui"])
                .env("VX_HOME", &vx_home_str)
                .env("VX_TEST", "1")
                .current_dir(project_dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .wrap_err_with(|| format!("failed to spawn build {i}"))?;

            children.push((i, child, Instant::now()));
        }

        // Wait for all children with timeout
        let mut hang_detected = false;
        let mut error_count = 0usize;
        for (idx, mut child, spawned) in children {
            let remaining = timeout.saturating_sub(spawned.elapsed());
            match tokio::time::timeout(remaining, child.wait()).await {
                Ok(Ok(status)) => {
                    if !status.success() {
                        error_count += 1;
                        let stderr = child.stderr.take();
                        let stderr_text = if let Some(mut stderr) = stderr {
                            use tokio::io::AsyncReadExt;
                            let mut buf = String::new();
                            let _ = stderr.read_to_string(&mut buf).await;
                            buf
                        } else {
                            String::new()
                        };
                        warn!(
                            idx,
                            round,
                            ?status,
                            stderr = %stderr_text.chars().take(500).collect::<String>(),
                            "build exited with error"
                        );
                    }
                }
                Ok(Err(e)) => {
                    error_count += 1;
                    warn!(idx, %e, "child wait error");
                }
                Err(_) => {
                    error!(
                        idx,
                        round,
                        age_ms = spawned.elapsed().as_millis() as u64,
                        "HANG DETECTED: build exceeded timeout"
                    );
                    hang_detected = true;
                    let _ = child.kill().await;
                }
            }
        }

        if hang_detected {
            let vx_home_path = Path::new(vx_home);
            dump_hang_diagnostics(vx_home_path);
            bail!("VFS hang detected in round {round}");
        }

        if error_count > 0 {
            warn!(round, error_count, "round had build errors");
        }

        info!(round, error_count, "round completed");
        round += 1;
    }

    Ok(())
}

// ── Main ─────────────────────────────────────────────────────────

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = parse_args();

    info!(
        projects = args.projects,
        concurrency = args.concurrency,
        timeout_secs = args.timeout_secs,
        rounds = args.rounds,
        forever = args.forever,
        skip_warmup = args.skip_warmup,
        cold_cache_each_round = args.cold_cache_each_round,
        "starting VFS stress test"
    );

    let vxd_bin = find_sibling_binary("vxd")?;
    let vx_bin = find_sibling_binary("vx")?;
    info!(vxd = %vxd_bin.display(), vx = %vx_bin.display(), "found binaries");

    let tmp_dir = tempfile::tempdir()?;
    let vx_home = tmp_dir.path().join("vx-home");
    std::fs::create_dir_all(&vx_home)?;
    info!(vx_home = %vx_home.display(), "created isolated VX_HOME");

    let projects_dir = tmp_dir.path().join("projects");
    std::fs::create_dir_all(&projects_dir)?;
    let mut project_dirs = Vec::new();
    for i in 0..args.projects {
        let project_dir = projects_dir.join(format!("stress-{i}"));
        create_test_project(&project_dir, i)?;
        project_dirs.push(project_dir);
    }
    info!(count = args.projects, "created test projects");

    info!("starting vxd...");
    let mut vxd_child = std::process::Command::new(&vxd_bin)
        .env("VX_HOME", &vx_home)
        .env("VX_TEST", "1")
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .wrap_err("failed to spawn vxd")?;

    let vxd_sock = vx_home.join("vxd.sock");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let ready = {
            #[cfg(unix)]
            {
                vxd_sock.exists() && std::os::unix::net::UnixStream::connect(&vxd_sock).is_ok()
            }
            #[cfg(not(unix))]
            {
                vxd_sock.exists()
            }
        };
        if ready {
            break;
        }
        if Instant::now() > deadline {
            let _ = vxd_child.kill();
            bail!("vxd did not become ready within 30 seconds");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    info!("vxd is ready");

    if !args.skip_warmup {
        info!("running warmup build...");
        let warmup_status = std::process::Command::new(&vx_bin)
            .args(["build", "--no-spawn", "--no-tui"])
            .env("VX_HOME", &vx_home)
            .env("VX_TEST", "1")
            .current_dir(&project_dirs[0])
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .wrap_err("failed to run warmup build")?;

        if !warmup_status.success() {
            let _ = vxd_child.kill();
            bail!("warmup build failed with status {warmup_status}");
        }
        info!("warmup build completed");
    } else {
        info!("skipping warmup build");
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let result = rt.block_on(stress_loop(&args, &vx_bin, &vx_home, &project_dirs));

    info!("shutting down vxd...");
    let _ = vxd_child.kill();
    let _ = vxd_child.wait();
    cleanup_mount(&vx_home);

    match result {
        Ok(()) => {
            info!("all rounds completed — no hangs detected");
            Ok(())
        }
        Err(e) => {
            error!("stress test failed: {e}");
            Err(e)
        }
    }
}
