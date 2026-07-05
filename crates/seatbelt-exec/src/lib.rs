//! macOS seatbelt profile generation for hermetic exec.
//!
//! The generator is intentionally parameterized: declared read mounts become
//! read allowances, write roots become write allowances, and toolchain roots
//! become executable/read-only allowances. The profile imports Apple's
//! `system.sb` only for bootstrap plumbing, then claws back the ambient file
//! grants that would otherwise let actions read undeclared host state.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const SYSTEM_SB_PATH: &str = "/System/Library/Sandbox/Profiles/system.sb";
pub const AUDITED_SYSTEM_SB_SHA256: &str =
    "180a0356611b5de7a7db84b800ed47a1d2f15ec884d0fb960445366ededac7c8";
pub const AUDITED_SYSTEM_SB_LINES: usize = 311;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatbeltPlan {
    read_roots: Vec<PathBuf>,
    write_roots: Vec<PathBuf>,
    toolchain_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatbeltProfile {
    source: String,
    params: Vec<SeatbeltParam>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatbeltParam {
    name: String,
    value: PathBuf,
}

impl SeatbeltPlan {
    pub fn new() -> Self {
        Self {
            read_roots: Vec::new(),
            write_roots: Vec::new(),
            toolchain_roots: Vec::new(),
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

    pub fn toolchain_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.toolchain_roots.push(path.into());
        self
    }

    pub fn canonicalize_existing(self) -> Result<Self, std::io::Error> {
        Ok(Self {
            read_roots: canonicalize_roots(self.read_roots)?,
            write_roots: canonicalize_roots(self.write_roots)?,
            toolchain_roots: canonicalize_roots(self.toolchain_roots)?,
        })
    }

    pub fn compile(&self) -> SeatbeltProfile {
        let mut params = Vec::new();
        let read_params = push_params(&mut params, "READ_ROOT", &self.read_roots);
        let write_params = push_params(&mut params, "WRITE_ROOT", &self.write_roots);
        let toolchain_params = push_params(&mut params, "TOOLCHAIN_ROOT", &self.toolchain_roots);

        let mut source = String::new();
        source.push_str("(version 1)\n");
        source.push_str("(debug deny)\n");
        source.push_str("(deny default)\n");
        source.push_str("(import \"system.sb\")\n");
        source.push_str("(deny file-read* file-test-existence\n");
        source.push_str("  (literal \"/tmp\")\n");
        source.push_str("  (literal \"/var\")\n");
        source.push_str("  (subpath \"/Library\")\n");
        source.push_str("  (subpath \"/private/tmp\")\n");
        source.push_str("  (subpath \"/private/var\")\n");
        source.push_str("  (subpath \"/private/etc\")\n");
        source.push_str("  (subpath \"/Users\")\n");
        source.push_str("  (subpath \"/usr/share\"))\n");
        source.push_str("(deny file-write*)\n");
        source.push_str("(deny network*)\n");
        source.push_str("(allow process-fork)\n");

        source.push_str("(allow file-read-metadata file-test-existence\n");
        emit_param_filters(&mut source, "path-ancestors", &read_params);
        emit_param_filters(&mut source, "path-ancestors", &write_params);
        emit_param_filters(&mut source, "path-ancestors", &toolchain_params);
        source.push_str(")\n");

        source.push_str("(allow process-exec\n");
        emit_param_filters(&mut source, "subpath", &toolchain_params);
        source.push_str(")\n");

        source.push_str("(allow file-read* file-test-existence\n");
        emit_param_filters(&mut source, "subpath", &read_params);
        emit_param_filters(&mut source, "subpath", &write_params);
        emit_param_filters(&mut source, "subpath", &toolchain_params);
        source.push_str(")\n");

        source.push_str("(allow file-map-executable\n");
        emit_param_filters(&mut source, "subpath", &toolchain_params);
        source.push_str(")\n");

        source.push_str("(allow file-write*\n");
        emit_param_filters(&mut source, "subpath", &write_params);
        source.push_str(")\n");

        SeatbeltProfile { source, params }
    }
}

impl Default for SeatbeltPlan {
    fn default() -> Self {
        Self::new()
    }
}

impl SeatbeltProfile {
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn params(&self) -> &[SeatbeltParam] {
        &self.params
    }

    pub fn command(&self, program: impl AsRef<Path>, args: &[String]) -> Command {
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command.arg("-p").arg(&self.source);
        for param in &self.params {
            command
                .arg("-D")
                .arg(format!("{}={}", param.name, param.value.display()));
        }
        command.arg(program.as_ref()).args(args);
        command
    }
}

impl SeatbeltParam {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> &Path {
        &self.value
    }
}

pub fn make_command(
    program: impl AsRef<Path>,
    args: &[String],
    plan: SeatbeltPlan,
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

fn push_params(params: &mut Vec<SeatbeltParam>, prefix: &str, paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let name = format!("{prefix}_{index}");
            params.push(SeatbeltParam {
                name: name.clone(),
                value: path.clone(),
            });
            name
        })
        .collect()
}

fn emit_param_filters(source: &mut String, filter: &str, names: &[String]) {
    for name in names {
        let escaped = sbpl_string(name);
        writeln!(source, "  ({filter} (param \"{escaped}\"))")
            .expect("writing to a String cannot fail");
    }
}

fn sbpl_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_profile_is_deny_default_and_parametric() {
        let profile = SeatbeltPlan::new()
            .read_root("/vfs/in")
            .write_root("/vfs/out")
            .toolchain_root("/toolchain")
            .compile();

        let source = profile.source();
        assert!(source.contains("(deny default)"));
        assert!(source.contains("(import \"system.sb\")"));
        assert!(source.contains("(deny network*)"));
        assert!(source.contains("(subpath \"/Library\")"));
        assert!(source.contains("(subpath \"/private/tmp\")"));
        assert!(source.contains("(subpath \"/usr/share\")"));
        assert!(source.contains("(subpath (param \"READ_ROOT_0\"))"));
        assert!(source.contains("(subpath (param \"WRITE_ROOT_0\"))"));
        assert!(source.contains("(subpath (param \"TOOLCHAIN_ROOT_0\"))"));
        assert!(!source.contains("/vfs/in"));
        assert!(!source.contains("/vfs/out"));
        assert!(!source.contains("/toolchain"));
    }
}
