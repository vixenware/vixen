#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use landlock_exec::{LandlockPlan, LandlockProfile};

#[test]
fn compile_from_declared_mount_succeeds() {
    let fixture = Fixture::new("cc");
    let source = fixture.read_root.join("main.c");
    fs::write(&source, "int main(void) { return 0; }\n").unwrap();

    let cc = find_program("cc");
    let profile = base_plan(&fixture)
        .read_root(&fixture.read_root)
        .exec_root(cc.parent().expect("cc has a parent"))
        .compile();

    let object = fixture.write_root.join("main.o");
    let output = run_profile_in(
        &profile,
        &fixture.read_root,
        &cc,
        &[
            "-pipe".to_string(),
            "-c".to_string(),
            source.display().to_string(),
            "-o".to_string(),
            object.display().to_string(),
        ],
    );

    assert_success(&output);
    assert!(object.exists(), "cc wrote the declared output object");
}

#[test]
fn undeclared_reads_network_and_writes_are_denied() {
    let fixture = Fixture::new("denials");
    let bash = find_program("bash");
    let network_probe = fixture.exec_root.join("network-probe");
    compile_network_probe(&network_probe);
    let tmp_probe =
        std::env::temp_dir().join(format!("landlock-exec-deny-probe-{}", std::process::id()));
    let home_probe = home_dir().join(format!("landlock-exec-deny-probe-{}", std::process::id()));
    fs::write(&tmp_probe, "tmp").unwrap();
    fs::write(&home_probe, "home").unwrap();

    let profile = base_plan(&fixture)
        .exec_root(bash.parent().expect("bash has a parent"))
        .exec_root(&fixture.exec_root)
        .compile();

    assert_denied(run_shell(
        &profile,
        &fixture.write_root,
        &format!("cat {}", shell_quote(&tmp_probe)),
    ));
    assert_denied(run_shell(
        &profile,
        &fixture.write_root,
        &format!("cat {}", shell_quote(&home_probe)),
    ));

    let outside = fixture.root.join("outside");
    assert_denied(run_shell(
        &profile,
        &fixture.write_root,
        &format!("printf x > {}", shell_quote(&outside)),
    ));
    assert!(
        !outside.exists(),
        "write outside WRITE_ROOT must not land on disk"
    );

    let network = run_profile_in(&profile, &fixture.write_root, &network_probe, &[]);
    assert!(
        !network.status.success(),
        "network access must fail under the generated profile"
    );
    let stderr = String::from_utf8_lossy(&network.stderr);
    assert!(
        stderr.contains("Operation not permitted") || stderr.contains("Permission denied"),
        "network denial should be sandbox-level, not connection state: {stderr}"
    );

    let _ = fs::remove_file(tmp_probe);
    let _ = fs::remove_file(home_probe);
}

struct Fixture {
    root: PathBuf,
    read_root: PathBuf,
    write_root: PathBuf,
    exec_root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("landlock-exec-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let read_root = root.join("read");
        let write_root = root.join("write");
        let exec_root = root.join("exec");
        fs::create_dir_all(&read_root).unwrap();
        fs::create_dir_all(&write_root).unwrap();
        fs::create_dir_all(&exec_root).unwrap();
        Self {
            root: root.canonicalize().unwrap(),
            read_root: read_root.canonicalize().unwrap(),
            write_root: write_root.canonicalize().unwrap(),
            exec_root: exec_root.canonicalize().unwrap(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn base_plan(fixture: &Fixture) -> LandlockPlan {
    let mut plan = LandlockPlan::new().write_root(&fixture.write_root);
    for root in existing_system_read_roots() {
        plan = plan.read_root(root);
    }
    for root in existing_system_exec_roots() {
        plan = plan.exec_root(root);
    }
    plan
}

fn run_shell(profile: &LandlockProfile, current_dir: &Path, script: &str) -> Output {
    run_profile_in(
        profile,
        current_dir,
        &find_program("bash"),
        &["-c".to_string(), script.into()],
    )
}

fn run_profile_in(
    profile: &LandlockProfile,
    current_dir: &Path,
    program: &Path,
    args: &[String],
) -> Output {
    let mut command =
        profile.command_with_wrapper(env!("CARGO_BIN_EXE_landlock-exec"), program, args);
    command
        .current_dir(current_dir)
        .output()
        .expect("landlock-exec should run")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstatus: {}\nsignal: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        output.status.signal(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_denied(output: Output) {
    assert!(
        !output.status.success(),
        "operation should have been denied\nstatus: {}\nsignal: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        output.status.signal(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn find_program(name: &str) -> PathBuf {
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {name}"))
        .output()
        .expect("sh should run");
    assert_success(&output);
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
        .canonicalize()
        .unwrap()
}

fn compile_network_probe(output: &Path) {
    let source = output.with_extension("c");
    fs::write(
        &source,
        r#"#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        perror("socket");
        return 1;
    }

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons(9);
    if (inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr) != 1) {
        fputs("inet_pton failed\n", stderr);
        close(fd);
        return 1;
    }

    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        perror("connect");
        close(fd);
        return 1;
    }

    close(fd);
    return 0;
}
"#,
    )
    .unwrap();

    let output_status = Command::new(find_program("cc"))
        .arg(&source)
        .arg("-o")
        .arg(output)
        .output()
        .expect("cc should run");
    assert_success(&output_status);
}

fn existing_system_read_roots() -> Vec<PathBuf> {
    existing_roots(["/etc", "/usr/include"])
}

fn existing_system_exec_roots() -> Vec<PathBuf> {
    existing_roots([
        "/bin",
        "/usr/bin",
        "/lib",
        "/lib64",
        "/usr/lib",
        "/usr/libexec",
        "/usr/lib64",
    ])
}

fn existing_roots<const N: usize>(roots: [&str; N]) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut existing = Vec::new();
    for root in roots {
        let path = Path::new(root);
        if path.exists() {
            let canonical = path.canonicalize().unwrap();
            if seen.insert(canonical.clone()) {
                existing.push(canonical);
            }
        }
    }
    existing
}

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME is set"))
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}
