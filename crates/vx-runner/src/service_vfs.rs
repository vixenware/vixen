//! VFS-based action execution
//!
//! This module implements compilation using the VFS instead of disk materialization.
//! Files are served lazily from CAS via the Vixen mount.

use jiff::Zoned;
use std::time::Instant;
use tokio::process::Command;
use tracing::{debug, info, trace};
use vx_runner_proto::{
    CcCompileRequest, CcCompileResult, CcLinkRequest, CcLinkResult, RustCompileRequest,
    RustCompileResult,
};
use vx_store_proto::{NodeId, NodeManifest, OutputEntry};
use vx_vfs::{VfsAction, VfsIssueCode};

use crate::{RunnerService, VfsStartupFailure};

impl RunnerService {
    /// Compile a Rust crate using VFS-based execution
    pub async fn compile_rust_vfs(&self, request: RustCompileRequest) -> RustCompileResult {
        let start = Instant::now();

        if let Some(reason) = self.get_vfs_startup_error().await {
            return RustCompileResult {
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: start.elapsed().as_millis() as u64,
                output_manifest: None,
                error: Some(format_vfs_startup_failure(&reason)),
            };
        }

        info!(
            crate_name = %request.crate_name,
            crate_type = %request.crate_type,
            deps_count = request.deps.len(),
            "compiling rust crate (VFS mode)"
        );

        // 1. Create action prefix
        let prefix_id = self.create_action_prefix().await;
        let mount_base = crate::runner_mount_base();
        let prefix_path = mount_base.join(&prefix_id);

        debug!(prefix_id = %prefix_id, prefix_path = %prefix_path, "created action prefix");

        // Check if VFS mount exists (use tokio::fs to avoid blocking the tokio executor)
        match tokio::fs::metadata(mount_base.as_str()).await {
            Ok(meta) => {
                debug!(mount_base = %mount_base, is_dir = meta.is_dir(), "VFS mount base exists");
            }
            Err(e) => {
                debug!(mount_base = %mount_base, error = %e, "VFS mount base does NOT exist");
            }
        }

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

        // Check if prefix directory exists after creation (use tokio::fs)
        match tokio::fs::metadata(prefix_path.as_str()).await {
            Ok(meta) => {
                debug!(prefix_path = %prefix_path, is_dir = meta.is_dir(), "prefix directory exists after creation");
            }
            Err(e) => {
                debug!(prefix_path = %prefix_path, error = %e, "prefix directory does NOT exist after creation");
            }
        }

        // 2. Add toolchain to prefix
        debug!(
            toolchain_manifest = %request.toolchain_manifest,
            "adding toolchain to VFS prefix"
        );
        let before = Instant::now();
        if let Err(e) = self
            .add_toolchain_to_prefix(&prefix_id, request.toolchain_manifest)
            .await
        {
            self.remove_action_prefix(&prefix_id).await;
            return cleanup_and_error(&prefix_id, format!("failed to add toolchain: {}", e), start);
        }
        info!(
            "Adding Rust toolchain took {:.2}s",
            before.elapsed().as_secs_f32()
        );
        debug!("toolchain added to prefix successfully");

        // Dump VFS state to see what we actually have
        self.vfs.dump_state(&prefix_id).await;

        // Check toolchain directory after adding
        let toolchain_dir = prefix_path.join("toolchain");
        match tokio::fs::metadata(toolchain_dir.as_str()).await {
            Ok(meta) => {
                debug!(toolchain_dir = %toolchain_dir, is_dir = meta.is_dir(), "toolchain directory exists");
                // List contents
                if let Ok(mut entries) = tokio::fs::read_dir(toolchain_dir.as_str()).await {
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        trace!(entry = ?entry.path(), "  toolchain entry");
                    }
                }
            }
            Err(e) => {
                debug!(toolchain_dir = %toolchain_dir, error = %e, "toolchain directory does NOT exist");
            }
        }

