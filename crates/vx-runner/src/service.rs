use vx_runner_proto::{
    CcCompileRequest, CcCompileResult, CcLinkRequest, CcLinkResult, RUNNER_PROTOCOL_VERSION,
    Runner, RustCompileRequest, RustCompileResult,
};
use vx_store_proto::ServiceVersion;

use crate::RunnerService;

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
        self.compile_rust_vfs(request).await
    }

    async fn compile_cc(&self, _cx: &roam::Context, request: CcCompileRequest) -> CcCompileResult {
        self.compile_cc_vfs(request).await
    }

    async fn link_cc(&self, _cx: &roam::Context, request: CcLinkRequest) -> CcLinkResult {
        self.link_cc_vfs(request).await
    }
}
