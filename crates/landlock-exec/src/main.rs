//! Landlock sandbox wrapper for compiler subprocesses (Linux).
//!
//! This binary applies landlock restrictions and then execs the target program.
//!
//! Usage:
//!
//!     landlock-exec --vfs-root <PATH> --write-dir <PATH> -- <program> [args...]
//!
//! Security model:
//! - File reads: restricted to VFS_ROOT + system paths (/usr, /lib, /etc for toolchain)
//! - File writes: restricted to WRITE_DIR + /tmp (rustc temp files, ignores TMPDIR)
//! - Network: denied entirely (no BindTcp/ConnectTcp rules added)
//! - Execution: allowed from VFS_ROOT + system paths

#[cfg(target_os = "linux")]
mod sandbox;

fn main() {
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("landlock-exec: this binary only works on Linux");
        std::process::exit(1);
    }
    // Harden process before ANYTHING else
    #[cfg(target_os = "linux")]
    if let Err(e) = secmem_proc::harden_process() {
        println!("ERROR: could not harden process, exiting");
        println!("ERROR: {}", e);
        return;
    }

    #[cfg(target_os = "linux")]
    {
        if let Err(e) = sandbox::run() {
            eprintln!("landlock-exec error: {e}");
            std::process::exit(1);
        }
    }
}
