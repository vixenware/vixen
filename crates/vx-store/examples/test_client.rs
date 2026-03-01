//! Simple test client to verify vx-store TCP connectivity

use eyre::Result;
use vx_store_proto::{ChunkHash, StoreClient};

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

    let endpoint = vx_io::net::Endpoint::parse(addr)?;
    let (root_caller, session_handle) = vx_io::net::connect_session(&endpoint).await?;
    let caller = vx_io::boot::boot_and_connect(&root_caller, &session_handle).await?;
    tracing::info!("Connected!");

    // Create CAS client
    let client = StoreClient::new(caller.clone());

    // Test: put a chunk
    tracing::info!("Testing put_chunk...");
    let test_data = b"Hello from vx-store test client!";
    let chunk_hash = ChunkHash(vx_store_proto::Blake3Hash::from_bytes(test_data));
    client
        .put_chunk(chunk_hash, test_data.to_vec())
        .await
        .map_err(|e| eyre::eyre!("put_chunk failed: {e:?}"))?;
    tracing::info!("Put chunk: {}", chunk_hash.to_hex());

    // Test: get the chunk back
    tracing::info!("Testing get_chunk...");
    let retrieved = client
        .get_chunk(chunk_hash)
        .await
        .map(|resp| resp.ret.clone())
        .map_err(|e| eyre::eyre!("get_chunk failed: {e:?}"))?;
    match retrieved {
        Some(data) => {
            assert_eq!(data.as_slice(), test_data);
            tracing::info!("Retrieved chunk: {}", String::from_utf8_lossy(&data));
        }
        None => {
            tracing::error!("Failed to retrieve chunk!");
            return Err(eyre::eyre!("Chunk not found"));
        }
    }

    // Test: has_chunk
    tracing::info!("Testing has_chunk...");
    let exists = client
        .has_chunk(chunk_hash)
        .await
        .map(|resp| resp.ret)
        .map_err(|e| eyre::eyre!("has_chunk failed: {e:?}"))?;
    tracing::info!("Chunk exists: {}", exists);

    // Close connection by dropping the caller
    drop(caller);
    tracing::info!("Done!");

    Ok(())
}
