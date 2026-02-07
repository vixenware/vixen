use camino::{Utf8Path, Utf8PathBuf};
use jiff::Zoned;
use std::process::Command;
use std::time::Instant;
use tracing::{debug, info, warn};
use vx_runner_proto::{
    CcCompileRequest, CcCompileResult, CcLinkRequest, CcLinkResult, RUNNER_PROTOCOL_VERSION,
    Runner, RustCompileRequest, RustCompileResult,
};
use vx_store_proto::ServiceVersion;
use vx_store_proto::{NodeId, NodeManifest, OutputEntry};

use crate::{RunnerResult, RunnerService};

/// Information about a crate extracted from its Cargo.toml
struct CrateInfo {
    /// The path to the crate root file (e.g., "src/lib.rs")
    lib_path: Utf8PathBuf,
    /// The Rust edition
    edition: String,
}

impl Runner for RunnerService {
    async fn version(&self, _cx: &roam::Context) -> ServiceVersion {
        ServiceVersion {
            service: "vx-runner".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: RUNNER_PROTOCOL_VERSION,
        }
    }

    async fn compile_rust(
        &self,
        _cx: &roam::Context,
        request: RustCompileRequest,
    ) -> RustCompileResult {
        tracing::info!("🔧 compile_rust via VFS");
        self.compile_rust_vfs(request).await
    }

    async fn compile_cc(&self, _cx: &roam::Context, request: CcCompileRequest) -> CcCompileResult {
        tracing::info!("🔧 compile_cc via VFS");
        self.compile_cc_vfs(request).await
    }

    async fn link_cc(&self, _cx: &roam::Context, request: CcLinkRequest) -> CcLinkResult {
        // TODO: Implement VFS-based linking
        self.link_cc_legacy(request).await
    }
}

