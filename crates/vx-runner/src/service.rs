use vx_runner_proto::{
    CcCompileRequest, CcCompileResult, CcLinkRequest, CcLinkResult, DiscoverRequest,
    DiscoverResult, RUNNER_PROTOCOL_VERSION, RunRequest, RunResult, Runner, RustCompileRequest,
    RustCompileResult,
};
use vx_store_proto::{ServiceVersion, StoreProgress};

use crate::RunnerService;
use moire::task::FutureExt;

impl RunnerService {
    async fn version(&self) -> ServiceVersion {
        ServiceVersion {
            service: "vx-runner".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: RUNNER_PROTOCOL_VERSION,
        }
    }

    async fn compile_rust(
        &self,
        request: RustCompileRequest,
        progress: roam::Tx<StoreProgress>,
    ) -> RustCompileResult {
        self.compile_rust_vfs(request, progress).await
    }

    async fn compile_cc(
        &self,
        request: CcCompileRequest,
        progress: roam::Tx<StoreProgress>,
    ) -> CcCompileResult {
        self.compile_cc_vfs(request, progress).await
    }

    async fn link_cc(
        &self,
        request: CcLinkRequest,
        progress: roam::Tx<StoreProgress>,
    ) -> CcLinkResult {
        self.link_cc_vfs(request, progress).await
    }

    async fn run(&self, request: RunRequest, progress: roam::Tx<StoreProgress>) -> RunResult {
        self.run_vfs(request, progress)
            .named("runner.run_vfs")
            .await
    }

    async fn discover(&self, request: DiscoverRequest) -> Result<DiscoverResult, String> {
        self.discover_vfs(request)
            .named("runner.discover_vfs")
            .await
    }
}

impl Runner for RunnerService {
    async fn version(&self) -> ServiceVersion {
        RunnerService::version(self).await
    }

    async fn compile_rust(
        &self,
        request: RustCompileRequest,
        progress: roam::Tx<StoreProgress>,
    ) -> RustCompileResult {
        RunnerService::compile_rust(self, request, progress).await
    }

    async fn compile_cc(
        &self,
        request: CcCompileRequest,
        progress: roam::Tx<StoreProgress>,
    ) -> CcCompileResult {
        RunnerService::compile_cc(self, request, progress).await
    }

    async fn link_cc(
        &self,
        request: CcLinkRequest,
        progress: roam::Tx<StoreProgress>,
    ) -> CcLinkResult {
        RunnerService::link_cc(self, request, progress).await
    }

    async fn run(&self, request: RunRequest, progress: roam::Tx<StoreProgress>) -> RunResult {
        RunnerService::run(self, request, progress).await
    }

    async fn discover(&self, request: DiscoverRequest) -> Result<DiscoverResult, String> {
        RunnerService::discover(self, request).await
    }
}
