// FSKit is a state machine from hell.

#[cfg(not(target_os = "macos"))]
compile_error!("vx-fskit is macOS-only");