// Legacy implementations kept for reference and link_cc
impl RunnerService {
    async fn link_cc_legacy(&self, request: CcLinkRequest) -> CcLinkResult {
        let start = Instant::now();

        info!(
            binary_name = %request.binary_name,
            object_count = request.object_manifests.len(),
            target = %request.target_triple,
            "linking C binary"
        );

        // 1. Materialize Zig toolchain (not used currently since we use system cc)
        let _toolchain_dir = match self.ensure_materialized(request.toolchain_manifest).await {
            Ok(dir) => dir,
            Err(e) => {
                return CcLinkResult {
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration_ms: start.elapsed().as_millis() as u64,
                    output_manifest: None,
                    error: Some(format!("failed to materialize Zig toolchain: {}", e)),
                };
            }
        };

        // 2. Create scratch directory
        let scratch_dir = match self.create_scratch_dir().await {
            Ok(dir) => dir,
            Err(e) => {
                return CcLinkResult {
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration_ms: start.elapsed().as_millis() as u64,
                    output_manifest: None,
                    error: Some(format!("failed to create scratch directory: {}", e)),
                };
            }
        };

        // 3. Materialize object files from CAS
        let obj_dir = scratch_dir.join(".vx/obj");
        if let Err(e) = tokio::fs::create_dir_all(&obj_dir).await {
            if let Err(cleanup_err) = tokio::fs::remove_dir_all(&scratch_dir).await {
                warn!(
                    "failed to remove scratch directory after obj directory creation failure: {cleanup_err}"
                );
            }
            return CcLinkResult {
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: start.elapsed().as_millis() as u64,
                output_manifest: None,
                error: Some(format!("failed to create obj directory: {}", e)),
            };
        }

        let mut object_paths = Vec::new();
        for (idx, obj_manifest_hash) in request.object_manifests.iter().enumerate() {
            // Fetch the manifest to get the blob hash
            let obj_manifest = match self.cas.get_manifest(*obj_manifest_hash).await {
                Ok(Some(m)) => m,
                Ok(None) => {
                    if let Err(e) = tokio::fs::remove_dir_all(&scratch_dir).await {
                        warn!(
                            "failed to remove scratch directory after object manifest not found: {e}"
                        );
                    }
                    return CcLinkResult {
                        success: false,
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: String::new(),
                        duration_ms: start.elapsed().as_millis() as u64,
                        output_manifest: None,
                        error: Some(format!(
                            "object manifest {} not found in CAS",
                            obj_manifest_hash
                        )),
                    };
                }
                Err(e) => {
                    if let Err(cleanup_err) = tokio::fs::remove_dir_all(&scratch_dir).await {
                        warn!(
                            "failed to remove scratch directory after object manifest fetch failure: {cleanup_err}"
                        );
                    }
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

            // Find the object output in the manifest
            let obj_entry = obj_manifest
                .outputs
                .iter()
                .find(|o| o.logical == "object")
                .ok_or_else(|| format!("no object output in manifest {}", idx));

            let obj_entry = match obj_entry {
                Ok(e) => e,
                Err(e) => {
                    if let Err(cleanup_err) = tokio::fs::remove_dir_all(&scratch_dir).await {
                        warn!(
                            "failed to remove scratch directory after object entry not found: {cleanup_err}"
                        );
                    }
                    return CcLinkResult {
                        success: false,
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: String::new(),
                        duration_ms: start.elapsed().as_millis() as u64,
                        output_manifest: None,
                        error: Some(e),
                    };
                }
            };

            // Fetch the object blob and write it
            let obj_data = match self.cas.get_blob(obj_entry.blob).await {
                Ok(Some(data)) => data,
                Ok(None) => {
                    if let Err(e) = tokio::fs::remove_dir_all(&scratch_dir).await {
                        warn!(
                            "failed to remove scratch directory after object blob not found: {e}"
                        );
                    }
                    return CcLinkResult {
                        success: false,
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: String::new(),
                        duration_ms: start.elapsed().as_millis() as u64,
                        output_manifest: None,
                        error: Some(format!("object blob {} not found in CAS", obj_entry.blob)),
                    };
                }
                Err(e) => {
                    if let Err(cleanup_err) = tokio::fs::remove_dir_all(&scratch_dir).await {
                        warn!(
                            "failed to remove scratch directory after object blob fetch failure: {cleanup_err}"
                        );
                    }
                    return CcLinkResult {
                        success: false,
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: String::new(),
                        duration_ms: start.elapsed().as_millis() as u64,
                        output_manifest: None,
                        error: Some(format!("failed to fetch object blob: {:?}", e)),
                    };
                }
            };

            let obj_path = obj_dir.join(format!("obj{}.o", idx));
            if let Err(e) = tokio::fs::write(&obj_path, &obj_data).await {
                if let Err(cleanup_err) = tokio::fs::remove_dir_all(&scratch_dir).await {
                    warn!(
                        "failed to remove scratch directory after object write failure: {cleanup_err}"
                    );
                }
                return CcLinkResult {
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration_ms: start.elapsed().as_millis() as u64,
                    output_manifest: None,
                    error: Some(format!("failed to write object file: {}", e)),
                };
            }

            object_paths.push(obj_path);
        }

        // 4. Create output directory
        let output_dir = scratch_dir.join(".vx/bin");
        if let Err(e) = tokio::fs::create_dir_all(&output_dir).await {
            if let Err(cleanup_err) = tokio::fs::remove_dir_all(&scratch_dir).await {
                warn!(
                    "failed to remove scratch directory after output directory creation failure: {cleanup_err}"
                );
            }
            return CcLinkResult {
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: start.elapsed().as_millis() as u64,
                output_manifest: None,
                error: Some(format!("failed to create output directory: {}", e)),
            };
        }

        let binary_filename = request.binary_name.clone();
        let output_path = output_dir.join(&binary_filename);

        // 5. Build linker command
        // NOTE: Using system cc for linking because Zig requires test files
        // from source tree when building compiler_rt, which aren't in binary distribution
        let mut cmd = Command::new("cc");

        // Add all object files
        for obj_path in &object_paths {
            cmd.arg(obj_path);
        }

        cmd.arg("-o").arg(&output_path);

        // Add libraries
        for lib in &request.libs {
            cmd.arg(format!("-l{}", lib));
        }

        cmd.current_dir(&scratch_dir);

        // Clean environment but preserve necessary variables
        cmd.env_clear();
        if let Ok(path) = std::env::var("PATH") {
            cmd.env("PATH", path);
        }
        if let Ok(home) = std::env::var("HOME") {
            cmd.env("HOME", home);
        }

        debug!(
            binary = %request.binary_name,
            objects = object_paths.len(),
            "executing system cc for linking"
        );

        // 6. Run linker
        let output = match cmd.output() {
            Ok(o) => o,
            Err(e) => {
                if let Err(cleanup_err) = tokio::fs::remove_dir_all(&scratch_dir).await {
                    warn!(
                        "failed to remove scratch directory after zig cc execution failure: {cleanup_err}"
                    );
                }
                return CcLinkResult {
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration_ms: start.elapsed().as_millis() as u64,
                    output_manifest: None,
                    error: Some(format!("failed to execute zig cc: {}", e)),
                };
            }
        };

        let duration_ms = start.elapsed().as_millis() as u64;
        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            if let Err(e) = tokio::fs::remove_dir_all(&scratch_dir).await {
                warn!("failed to remove scratch directory after zig cc linking failure: {e}");
            }
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

        // 7. Ingest binary to CAS
        let binary_data = match tokio::fs::read(&output_path).await {
            Ok(d) => d,
            Err(e) => {
                if let Err(cleanup_err) = tokio::fs::remove_dir_all(&scratch_dir).await {
                    warn!(
                        "failed to remove scratch directory after binary read failure: {cleanup_err}"
                    );
                }
                return CcLinkResult {
                    success: false,
                    exit_code: -1,
                    stdout,
                    stderr,
                    duration_ms,
                    output_manifest: None,
                    error: Some(format!("failed to read binary: {}", e)),
                };
            }
        };

        let blob_hash = match self.cas.put_blob(binary_data).await {
            Ok(h) => h,
            Err(e) => {
                if let Err(cleanup_err) = tokio::fs::remove_dir_all(&scratch_dir).await {
                    warn!(
                        "failed to remove scratch directory after CAS blob storage failure: {cleanup_err}"
                    );
                }
                return CcLinkResult {
                    success: false,
                    exit_code: -1,
                    stdout,
                    stderr,
                    duration_ms,
                    output_manifest: None,
                    error: Some(format!("failed to store binary in CAS: {:?}", e)),
                };
            }
        };

        // 8. Create and store output manifest
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
                filename: binary_filename.clone(),
                blob: blob_hash,
                executable: true,
            }],
        };

