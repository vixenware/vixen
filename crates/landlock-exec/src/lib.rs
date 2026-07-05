//! Linux Landlock profile generation for hermetic exec.
//!
//! The generator mirrors `seatbelt-exec`: declared read mounts become
//! read-only file rules, write roots become read/write file rules, and exec
//! roots become read/execute file rules. Everything else expressible by the
//! targeted Landlock ABI is denied by default.

use std::path::{Path, PathBuf};
use std::process::Command;

pub const TARGET_LANDLOCK_ABI: &str = "V4";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandlockPlan {
    read_roots: Vec<PathBuf>,
    write_roots: Vec<PathBuf>,
    exec_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandlockProfile {
    read_roots: Vec<PathBuf>,
    write_roots: Vec<PathBuf>,
    exec_roots: Vec<PathBuf>,
}

impl LandlockPlan {
    pub fn new() -> Self {
        Self {
            read_roots: Vec::new(),
            write_roots: Vec::new(),
            exec_roots: Vec::new(),
        }
    }

    pub fn read_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.read_roots.push(path.into());
        self
    }

    pub fn write_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.write_roots.push(path.into());
        self
    }

    pub fn exec_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.exec_roots.push(path.into());
        self
    }

    pub fn toolchain_root(self, path: impl Into<PathBuf>) -> Self {
        self.exec_root(path)
    }

    pub fn canonicalize_existing(self) -> Result<Self, std::io::Error> {
        Ok(Self {
            read_roots: canonicalize_roots(self.read_roots)?,
            write_roots: canonicalize_roots(self.write_roots)?,
            exec_roots: canonicalize_roots(self.exec_roots)?,
        })
    }

    pub fn compile(&self) -> LandlockProfile {
        LandlockProfile {
            read_roots: self.read_roots.clone(),
            write_roots: self.write_roots.clone(),
            exec_roots: self.exec_roots.clone(),
        }
    }
}

impl Default for LandlockPlan {
    fn default() -> Self {
        Self::new()
    }
}

impl LandlockProfile {
    pub fn target_abi(&self) -> &'static str {
        TARGET_LANDLOCK_ABI
    }

    pub fn read_roots(&self) -> &[PathBuf] {
        &self.read_roots
    }

    pub fn write_roots(&self) -> &[PathBuf] {
        &self.write_roots
    }

    pub fn exec_roots(&self) -> &[PathBuf] {
        &self.exec_roots
    }

    pub fn command(&self, program: impl AsRef<Path>, args: &[String]) -> Command {
        self.command_with_wrapper("landlock-exec", program, args)
    }

    pub fn command_with_wrapper(
        &self,
        wrapper: impl AsRef<Path>,
        program: impl AsRef<Path>,
        args: &[String],
    ) -> Command {
        let mut command = Command::new(wrapper.as_ref());
        for root in &self.read_roots {
            command.arg("--read-root").arg(root);
        }
        for root in &self.write_roots {
            command.arg("--write-root").arg(root);
        }
        for root in &self.exec_roots {
            command.arg("--exec-root").arg(root);
        }
        command.arg("--").arg(program.as_ref()).args(args);
        command
    }
}

pub fn make_command(
    program: impl AsRef<Path>,
    args: &[String],
    plan: LandlockPlan,
) -> Result<Command, std::io::Error> {
    Ok(plan
        .canonicalize_existing()?
        .compile()
        .command(program, args))
}

fn canonicalize_roots(paths: Vec<PathBuf>) -> Result<Vec<PathBuf>, std::io::Error> {
    paths
        .into_iter()
        .map(std::fs::canonicalize)
        .collect::<Result<Vec<_>, _>>()
}

#[cfg(target_os = "linux")]
mod sandbox;

#[cfg(target_os = "linux")]
pub use sandbox::{apply_to_current_thread, exec_under_sandbox};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_profile_is_deny_default_and_parametric() {
        let profile = LandlockPlan::new()
            .read_root("/vfs/in")
            .write_root("/vfs/out")
            .exec_root("/toolchain")
            .compile();

        assert_eq!(profile.target_abi(), "V4");
        assert_eq!(profile.read_roots(), &[PathBuf::from("/vfs/in")]);
        assert_eq!(profile.write_roots(), &[PathBuf::from("/vfs/out")]);
        assert_eq!(profile.exec_roots(), &[PathBuf::from("/toolchain")]);

        let command = profile.command_with_wrapper(
            "/usr/bin/landlock-exec",
            "/toolchain/bin/rustc",
            &["--version".to_string()],
        );
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.iter().any(|arg| arg == "--read-root"));
        assert!(args.iter().any(|arg| arg == "/vfs/in"));
        assert!(args.iter().any(|arg| arg == "--write-root"));
        assert!(args.iter().any(|arg| arg == "/vfs/out"));
        assert!(args.iter().any(|arg| arg == "--exec-root"));
        assert!(args.iter().any(|arg| arg == "/toolchain"));
    }
}
