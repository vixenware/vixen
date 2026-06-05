//! vx-daemon - as a library, launched by `vx` as `vx daemon`,
//! its own subprocess (so it's long-lived).
//!
//! Hosts a configurable subset of internal RPC services.

use camino::Utf8PathBuf;
use eyre::{Result, bail};
use facet::Facet;
use tokio::sync::watch;
use vox::{ConnectionAcceptor, Metadata, TransportMode};
use vx_runner::RunnerService;
use vx_runtime::observe::ObserveDispatcher;
use vx_services::{
    DAEMON_SOCK, DaemonDispatcher, RunnerDispatcher, StoreDispatcher, TestRunnerDispatcher,
};
use vx_store::StoreService;
use vx_vfsd::{VfsdDispatcher, VfsdService};

mod service;
mod test_runner;
pub use service::DaemonService;
pub use test_runner::TestRunnerService;

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
pub struct OrchestratorConfig {
    pub store: Option<ServiceRef>,
    pub runners: Vec<ServiceRef>,
    pub vfsd: Option<ServiceRef>,
}

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
                orchestrator: Some(OrchestratorConfig::default()),
                store: Some(StoreConfig {
                    root: store_root,
                    upstream: None,
                }),
                runner: Some(RunnerConfig::default()),
                vfsd: default_local_vfsd(vfs_mount),
            },
        }
    }
}

fn default_local_vfsd(vfs_mount: Utf8PathBuf) -> Option<VfsdConfig> {
    Some(VfsdConfig {
        mount: vfs_mount,
        store: None,
    })
}

#[derive(Debug, Clone, Facet)]
pub struct DaemonFile {
    pub daemon: DaemonFileConfig,
}

#[derive(Debug, Clone, Facet)]
pub struct DaemonFileConfig {
    pub listen: Option<Vec<String>>,
    pub services: HostedServicesFile,
}

#[derive(Debug, Clone, Default, Facet)]
pub struct HostedServicesFile {
    pub orchestrator: Option<OrchestratorFileConfig>,
    pub store: Option<StoreFileConfig>,
    pub runner: Option<RunnerFileConfig>,
    pub vfsd: Option<VfsdFileConfig>,
}

#[derive(Debug, Clone, Default, Facet)]
pub struct OrchestratorFileConfig {
    pub store: Option<ServiceRefFile>,
    pub runners: Option<Vec<ServiceRefFile>>,
    pub vfsd: Option<ServiceRefFile>,
}

#[derive(Debug, Clone, Facet)]
pub struct StoreFileConfig {
    pub root: Utf8PathBuf,
    pub upstream: Option<ServiceRefFile>,
}

#[derive(Debug, Clone, Default, Facet)]
pub struct RunnerFileConfig {
    pub store: Option<ServiceRefFile>,
}

