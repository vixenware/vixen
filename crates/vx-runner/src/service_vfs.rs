//! VFS-based action execution
//!
//! This module implements compilation using the VFS instead of disk materialization.
//! Files are served lazily from CAS via the Vixen mount.

use camino::Utf8PathBuf;
use jiff::Zoned;
use moire::task::FutureExt;
use std::time::Instant;
use tracing::{debug, info};
use vx_eval::PathValue;
use vx_runner_proto::{
    CcCompileRequest, CcCompileResult, CcLinkRequest, CcLinkResult, RunArg, RunFileMount,
    RunOutput, RunRequest, RunResult, RunTreeMount, RustCompileRequest, RustCompileResult,
    TransportValue,
};
use vx_store_proto::{
    Blake3Hash, CasNode, ChunkHash, NodeHash, NodeId, NodeManifest, OutputBlobRef, OutputEntry,
    StoreProgress,
};

use crate::RunnerService;
use crate::eval_boundary::evaluate_shipped_observer;
use crate::sandbox::{ActionKind, CommandError};

fn is_relative_vfs_path(path: &str) -> bool {
    !path.is_empty() && !path.starts_with('/') && !path.split('/').any(|segment| segment == "..")
}

fn is_tree_mount_path(path: &str) -> bool {
    path.is_empty() || is_relative_vfs_path(path)
}

