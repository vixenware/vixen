use std::sync::Arc;

use camino::Utf8PathBuf;

use crate::Result;
use crate::vfs::CasVfs;

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

/// Register SIGUSR1 handler that dumps diagnostics.
pub fn setup_diagnostics_signal(vfs: Arc<CasVfs>, mount_path: Utf8PathBuf) {
    peeps::tasks::spawn_tracked("sigusr1_handler", async move {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigusr1 =
            signal(SignalKind::user_defined1()).expect("failed to register SIGUSR1 handler");
        loop {
            sigusr1.recv().await;
            tracing::info!("SIGUSR1 received, dumping diagnostics");
            super::os::dump_platform_diagnostics(&mount_path).await;
            vfs.dump_all_state();
            eprint!("{}", roam_session::diagnostic::dump_all_diagnostics());
            #[cfg(target_os = "macos")]
            {
                eprintln!("\n=== SHM STATE ===");
                eprintln!("{}", roam_shm::dump_all_state());
            }
        }
    });
}
