use vx_runner_proto::{
    CcCompileRequest, CcCompileResult, CcLinkRequest, CcLinkResult, RUNNER_PROTOCOL_VERSION,
    RunRequest, RunResult, RunnerServer as Runner, RustCompileRequest, RustCompileResult,
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
}

impl Runner for RunnerService {
    async fn version(&self, call: impl roam::Call<ServiceVersion, core::convert::Infallible>) {
        call.ok(RunnerService::version(self).await).await;
    }

    async fn compile_rust(
        &self,
        call: impl roam::Call<RustCompileResult, core::convert::Infallible>,
        request: RustCompileRequest,
        progress: roam::Tx<StoreProgress>,
    ) {
        call.ok(RunnerService::compile_rust(self, request, progress).await)
            .await;
    }

    async fn compile_cc(
        &self,
        call: impl roam::Call<CcCompileResult, core::convert::Infallible>,
        request: CcCompileRequest,
        progress: roam::Tx<StoreProgress>,
    ) {
        call.ok(RunnerService::compile_cc(self, request, progress).await)
            .await;
    }

    async fn link_cc(
        &self,
        call: impl roam::Call<CcLinkResult, core::convert::Infallible>,
        request: CcLinkRequest,
        progress: roam::Tx<StoreProgress>,
    ) {
        call.ok(RunnerService::link_cc(self, request, progress).await)
            .await;
    }

    async fn run(
        &self,
        call: impl roam::Call<RunResult, core::convert::Infallible>,
        request: RunRequest,
        progress: roam::Tx<StoreProgress>,
    ) {
        call.ok(RunnerService::run(self, request, progress).await)
            .await;
    }
}
