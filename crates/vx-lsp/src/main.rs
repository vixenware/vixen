#[tokio::main]
pub async fn main() -> eyre::Result<()> {
    vx_lsp::serve().await
}
