#![cfg(target_os = "macos")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use seatbelt_exec::{
    AUDITED_SYSTEM_SB_LINES, AUDITED_SYSTEM_SB_SHA256S, SYSTEM_SB_PATH, SeatbeltPlan,
};

#[test]
fn audited_system_sb_hash_matches_this_host() {
    let line_count = fs::read_to_string(SYSTEM_SB_PATH)
        .expect("system.sb must be readable")
        .lines()
        .count();
    assert_eq!(
        line_count, AUDITED_SYSTEM_SB_LINES,
        "Apple changed system.sb line count; re-audit the imported profile"
    );

    let output = Command::new("/usr/bin/shasum")
        .arg("-a")
        .arg("256")
        .arg(SYSTEM_SB_PATH)
        .output()
        .expect("shasum must run");
    assert!(
        output.status.success(),
        "shasum failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let actual = stdout
        .split_whitespace()
        .next()
        .expect("shasum prints a digest");
    assert!(
        AUDITED_SYSTEM_SB_SHA256S.contains(&actual),
        "Apple changed system.sb to {actual}; re-audit every imported allow before trusting this profile"
    );
}

#[test]
fn clang_compile_from_declared_mount_succeeds() {
    let fixture = Fixture::new("clang");
    let source = fixture.read_root.join("main.c");
    fs::write(&source, "int main(void) { return 0; }\n").unwrap();

    let clang = xcrun_find("clang");
    let toolchain = xcode_toolchain_root(&clang);
    let profile = SeatbeltPlan::new()
        .read_root(&fixture.read_root)
        .write_root(&fixture.write_root)
        .toolchain_root(&toolchain)
        .canonicalize_existing()
        .unwrap()
        .compile();

    let object = fixture.write_root.join("main.o");
    let output = run_profile_in(
        &profile,
        &fixture.read_root,
        &clang,
        &[
            "-c".to_string(),
            source.display().to_string(),
            "-o".to_string(),
            object.display().to_string(),
        ],
    );

    assert_success(&output);
    assert!(object.exists(), "clang wrote the declared output object");
}

#[test]
fn rustc_compile_from_declared_mount_succeeds() {
    let fixture = Fixture::new("rustc");
    let source = fixture.read_root.join("main.rs");
    fs::write(&source, "fn main() {}\n").unwrap();

    let rustc = rustc_direct_path();
    let sysroot = rustc_sysroot(&rustc);
    let profile = SeatbeltPlan::new()
        .read_root(&fixture.read_root)
        .write_root(&fixture.write_root)
        .toolchain_root(&sysroot)
        .canonicalize_existing()
        .unwrap()
        .compile();

    let metadata = fixture.write_root.join("main.rmeta");
    let output = run_profile_in(
        &profile,
        &fixture.read_root,
        &rustc,
        &[
            "--emit=metadata".to_string(),
            source.display().to_string(),
            "-o".to_string(),
            metadata.display().to_string(),
        ],
    );

    assert_success(&output);
    assert!(
        metadata.exists(),
        "rustc wrote the declared metadata output"
    );
}

#[test]
fn undeclared_reads_network_and_writes_are_denied() {
    let fixture = Fixture::new("denials");
    let home_probe = home_dir().join("seatbelt-exec-deny-probe");
    fs::write(&home_probe, "home").unwrap();
    fs::write("/tmp/seatbelt-exec-deny-probe", "tmp").unwrap();

    let profile = SeatbeltPlan::new()
        .write_root(&fixture.write_root)
        .toolchain_root("/bin")
        .toolchain_root("/usr/bin")
        .canonicalize_existing()
        .unwrap()
        .compile();

    assert_denied(run_shell(
        &profile,
        &fixture.write_root,
        "cat /tmp/seatbelt-exec-deny-probe",
    ));
    assert_denied(run_shell(
        &profile,
        &fixture.write_root,
        "cat /var/db/timezone/zoneinfo/+VERSION",
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

    let network = run_profile_in(
        &profile,
        &fixture.write_root,
        Path::new("/usr/bin/nc"),
        &["-vz".to_string(), "127.0.0.1".to_string(), "9".to_string()],
    );
    assert!(
        !network.status.success(),
        "network access must fail under the generated profile"
    );
    assert!(
        String::from_utf8_lossy(&network.stderr).contains("Operation not permitted"),
        "network denial should be the sandbox, not connection state: {}",
        String::from_utf8_lossy(&network.stderr)
    );

    let _ = fs::remove_file(home_probe);
    let _ = fs::remove_file("/tmp/seatbelt-exec-deny-probe");
}

struct Fixture {
    root: PathBuf,
    read_root: PathBuf,
    write_root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("seatbelt-exec-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let read_root = root.join("read");
        let write_root = root.join("write");
        fs::create_dir_all(&read_root).unwrap();
        fs::create_dir_all(&write_root).unwrap();
        Self {
            root: root.canonicalize().unwrap(),
            read_root: read_root.canonicalize().unwrap(),
            write_root: write_root.canonicalize().unwrap(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn run_shell(profile: &seatbelt_exec::SeatbeltProfile, current_dir: &Path, script: &str) -> Output {
    run_profile_in(
        profile,
        current_dir,
        Path::new("/bin/sh"),
        &["-c".to_string(), script.into()],
    )
}

fn run_profile_in(
    profile: &seatbelt_exec::SeatbeltProfile,
    current_dir: &Path,
    program: &Path,
    args: &[String],
) -> Output {
    let mut command = profile.command(program, args);
    command
        .current_dir(current_dir)
        .output()
        .expect("sandbox-exec should run")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_denied(output: Output) {
    assert!(
        !output.status.success(),
        "operation should have been denied\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn xcrun_find(tool: &str) -> PathBuf {
    let output = Command::new("/usr/bin/xcrun")
        .arg("--find")
        .arg(tool)
        .output()
        .expect("xcrun should run");
    assert_success(&output);
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}

fn xcode_toolchain_root(clang: &Path) -> PathBuf {
    let marker = Path::new("Contents/Developer/Toolchains/XcodeDefault.xctoolchain");
    let command_line_tools = Path::new("Library/Developer/CommandLineTools");
    let mut root = PathBuf::new();
    for component in clang.components() {
        root.push(component.as_os_str());
        if root.ends_with(marker) || root.ends_with(command_line_tools) {
            return root;
        }
    }
    panic!("could not find Xcode toolchain root in {}", clang.display());
}

fn rustc_direct_path() -> PathBuf {
    let output = Command::new("rustc")
        .arg("--print")
        .arg("sysroot")
        .output()
        .expect("rustc should run");
    assert_success(&output);
    let sysroot = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    sysroot.join("bin/rustc")
}

fn rustc_sysroot(rustc: &Path) -> PathBuf {
    rustc
        .parent()
        .and_then(Path::parent)
        .expect("rustc lives under <sysroot>/bin")
        .to_path_buf()
}

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME is set"))
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}
