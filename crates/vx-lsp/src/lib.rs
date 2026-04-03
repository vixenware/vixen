use eyre::Result;
use tower_lsp::{LspService, Server};

mod server;

pub async fn serve() -> Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(server::VxLanguageServer::new);
    Server::new(stdin, stdout, socket).serve(service).await;

    Ok(())
}
