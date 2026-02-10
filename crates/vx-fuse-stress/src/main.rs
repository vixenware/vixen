//! FUSE stress test orchestrator.
//!
//! Launches real vxd → vx-store → vx-runner processes, then drives concurrent
//! `vx build` commands to exercise the full FUSE → FuseBridge → CasVfs code path.
//! Detects hangs and dumps FUSE telemetry + kernel state from the runner process.
//!
//! This tool is Unix-only (requires /proc, FUSE, Unix signals).

#[cfg(not(unix))]
fn main() {
    eprintln!("vx-fuse-stress is only supported on Unix platforms");
    std::process::exit(1);
}

#[cfg(unix)]
mod unix_main {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use eyre::{bail, Result, WrapErr};
    use tracing::{error, info, warn};

    // ── CLI args ──────────────────────────────────────────────────────

    struct Args {
        projects: usize,
        concurrency: usize,
        timeout_secs: u64,
        rounds: usize,
    }

    impl Default for Args {
        fn default() -> Self {
            Args {
                projects: 8,
                concurrency: 8,
                timeout_secs: 10,
                rounds: 20,
            }
        }
    }

    fn parse_args() -> Args {
        let mut args = Args::default();
        let mut iter = std::env::args().skip(1);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--projects" => {
                    args.projects =
                        iter.next().expect("--projects value").parse().expect("number")
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
                other => {
                    eprintln!("Unknown argument: {other}");
                    eprintln!("Usage: vx-fuse-stress [OPTIONS]");
                    eprintln!("  --projects <N>      Number of test projects (default: 8)");
                    eprintln!("  --concurrency <N>   Concurrent builds per round (default: 8)");
                    eprintln!("  --timeout-secs <N>  Hang detection timeout (default: 30)");
                    eprintln!("  --rounds <N>        Number of stress rounds (default: 20)");
                    std::process::exit(1);
                }
            }
        }
        args
    }

    // ── Binary discovery ─────────────────────────────────────────────

    /// Find workspace binaries (vxd, vx) next to the current executable.
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

        // Initial source — will be mutated each round
        write_project_source(dir, index, 0)?;
        Ok(())
    }

    fn write_project_source(dir: &Path, index: usize, round: usize) -> Result<()> {
        let src = format!("fn main() {{ let _project = {index}; let _round = {round}; }}\n");
        std::fs::write(dir.join("src/main.rs"), src)?;
        Ok(())
    }

    // ── Runner PID discovery ─────────────────────────────────────────

    /// Find the vx-runner PID by scanning /proc for a process whose environ
    /// contains our VX_HOME and whose comm is "vx-runner".
    fn find_runner_pid(vx_home: &Path) -> Option<u32> {
        let proc = Path::new("/proc");
        let entries = std::fs::read_dir(proc).ok()?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(pid_str) = name.to_str() else {
                continue;
            };
            let Ok(pid) = pid_str.parse::<u32>() else {
                continue;
            };

            // Check comm first (cheap)
            let comm_path = proc.join(pid_str).join("comm");
            let Ok(comm) = std::fs::read_to_string(&comm_path) else {
                continue;
            };
            if comm.trim() != "vx-runner" {
                continue;
            }

            // Check /proc/<pid>/environ for our VX_HOME
            let environ_path = proc.join(pid_str).join("environ");
            if let Ok(environ) = std::fs::read(&environ_path) {
                let needle = format!("VX_HOME={}", vx_home.display());
                let needle_bytes = needle.as_bytes();
                // environ is NUL-separated
                if environ
                    .split(|&b| b == 0)
                    .any(|var| var == needle_bytes)
                {
                    return Some(pid);
                }
            }
        }
        None
    }

    // ── Diagnostics ──────────────────────────────────────────────────

    fn dump_hang_diagnostics(vx_home: &Path) {
        eprintln!("\n=== HANG DIAGNOSTICS ===\n");

        // Find and signal vx-runner
        if let Some(runner_pid) = find_runner_pid(vx_home) {
            eprintln!("Found vx-runner at PID {runner_pid}");

            // Send SIGUSR1 to trigger telemetry dump from the runner itself
            eprintln!("Sending SIGUSR1 to vx-runner...");
            unsafe {
                libc::kill(runner_pid as i32, libc::SIGUSR1);
            }
            // Give the runner a moment to dump
            std::thread::sleep(Duration::from_millis(500));

            // Dump thread info for vx-runner
            dump_process_threads("vx-runner", runner_pid);
        } else {
            eprintln!("WARNING: Could not find vx-runner PID");
        }

        // Find and dump all related processes (vxd, vx-store, vx)
        dump_related_processes(vx_home);

        // Kernel FUSE state
        eprintln!("\n--- Kernel FUSE state ---");
        if let Ok(entries) = std::fs::read_dir("/sys/fs/fuse/connections") {
            for entry in entries.flatten() {
                let waiting_path = entry.path().join("waiting");
                if let Ok(val) = std::fs::read_to_string(&waiting_path) {
                    eprintln!(
                        "  {}: waiting={}",
                        entry.path().display(),
                        val.trim()
                    );
                }
            }
        }

        eprintln!("\n=== END DIAGNOSTICS ===\n");
    }

    /// Dump thread info for a process: state, wchan, and kernel stack (if readable).
    fn dump_process_threads(label: &str, pid: u32) {
        eprintln!("\n--- {label} (PID {pid}) threads ---");
        let task_dir = format!("/proc/{pid}/task");
        let Ok(tasks) = std::fs::read_dir(&task_dir) else {
            eprintln!("  (cannot read {task_dir})");
            return;
        };
        for task in tasks.flatten() {
            let tid = task.file_name();
            let tid_str = tid.to_string_lossy();
            let comm = std::fs::read_to_string(task.path().join("comm")).unwrap_or_default();
            let comm = comm.trim();

            // Parse status file for State and voluntary/nonvoluntary context switches
            let status =
                std::fs::read_to_string(task.path().join("status")).unwrap_or_default();
            let state = status
                .lines()
                .find(|l| l.starts_with("State:"))
                .unwrap_or("State: ?");

            // wchan shows what kernel function the thread is sleeping in
            let wchan =
                std::fs::read_to_string(task.path().join("wchan")).unwrap_or_default();
            let wchan = wchan.trim();
            let wchan_display = if wchan.is_empty() || wchan == "0" {
                "running"
            } else {
                wchan
            };

            // syscall file shows current syscall number + args
            let syscall =
                std::fs::read_to_string(task.path().join("syscall")).unwrap_or_default();
            let syscall = syscall.trim();

            eprintln!(
                "  TID {tid_str} ({comm}): {state} | wchan={wchan_display} | syscall={syscall}"
            );

            // Try kernel stack (may be empty without CAP_SYS_PTRACE)
            let stack =
                std::fs::read_to_string(task.path().join("stack")).unwrap_or_default();
            if !stack.trim().is_empty() {
                for line in stack.lines() {
                    eprintln!("    {line}");
                }
            }
        }
    }

    /// Find all processes belonging to our VX_HOME and dump their threads.
    fn dump_related_processes(vx_home: &Path) {
        let needle = format!("VX_HOME={}", vx_home.display());
        let needle_bytes = needle.as_bytes();

        let Ok(entries) = std::fs::read_dir("/proc") else {
            return;
        };

        let mut found: Vec<(String, u32)> = Vec::new();

        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(pid_str) = name.to_str() else {
                continue;
            };
            let Ok(pid) = pid_str.parse::<u32>() else {
                continue;
            };

            let comm_path = Path::new("/proc").join(pid_str).join("comm");
            let Ok(comm) = std::fs::read_to_string(&comm_path) else {
                continue;
            };
            let comm = comm.trim().to_string();

            // Only look at vixen-related processes
            if !matches!(comm.as_str(), "vxd" | "vx-store" | "vx" | "vx-runner") {
                continue;
            }

            // Check environ for our VX_HOME
            let environ_path = Path::new("/proc").join(pid_str).join("environ");
            if let Ok(environ) = std::fs::read(&environ_path)
                && environ
                    .split(|&b| b == 0)
                    .any(|var| var == needle_bytes)
            {
                found.push((comm, pid));
            }
        }

        found.sort_by_key(|(name, _)| match name.as_str() {
            "vxd" => 0,
            "vx-store" => 1,
            "vx-runner" => 2,
            _ => 3,
        });

        for (name, pid) in &found {
            // Skip vx-runner, already dumped above
            if name == "vx-runner" {
                continue;
            }
            dump_process_threads(name, *pid);
        }
    }

    // ── Stress loop ──────────────────────────────────────────────────

    async fn stress_loop(
        args: &Args,
        vx_bin: &Path,
        vx_home: &str,
        project_dirs: &[PathBuf],
    ) -> Result<()> {
        let timeout = Duration::from_secs(args.timeout_secs);

        for round in 0..args.rounds {
            info!(round, "starting round");

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
                    .env("VX_HOME", vx_home)
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
            for (idx, mut child, spawned) in children {
                let remaining = timeout.saturating_sub(spawned.elapsed());
                match tokio::time::timeout(remaining, child.wait()).await {
                    Ok(Ok(status)) => {
                        if !status.success() {
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
                                ?status,
                                stderr = %stderr_text.chars().take(500).collect::<String>(),
                                "build exited with error"
                            );
                        }
                    }
                    Ok(Err(e)) => {
                        warn!(idx, %e, "child wait error");
                    }
                    Err(_) => {
                        error!(
                            idx,
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
                bail!("FUSE hang detected in round {round}");
            }

            info!(round, "round completed");
        }

        Ok(())
    }

    // ── Main ─────────────────────────────────────────────────────────

    pub fn main() -> Result<()> {
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
            "starting FUSE stress test (real runner mode)"
        );

        // Discover binaries
        let vxd_bin = find_sibling_binary("vxd")?;
        let vx_bin = find_sibling_binary("vx")?;
        info!(vxd = %vxd_bin.display(), vx = %vx_bin.display(), "found binaries");

        // Create isolated VX_HOME
        let tmp_dir = tempfile::tempdir()?;
        let vx_home = tmp_dir.path().join("vx-home");
        std::fs::create_dir_all(&vx_home)?;
        let vx_home_str = vx_home.to_str().unwrap().to_string();
        info!(vx_home = %vx_home_str, "created isolated VX_HOME");

        // Create test projects
        let projects_dir = tmp_dir.path().join("projects");
        std::fs::create_dir_all(&projects_dir)?;
        let mut project_dirs = Vec::new();
        for i in 0..args.projects {
            let project_dir = projects_dir.join(format!("stress-{i}"));
            create_test_project(&project_dir, i)?;
            project_dirs.push(project_dir);
        }
        info!(count = args.projects, "created test projects");

        // Start vxd
        info!("starting vxd...");
        let mut vxd_child = std::process::Command::new(&vxd_bin)
            .env("VX_HOME", &vx_home_str)
            .env("VX_TEST", "1")
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .wrap_err("failed to spawn vxd")?;

        // Wait for daemon readiness (poll for vxd socket)
        let vxd_sock = vx_home.join("vxd.sock");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if vxd_sock.exists() {
                // Try connecting to verify it's listening
                if std::os::unix::net::UnixStream::connect(&vxd_sock).is_ok() {
                    break;
                }
            }
            if Instant::now() > deadline {
                let _ = vxd_child.kill();
                bail!("vxd did not become ready within 10 seconds");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        info!("vxd is ready");

        // Warmup build (downloads toolchain on first run)
        info!("running warmup build...");
        let warmup_status = std::process::Command::new(&vx_bin)
            .args(["build", "--no-spawn", "--no-tui"])
            .env("VX_HOME", &vx_home_str)
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

        // Run stress loop
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;

        let result = rt.block_on(stress_loop(
            &args,
            &vx_bin,
            &vx_home_str,
            &project_dirs,
        ));

        // Cleanup
        info!("shutting down vxd...");
        let _ = vxd_child.kill();
        let _ = vxd_child.wait();

        // Lazy unmount the runner mount path
        let runner_mount = vx_home.join("vixen").join("runner");
        if runner_mount.exists() {
            let _ = std::process::Command::new("fusermount3")
                .args(["-uz", runner_mount.to_str().unwrap()])
                .status();
        }

        match result {
            Ok(()) => {
                info!("all rounds completed successfully — no hangs detected");
                Ok(())
            }
            Err(e) => {
                error!("stress test failed: {e}");
                Err(e)
            }
        }
    }
}

#[cfg(unix)]
fn main() -> eyre::Result<()> {
    unix_main::main()
}
