//! Temporary stub for legacy toolchain acquisition during the evaluator purge.
//!
//! The old `vx-toolchain`-backed acquisition path has been removed. Keep the
//! store RPC surface intact, but fail explicitly until a new toolchain model is
//! designed.

use vx_store_proto::{
    BlobRef, EnsureRustToolchainComponentResult, EnsureStatus, EnsureToolchainResult, NodeHash,
    RustComponent, RustToolchainSpec, ToolchainSpecKey, ZigToolchainSpec,
};

use crate::types::StoreService;

#[derive(Debug, Clone)]
pub(crate) struct ExtractedRustComponent {
    pub(crate) tree_root: NodeHash,
    pub(crate) target: String,
    pub(crate) total_size_bytes: u64,
    pub(crate) file_count: u32,
    pub(crate) unique_chunks: u32,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ToolchainManager;

impl ToolchainManager {
    pub(crate) fn new() -> Self {
        Self
    }
}

fn unavailable_toolchain_result(
    spec_key: Result<ToolchainSpecKey, &'static str>,
    error: &str,
) -> EnsureToolchainResult {
    EnsureToolchainResult {
        spec_key: spec_key.ok(),
        toolchain_id: None,
        manifest_hash: None,
        status: EnsureStatus::Failed,
        error: Some(error.to_string()),
    }
}

fn unavailable_component_result(
    spec_key: Result<ToolchainSpecKey, &'static str>,
    component: RustComponent,
    error: &str,
) -> EnsureRustToolchainComponentResult {
    EnsureRustToolchainComponentResult {
        spec_key: spec_key.ok(),
        component,
        tarball: None,
        tree_root: None,
        status: EnsureStatus::Failed,
        error: Some(error.to_string()),
    }
}

impl StoreService {
    pub async fn ensure_rust_toolchain(
        &self,
        spec: RustToolchainSpec,
        _progress: roam::Tx<vx_store_proto::StoreProgress>,
    ) -> EnsureToolchainResult {
        unavailable_toolchain_result(
            spec.spec_key(),
            "rust toolchain acquisition is temporarily unavailable after the legacy toolchain purge",
        )
    }

    pub async fn download_rust_toolchain_component(
        &self,
        spec: RustToolchainSpec,
        component: RustComponent,
        _progress: roam::Tx<vx_store_proto::StoreProgress>,
    ) -> EnsureRustToolchainComponentResult {
        unavailable_component_result(
            spec.spec_key(),
            component,
            "rust toolchain component download is temporarily unavailable after the legacy toolchain purge",
        )
    }

    pub async fn extract_rust_toolchain_component(
        &self,
        spec: RustToolchainSpec,
        component: RustComponent,
        _tarball: BlobRef,
        _progress: roam::Tx<vx_store_proto::StoreProgress>,
    ) -> EnsureRustToolchainComponentResult {
        unavailable_component_result(
            spec.spec_key(),
            component,
            "rust toolchain component extraction is temporarily unavailable after the legacy toolchain purge",
        )
    }

    pub async fn finalize_rust_toolchain(&self, spec: RustToolchainSpec) -> EnsureToolchainResult {
        unavailable_toolchain_result(
            spec.spec_key(),
            "rust toolchain finalization is temporarily unavailable after the legacy toolchain purge",
        )
    }

    pub async fn ensure_zig_toolchain(
        &self,
        spec: ZigToolchainSpec,
        _progress: roam::Tx<vx_store_proto::StoreProgress>,
    ) -> EnsureToolchainResult {
        unavailable_toolchain_result(
            spec.spec_key(),
            "zig toolchain acquisition is temporarily unavailable after the legacy toolchain purge",
        )
    }
}
