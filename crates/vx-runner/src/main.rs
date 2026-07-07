use eyre::Result;
use vx_exec::SandboxedExecutor;
use vx_services::RunnerDispatcher;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://127.0.0.1:4178".to_string());

    tracing::info!(%addr, "starting vx-runner");
    vox::serve(addr, RunnerDispatcher::new(SandboxedExecutor::new()))
        .metadata(vox::metadata().str("vx-service", "runner").build())
        .await?;
    Ok(())
}
