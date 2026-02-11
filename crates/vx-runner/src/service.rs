use vx_runner_proto::{
    CcCompileRequest, CcCompileResult, CcLinkRequest, CcLinkResult, RUNNER_PROTOCOL_VERSION,
    RunRequest, RunResult, Runner, RustCompileRequest, RustCompileResult,
};
use vx_store_proto::{ServiceVersion, StoreProgress};

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
        progress: roam::Tx<StoreProgress>,
    ) -> RustCompileResult {
        self.compile_rust_vfs(request, progress).await
    }

    async fn compile_cc(
        &self,
        _cx: &roam::Context,
        request: CcCompileRequest,
        progress: roam::Tx<StoreProgress>,
    ) -> CcCompileResult {
        self.compile_cc_vfs(request, progress).await
    }

    async fn link_cc(
        &self,
        _cx: &roam::Context,
        request: CcLinkRequest,
        progress: roam::Tx<StoreProgress>,
    ) -> CcLinkResult {
        self.link_cc_vfs(request, progress).await
    }

    async fn run(
        &self,
        _cx: &roam::Context,
        request: RunRequest,
        progress: roam::Tx<StoreProgress>,
    ) -> RunResult {
        self.run_vfs(request, progress).await
    }
}
