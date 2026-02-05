//! Simple test client to verify vx-cass TCP connectivity

use eyre::Result;
use roam_stream::{CobsFramed, HandshakeConfig, NoDispatcher, initiate_framed};
use tokio::net::TcpStream;
use vx_cass_proto::CassClient;

fn main() -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("test_client=debug")
        .init();

    let addr = "127.0.0.1:9002";
    tracing::info!("Connecting to CAS at {}...", addr);

    // Connect to server
    let stream = TcpStream::connect(addr).await?;
    tracing::info!("Connected!");

    let framed = CobsFramed::new(stream);
    let (handle, _incoming, driver) =
        initiate_framed(framed, HandshakeConfig::default(), NoDispatcher).await?;

    tokio::spawn(async move {
        if let Err(e) = driver.run().await {
            tracing::error!("Session error: {}", e);
        }
    });

    // Create CAS client
    let client = CassClient::new(handle.clone());

    // Test: put a blob
    tracing::info!("Testing put_blob...");
    let test_data = b"Hello from vx-cass test client!";
    let blob_hash = client.put_blob(test_data.to_vec()).await?;
    tracing::info!("Put blob: {}", blob_hash.to_hex());

    // Test: get the blob back
    tracing::info!("Testing get_blob...");
    let retrieved = client.get_blob(blob_hash).await?;
    match retrieved {
        Some(data) => {
            assert_eq!(data, test_data);
            tracing::info!("Retrieved blob: {}", String::from_utf8_lossy(&data));
        }
        None => {
            tracing::error!("Failed to retrieve blob!");
            return Err(eyre::eyre!("Blob not found"));
        }
    }

    // Test: has_blob
    tracing::info!("Testing has_blob...");
    let exists = client.has_blob(blob_hash).await?;
    tracing::info!("Blob exists: {}", exists);

    // Close connection by dropping the handle
    drop(handle);
    tracing::info!("Done!");

    Ok(())
}