        // 3. Add source tree to prefix
        if let Err(e) = self
            .add_source_tree_to_prefix(&prefix_id, request.source_manifest)
            .await
        {
            self.remove_action_prefix(&prefix_id).await;
            return cleanup_and_error(&prefix_id, format!("failed to add sources: {}", e), start);
        }

        // 4. Add dependencies to prefix
        for dep in &request.deps {
            if let Err(e) = self
                .add_dep_to_prefix(&prefix_id, &dep.extern_name, dep.manifest_hash)
                .await
            {
                self.remove_action_prefix(&prefix_id).await;
                return cleanup_and_error(
                    &prefix_id,
                    format!("failed to add dep {}: {}", dep.extern_name, e),
                    start,
                );
            }
        }

        // 5. Create output directory
        self.create_output_dir_in_prefix(&prefix_id).await;

        // 6. Build rustc command
        let rustc_path = prefix_path.join("toolchain/bin/rustc");
        let sysroot_path = prefix_path.join("toolchain/sysroot");
        let src_path = prefix_path.join("src");
        let deps_path = prefix_path.join("deps");
        let out_path = prefix_path.join("out");

        let output_filename = if request.crate_type == "lib" {
            format!("lib{}.rlib", request.crate_name)
        } else {
            request.crate_name.clone()
        };
        let output_file = out_path.join(&output_filename);

        let crate_root = src_path.join(&request.crate_root);

        let mut cmd = Command::new(rustc_path.as_str());
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

        // Crate root
        cmd.arg(crate_root.as_str());

        // Working directory
        cmd.current_dir(prefix_path.as_str());

        // Clean environment for hermeticity
        cmd.env_clear();
        // Need PATH for linker (for now - later sandbox will provide this)
        if let Ok(path) = std::env::var("PATH") {
            cmd.env("PATH", path);
        }

        debug!(
            rustc = %rustc_path,
            crate_name = %request.crate_name,
            crate_root = %crate_root,
            "executing rustc via VFS"
        );

        // Debug: Check if rustc exists before executing
        debug!(rustc_path = %rustc_path, "checking if rustc exists");
        match tokio::fs::metadata(rustc_path.as_str()).await {
            Ok(meta) => {
                debug!(
                    rustc_path = %rustc_path,
                    is_file = meta.is_file(),
                    len = meta.len(),
                    permissions = ?meta.permissions(),
                    "rustc metadata"
                );
            }
            Err(e) => {
                debug!(rustc_path = %rustc_path, error = %e, "rustc does NOT exist");
                // Try to list parent directory
                let parent = rustc_path.parent().unwrap_or(&prefix_path);
                debug!(parent = %parent, "listing parent directory");
                match tokio::fs::read_dir(parent.as_str()).await {
                    Ok(mut entries) => {
                        while let Ok(Some(entry)) = entries.next_entry().await {
                            trace!(entry = ?entry.path(), "  entry in parent");
                        }
                    }
                    Err(e) => {
                        debug!(parent = %parent, error = %e, "failed to read parent directory");
                        // Try grandparent
                        if let Some(grandparent) = parent.parent() {
                            debug!(grandparent = %grandparent, "listing grandparent");
                            if let Ok(mut entries) = tokio::fs::read_dir(grandparent.as_str()).await
                            {
                                while let Ok(Some(entry)) = entries.next_entry().await {
                                    trace!(entry = ?entry.path(), "  entry in grandparent");
                                }
                            }
                        }
                    }
                }
            }
        }

        // 7. Run rustc
        let output = match cmd.output().await {
            Ok(o) => o,
            Err(e) => {
                self.remove_action_prefix(&prefix_id).await;
                return cleanup_and_error(
                    &prefix_id,
                    format!("failed to execute rustc: {}", e),
                    start,
                );
            }
        };

