//! Test harness for vx integration tests
//!
//! # Architecture
//!
//! ```text
//!                     Shared daemons (one instance each)
//!                     ┌─────────┐  ┌──────────┐  ┌──────────┐
//!                     │   vxd   │  │ vx-runner │  │ vx-store │
//!                     └────┬────┘  └─────┬─────┘  └─────┬────┘
//!                          │             │              │
//!          ┌───────────────┼─────────────┼──────────────┤
//!          │               │             │              │
//!     ┌────┴────┐    ┌─────┴───┐   ┌─────┴───┐    ┌────┴────┐
//!     │ vx      │    │ vx      │   │ vx      │    │ vx      │
//!     │ pid=100 │    │ pid=101 │   │ pid=102 │    │ pid=103 │
//!     │ test-100│    │ test-101│   │ test-102│    │ test-103│
//!     └─────────┘    └─────────┘   └─────────┘    └─────────┘
//!      test_1         test_2        test_3         test_4
//! ```
//!
//! Each test is a separate process (nextest runs one process per test).
//! All tests share the same vxd, vx-runner, and vx-store daemon processes.
//! Each test gets its own session ID (based on PID), which is mixed into
//! cache keys so tests don't interfere with each other's cache state.
//! Per-connection state (session ID, progress listener) is fresh for each
//! roam connection via `VxdService::new_session()`.

// Each integration test file compiles this module separately, so functions
// used by one test file appear "unused" when compiling another.

mod scaffold;
pub use scaffold::*;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

/// Initialize tracing for tests
///
/// This replaces test-log's functionality without pulling in syn-dependent macros.
/// Call this at the start of tests that need tracing output.
pub fn init_test_tracing() {
    use tracing_subscriber::EnvFilter;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();
}

/// A test environment with isolated CAS, project directories, and Unix sockets
pub struct TestEnv {
    /// Session ID for cache key namespacing (isolates parallel tests)
    session_id: String,

    /// Temp dir for the test project
    pub project: TempDir,
}

impl TestEnv {
    /// Create a new isolated test environment
    pub fn new() -> Self {
        let session_id = format!("test-pid{}-{}", std::process::id(), ulid::Ulid::new());
        let project = TempDir::new().expect("failed to create project temp dir");

        Self {
            project,
            session_id,
        }
    }

    /// Path to the binary directory (target/debug or target/release)
    fn bindir() -> PathBuf {
        // TODO: What about release builds?
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug");
        path.canonicalize()
            .expect("bindir does not exist - did you run `cargo build`?")
    }

    /// Path to the vx binary (built by cargo)
    fn vx_binary() -> PathBuf {
        let path = Self::bindir().join(format!("vx{}", std::env::consts::EXE_SUFFIX));
        assert!(path.exists(), "vx binary does not exist at {:?}", path);
        path
    }

    /// Run `vx build` in this environment
    pub fn build(&self) -> BuildBuilder {
        BuildBuilder::new(self)
    }

    /// Run `vx clean` in this environment
    pub fn clean(&self) -> VxOutput {
        let mut cmd = Command::new(Self::vx_binary());
        cmd.current_dir(self.project.path());
        cmd.env("VX_TEST_SESSION", &self.session_id);
        cmd.arg("clean");

        let output = cmd.output().expect("failed to run vx");
        VxOutput::from(output)
    }

    /// Run `vx clean --cache` in this environment
    pub fn clean_cache(&self) -> VxOutput {
        let mut cmd = Command::new(Self::vx_binary());
        cmd.current_dir(self.project.path());
        cmd.env("VX_TEST_SESSION", &self.session_id);
        cmd.args(["clean", "--cache"]);

        let output = cmd.output().expect("failed to run vx");
        VxOutput::from(output)
    }

    /// Write a file to the project directory
    pub fn write_file(&self, relative_path: &str, contents: &str) {
        let path = self.project.path().join(relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("failed to create parent dirs");
        }
        std::fs::write(&path, contents).expect("failed to write file");
    }

    /// Check if a file exists in the project directory
    pub fn file_exists(&self, relative_path: &str) -> bool {
        self.project.path().join(relative_path).exists()
    }

    /// Get the project path
    pub fn project_path(&self) -> &Path {
        self.project.path()
    }
}

/// Output from running vx
#[derive(Debug)]
pub struct VxOutput {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl VxOutput {
    /// Check if the build was a cache hit
    pub fn was_cached(&self) -> bool {
        // Check for "Cached (no rebuild needed)" in build output
        self.stdout.contains("Cached (no rebuild needed)")
    }

    /// Check if output contains a specific string
    pub fn contains(&self, s: &str) -> bool {
        self.stdout.contains(s) || self.stderr.contains(s)
    }

    /// Assert the command succeeded, panicking with full output on failure
    #[track_caller]
    pub fn assert_success(&self) {
        assert!(
            self.success,
            "command failed (exit code {:?}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.exit_code, self.stdout, self.stderr
        );
    }

    /// Assert the build was a cache hit, panicking with full output on failure
    #[track_caller]
    pub fn assert_cached(&self) {
        self.assert_success();
        assert!(
            self.was_cached(),
            "expected cache hit but was not cached:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout,
            self.stderr
        );
    }

    /// Assert the command failed
    #[track_caller]
    pub fn assert_failure(&self) {
        assert!(
            !self.success,
            "expected command to fail but it succeeded (exit code {:?}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.exit_code, self.stdout, self.stderr
        );
    }

    /// Assert the command failed and output contains the given string
    #[track_caller]
    pub fn assert_failure_contains(&self, needle: &str) {
        self.assert_failure();
        assert!(
            self.contains(needle),
            "expected output to contain {needle:?}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout,
            self.stderr
        );
    }

    /// Assert the build was NOT a cache hit, panicking with full output on failure
    #[track_caller]
    pub fn assert_not_cached(&self) {
        self.assert_success();
        assert!(
            !self.was_cached(),
            "expected cache miss but was cached:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout,
            self.stderr
        );
    }
}

impl From<Output> for VxOutput {
    fn from(output: Output) -> Self {
        Self {
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

/// Builder for `vx build` commands
pub struct BuildBuilder {
    project_dir: PathBuf,
    session_id: String,
    release: bool,
    json: bool,
    subdir: Option<String>,
    extra_env: Vec<(String, String)>,
}

impl BuildBuilder {
    fn new(env: &TestEnv) -> Self {
        Self {
            project_dir: env.project.path().to_path_buf(),
            session_id: env.session_id.clone(),
            release: false,
            json: false,
            subdir: None,
            extra_env: Vec::new(),
        }
    }

    pub fn release(mut self) -> Self {
        self.release = true;
        self
    }

    pub fn json(mut self) -> Self {
        self.json = true;
        self
    }

    pub fn in_subdir(mut self, subdir: &str) -> Self {
        self.subdir = Some(subdir.to_owned());
        self
    }

    pub fn run(self) -> VxOutput {
        let mut cmd = Command::new(TestEnv::vx_binary());
        let work_dir = match &self.subdir {
            Some(sub) => self.project_dir.join(sub),
            None => self.project_dir.clone(),
        };
        cmd.current_dir(&work_dir);

        cmd.env("VX_TEST_SESSION", &self.session_id);
        for (key, value) in &self.extra_env {
            cmd.env(key, value);
        }
        cmd.arg("build");
        // Let vx spawn daemons if needed (they'll be shared due to VX_HOME)
        if self.release {
            cmd.arg("--release");
        }
        if self.json {
            cmd.arg("--json");
        }

        let output = cmd.output().expect("failed to run vx");
        VxOutput::from(output)
    }
}
