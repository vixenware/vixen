use std::time::Duration;

use vox::{MetadataEntry, NoopClient};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let socket_path = args.next().ok_or(
        "usage: cargo run -p vx-vfs --example vox_attach_probe -- /tmp/vox-attach-probe.sock",
    )?;
    let addr = format!("local://{socket_path}");

    let client = vox::connect::<NoopClient>(&addr)
        .metadata(vec![MetadataEntry::str("probe", "rust")])
        .connect_timeout(Duration::from_secs(5))
        .establish()
        .await?;

    println!("connected {addr}");
    client.caller.closed().await;
    println!("closed");
    Ok(())
}