        let duration_ms = start.elapsed().as_millis() as u64;
        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            self.remove_action_prefix(&prefix_id).await;
            return RustCompileResult {
                success: false,
                exit_code,
                stdout,
                stderr,
                duration_ms,
                output_manifest: None,
                error: None,
            };
        }

        // 8. Flush outputs from VFS to CAS
        let flushed = self.flush_prefix_outputs(&prefix_id).await;

        // Find the output blob hash and size
        let output_rel_path = format!("out/{}", output_filename);
        let (blob_hash, blob_size) = match flushed.get(&output_rel_path) {
            Some(&(hash, size)) => (hash, size),
            None => {
                self.remove_action_prefix(&prefix_id).await;
                return cleanup_and_error(
                    &prefix_id,
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
                blob: blob_hash,
                size: blob_size,
                executable: request.crate_type == "bin",
            }],
        };

        let manifest_hash = match self.cas.put_manifest(manifest).await {
            Ok(h) => h,
            Err(e) => {
                self.remove_action_prefix(&prefix_id).await;
                return cleanup_and_error(
                    &prefix_id,
                    format!("failed to store manifest: {:?}", e),
                    start,
                );
            }
        };

        // 10. Cleanup
        self.remove_action_prefix(&prefix_id).await;

        info!(
            crate_name = %request.crate_name,
            manifest_hash = %manifest_hash,
            duration_ms,
            "compilation complete (VFS mode)"
        );

        RustCompileResult {
            success: true,
            exit_code,
            stdout,
            stderr,
            duration_ms,
            output_manifest: Some(manifest_hash),
            error: None,
        }
    }

    /// Compile a C source file using VFS-based execution
    pub async fn compile_cc_vfs(&self, request: CcCompileRequest) -> CcCompileResult {
        let start = Instant::now();

        if let Some(reason) = self.get_vfs_startup_error().await {
            return CcCompileResult {
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: start.elapsed().as_millis() as u64,
                output_manifest: None,
                discovered_deps: vec![],
                error: Some(format_vfs_startup_failure(&reason)),
            };
        }

        info!(
            source = %request.source_path,
            target = %request.target_triple,
            "compiling C source (VFS mode)"
        );

        // 1. Create action prefix
        let prefix_id = self.create_action_prefix().await;
        let prefix_path = crate::runner_mount_base().join(&prefix_id);

        // 2. Add Zig toolchain to prefix
        if let Err(e) = self
            .add_toolchain_to_prefix(&prefix_id, request.toolchain_manifest)
            .await
        {
            self.remove_action_prefix(&prefix_id).await;
            return CcCompileResult {
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: start.elapsed().as_millis() as u64,
                output_manifest: None,
                discovered_deps: vec![],
                error: Some(format!("failed to add toolchain: {}", e)),
            };
        }

        // 3. Add source tree to prefix
        if let Err(e) = self
            .add_source_tree_to_prefix(&prefix_id, request.source_manifest)
            .await
        {
            self.remove_action_prefix(&prefix_id).await;
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
        self.create_output_dir_in_prefix(&prefix_id).await;

        // 5. Build zig cc command
        let zig_path = prefix_path.join("toolchain/zig");
        let src_path = prefix_path.join("src");
        let out_path = prefix_path.join("out");

        let source_file = src_path.join(&request.source_path);
        let object_filename = format!(
            "{}.o",
            request
                .source_path
                .strip_suffix(".c")
                .unwrap_or(&request.source_path)
        );
        let object_file = out_path.join(&object_filename);

        let mut cmd = Command::new(zig_path.as_str());
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

        cmd.current_dir(prefix_path.as_str());

        // Clean environment
        cmd.env_clear();
        if let Ok(path) = std::env::var("PATH") {
            cmd.env("PATH", path);
        }
        if let Ok(home) = std::env::var("HOME") {
            cmd.env("HOME", home);
        }
        cmd.env("ZIG_LIB_DIR", prefix_path.join("toolchain/lib").as_str());

        debug!(
            zig = %zig_path,
            source = %request.source_path,
            output = %object_file,
            "executing zig cc via VFS"
        );

        // 6. Run zig cc
        let output = match cmd.output().await {
            Ok(o) => o,
            Err(e) => {
                self.remove_action_prefix(&prefix_id).await;
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
        };

        let duration_ms = start.elapsed().as_millis() as u64;
        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            self.remove_action_prefix(&prefix_id).await;
            return CcCompileResult {
                success: false,
                exit_code,
                stdout,
                stderr,
                duration_ms,
                output_manifest: None,
                discovered_deps: vec![],
                error: None,
            };
        }

        // 7. Flush outputs from VFS to CAS
        let flushed = self.flush_prefix_outputs(&prefix_id).await;

        let output_rel_path = format!("out/{}", object_filename);
        let (blob_hash, blob_size) = match flushed.get(&output_rel_path) {
            Some(&(hash, size)) => (hash, size),
            None => {
                self.remove_action_prefix(&prefix_id).await;
                return CcCompileResult {
                    success: false,
                    exit_code: -1,
                    stdout,
                    stderr,
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
                blob: blob_hash,
                size: blob_size,
                executable: false,
            }],
        };

        let manifest_hash = match self.cas.put_manifest(manifest).await {
            Ok(h) => h,
            Err(e) => {
                self.remove_action_prefix(&prefix_id).await;
                return CcCompileResult {
                    success: false,
                    exit_code: -1,
                    stdout,
                    stderr,
                    duration_ms,
                    output_manifest: None,
                    discovered_deps: vec![],
                    error: Some(format!("failed to store manifest: {:?}", e)),
                };
            }
        };

        // 9. Cleanup
        self.remove_action_prefix(&prefix_id).await;

        info!(
            source = %request.source_path,
            manifest_hash = %manifest_hash,
            duration_ms,
            "C compilation complete (VFS mode)"
        );

        CcCompileResult {
            success: true,
            exit_code,
            stdout,
            stderr,
            duration_ms,
            output_manifest: Some(manifest_hash),
            discovered_deps: vec![], // TODO: parse depfile if generated
            error: None,
        }
    }

    /// Link C object files into a binary using the VFS mount.
    pub async fn link_cc_vfs(&self, request: CcLinkRequest) -> CcLinkResult {
        let start = Instant::now();

        if let Some(reason) = self.get_vfs_startup_error().await {
            return CcLinkResult {
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: start.elapsed().as_millis() as u64,
                output_manifest: None,
                error: Some(format_vfs_startup_failure(&reason)),
            };
        }

        let prefix_id = self.create_action_prefix().await;
        let prefix_path = crate::runner_mount_base().join(&prefix_id);

        if let Err(e) = self
            .add_toolchain_to_prefix(&prefix_id, request.toolchain_manifest)
            .await
        {
            self.remove_action_prefix(&prefix_id).await;
            return CcLinkResult {
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: start.elapsed().as_millis() as u64,
                output_manifest: None,
                error: Some(format!("failed to add toolchain: {}", e)),
            };
        }

        self.vfs.add_dir_to_prefix(&prefix_id, "obj").await;
        self.create_output_dir_in_prefix(&prefix_id).await;

        let mut object_paths = Vec::with_capacity(request.object_manifests.len());
        for (idx, manifest_hash) in request.object_manifests.iter().enumerate() {
            let manifest = match self.cas.get_manifest(*manifest_hash).await {
                Ok(Some(m)) => m,
                Ok(None) => {
                    self.remove_action_prefix(&prefix_id).await;
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
                    self.remove_action_prefix(&prefix_id).await;
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
                    self.remove_action_prefix(&prefix_id).await;
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

            let obj_rel_path = format!("obj/{idx:04}.o");
            self.vfs
                .add_file_to_prefix(
                    &prefix_id,
                    &obj_rel_path,
                    object_entry.blob,
                    object_entry.size,
                    false,
                )
                .await;
            object_paths.push(prefix_path.join(obj_rel_path));
        }

        let output_rel_path = format!("out/{}", request.binary_name);
        let output_path = prefix_path.join(&output_rel_path);

        let mut cmd = Command::new("cc");
        for object_path in &object_paths {
            cmd.arg(object_path.as_str());
        }
        cmd.arg("-o").arg(output_path.as_str());
        for lib in &request.libs {
            cmd.arg(format!("-l{}", lib));
        }
        cmd.current_dir(prefix_path.as_str());
        cmd.env_clear();
        if let Ok(path) = std::env::var("PATH") {
            cmd.env("PATH", path);
        }
        if let Ok(home) = std::env::var("HOME") {
            cmd.env("HOME", home);
        }

        let output = match cmd.output().await {
            Ok(o) => o,
            Err(e) => {
                self.remove_action_prefix(&prefix_id).await;
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
        };

        let duration_ms = start.elapsed().as_millis() as u64;
        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            self.remove_action_prefix(&prefix_id).await;
            return CcLinkResult {
                success: false,
                exit_code,
                stdout,
                stderr,
                duration_ms,
                output_manifest: None,
                error: None,
            };
        }

        let flushed = self.flush_prefix_outputs(&prefix_id).await;
        let (blob_hash, blob_size) = match flushed.get(&output_rel_path) {
            Some(&(hash, size)) => (hash, size),
            None => {
                self.remove_action_prefix(&prefix_id).await;
                return CcLinkResult {
                    success: false,
                    exit_code: -1,
                    stdout,
                    stderr,
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
                blob: blob_hash,
                size: blob_size,
                executable: true,
            }],
        };

        let manifest_hash = match self.cas.put_manifest(manifest).await {
            Ok(h) => h,
            Err(e) => {
                self.remove_action_prefix(&prefix_id).await;
                return CcLinkResult {
                    success: false,
                    exit_code: -1,
                    stdout,
                    stderr,
                    duration_ms,
                    output_manifest: None,
                    error: Some(format!("failed to store manifest: {:?}", e)),
                };
            }
        };

        self.remove_action_prefix(&prefix_id).await;

        CcLinkResult {
            success: true,
            exit_code,
            stdout,
            stderr,
            duration_ms,
            output_manifest: Some(manifest_hash),
            error: None,
        }
    }
}

fn format_vfs_startup_failure(failure: &VfsStartupFailure) -> String {
    let mut lines = Vec::new();
    lines.push(format!("VFS unavailable: {}", failure.reason));

    for diagnostic in &failure.diagnostics {
        lines.push(format!(
            "- [{}] {}",
            issue_code_name(diagnostic.code),
            diagnostic.summary
        ));
        if let Some(detail) = &diagnostic.detail {
            lines.push(format!("  detail: {}", detail));
        }
        for action in &diagnostic.actions {
            lines.push(format!("  action: {}", render_action(action)));
        }
    }

    lines.join("\n")
}

fn issue_code_name(code: VfsIssueCode) -> &'static str {
    match code {
        VfsIssueCode::UnsupportedPlatform => "unsupported_platform",
        VfsIssueCode::AppMissing => "app_missing",
        VfsIssueCode::ExtensionNotInstalled => "extension_not_installed",
        VfsIssueCode::ExtensionDisabled => "extension_disabled",
        VfsIssueCode::HealthCheckFailed => "health_check_failed",
        VfsIssueCode::MountFailed => "mount_failed",
    }
}

fn render_action(action: &VfsAction) -> String {
    match action {
        VfsAction::RetryBuild => "retry the build".to_string(),
        VfsAction::RunVxApp => {
            if cfg!(target_os = "macos") {
                "run `vx doctor`".to_string()
            } else {
                "run the platform-specific Vixen setup flow".to_string()
            }
        }
        VfsAction::OpenDownloadUrl(url) => format!("install Vixen from {}", url),
    }
}
