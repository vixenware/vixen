use eyre::Result;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--stdio") => vx_bootstrap::serve_stdio(),
        Some(other) => {
            eyre::bail!("unknown argument: {other}");
        }
        None => {
            eyre::bail!("usage: vx-bootstrap --stdio");
        }
    }
}
