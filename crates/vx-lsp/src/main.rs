use eyre::Result;
use tower_lsp::{LspService, Server};

mod server;

fn main() -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(server::VxLanguageServer::new);
    Server::new(stdin, stdout, socket).serve(service).await;

    Ok(())
}
