#[cfg(target_os = "macos")]
use std::path::PathBuf;

#[cfg(target_os = "macos")]
use seatbelt_exec::{SeatbeltPlan, make_command};

fn main() {
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("seatbelt-exec: this binary only works on macOS");
        std::process::exit(1);
    }

    #[cfg(target_os = "macos")]
    {
        if let Err(err) = run() {
            eprintln!("seatbelt-exec error: {err}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "macos")]
fn run() -> Result<(), String> {
    let args = parse_args()?;
    let mut command = make_command(args.program, &args.program_args, args.plan)
        .map_err(|err| format!("failed to canonicalize sandbox roots: {err}"))?;
    let status = command
        .status()
        .map_err(|err| format!("failed to run sandbox-exec: {err}"))?;
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(target_os = "macos")]
struct Args {
    plan: SeatbeltPlan,
    program: PathBuf,
    program_args: Vec<String>,
}

#[cfg(target_os = "macos")]
fn parse_args() -> Result<Args, String> {
    let mut plan = SeatbeltPlan::new();
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
            "--toolchain-root" => {
                let path = args
                    .next()
                    .ok_or("--toolchain-root requires a path argument")?;
                plan = plan.toolchain_root(path);
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