fn covers_path(prefix: &str, path: &str) -> bool {
    prefix.is_empty()
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn path_is_covered(
    path: &str,
    tree_mounts: &[RunTreeMount],
    file_mounts: &[RunFileMount],
    writable_roots: &[String],
) -> bool {
    if path.starts_with("${VX_TOOLCHAIN}/") {
        return true;
    }

    tree_mounts
        .iter()
        .any(|mount| covers_path(&mount.mount, path))
        || file_mounts.iter().any(|mount| mount.path == path)
        || writable_roots.iter().any(|root| covers_path(root, path))
}

fn outputs_stay_within_writable_roots(outputs: &[RunOutput], writable_roots: &[String]) -> bool {
    outputs.iter().all(|out| {
        writable_roots
            .iter()
            .any(|root| covers_path(root, &out.path))
    })
}

fn uses_toolchain_placeholder(value: &RunArg) -> bool {
    value.as_str().contains("${VX_TOOLCHAIN}")
}

fn run_failure_result(
    start: Instant,
    exit_code: i32,
    stdout: String,
    stderr: String,
    error: Option<String>,
) -> RunResult {
    RunResult {
        success: false,
        exit_code,
        stdout,
        stderr,
        duration_ms: start.elapsed().as_millis() as u64,
        output_manifest: None,
        observer_value: None,
        error,
    }
}

fn run_success_result(
    start: Instant,
    exit_code: i32,
    stdout: String,
    stderr: String,
    output_manifest: Option<Blake3Hash>,
    observer_value: Option<TransportValue>,
) -> RunResult {
    RunResult {
        success: true,
        exit_code,
        stdout,
        stderr,
        duration_ms: start.elapsed().as_millis() as u64,
        output_manifest,
        observer_value,
        error: None,
    }
}

/// Action execution context with all paths and sandbox config.
pub struct ActionContext {
    pub prefix_id: String,
    pub mount_base: Utf8PathBuf,
    pub prefix_path: Utf8PathBuf,
    pub out_path: Utf8PathBuf,
}

impl ActionContext {
    /// VFS subdirectory name for source trees.
    /// Distinct from "src" to avoid `sources/src/` when crate_root is "src/main.rs".
    pub const SOURCES_DIR: &str = "sources";
    pub const DEPS_DIR: &str = "deps";

    pub fn sources_path(&self) -> Utf8PathBuf {
        self.prefix_path.join(Self::SOURCES_DIR)
    }

    pub fn deps_path(&self) -> Utf8PathBuf {
        self.prefix_path.join(Self::DEPS_DIR)
    }

    /// Clear the environment and re-add only what's needed for hermetic builds.
    ///
    /// Sets PATH (linker discovery), HOME, and temp dirs. On Windows the temp
    /// vars are critical — without them `GetTempPathW` falls back to `C:\WINDOWS`.
    pub fn hermetic_env(&self, cmd: &mut moire::process::Command) {
        cmd.env_clear();
        // Passthrough vars needed by compilers/linkers on the host.
        // On Windows, LIB/INCLUDE are required for MSVC link.exe to find
        // kernel32.lib and friends.
        const PASSTHROUGH: &[&str] = &[
            "PATH",
            "HOME",
            // MSVC toolchain
            "LIB",
            "INCLUDE",
            "LIBPATH",
            // Windows SDK
            "WindowsSdkDir",
            "WindowsSDKVersion",
        ];
        for var in PASSTHROUGH {
            if let Ok(val) = std::env::var(var) {
                cmd.env(var, val);
            }
        }
        cmd.env("TMPDIR", self.out_path.as_str());
        cmd.env("TMP", self.out_path.as_str());
        cmd.env("TEMP", self.out_path.as_str());
    }
}

impl RunnerService {
    // r[impl exec.arg]
    fn expand_run_arg(arg: &RunArg, toolchain_path: Option<&str>) -> String {
        let expanded = match toolchain_path {
            Some(toolchain_path) => arg.as_str().replace("${VX_TOOLCHAIN}", toolchain_path),
            None => arg.as_str().to_string(),
        };
        match arg {
            RunArg::Literal(_) => expanded,
            RunArg::Path(_) => RunArg::path(expanded).render_for_host(),
        }
    }

    /// Create action execution context with prefix, paths, and sandbox config.
    async fn create_action_context(&self) -> ActionContext {
        let prefix_id = self.create_action_prefix().await;
        let mount_base = crate::runner_mount_base();
        let prefix_path = mount_base.join(&prefix_id);
        let out_path = prefix_path.join("out");

        ActionContext {
            prefix_id,
            mount_base,
            prefix_path,
            out_path,
        }
    }

    /// Compile a Rust crate using VFS-based execution
    pub async fn compile_rust_vfs(
        &self,
        request: RustCompileRequest,
        progress: roam::Tx<StoreProgress>,
    ) -> RustCompileResult {
        let start = Instant::now();

        info!(
            crate_name = %request.crate_name,
            crate_type = %request.crate_type,
            deps_count = request.deps.len(),
            "compiling rust crate (VFS mode)"
        );

        // 1. Create action context
        let action = self
            .create_action_context()
            .named("runner.run_vfs.create_action_context")
            .await;
        debug!(action.prefix_id = %action.prefix_id, action.prefix_path = %action.prefix_path, "created action prefix");

        // Helper to return an error result (cleanup is done before calling this)
        let cleanup_and_error =
            |_prefix_id: &str, error: String, start: Instant| RustCompileResult {
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: start.elapsed().as_millis() as u64,
                output_manifest: None,
                error: Some(error),
            };

        // 2. Ensure toolchain prefix exists (stable, persists across compilations)
        debug!(
            toolchain_manifest = %request.toolchain_manifest,
            "ensuring toolchain VFS prefix"
        );
        let before = Instant::now();
        let toolchain_prefix_id = match self
            .ensure_toolchain_prefix(request.toolchain_manifest)
            .named("runner.run_vfs.ensure_toolchain_prefix")
            .await
        {
            Ok(id) => id,
            Err(e) => {
                self.remove_action_prefix(&action.prefix_id).await;
                return cleanup_and_error(
                    &action.prefix_id,
                    format!("failed to ensure toolchain: {}", e),
                    start,
                );
            }
        };
        let toolchain_path = action.mount_base.join(&toolchain_prefix_id);
        info!(
            toolchain_prefix = %toolchain_prefix_id,
            "Rust toolchain ready in {:.2}s",
            before.elapsed().as_secs_f32()
        );

        // 3. Add source tree to prefix
        if let Err(e) = self
            .add_source_tree_to_prefix(&action, request.source_manifest)
            .await
        {
            self.remove_action_prefix(&action.prefix_id).await;
            return cleanup_and_error(
                &action.prefix_id,
                format!("failed to add sources: {}", e),
                start,
            );
        }

        // 4. Add dependencies to prefix
        for dep in &request.deps {
            if let Err(e) = self
                .add_dep_to_prefix(&action, &dep.extern_name, dep.manifest_hash)
                .await
            {
                self.remove_action_prefix(&action.prefix_id).await;
                return cleanup_and_error(
                    &action.prefix_id,
                    format!("failed to add dep {}: {}", dep.extern_name, e),
                    start,
                );
            }
        }

        // 5. Create output directory
        self.create_output_dir_in_prefix(&action.prefix_id).await;

        // 6. Build rustc command
        let rustc_path = toolchain_path.join(format!("bin/rustc{}", std::env::consts::EXE_SUFFIX));
        let sysroot_path = toolchain_path.join("sysroot");
        let src_path = action.sources_path();
        let deps_path = action.deps_path();

        let output_filename = if request.crate_type == "lib" {
            format!("lib{}.rlib", request.crate_name)
        } else {
            request.crate_name.clone()
        };
        let output_file = action.out_path.join(&output_filename);

        let crate_root = src_path.join(&request.crate_root);

        let mut cmd = crate::sandbox::sandboxed_command(rustc_path.as_str(), &action);
        cmd.arg("--sysroot").arg(sysroot_path.as_str());
        cmd.arg("--crate-name").arg(&request.crate_name);
        cmd.arg("--crate-type").arg(&request.crate_type);
        cmd.arg("--edition").arg(&request.edition);
        cmd.arg("-o").arg(output_file.as_str());

        // Path remapping for reproducibility
        cmd.arg(format!("--remap-path-prefix={}=/src", src_path));

        // Add --extern flags for dependencies
        for dep in &request.deps {
            let rlib_path = deps_path.join(format!("lib{}.rlib", dep.extern_name));
            cmd.arg("--extern");
            cmd.arg(format!("{}={}", dep.extern_name, rlib_path));
        }

        // Profile-specific flags
        if request.profile == "release" {
            cmd.arg("-C").arg("opt-level=3");
        }

        // On Linux, use lld instead of GNU ld (which doesn't support Rust's
        // __rustc:: namespaced allocator symbols introduced in Rust 1.91+).
        if cfg!(target_os = "linux") {
            cmd.arg("-C").arg("link-arg=-fuse-ld=lld");
        }

        // Crate root
        cmd.arg(crate_root.as_str());

        // Working directory
        cmd.current_dir(action.prefix_path.as_str());

        action.hermetic_env(&mut cmd);

        debug!(
            rustc = %rustc_path,
            crate_name = %request.crate_name,
            crate_root = %crate_root,
            "executing rustc via VFS"
        );

        // Log full command details at trace level for debugging sandbox issues
        {
            let cmd_std = cmd.as_std();
            let program = cmd_std.get_program().to_string_lossy();
            let args: Vec<_> = cmd_std.get_args().map(|a| a.to_string_lossy()).collect();
            let envs: Vec<_> = cmd_std
                .get_envs()
                .map(|(k, v)| {
                    format!(
                        "{}={}",
                        k.to_string_lossy(),
                        v.map(|v| v.to_string_lossy()).unwrap_or_default()
                    )
                })
                .collect();
            let cwd = cmd_std
                .get_current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            tracing::trace!(
                %program,
                ?args,
                ?envs,
                cwd,
                "rustc: full command before spawn"
            );
        }

        // 7. Run rustc with streaming output and timeout
        let action_desc = format!("rustc {}", request.crate_name);
        let output = match crate::sandbox::run_sandboxed_command(
            cmd,
            &ActionKind::RustCompile,
            &action_desc,
            progress,
            None,
        )
        .await
        {
            Ok(o) => o,
            Err(CommandError::SpawnFailed(e)) => {
                tracing::error!(
                    error = %e,
                    error_kind = ?e.kind(),
                    rustc_path = %rustc_path,
                    "failed to spawn rustc process"
                );
                self.remove_action_prefix(&action.prefix_id).await;
                return cleanup_and_error(
                    &action.prefix_id,
                    format!("failed to execute rustc: {}", e),
                    start,
                );
            }
            Err(CommandError::TimedOut {
                action_desc,
                timeout,
                partial_output,
                ..
            }) => {
                self.remove_action_prefix(&action.prefix_id).await;
                return RustCompileResult {
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: partial_output,
                    duration_ms: start.elapsed().as_millis() as u64,
                    output_manifest: None,
                    error: Some(format!(
                        "action '{}' timed out after {}s (limit: {}s)",
                        action_desc,
                        timeout.as_secs(),
                        timeout.as_secs()
                    )),
                };
            }
            Err(CommandError::Cancelled {
                action_desc,
                partial_output,
            }) => {
                self.remove_action_prefix(&action.prefix_id).await;
                return RustCompileResult {
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: partial_output,
                    duration_ms: start.elapsed().as_millis() as u64,
                    output_manifest: None,
                    error: Some(format!("action '{}' cancelled", action_desc)),
                };
            }
        };

        let duration_ms = start.elapsed().as_millis() as u64;
        let exit_code = output.exit_status.code().unwrap_or(-1);

        if !output.exit_status.success() {
            let mut stderr = if output.stderr.trim().is_empty() {
                output.combined_output.clone()
            } else {
                output.stderr.clone()
            };

            // When sandbox-exec wraps the compiler, it can exit non-zero with no
            // output if the inner program is missing. Add diagnostics so the
            // error message is never empty.
            if stderr.trim().is_empty() {
                let rustc_exists = rustc_path.as_std_path().exists();
                let crate_root_exists = crate_root.as_std_path().exists();
                stderr = format!(
                    "rustc exited with code {} but produced no output.\n\
                     rustc_path={} (exists={})\n\
                     crate_root={} (exists={})",
                    exit_code, rustc_path, rustc_exists, crate_root, crate_root_exists,
                );
                tracing::error!(
                    %rustc_path,
                    rustc_exists,
                    %crate_root,
                    crate_root_exists,
                    exit_code,
                    "compilation failed with empty output"
                );
            }

            self.remove_action_prefix(&action.prefix_id).await;
            return RustCompileResult {
                success: false,
                exit_code,
                stdout: output.stdout.clone(),
                stderr,
                duration_ms,
                output_manifest: None,
                error: None,
            };
        }

        // 8. Flush outputs from VFS to CAS
        let flushed = self.flush_prefix_outputs(&action.prefix_id).await;

        // Find the output blob hash and size
        let output_rel_path = format!("out/{}", output_filename);
        let (blob_hash, blob_size) = match flushed.get(&output_rel_path) {
            Some(&(hash, size)) => (hash, size),
            None => {
                self.remove_action_prefix(&action.prefix_id).await;
                return cleanup_and_error(
                    &action.prefix_id,
                    format!(
                        "output {} not found in VFS after compilation",
                        output_rel_path
                    ),
                    start,
                );
            }
        };

        // 9. Create and store output manifest
        let logical = if request.crate_type == "lib" {
            "rlib"
        } else {
            "bin"
        };

        let node_id = NodeId(format!(
            "compile-{}:{}:{}:{}",
            request.crate_type, request.crate_name, request.target_triple, request.profile
        ));

        let cache_key = vx_cache::rust_compile_cache_key(&request);

        let manifest = NodeManifest {
            node_id,
            cache_key,
            produced_at: Zoned::now().to_string(),
            outputs: vec![OutputEntry {
                logical: logical.to_string(),
                filename: output_filename,
                blob: OutputBlobRef::Small {
                    chunk: ChunkHash(blob_hash),
                    size: blob_size,
                },
                executable: request.crate_type == "bin",
            }],
        };

        let cas_node = CasNode::Manifest(manifest);
        let (node_bytes, node_hash) = cas_node.to_bytes_and_hash();
        let manifest_hash = node_hash.0;

        if let Err(e) = self.cas.put_node(node_hash, node_bytes).await {
            self.remove_action_prefix(&action.prefix_id).await;
            return cleanup_and_error(
                &action.prefix_id,
                format!("failed to store manifest: {:?}", e),
                start,
            );
        }

        // 10. Cleanup
        self.remove_action_prefix(&action.prefix_id).await;

        info!(
            crate_name = %request.crate_name,
            manifest_hash = %manifest_hash,
            duration_ms,
            "compilation complete (VFS mode)"
        );

        RustCompileResult {
            success: true,
            exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
            duration_ms,
            output_manifest: Some(manifest_hash),
            error: None,
        }
    }

    /// Compile a C source file using VFS-based execution
    pub async fn compile_cc_vfs(
        &self,
        request: CcCompileRequest,
        progress: roam::Tx<StoreProgress>,
    ) -> CcCompileResult {
        let start = Instant::now();

        info!(
            source = %request.source_path,
            target = %request.target_triple,
            "compiling C source (VFS mode)"
        );

        // 1. Create action context
        let action = self
            .create_action_context()
            .named("runner.run_vfs.create_action_context")
            .await;

        // 2. Ensure Zig toolchain prefix exists
        let toolchain_prefix_id = match self
            .ensure_toolchain_prefix(request.toolchain_manifest)
            .await
        {
            Ok(id) => id,
            Err(e) => {
                self.remove_action_prefix(&action.prefix_id).await;
                return CcCompileResult {
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration_ms: start.elapsed().as_millis() as u64,
                    output_manifest: None,
                    discovered_deps: vec![],
                    error: Some(format!("failed to ensure toolchain: {}", e)),
                };
            }
        };
        let toolchain_path = action.mount_base.join(&toolchain_prefix_id);

        // 3. Add source tree to prefix
        if let Err(e) = self
            .add_source_tree_to_prefix(&action, request.source_manifest)
            .await
        {
            self.remove_action_prefix(&action.prefix_id).await;
            return CcCompileResult {
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: start.elapsed().as_millis() as u64,
                output_manifest: None,
                discovered_deps: vec![],
                error: Some(format!("failed to add sources: {}", e)),
            };
        }

        // 4. Create output directory
        self.create_output_dir_in_prefix(&action.prefix_id).await;

        // 5. Build zig cc command
        let zig_path = toolchain_path.join("zig");
        let src_path = action.sources_path();

        let source_file = src_path.join(&request.source_path);
        let object_filename = format!(
            "{}.o",
            request
                .source_path
                .strip_suffix(".c")
                .unwrap_or(&request.source_path)
        );
        let object_file = action.out_path.join(&object_filename);

        let mut cmd = crate::sandbox::sandboxed_command(zig_path.as_str(), &action);
        cmd.arg("cc");
        cmd.arg("-c");
        cmd.arg(source_file.as_str());
        cmd.arg("-o").arg(object_file.as_str());

        // Profile-specific flags
        if request.profile == "release" {
            cmd.arg("-O3");
        } else {
            cmd.arg("-O0");
            cmd.arg("-g");
        }

        // Add include paths (relative to src/)
        for include_path in &request.include_paths {
            cmd.arg("-I").arg(src_path.join(include_path).as_str());
        }

        // Add defines
        for (key, value) in &request.defines {
            if let Some(v) = value {
                cmd.arg(format!("-D{}={}", key, v));
            } else {
                cmd.arg(format!("-D{}", key));
            }
        }

        cmd.current_dir(action.prefix_path.as_str());

        action.hermetic_env(&mut cmd);
        cmd.env("ZIG_LIB_DIR", toolchain_path.join("lib").as_str());

        debug!(
            zig = %zig_path,
            source = %request.source_path,
            output = %object_file,
            "executing zig cc via VFS"
        );

        // 6. Run zig cc with streaming output and timeout
        let action_desc = format!("zig cc {}", request.source_path);
        let output = match crate::sandbox::run_sandboxed_command(
            cmd,
            &ActionKind::CcCompile,
            &action_desc,
            progress,
            None,
        )
        .await
        {
            Ok(o) => o,
            Err(CommandError::SpawnFailed(e)) => {
                self.remove_action_prefix(&action.prefix_id).await;
                return CcCompileResult {
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration_ms: start.elapsed().as_millis() as u64,
                    output_manifest: None,
                    discovered_deps: vec![],
                    error: Some(format!("failed to execute zig cc: {}", e)),
                };
            }
            Err(CommandError::TimedOut {
                action_desc,
                timeout,
                partial_output,
                ..
            }) => {
                self.remove_action_prefix(&action.prefix_id).await;
                return CcCompileResult {
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: partial_output,
                    duration_ms: start.elapsed().as_millis() as u64,
                    output_manifest: None,
                    discovered_deps: vec![],
                    error: Some(format!(
                        "action '{}' timed out after {}s (limit: {}s)",
                        action_desc,
                        timeout.as_secs(),
                        timeout.as_secs()
                    )),
                };
            }
            Err(CommandError::Cancelled {
                action_desc,
                partial_output,
            }) => {
                self.remove_action_prefix(&action.prefix_id).await;
                return CcCompileResult {
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: partial_output,
                    duration_ms: start.elapsed().as_millis() as u64,
                    output_manifest: None,
                    discovered_deps: vec![],
                    error: Some(format!("action '{}' cancelled", action_desc)),
                };
            }
        };

        let duration_ms = start.elapsed().as_millis() as u64;
        let exit_code = output.exit_status.code().unwrap_or(-1);

        if !output.exit_status.success() {
            self.remove_action_prefix(&action.prefix_id).await;
            return CcCompileResult {
                success: false,
                exit_code,
                stdout: output.stdout.clone(),
                stderr: output.stderr.clone(),
                duration_ms,
                output_manifest: None,
                discovered_deps: vec![],
                error: None,
            };
        }

        // 7. Flush outputs from VFS to CAS
        let flushed = self.flush_prefix_outputs(&action.prefix_id).await;

        let output_rel_path = format!("out/{}", object_filename);
        let (blob_hash, blob_size) = match flushed.get(&output_rel_path) {
            Some(&(hash, size)) => (hash, size),
            None => {
                self.remove_action_prefix(&action.prefix_id).await;
                return CcCompileResult {
                    success: false,
                    exit_code: -1,
                    stdout: output.stdout.clone(),
                    stderr: output.stderr.clone(),
                    duration_ms,
                    output_manifest: None,
                    discovered_deps: vec![],
                    error: Some(format!("output {} not found in VFS", output_rel_path)),
                };
            }
        };

        // 8. Create manifest
        let node_id = NodeId(format!(
            "compile-c:{}:{}:{}",
            request.source_path, request.target_triple, request.profile
        ));

        let cache_key = vx_cache::cc_compile_cache_key(&request);

        let manifest = NodeManifest {
            node_id,
            cache_key,
            produced_at: Zoned::now().to_string(),
            outputs: vec![OutputEntry {
                logical: "object".to_string(),
                filename: object_filename,
                blob: OutputBlobRef::Small {
                    chunk: ChunkHash(blob_hash),
                    size: blob_size,
                },
                executable: false,
            }],
        };

        let cas_node = CasNode::Manifest(manifest);
        let (node_bytes, node_hash) = cas_node.to_bytes_and_hash();
        let manifest_hash = node_hash.0;

        if let Err(e) = self.cas.put_node(node_hash, node_bytes).await {
            self.remove_action_prefix(&action.prefix_id).await;
            return CcCompileResult {
                success: false,
                exit_code: -1,
                stdout: output.stdout.clone(),
                stderr: output.stderr.clone(),
                duration_ms,
                output_manifest: None,
                discovered_deps: vec![],
                error: Some(format!("failed to store manifest: {:?}", e)),
            };
        }

        // 9. Cleanup
        self.remove_action_prefix(&action.prefix_id).await;

        info!(
            source = %request.source_path,
            manifest_hash = %manifest_hash,
            duration_ms,
            "C compilation complete (VFS mode)"
        );

        CcCompileResult {
            success: true,
            exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
            duration_ms,
            output_manifest: Some(manifest_hash),
            discovered_deps: vec![], // TODO: parse depfile if generated
            error: None,
        }
    }

    /// Link C object files into a binary using the VFS mount.
    pub async fn link_cc_vfs(
        &self,
        request: CcLinkRequest,
        progress: roam::Tx<StoreProgress>,
    ) -> CcLinkResult {
        let start = Instant::now();

        let action = self
            .create_action_context()
            .named("runner.run_vfs.create_action_context")
            .await;

        let _toolchain_prefix_id = match self
            .ensure_toolchain_prefix(request.toolchain_manifest)
            .await
        {
            Ok(id) => id,
            Err(e) => {
                self.remove_action_prefix(&action.prefix_id).await;
                return CcLinkResult {
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration_ms: start.elapsed().as_millis() as u64,
                    output_manifest: None,
                    error: Some(format!("failed to ensure toolchain: {}", e)),
                };
            }
        };

        self.vfs.add_dir_to_prefix(&action.prefix_id, "obj").await;
        self.create_output_dir_in_prefix(&action.prefix_id).await;

        // Build directory node with all object files
        use vx_store_proto::{ContentHash, DirEntry, DirEntryKind, DirectoryNode};
        let mut dir_entries = Vec::with_capacity(request.object_manifests.len());
        let mut dir_total_size: u64 = 0;
        let mut object_paths = Vec::with_capacity(request.object_manifests.len());

        for (idx, manifest_hash) in request.object_manifests.iter().enumerate() {
            let manifest = match self.cas.get_node(NodeHash(*manifest_hash)).await {
                Ok(Some(data)) => match CasNode::from_bytes(&data) {
                    Ok(CasNode::Manifest(m)) => m,
                    Ok(_) => {
                        self.remove_action_prefix(&action.prefix_id).await;
                        return CcLinkResult {
                            success: false,
                            exit_code: -1,
                            stdout: String::new(),
                            stderr: String::new(),
                            duration_ms: start.elapsed().as_millis() as u64,
                            output_manifest: None,
                            error: Some(format!(
                                "object manifest {} is not a Manifest node",
                                manifest_hash
                            )),
                        };
                    }
                    Err(e) => {
                        self.remove_action_prefix(&action.prefix_id).await;
                        return CcLinkResult {
                            success: false,
                            exit_code: -1,
                            stdout: String::new(),
                            stderr: String::new(),
                            duration_ms: start.elapsed().as_millis() as u64,
                            output_manifest: None,
                            error: Some(format!(
                                "failed to deserialize object manifest {}: {}",
                                manifest_hash, e
                            )),
                        };
                    }
                },
                Ok(None) => {
                    self.remove_action_prefix(&action.prefix_id).await;
                    return CcLinkResult {
                        success: false,
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: String::new(),
                        duration_ms: start.elapsed().as_millis() as u64,
                        output_manifest: None,
                        error: Some(format!(
                            "object manifest {} not found in CAS",
                            manifest_hash
                        )),
                    };
                }
                Err(e) => {
                    self.remove_action_prefix(&action.prefix_id).await;
                    return CcLinkResult {
                        success: false,
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: String::new(),
                        duration_ms: start.elapsed().as_millis() as u64,
                        output_manifest: None,
                        error: Some(format!("failed to fetch object manifest: {:?}", e)),
                    };
                }
            };

            let object_entry = match manifest.outputs.iter().find(|o| o.logical == "object") {
                Some(entry) => entry,
                None => {
                    self.remove_action_prefix(&action.prefix_id).await;
                    return CcLinkResult {
                        success: false,
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: String::new(),
                        duration_ms: start.elapsed().as_millis() as u64,
                        output_manifest: None,
                        error: Some(format!("no object output in manifest {}", manifest_hash)),
                    };
                }
            };

            let obj_filename = format!("{idx:04}.o");
            let obj_rel_path = format!("obj/{}", obj_filename);
            let (blob_hash, blob_size) = match &object_entry.blob {
                OutputBlobRef::Small { chunk, size } => (chunk.0, *size),
                OutputBlobRef::Large {
                    content_hash, size, ..
                } => (content_hash.0, *size),
            };
            dir_entries.push(DirEntry {
                name: obj_filename,
                kind: DirEntryKind::SmallFile {
                    content_hash: ContentHash(blob_hash),
                    size: blob_size,
                    executable: false,
                },
            });
            dir_total_size += blob_size;
            object_paths.push(action.prefix_path.join(obj_rel_path));
        }

        // Sort entries by name for deterministic hashing
        dir_entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));

        // Wrap in an "obj" subdirectory node
        let obj_dir_node = CasNode::Directory(DirectoryNode {
            entries: dir_entries,
            total_size: dir_total_size,
        });
        let (obj_node_bytes, obj_node_hash) = obj_dir_node.to_bytes_and_hash();

        if let Err(e) = self.cas.put_node(obj_node_hash, obj_node_bytes).await {
            self.remove_action_prefix(&action.prefix_id).await;
            return CcLinkResult {
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: start.elapsed().as_millis() as u64,
                output_manifest: None,
                error: Some(format!(
                    "failed to store object files directory node to CAS: {:?}",
                    e
                )),
            };
        }

        // Build root directory node containing "obj" subdirectory
        let root_dir_node = CasNode::Directory(DirectoryNode {
            entries: vec![DirEntry {
                name: "obj".to_string(),
                kind: DirEntryKind::Dir {
                    hash: obj_node_hash,
                    total_size: dir_total_size,
                },
            }],
            total_size: dir_total_size,
        });
        let (root_node_bytes, root_node_hash) = root_dir_node.to_bytes_and_hash();

        if let Err(e) = self.cas.put_node(root_node_hash, root_node_bytes).await {
            self.remove_action_prefix(&action.prefix_id).await;
            return CcLinkResult {
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: start.elapsed().as_millis() as u64,
                output_manifest: None,
                error: Some(format!(
                    "failed to store root directory node to CAS: {:?}",
                    e
                )),
            };
        }

        if let Err(e) = self
            .vfs
            .mount_tree(&action.prefix_id, "", root_node_hash.0)
            .await
        {
            self.remove_action_prefix(&action.prefix_id).await;
            return CcLinkResult {
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: start.elapsed().as_millis() as u64,
                output_manifest: None,
                error: Some(format!(
                    "failed to mount object files tree manifest in VFS: {}",
                    e
                )),
            };
        }

        let output_rel_path = format!("out/{}", request.binary_name);
        let output_path = action.prefix_path.join(&output_rel_path);

        let mut cmd = crate::sandbox::sandboxed_command("cc", &action);
        for object_path in &object_paths {
            cmd.arg(object_path.as_str());
        }
        cmd.arg("-o").arg(output_path.as_str());
        for lib in &request.libs {
            cmd.arg(format!("-l{}", lib));
        }
        cmd.current_dir(action.prefix_path.as_str());
        action.hermetic_env(&mut cmd);

        let action_desc = format!("link {}", request.binary_name);
        let output = match crate::sandbox::run_sandboxed_command(
            cmd,
            &ActionKind::CcLink,
            &action_desc,
            progress,
            None,
        )
        .await
        {
            Ok(o) => o,
            Err(CommandError::SpawnFailed(e)) => {
                self.remove_action_prefix(&action.prefix_id).await;
                return CcLinkResult {
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration_ms: start.elapsed().as_millis() as u64,
                    output_manifest: None,
                    error: Some(format!("failed to execute linker: {}", e)),
                };
            }
            Err(CommandError::TimedOut {
                action_desc,
                timeout,
                partial_output,
                ..
            }) => {
                self.remove_action_prefix(&action.prefix_id).await;
                return CcLinkResult {
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: partial_output,
                    duration_ms: start.elapsed().as_millis() as u64,
                    output_manifest: None,
                    error: Some(format!(
                        "action '{}' timed out after {}s (limit: {}s)",
                        action_desc,
                        timeout.as_secs(),
                        timeout.as_secs()
                    )),
                };
            }
            Err(CommandError::Cancelled {
                action_desc,
                partial_output,
            }) => {
                self.remove_action_prefix(&action.prefix_id).await;
                return CcLinkResult {
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: partial_output,
                    duration_ms: start.elapsed().as_millis() as u64,
                    output_manifest: None,
                    error: Some(format!("action '{}' was cancelled", action_desc)),
                };
            }
        };

        let duration_ms = start.elapsed().as_millis() as u64;
        let exit_code = output.exit_status.code().unwrap_or(-1);

        if !output.exit_status.success() {
            self.remove_action_prefix(&action.prefix_id).await;
            return CcLinkResult {
                success: false,
                exit_code,
                stdout: output.stdout.clone(),
                stderr: output.stderr.clone(),
                duration_ms,
                output_manifest: None,
                error: None,
            };
        }

        let flushed = self.flush_prefix_outputs(&action.prefix_id).await;
        let (blob_hash, blob_size) = match flushed.get(&output_rel_path) {
            Some(&(hash, size)) => (hash, size),
            None => {
                self.remove_action_prefix(&action.prefix_id).await;
                return CcLinkResult {
                    success: false,
                    exit_code: -1,
                    stdout: output.stdout.clone(),
                    stderr: output.stderr.clone(),
                    duration_ms,
                    output_manifest: None,
                    error: Some(format!("output {} not found in VFS", output_rel_path)),
                };
            }
        };

        let node_id = NodeId(format!(
            "link-c:{}:{}",
            request.binary_name, request.target_triple
        ));
        let cache_key = vx_cache::cc_link_cache_key(&request);
        let manifest = NodeManifest {
            node_id,
            cache_key,
            produced_at: Zoned::now().to_string(),
            outputs: vec![OutputEntry {
                logical: "binary".to_string(),
                filename: request.binary_name,
                blob: OutputBlobRef::Small {
                    chunk: ChunkHash(blob_hash),
                    size: blob_size,
                },
                executable: true,
            }],
        };

        let cas_node = CasNode::Manifest(manifest);
        let (node_bytes, node_hash) = cas_node.to_bytes_and_hash();
        let manifest_hash = node_hash.0;

        if let Err(e) = self.cas.put_node(node_hash, node_bytes).await {
            self.remove_action_prefix(&action.prefix_id).await;
            return CcLinkResult {
                success: false,
                exit_code: -1,
                stdout: output.stdout.clone(),
                stderr: output.stderr.clone(),
                duration_ms,
                output_manifest: None,
                error: Some(format!("failed to store manifest: {:?}", e)),
            };
        }

        self.remove_action_prefix(&action.prefix_id).await;

        CcLinkResult {
            success: true,
            exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
            duration_ms,
            output_manifest: Some(manifest_hash),
            error: None,
        }
    }

    /// Run a sandboxed toolchain command with declared CAS/VFS inputs and outputs.
    pub async fn run_vfs(
        &self,
        request: RunRequest,
        progress: roam::Tx<StoreProgress>,
    ) -> RunResult {
        let start = Instant::now();

        let action = self
            .create_action_context()
            .named("runner.run_vfs.create_action_context")
            .await;

        let needs_toolchain = request.program.is_none()
            || request
                .program
                .as_ref()
                .is_some_and(uses_toolchain_placeholder)
            || request.args.iter().any(uses_toolchain_placeholder)
            || request
                .env
                .iter()
                .any(|(_, value)| uses_toolchain_placeholder(value));

        let toolchain_path = if needs_toolchain {
            let toolchain_prefix_id = match self
                .ensure_toolchain_prefix(request.toolchain_manifest)
                .await
            {
                Ok(id) => id,
                Err(e) => {
                    self.remove_action_prefix(&action.prefix_id).await;
                    return run_failure_result(
                        start,
                        -1,
                        String::new(),
                        String::new(),
                        Some(format!("failed to ensure toolchain: {e}")),
                    );
                }
            };
            Some(action.mount_base.join(&toolchain_prefix_id))
        } else {
            None
        };
        let toolchain_path_str = toolchain_path
            .as_ref()
            .map(|path| path.as_str().to_string());

        for mount in &request.tree_mounts {
            if !is_tree_mount_path(&mount.mount) {
                self.remove_action_prefix(&action.prefix_id).await;
                return run_failure_result(
                    start,
                    -1,
                    String::new(),
                    String::new(),
                    Some(format!(
                        "tree mount '{}' must be a relative VFS path",
                        mount.mount
                    )),
                );
            }
        }

        for mount in &request.file_mounts {
            if !is_relative_vfs_path(&mount.path) {
                self.remove_action_prefix(&action.prefix_id).await;
                return run_failure_result(
                    start,
                    -1,
                    String::new(),
                    String::new(),
                    Some(format!(
                        "file mount '{}' must be a relative VFS path",
                        mount.path
                    )),
                );
            }
        }

        for root in &request.writable_roots {
            if !is_relative_vfs_path(root) {
                self.remove_action_prefix(&action.prefix_id).await;
                return run_failure_result(
                    start,
                    -1,
                    String::new(),
                    String::new(),
                    Some(format!(
                        "writable root '{root}' must be a relative VFS path"
                    )),
                );
            }
        }

        // r[impl exec.mount]
        // Mount CAS trees into the prefix.
        for mount in &request.tree_mounts {
            match self.cas.get_node(NodeHash(mount.manifest)).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    self.remove_action_prefix(&action.prefix_id).await;
                    return run_failure_result(
                        start,
                        -1,
                        String::new(),
                        String::new(),
                        Some(format!(
                            "tree manifest {} not found in CAS (mount '{}')",
                            mount.manifest, mount.mount
                        )),
                    );
                }
                Err(e) => {
                    self.remove_action_prefix(&action.prefix_id).await;
                    return run_failure_result(
                        start,
                        -1,
                        String::new(),
                        String::new(),
                        Some(format!("failed to fetch tree manifest: {e:?}")),
                    );
                }
            }

            if let Err(e) = self
                .vfs
                .mount_tree(&action.prefix_id, &mount.mount, mount.manifest)
                .named("runner.run_vfs.mount_tree")
                .await
            {
                self.remove_action_prefix(&action.prefix_id).await;
                return run_failure_result(
                    start,
                    -1,
                    String::new(),
                    String::new(),
                    Some(format!("failed to mount tree '{}': {e}", mount.mount)),
                );
            }
        }

        // Materialize individual files from node manifests into a tree and mount it.
        if !request.file_mounts.is_empty() {
            for fm in &request.file_mounts {
                let manifest = match self.cas.get_node(NodeHash(fm.manifest)).await {
                    Ok(Some(node_data)) => match CasNode::from_bytes(&node_data) {
                        Ok(CasNode::Manifest(m)) => m,
                        Ok(_) => {
                            self.remove_action_prefix(&action.prefix_id).await;
                            return run_failure_result(
                                start,
                                -1,
                                String::new(),
                                String::new(),
                                Some(format!(
                                    "input manifest {} is not a Manifest node",
                                    fm.manifest
                                )),
                            );
                        }
                        Err(e) => {
                            self.remove_action_prefix(&action.prefix_id).await;
                            return run_failure_result(
                                start,
                                -1,
                                String::new(),
                                String::new(),
                                Some(format!(
                                    "failed to decode input manifest {}: {}",
                                    fm.manifest, e
                                )),
                            );
                        }
                    },
                    Ok(None) => {
                        self.remove_action_prefix(&action.prefix_id).await;
                        return run_failure_result(
                            start,
                            -1,
                            String::new(),
                            String::new(),
                            Some(format!("input manifest {} not found in CAS", fm.manifest)),
                        );
                    }
                    Err(e) => {
                        self.remove_action_prefix(&action.prefix_id).await;
                        return run_failure_result(
                            start,
                            -1,
                            String::new(),
                            String::new(),
                            Some(format!("failed to fetch input manifest: {e:?}")),
                        );
                    }
                };

                let entry = match manifest.outputs.iter().find(|o| o.logical == fm.logical) {
                    Some(e) => e,
                    None => {
                        self.remove_action_prefix(&action.prefix_id).await;
                        return run_failure_result(
                            start,
                            -1,
                            String::new(),
                            String::new(),
                            Some(format!(
                                "input manifest {} missing logical output '{}'",
                                fm.manifest, fm.logical
                            )),
                        );
                    }
                };

                let (blob_hash, blob_size) = match &entry.blob {
                    OutputBlobRef::Small { chunk, size } => (chunk.0, *size),
                    OutputBlobRef::Large {
                        content_hash, size, ..
                    } => (content_hash.0, *size),
                };
                if let Err(e) = self
                    .vfs
                    .add_file_to_prefix(
                        &action.prefix_id,
                        &fm.path,
                        blob_hash,
                        blob_size,
                        entry.executable,
                    )
                    .named("runner.run_vfs.add_file_to_prefix")
                    .await
                {
                    self.remove_action_prefix(&action.prefix_id).await;
                    return run_failure_result(
                        start,
                        -1,
                        String::new(),
                        String::new(),
                        Some(format!(
                            "failed to materialize file mount '{}': {e}",
                            fm.path
                        )),
                    );
                }
            }
        }

        // r[impl exec.writable]
        for root in &request.writable_roots {
            self.vfs.add_dir_to_prefix(&action.prefix_id, root).await;
        }

        if !outputs_stay_within_writable_roots(&request.outputs, &request.writable_roots) {
            let offending = request
                .outputs
                .iter()
                .find(|out| {
                    !request
                        .writable_roots
                        .iter()
                        .any(|root| covers_path(root, &out.path))
                })
                .expect("offending output must exist when containment check fails");
            self.remove_action_prefix(&action.prefix_id).await;
            return run_failure_result(
                start,
                -1,
                String::new(),
                String::new(),
                Some(format!(
                    "output '{}' must be under a writable root (got '{}')",
                    offending.logical, offending.path
                )),
            );
        }

        for arg in &request.args {
            if let RunArg::Path(path) = arg
                && !path_is_covered(
                    path,
                    &request.tree_mounts,
                    &request.file_mounts,
                    &request.writable_roots,
                )
            {
                self.remove_action_prefix(&action.prefix_id).await;
                return run_failure_result(
                    start,
                    -1,
                    String::new(),
                    String::new(),
                    Some(format!(
                        "path arg '{}' is not covered by a mount or writable root",
                        path
                    )),
                );
            }
        }

        for (key, value) in &request.env {
            if let RunArg::Path(path) = value
                && !path_is_covered(
                    path,
                    &request.tree_mounts,
                    &request.file_mounts,
                    &request.writable_roots,
                )
            {
                self.remove_action_prefix(&action.prefix_id).await;
                return run_failure_result(
                    start,
                    -1,
                    String::new(),
                    String::new(),
                    Some(format!(
                        "env path '{}' for '{}' is not covered by a mount or writable root",
                        path, key
                    )),
                );
            }
        }

        if let Some(cwd) = &request.cwd
            && !path_is_covered(
                cwd,
                &request.tree_mounts,
                &request.file_mounts,
                &request.writable_roots,
            )
        {
            self.remove_action_prefix(&action.prefix_id).await;
            return run_failure_result(
                start,
                -1,
                String::new(),
                String::new(),
                Some(format!(
                    "cwd '{cwd}' is not covered by a mount or writable root"
                )),
            );
        }

        let program_path = match &request.program {
            Some(program) => Self::expand_run_arg(program, toolchain_path_str.as_deref()),
            None => {
                let toolchain_path = toolchain_path
                    .as_ref()
                    .expect("legacy run requests must resolve a toolchain path");
                toolchain_path
                    .join(format!(
                        "{}{}",
                        request.program_rel,
                        std::env::consts::EXE_SUFFIX
                    ))
                    .as_str()
                    .to_string()
            }
        };
        let mut cmd = crate::sandbox::sandboxed_command(&program_path, &action);
        for arg in &request.args {
            cmd.arg(Self::expand_run_arg(arg, toolchain_path_str.as_deref()));
        }

        let cwd = request
            .cwd
            .as_deref()
            .map(|p| action.prefix_path.join(p))
            .unwrap_or_else(|| action.prefix_path.clone());
        cmd.current_dir(cwd.as_str());

        if request.clear_env {
            action.hermetic_env(&mut cmd);
        }

        for (k, v) in &request.env {
            cmd.env(k, Self::expand_run_arg(v, toolchain_path_str.as_deref()));
        }

        // Zig needs its lib dir set to locate libc and friends.
        if request.program.is_none() && request.program_rel == "zig" {
            let toolchain_path = toolchain_path
                .as_ref()
                .expect("zig toolchain requests must resolve a toolchain path");
            cmd.env("ZIG_LIB_DIR", toolchain_path.join("lib").as_str());
        }

        let action_desc = format!(
            "{} {}",
            request
                .program
                .as_ref()
                .map(RunArg::as_str)
                .unwrap_or(&request.program_rel),
            request.args.first().map(RunArg::as_str).unwrap_or_default()
        );

        let action_kind = if request.args.iter().any(|arg| arg.as_str() == "-c") {
            ActionKind::CcCompile
        } else {
            ActionKind::CcLink
        };

        let output = match crate::sandbox::run_sandboxed_command(
            cmd,
            &action_kind,
            &action_desc,
            progress,
            None,
        )
        .named("runner.run_vfs.run_sandboxed_command")
        .await
        {
            Ok(o) => o,
            Err(CommandError::SpawnFailed(e)) => {
                self.remove_action_prefix(&action.prefix_id).await;
                return run_failure_result(
                    start,
                    -1,
                    String::new(),
                    String::new(),
                    Some(format!("failed to execute command: {e}")),
                );
            }
            Err(CommandError::TimedOut {
                action_desc,
                timeout,
                partial_output,
                ..
            }) => {
                self.remove_action_prefix(&action.prefix_id).await;
                return run_failure_result(
                    start,
                    -1,
                    String::new(),
                    partial_output,
                    Some(format!(
                        "action '{}' timed out after {}s (limit: {}s)",
                        action_desc,
                        timeout.as_secs(),
                        timeout.as_secs()
                    )),
                );
            }
            Err(CommandError::Cancelled {
                action_desc,
                partial_output,
            }) => {
                self.remove_action_prefix(&action.prefix_id).await;
                return run_failure_result(
                    start,
                    -1,
                    String::new(),
                    partial_output,
                    Some(format!("action '{}' cancelled", action_desc)),
                );
            }
        };

        let exit_code = output.exit_status.code().unwrap_or(-1);
        let observer_value = match request.eval.as_ref() {
            Some(eval) => match evaluate_shipped_observer(
                self.cas.as_ref(),
                eval,
                exit_code,
                &output.stdout,
                &output.stderr,
                &PathValue::new(action.prefix_path.as_str()),
            )
            .await
            {
                Ok(observer_value) => observer_value,
                Err(error) => {
                    self.remove_action_prefix(&action.prefix_id).await;
                    return run_failure_result(
                        start,
                        exit_code,
                        output.stdout.clone(),
                        output.stderr.clone(),
                        Some(format!("failed to evaluate shipped observer: {error}")),
                    );
                }
            },
            None => None,
        };

        if !output.exit_status.success() && observer_value.is_none() {
            self.remove_action_prefix(&action.prefix_id).await;
            return run_failure_result(start, exit_code, output.stdout, output.stderr, None);
        }

        let flushed = self
            .flush_prefix_outputs(&action.prefix_id)
            .named("runner.run_vfs.flush_prefix_outputs")
            .await;

        let mut outputs = Vec::new();
        for out in &request.outputs {
            let (blob, size) = match flushed.get(&out.path) {
                Some(&(h, s)) => (h, s),
                None => {
                    self.remove_action_prefix(&action.prefix_id).await;
                    return run_failure_result(
                        start,
                        -1,
                        output.stdout.clone(),
                        output.stderr.clone(),
                        Some(format!("output {} not found in VFS", out.path)),
                    );
                }
            };

            let filename = out
                .path
                .rsplit_once('/')
                .map(|(_, f)| f.to_string())
                .unwrap_or_else(|| out.path.clone());

            outputs.push(OutputEntry {
                logical: out.logical.clone(),
                filename,
                blob: OutputBlobRef::Small {
                    chunk: ChunkHash(blob),
                    size,
                },
                executable: out.executable,
            });
        }

        let cache_key = vx_cache::run_cache_key(&request);
        let manifest = NodeManifest {
            node_id: NodeId(format!(
                "run:{}",
                request
                    .program
                    .as_ref()
                    .map(RunArg::as_str)
                    .unwrap_or(&request.program_rel)
            )),
            cache_key,
            produced_at: Zoned::now().to_string(),
            outputs,
        };

        let cas_node = CasNode::Manifest(manifest);
        let (node_bytes, node_hash) = cas_node.to_bytes_and_hash();
        let manifest_hash = node_hash.0;
        if let Err(e) = self.cas.put_node(node_hash, node_bytes).await {
            self.remove_action_prefix(&action.prefix_id).await;
            return run_failure_result(
                start,
                -1,
                output.stdout.clone(),
                output.stderr.clone(),
                Some(format!("failed to store manifest: {:?}", e)),
            );
        }

        self.remove_action_prefix(&action.prefix_id).await;

        run_success_result(
            start,
            exit_code,
            output.stdout,
            output.stderr,
            Some(manifest_hash),
            observer_value,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vx_store_proto::Blake3Hash;

    #[test]
    // r[verify exec.mount]
    fn path_coverage_uses_mounts_and_writable_roots() {
        let tree_mounts = vec![RunTreeMount {
            mount: "src".to_string(),
            manifest: Blake3Hash([1; 32]),
        }];
        let file_mounts = vec![RunFileMount {
            path: "deps/libutil.rlib".to_string(),
            manifest: Blake3Hash([2; 32]),
            logical: "rlib".to_string(),
        }];
        let writable_roots = vec!["out".to_string()];

        assert!(path_is_covered(
            "src/main.rs",
            &tree_mounts,
            &file_mounts,
            &writable_roots,
        ));
        assert!(path_is_covered(
            "deps/libutil.rlib",
            &tree_mounts,
            &file_mounts,
            &writable_roots,
        ));
        assert!(path_is_covered(
            "out/app",
            &tree_mounts,
            &file_mounts,
            &writable_roots,
        ));
        assert!(path_is_covered(
            "${VX_TOOLCHAIN}/sysroot",
            &tree_mounts,
            &file_mounts,
            &writable_roots,
        ));
        assert!(!path_is_covered(
            "tmp/scratch",
            &tree_mounts,
            &file_mounts,
            &writable_roots,
        ));
    }

    #[test]
    // r[verify exec.mount]
    fn vfs_paths_must_be_relative() {
        assert!(is_relative_vfs_path("src"));
        assert!(is_relative_vfs_path("out/app"));
        assert!(!is_relative_vfs_path(""));
        assert!(!is_relative_vfs_path("/src"));
        assert!(!is_relative_vfs_path("../src"));
    }

    #[test]
    fn root_tree_mounts_are_allowed() {
        assert!(is_tree_mount_path(""));
        assert!(is_tree_mount_path("src"));
        assert!(!is_tree_mount_path("/src"));
    }

    #[test]
    // r[verify exec.writable]
    fn outputs_must_stay_within_writable_roots() {
        let outputs = vec![
            RunOutput {
                logical: "bin".to_string(),
                path: "out/bin/app".to_string(),
                executable: true,
            },
            RunOutput {
                logical: "dep-info".to_string(),
                path: "scratch/dep/app.d".to_string(),
                executable: false,
            },
        ];
        let writable_roots = vec!["out".to_string(), "scratch".to_string()];

        assert!(outputs_stay_within_writable_roots(
            &outputs,
            &writable_roots,
        ));

        let uncovered = vec![RunOutput {
            logical: "bin".to_string(),
            path: "tmp/app".to_string(),
            executable: true,
        }];
        assert!(!outputs_stay_within_writable_roots(
            &uncovered,
            &writable_roots,
        ));
    }
}
