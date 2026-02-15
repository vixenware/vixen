use camino::Utf8PathBuf;

use crate::Result;

/// Sentinel mode: watch parent process, unmount when it dies.
pub async fn run_sentinel(mount_path: &Utf8PathBuf) -> Result<()> {
    tracing::info!(?mount_path, "running in sentinel mode");

    let parent_pid = unsafe { libc::getppid() };
    tracing::info!(parent_pid, "watching parent process");

    // Poll parent with kill(pid, 0) until it dies
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        let alive = unsafe { libc::kill(parent_pid, 0) == 0 };
        if !alive {
            tracing::info!(parent_pid, "parent died, unmounting");
            break;
        }
    }

    // Unmount using the platform-specific command
    super::os::unmount(mount_path);

    Ok(())
}
