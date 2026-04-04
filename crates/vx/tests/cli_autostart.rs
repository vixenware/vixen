use std::process::Command;

use tempfile::TempDir;

fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root should resolve")
}

fn vx_bin() -> String {
    std::env::var("CARGO_BIN_EXE_vx").expect("vx binary should be built for integration tests")
}

fn write_daemon_config(temp_dir: &TempDir) -> std::path::PathBuf {
    let cfg = temp_dir.path().join("daemon.styx");
    let sock = temp_dir.path().join("vx.sock");
    let store = temp_dir.path().join("store");
    let mount = temp_dir.path().join("vfs");
    std::fs::write(
        &cfg,
        format!(
            "daemon {{\n  listen (\"local://{}\")\n  services {{\n    orchestrator {{}}\n    store {{ root \"{}\" }}\n    vfsd {{ mount \"{}\" }}\n  }}\n}}\n",
            sock.display(),
            store.display(),
            mount.display(),
        ),
    )
    .expect("daemon config should write");
    cfg
}

fn write_daemon_config_without_vfsd(temp_dir: &TempDir) -> std::path::PathBuf {
    let cfg = temp_dir.path().join("daemon.styx");
    let sock = temp_dir.path().join("vx.sock");
    let store = temp_dir.path().join("store");
    std::fs::write(
        &cfg,
        format!(
            "daemon {{\n  listen (\"local://{}\")\n  services {{\n    orchestrator {{}}\n    store {{ root \"{}\" }}\n  }}\n}}\n",
            sock.display(),
            store.display(),
        ),
    )
    .expect("daemon config should write");
    cfg
}

fn cleanup_daemon(config_path: &std::path::Path) {
    let _ = Command::new("pkill")
        .args([
            "-f",
            &format!("vx daemon --config {}", config_path.display()),
        ])
        .status();
}

fn start_daemon(config_path: &std::path::Path) -> std::process::Child {
    Command::new(vx_bin())
        .current_dir(workspace_root())
        .args(["daemon", "--config"])
        .arg(config_path)
        .spawn()
        .expect("vx daemon should spawn")
}

fn write_simple_eval_file(temp_dir: &TempDir) -> std::path::PathBuf {
    let file = temp_dir.path().join("simple.vx");
    std::fs::write(&file, "fn one() -> U64 ( 1 )\n").expect("simple eval file should write");
    file
}

#[test]
fn eval_auto_starts_daemon_and_runs_rustc_hello() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let config_path = write_daemon_config(&temp_dir);

    let output = Command::new(vx_bin())
        .current_dir(workspace_root())
        .env("VX_DAEMON_CONFIG", &config_path)
        .args([
            "eval",
            "--file",
            "zoo/rustc-hello.vx",
            "--function",
            "build",
        ])
        .output()
        .expect("vx eval should run");

    cleanup_daemon(&config_path);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.trim().starts_with("tree:"), "stdout: {stdout}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("phase:"), "stderr: {stderr}");
}

#[test]
fn eval_fresh_daemon_replaces_running_daemon_and_runs_rustc_hello() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let config_path = write_daemon_config_without_vfsd(&temp_dir);
    let eval_file = write_simple_eval_file(&temp_dir);
    let mut daemon = start_daemon(&config_path);
    std::thread::sleep(std::time::Duration::from_millis(300));

    let output = Command::new(vx_bin())
        .current_dir(workspace_root())
        .env("VX_DAEMON_CONFIG", &config_path)
        .args(["eval", "--fresh-daemon", "--file"])
        .arg(&eval_file)
        .args(["--function", "one"])
        .output()
        .expect("vx eval should run");

    let _ = daemon.kill();
    let _ = daemon.wait();
    cleanup_daemon(&config_path);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "1", "stdout: {stdout}");
}
