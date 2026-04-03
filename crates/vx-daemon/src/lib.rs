//! vx-daemon - as a library, launched by `vx` as `vx daemon`,
//! its own subprocess (so it's long-lived).
//!
//! Hosts a configurable subset of internal RPC services.

use camino::Utf8PathBuf;
use eyre::{Result, bail};
use vox::{ConnectionAcceptor, Metadata};
use vx_runner::RunnerService;
use vx_services::{DaemonDispatcher, OrchestratorDispatcher, RunnerDispatcher, StoreDispatcher};
use vx_store::StoreService;
use vx_vfsd::{VfsdDispatcher, VfsdService};

mod orchestrator;
mod service;
pub use orchestrator::OrchestratorService;
pub use service::DaemonService;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceRef {
    InProcess,
    Rpc { address: String },
}

#[derive(Debug, Clone)]
pub struct StoreConfig {
    pub root: Utf8PathBuf,
    pub upstream: Option<ServiceRef>,
}

#[derive(Debug, Clone, Default)]
pub struct OrchestratorConfig;

#[derive(Debug, Clone, Default)]
pub struct RunnerConfig {
    pub store: Option<ServiceRef>,
}

#[derive(Debug, Clone)]
pub struct VfsdConfig {
    pub mount: Utf8PathBuf,
    pub store: Option<ServiceRef>,
}

#[derive(Debug, Clone, Default)]
pub struct HostedServices {
    pub orchestrator: Option<OrchestratorConfig>,
    pub store: Option<StoreConfig>,
    pub runner: Option<RunnerConfig>,
    pub vfsd: Option<VfsdConfig>,
}

pub struct DaemonConfig {
    /// All addresses to listen on.
    pub listen: Vec<String>,
    /// Services hosted by this daemon process.
    pub services: HostedServices,
}

impl DaemonConfig {
    pub fn local_all_in_one(
        listen: Vec<String>,
        store_root: Utf8PathBuf,
        vfs_mount: Utf8PathBuf,
    ) -> Self {
        Self {
            listen,
            services: HostedServices {
                orchestrator: Some(OrchestratorConfig),
                store: Some(StoreConfig {
                    root: store_root,
                    upstream: None,
                }),
                runner: Some(RunnerConfig::default()),
                vfsd: Some(VfsdConfig {
                    mount: vfs_mount,
                    store: None,
                }),
            },
        }
    }
}

/// Serve the VXD service on all configured listen addresses.
pub async fn serve(config: DaemonConfig) -> Result<()> {
    if config.listen.is_empty() {
        return Err(eyre::eyre!("no listen addresses configured"));
    }

    let local_addr = config.listen[0].clone();
    let services = config.services;
    let service = DaemonService::new();
    let orchestrator_service = services
        .orchestrator
        .as_ref()
        .map(|_| OrchestratorService::new());

    let store_service = if let Some(store_cfg) = services.store.clone() {
        let store_service = vx_store::open(store_cfg.root).await?;
        if let Some(upstream_addr) = resolve_optional_dependency(
            store_cfg.upstream.as_ref(),
            false,
            &local_addr,
            "store.upstream",
        )? {
            let upstream = std::sync::Arc::new(vox::connect(&upstream_addr).await?);
            store_service.set_upstream(upstream);
        }
        Some(store_service)
    } else {
        None
    };

    let runner_service = if let Some(runner_cfg) = services.runner.clone() {
        let store_addr = resolve_required_dependency(
            runner_cfg.store.as_ref(),
            store_service.is_some(),
            &local_addr,
            "runner.store",
        )?;
        let store = std::sync::Arc::new(vox::connect(&store_addr).await?);
        Some(RunnerService::new(Some(store)))
    } else {
        None
    };

    let vfsd_service = if let Some(vfsd_cfg) = services.vfsd.clone() {
        let store_addr = resolve_required_dependency(
            vfsd_cfg.store.as_ref(),
            store_service.is_some(),
            &local_addr,
            "vfsd.store",
        )?;
        let vfs_mount = vx_vfsd::effective_mount_path(&vfsd_cfg.mount);
        Some(vx_vfsd::boot(&store_addr, &vfs_mount).await?)
    } else {
        None
    };

    let acceptor = DaemonAcceptor {
        service,
        orchestrator_service,
        store_service,
        runner_service,
        vfsd_service,
    };

    let mut listeners = tokio::task::JoinSet::new();
    for l in config.listen {
        tracing::info!("listening on {}", l);
        let acceptor = acceptor.clone();
        listeners.spawn(async move { vox::serve(l, acceptor).await });
    }

    while let Some(result) = listeners.join_next().await {
        result??;
    }

    Ok(())
}

fn resolve_optional_dependency(
    configured: Option<&ServiceRef>,
    local_available: bool,
    local_addr: &str,
    label: &str,
) -> Result<Option<String>> {
    match configured {
        Some(ServiceRef::InProcess) => {
            if local_available {
                Ok(Some(local_addr.to_owned()))
            } else {
                bail!("{label} is configured as @in_process, but no local service is hosted")
            }
        }
        Some(ServiceRef::Rpc { address }) => Ok(Some(address.clone())),
        None => {
            if local_available {
                Ok(Some(local_addr.to_owned()))
            } else {
                Ok(None)
            }
        }
    }
}