        let manifest_hash = match self.cas.put_manifest(manifest).await {
            Ok(h) => h,
            Err(e) => {
                if let Err(cleanup_err) = tokio::fs::remove_dir_all(&scratch_dir).await {
                    warn!(
                        "failed to remove scratch directory after CAS manifest storage failure: {cleanup_err}"
                    );
                }
                return CcLinkResult {
                    success: false,
                    exit_code: -1,
                    stdout,
                    stderr,
                    duration_ms,
                    output_manifest: None,
                    error: Some(format!("failed to store manifest in CAS: {:?}", e)),
                };
            }
        };

        // Cleanup scratch directory
        if let Err(e) = tokio::fs::remove_dir_all(&scratch_dir).await {
            warn!("failed to remove scratch directory after successful linking: {e}");
        }

        info!(
            binary = %request.binary_name,
            manifest_hash = %manifest_hash,
            duration_ms,
            "C linking complete"
        );

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

impl RunnerService {
    /// Create a scratch directory for compilation
    async fn create_scratch_dir(&self) -> RunnerResult<Utf8PathBuf> {
        let scratch_base = self.toolchains_dir.parent().unwrap_or(&self.toolchains_dir);
        vx_io::create_scratch_dir(scratch_base)
            .await
            .map_err(|e| crate::RunnerError::ScratchDir(e.to_string()))
    }
}

/// Parse a Cargo.toml to extract lib_path and edition.
///
/// This is used when compiling registry crates, where the request contains
/// placeholder values and we need to determine the actual values from the crate.
async fn parse_crate_info(crate_dir: &Utf8Path) -> Result<CrateInfo, String> {
    let cargo_toml_path = crate_dir.join("Cargo.toml");

    // Use blocking task to read and parse the file synchronously (facet_cargo_toml uses std::fs)
    let cargo_toml_path_clone = cargo_toml_path.clone();
    let manifest = tokio::task::spawn_blocking(move || {
        facet_cargo_toml::CargoToml::from_path(&cargo_toml_path_clone)
    })
    .await
    .map_err(|e| format!("task join error: {}", e))?
    .map_err(|e| format!("failed to parse {}: {}", cargo_toml_path, e))?;

    // Determine lib_path
    let lib_path = if let Some(ref lib) = manifest.lib {
        if let Some(ref path) = lib.path {
            Utf8PathBuf::from(path.value.as_str())
        } else {
            Utf8PathBuf::from("src/lib.rs")
        }
    } else {
        Utf8PathBuf::from("src/lib.rs")
    };

    // Determine edition
    let edition = if let Some(ref package) = manifest.package {
        match &package.edition {
            Some(facet_cargo_toml::EditionOrWorkspace::Edition(e)) => match e.value {
                facet_cargo_toml::Edition::E2015 => "2015".to_string(),
                facet_cargo_toml::Edition::E2018 => "2018".to_string(),
                facet_cargo_toml::Edition::E2021 => "2021".to_string(),
                facet_cargo_toml::Edition::E2024 => "2024".to_string(),
            },
            _ => "2021".to_string(), // Default
        }
    } else {
        "2021".to_string() // Default
    };

    Ok(CrateInfo { lib_path, edition })
}

/// Parse a dependency file (.d) generated by -MD flag
///
/// Depfile format: "output.o: dep1.h dep2.h dep3.h"
/// We extract the dependency paths (relative to scratch dir)
#[allow(dead_code)] // TODO: Use in VFS-based C compilation for incremental builds
fn parse_depfile(contents: &str) -> Vec<String> {
    // Simple parser: split by ':', take RHS, split by whitespace
    if let Some((_lhs, rhs)) = contents.split_once(':') {
        rhs.split_whitespace()
            .filter(|s| !s.is_empty() && !s.ends_with('\\'))
            .map(|s| s.to_string())
            .collect()
    } else {
        vec![]
    }
}
