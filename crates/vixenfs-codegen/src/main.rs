use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .ok_or("failed to locate workspace root")?
        .to_path_buf();

    let service = vixenfs_proto::vfs_service_detail();
    let swift = roam_codegen::targets::swift::generate_service_with_bindings(
        &service,
        roam_codegen::targets::swift::SwiftBindings::Client,
    );

    let output = workspace_root.join("apps/mac/xcode/VixenFSExtension/VfsClient.swift");

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&output, &swift)?;
    println!("Wrote {}", output.display());

    Ok(())
}