fn resolve_required_dependency(
    configured: Option<&ServiceRef>,
    local_available: bool,
    local_addr: &str,
    label: &str,
) -> Result<String> {
    match resolve_optional_dependency(configured, local_available, local_addr, label)? {
        Some(addr) => Ok(addr),
        None => bail!(
            "{label} is required, but no local service is hosted and no remote dependency was configured"
        ),
    }
}

/// A connection acceptor that can serve multiple sub-services:
/// store, runner, vfs, etc.
#[derive(Clone)]
struct DaemonAcceptor {
    service: DaemonService,
    orchestrator_service: Option<OrchestratorService>,
    store_service: Option<StoreService>,
    runner_service: Option<RunnerService>,
    vfsd_service: Option<VfsdService>,
}

impl ConnectionAcceptor for DaemonAcceptor {
    fn accept(
        &self,
        request: &vox::ConnectionRequest,
        connection: vox::PendingConnection,
    ) -> std::result::Result<(), Metadata<'static>> {
        tracing::info!("daemon got request for service: {}", request.service());
        match request.service() {
            "Daemon" => connection.handle_with(DaemonDispatcher::new(self.service.clone())),
            "Orchestrator" => {
                let Some(service) = &self.orchestrator_service else {
                    tracing::warn!("orchestrator service requested but not hosted");
                    return Err(vec![]);
                };
                connection.handle_with(OrchestratorDispatcher::new(service.clone()));
            }
            "Store" => {
                let Some(service) = &self.store_service else {
                    tracing::warn!("store service requested but not hosted");
                    return Err(vec![]);
                };
                connection.handle_with(StoreDispatcher::new(service.clone()));
            }
            "Runner" => {
                let Some(service) = &self.runner_service else {
                    tracing::warn!("runner service requested but not hosted");
                    return Err(vec![]);
                };
                connection.handle_with(RunnerDispatcher::new(service.clone()));
            }
            "Vfsd" => {
                let Some(service) = &self.vfsd_service else {
                    tracing::warn!("vfsd service requested but not hosted");
                    return Err(vec![]);
                };
                connection.handle_with(VfsdDispatcher::new(service.clone()));
            }
            other => {
                tracing::warn!("unknown service requested: {}", other);
                return Err(vec![]);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::TempDir;
    use tokio::time::sleep;
    use vx_services::{
        EvalStrictFunctionRequest, EvalStrictFunctionResult, OrchestratorClient, StrictEvalBackend,
        StrictValue,
    };

    use super::{
        DaemonConfig, HostedServices, OrchestratorConfig, ServiceRef, resolve_optional_dependency,
        resolve_required_dependency,
    };

    #[test]
    fn omitted_dependency_defaults_to_local_when_available() {
        let got = resolve_required_dependency(None, true, "local:///tmp/vixen.vox", "runner.store")
            .expect("local dependency should resolve");
        assert_eq!(got, "local:///tmp/vixen.vox");
    }

    #[test]
    fn omitted_dependency_errors_when_required_and_not_available() {
        let err =
            resolve_required_dependency(None, false, "local:///tmp/vixen.vox", "runner.store")
                .expect_err("missing required dependency should fail");
        assert!(err.to_string().contains("runner.store is required"));
    }

    #[test]
    fn explicit_rpc_dependency_wins() {
        let got = resolve_required_dependency(
            Some(&ServiceRef::Rpc {
                address: "tcp://store-a:5000".to_owned(),
            }),
            true,
            "local:///tmp/vixen.vox",
            "runner.store",
        )
        .expect("rpc dependency should resolve");
        assert_eq!(got, "tcp://store-a:5000");
    }

    #[test]
    fn optional_dependency_defaults_to_none_when_not_available() {
        let got =
            resolve_optional_dependency(None, false, "local:///tmp/vixen.vox", "store.upstream")
                .expect("optional dependency should resolve");
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn orchestrator_round_trips_over_local_rpc() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let socket_path = temp_dir.path().join("vx-daemon.sock");
        let listen = format!("local://{}", socket_path.display());

        let daemon = tokio::spawn(super::serve(DaemonConfig {
            listen: vec![listen.clone()],
            services: HostedServices {
                orchestrator: Some(OrchestratorConfig),
                ..HostedServices::default()
            },
        }));

        let client = connect_orchestrator(&listen).await;
        let result = client
            .eval_strict_function(EvalStrictFunctionRequest {
                source_text: r#"
fn add(lhs: U64, rhs: U64) -> U64 (
  lhs + rhs
)
"#
                .to_owned(),
                function_name: "add".to_owned(),
                args: vec![StrictValue::U64(30), StrictValue::U64(12)],
                backend: StrictEvalBackend::Kajit,
                include_kernel_dump: false,
                include_hir: false,
                include_ir: false,
            })
            .await
            .expect("rpc call should succeed");

        daemon.abort();
        let _ = daemon.await;

        match result {
            EvalStrictFunctionResult::Ok(success) => {
                assert_eq!(success.value, StrictValue::U64(42));
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    async fn connect_orchestrator(address: &str) -> OrchestratorClient {
        let mut last_error = None;
        for _ in 0..50 {
            match vox::connect(address).await {
                Ok(client) => return client,
                Err(error) => {
                    last_error = Some(error);
                    sleep(Duration::from_millis(20)).await;
                }
            }
        }

        panic!(
            "failed to connect orchestrator client to {address}: {}",
            last_error.expect("retry loop should record at least one connection error")
        );
    }
}
