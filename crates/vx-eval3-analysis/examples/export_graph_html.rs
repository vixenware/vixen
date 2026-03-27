use std::{env, path::PathBuf, process};

use vx_eval3_analysis::export_graph_html_from_file;

fn main() {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() || args.len() > 2 {
        eprintln!(
            "usage: cargo run -p vx-eval3-analysis --example export_graph_html -- <input.vx> [output.html]"
        );
        process::exit(2);
    }

    let input = PathBuf::from(args.remove(0));
    let output = if let Some(path) = args.first() {
        PathBuf::from(path)
    } else {
        let stem = input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("graph");
        input.with_file_name(format!("{stem}.analysis-graph.html"))
    };

    if let Err(error) = export_graph_html_from_file(&input, &output) {
        eprintln!(
            "failed to export structural analysis graph for {}: {error}",
            input.display()
        );
        process::exit(1);
    }

    println!("{}", output.display());
}
