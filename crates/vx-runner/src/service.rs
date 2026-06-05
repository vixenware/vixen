use vx_exec_protocol::{Observe, ObserveRequest, Observed};
use vx_runtime::observe::{CompletedRun, evaluate_observer};
use vx_runtime::run_value::ProcOutcome;
use vx_services::{
    DiscoverProbeRequest, DiscoverResult, ExecEnv as RunnerExecEnv, ExecRequest, ExecResult, Runner,
};

use crate::RunnerService;
use crate::exec::run_exec;
use crate::output_tree::CasOutputTreeService;

impl RunnerService {
    pub(crate) fn install_producing_outputs_for_observed(
        &self,
        observed: Observed,
        outputs: Vec<vx_services::ExecOutput>,
    ) -> Observed {
        match observed {
            Observed::Tree => match self.store() {
                Some(store) => {
                    self.install_produced_outputs(Some(CasOutputTreeService::new(store, outputs)));
                    Observed::Tree
                }
                None => Observed::Poisoned(
                    "runner-backed observe returned a tree, but the runner has no CAS store \
                     configured for Producing output serving"
                        .to_string(),
                ),
            },
            Observed::ValueWithTree(value) => match self.store() {
                Some(store) => {
                    self.install_produced_outputs(Some(CasOutputTreeService::new(store, outputs)));
                    Observed::ValueWithTree(value)
                }
                None => Observed::Poisoned(
                    "runner-backed observe returned a value containing a tree, but the runner \
                     has no CAS store configured for Producing output serving"
                        .to_string(),
                ),
            },
            other => other,
        }
    }

    async fn exec(&self, request: ExecRequest) -> ExecResult {
        let Some(vfsd) = self.vfsd() else {
            return ExecResult {
                exit_code: -1,
                stdout: Vec::new(),
                stderr: Vec::new(),
                outputs: Vec::new(),
                error: Some("runner has no CAS-VFS configured for exec".to_string()),
            };
        };
        // Mount → run → flush against the runner's local CAS-VFS.
        run_exec(vfsd, &request).await
    }

    async fn discover_probe(&self, _request: DiscoverProbeRequest) -> DiscoverResult {
        DiscoverResult {}
    }

    async fn observe(&self, request: ObserveRequest) -> Observed {
        self.install_produced_outputs(None);
        let Some(vfsd) = self.vfsd() else {
            return Observed::Poisoned(
                "runner-backed observe has no CAS-VFS configured for exec".to_string(),
            );
        };
        let result = run_exec(
            vfsd,
            &ExecRequest {
                program: request.command.program,
                args: request.command.args,
                env: request
                    .env
                    .into_iter()
                    .map(|env| RunnerExecEnv {
                        key: env.key,
                        value: env.value,
                    })
                    .collect(),
                mounts: Vec::new(),
                outputs: request.outputs,
                create_dirs: request.create_dirs,
            },
        )
        .await;
        let ExecResult {
            exit_code,
            stdout,
            stderr,
            outputs,
            error,
        } = result;
        if let Some(error) = error {
            return Observed::Poisoned(error);
        }
        let observed = evaluate_observer(
            CompletedRun::new(ProcOutcome {
                exit_code,
                stdout,
                stderr,
            }),
            request.observer,
        );
        self.install_producing_outputs_for_observed(observed, outputs)
    }
}

impl Runner for RunnerService {
    async fn exec(&self, request: ExecRequest) -> ExecResult {
        RunnerService::exec(self, request).await
    }

    async fn discover_probe(&self, request: DiscoverProbeRequest) -> DiscoverResult {
        RunnerService::discover_probe(self, request).await
    }
}

impl Observe for RunnerService {
    async fn observe(&self, request: ObserveRequest) -> Observed {
        RunnerService::observe(self, request).await
    }
}
