//! Landlock sandbox wrapper for compiler subprocesses (Linux).
//!
//! Usage:
//!
//!     landlock-exec --read-root <PATH> --write-root <PATH> --exec-root <PATH> -- <program> [args...]

#[cfg(all(target_pointer_width = "64", target_os = "linux"))]
#[global_allocator]
static GLOBAL: hardened_malloc::HardenedMalloc = hardened_malloc::HardenedMalloc;

#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use landlock_exec::{LandlockPlan, exec_under_sandbox};

fn main() {
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("landlock-exec: this binary only works on Linux");
        std::process::exit(1);
    }

    #[cfg(target_os = "linux")]
    if let Err(e) = secmem_proc::harden_process() {
        eprintln!("landlock-exec error: could not harden process: {e}");
        std::process::exit(1);
    }

    #[cfg(target_os = "linux")]
    {
        if let Err(e) = run() {
            eprintln!("landlock-exec error: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "linux")]
fn run() -> Result<(), String> {
    let args = parse_args()?;
    let profile = args
        .plan
        .canonicalize_existing()
        .map_err(|err| format!("failed to canonicalize sandbox roots: {err}"))?
        .compile();
    exec_under_sandbox(&profile, args.program, &args.program_args)
}

#[cfg(target_os = "linux")]
struct Args {
    plan: LandlockPlan,
    program: PathBuf,
    program_args: Vec<String>,
}

#[cfg(target_os = "linux")]
fn parse_args() -> Result<Args, String> {
    let mut plan = LandlockPlan::new();
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--read-root" => {
                let path = args.next().ok_or("--read-root requires a path argument")?;
                plan = plan.read_root(path);
            }
            "--write-root" => {
                let path = args.next().ok_or("--write-root requires a path argument")?;
                plan = plan.write_root(path);
            }
            "--exec-root" | "--toolchain-root" => {
                let path = args.next().ok_or("--exec-root requires a path argument")?;
                plan = plan.exec_root(path);
            }
            "--" => {
                let program = args.next().ok_or("no program specified after --")?.into();
                let program_args = args.collect();
                return Ok(Args {
                    plan,
                    program,
                    program_args,
                });
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Err("missing -- before program".to_string())
}