#[derive(Debug, Clone, Facet)]
pub struct VfsdFileConfig {
    pub mount: Utf8PathBuf,
    pub store: Option<ServiceRefFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[facet(rename_all = "snake_case")]
#[repr(u8)]
pub enum ServiceRefFile {
    InProcess,
    Rpc { address: String },
}

impl From<ServiceRefFile> for ServiceRef {
    fn from(value: ServiceRefFile) -> Self {
        match value {
            ServiceRefFile::InProcess => ServiceRef::InProcess,
            ServiceRefFile::Rpc { address } => ServiceRef::Rpc { address },
        }
    }
}

impl From<HostedServicesFile> for HostedServices {
    fn from(value: HostedServicesFile) -> Self {
        Self {
            orchestrator: value.orchestrator.map(|orchestrator| OrchestratorConfig {
                store: orchestrator.store.map(Into::into),
                runners: orchestrator
                    .runners
                    .unwrap_or_default()
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                vfsd: orchestrator.vfsd.map(Into::into),
            }),
            store: value.store.map(|store| StoreConfig {
                root: store.root,
                upstream: store.upstream.map(Into::into),
            }),
            runner: value.runner.map(|runner| RunnerConfig {
                store: runner.store.map(Into::into),
            }),
            vfsd: value.vfsd.map(|vfsd| VfsdConfig {
                mount: vfsd.mount,
                store: vfsd.store.map(Into::into),
            }),
        }
    }
}

impl From<DaemonFile> for DaemonConfig {
    fn from(value: DaemonFile) -> Self {
        Self {
            listen: value
                .daemon
                .listen
                .unwrap_or_else(|| vec![DAEMON_SOCK.to_owned()]),
            services: value.daemon.services.into(),
        }
    }
}

pub fn load_config_file(path: &Utf8PathBuf) -> Result<DaemonConfig> {
    let text = std::fs::read_to_string(path)?;
    let parsed: DaemonFile = facet_styx::from_str(&text)
        .map_err(|error| eyre::eyre!("failed to parse daemon config `{path}`: {error}"))?;
    Ok(parsed.into())
}

/// Serve the VXD service on all configured listen addresses.
pub async fn serve(config: DaemonConfig) -> Result<()> {
    if config.listen.is_empty() {
        return Err(eyre::eyre!("no listen addresses configured"));
    }

    let listen_addrs = config.listen.clone();
    let services = config.services;

    let store_service = if let Some(store_cfg) = services.store.clone() {
        let store_service = vx_store::open(store_cfg.root).await?;
        if let Some(upstream_ref) = store_cfg.upstream.as_ref() {
            let upstream = match upstream_ref {
                ServiceRef::InProcess => {
                    bail!("store.upstream cannot be @in_process; a store cannot upstream to itself")
                }
                ServiceRef::Rpc { address } => std::sync::Arc::new(vox::connect(address).await?),
            };
            store_service.set_upstream(upstream);
        }
        Some(store_service)
    } else {
        None
    };

    // Boot the CAS-VFS first so the runner can mount actions into it (the in-process
    // executor drives its local VFS directly — see `RunnerService::with_vfsd`).
    let vfsd_service = if let Some(vfsd_cfg) = services.vfsd.clone() {
        let store = std::sync::Arc::new(
            resolve_required_store_client(
                vfsd_cfg.store.as_ref(),
                store_service.as_ref(),
                "vfsd.store",
            )
            .await?,
        );
        let vfs_mount = vx_vfsd::effective_mount_path(&vfsd_cfg.mount);
        Some(vx_vfsd::boot_with_store(store, &vfs_mount).await?)
    } else {
        None
    };

    let runner_service = if let Some(runner_cfg) = services.runner.clone() {
        let store = std::sync::Arc::new(
            resolve_required_store_client(
                runner_cfg.store.as_ref(),
                store_service.as_ref(),
                "runner.store",
            )
            .await?,
        );
        let mut runner = RunnerService::new(Some(store));
        if let Some(vfsd) = &vfsd_service {
            runner = runner.with_vfsd(vfsd.clone());
        }
        Some(runner)
    } else {
        None
    };

    // Eval is gone — the eager runtime is deleted. The `orchestrator` config slot
    // now gates only the test surface, which runs on vx-runtime and needs no
    // store/runner/vfsd wiring of its own.
    let test_runner_service = services.orchestrator.is_some().then(TestRunnerService::new);
    let mut hosted_services = vec!["Daemon".to_owned()];
    if test_runner_service.is_some() {
        hosted_services.push("TestRunner".to_owned());
    }
    if store_service.is_some() {
        hosted_services.push("Store".to_owned());
    }
    if runner_service.is_some() {
        hosted_services.push("Runner".to_owned());
        hosted_services.push("Observe".to_owned());
        hosted_services.push("Producing".to_owned());
    }
    if vfsd_service.is_some() {
        hosted_services.push("Vfsd".to_owned());
    }
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let service = DaemonService::new(listen_addrs, hosted_services, shutdown_tx);

    let acceptor = DaemonAcceptor {
        service,
        test_runner_service,
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

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    listeners.abort_all();
                    while let Some(result) = listeners.join_next().await {
                        match result {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => return Err(error.into()),
                            Err(join_error) if join_error.is_cancelled() => {}
                            Err(join_error) => return Err(join_error.into()),
                        }
                    }
                    return Ok(());
                }
            }
            maybe_result = listeners.join_next() => {
                match maybe_result {
                    Some(result) => result??,
                    None => return Ok(()),
                }
            }
        }
    }
}

async fn resolve_required_store_client(
    configured: Option<&ServiceRef>,
    local_store: Option<&StoreService>,
    label: &str,
) -> Result<vx_services::StoreClient> {
    match configured {
        Some(ServiceRef::InProcess) => {
            let Some(local_store) = local_store else {
                bail!("{label} is configured as @in_process, but no local service is hosted");
            };
            connect_in_process_store(local_store.clone()).await
        }
        Some(ServiceRef::Rpc { address }) => Ok(vox::connect(address).await?),
        None => {
            if let Some(local_store) = local_store {
                connect_in_process_store(local_store.clone()).await
            } else {
                bail!(
                    "{label} is required, but no local service is hosted and no remote dependency was configured"
                )
            }
        }
    }
}

async fn connect_in_process_store(service: StoreService) -> Result<vx_services::StoreClient> {
    let (client_link, server_link) = vox::memory_link_pair(64);
    tokio::spawn(async move {
        let acceptor = vox::acceptor_fn(
            move |request: &vox::ConnectionRequest, connection: vox::PendingConnection| {
                match request.service() {
                    "Store" => {
                        connection.handle_with(StoreDispatcher::new(service.clone()));
                        Ok(())
                    }
                    _ => Err(vec![]),
                }
            },
        );

        match vox::acceptor_on(server_link)
            .on_connection(acceptor)
            .establish::<vox::NoopClient>()
            .await
        {
            Ok(client) => {
                client.caller.closed().await;
            }
            Err(error) => {
                tracing::warn!(?error, "in-process store acceptor exited");
            }
        }
    });

    Ok(vox::initiator_on(client_link, TransportMode::Bare)
        .establish::<vx_services::StoreClient>()
        .await?)
}

/// A connection acceptor that can serve multiple sub-services:
/// store, runner, vfs, etc.
#[derive(Clone)]
struct DaemonAcceptor {
    service: DaemonService,
    test_runner_service: Option<TestRunnerService>,
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
            "TestRunner" => {
                let Some(service) = &self.test_runner_service else {
                    tracing::warn!("test runner service requested but not hosted");
                    return Err(vec![]);
                };
                connection.handle_with(TestRunnerDispatcher::new(service.clone()));
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
            "Observe" => {
                let Some(service) = &self.runner_service else {
                    tracing::warn!("observe service requested but runner is not hosted");
                    return Err(vec![]);
                };
                connection.handle_with(ObserveDispatcher::new(service.clone()));
            }
            "Producing" => {
                let Some(service) = &self.runner_service else {
                    tracing::warn!("producing service requested but runner is not hosted");
                    return Err(vec![]);
                };
                service.handle_producing_connection(connection)?;
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
