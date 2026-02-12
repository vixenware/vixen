use camino::Utf8PathBuf;
use eyre::Result;
use owo_colors::OwoColorize;
use vx_report::{CacheOutcome, ReportStore, RunDiff};

use crate::ExplainArgs;

/// Options for displaying the explain report (internal)
#[derive(Debug, Clone, Copy)]
struct ReportDisplayOptions {
    /// Show all nodes including cache hits
    show_all: bool,

    /// Show verbose output (full hashes, unchanged deps)
    verbose: bool,

    /// Show only fanout analysis
    fanout_only: bool,
}

/// Display a hash in short form (first 8 hex chars) unless verbose
fn short_hex(hash: &str, verbose: bool) -> String {
    if verbose || hash.len() <= 8 {
        hash.to_string()
    } else {
        format!("{}…", &hash[..8])
    }
}

pub fn cmd_explain(args: ExplainArgs) -> Result<()> {
    let cwd = Utf8PathBuf::try_from(std::env::current_dir()?)?;
    let store = ReportStore::new(&cwd);

    // Load the latest report
    let Some(report) = store.load_latest()? else {
        println!(
            "{} No build reports found. Run {} first.",
            "Error:".red().bold(),
            "vx build".cyan()
        );
        return Ok(());
    };

    // Handle --web: open interactive graph in browser
    if args.web {
        return cmd_explain_web(&report);
    }

    // Handle --diff: compare last two builds
    if args.diff {
        let Some(prev_report) = store.load_previous()? else {
            println!(
                "{} Need at least two builds to show diff. Run {} again.",
                "Error:".red().bold(),
                "vx build".cyan()
            );
            return Ok(());
        };
        return cmd_explain_diff(&prev_report, &report);
    }

    // Handle --last-miss: show details of the last cache miss
    if args.last_miss {
        return cmd_explain_last_miss(&report);
    }

    // Load previous report for diffing deps
    let prev_report = store.load_previous().ok().flatten();

    if args.json {
        cmd_explain_json(&report, prev_report.as_ref(), args.node.clone())
    } else {
        let options = ReportDisplayOptions {
            show_all: args.all,
            verbose: args.verbose,
            fanout_only: args.fanout,
        };
        cmd_explain_report(&report, prev_report.as_ref(), args.node.clone(), options)
    }
}

fn cmd_explain_report(
    report: &vx_report::BuildReport,
    prev_report: Option<&vx_report::BuildReport>,
    node_filter: Option<String>,
    options: ReportDisplayOptions,
) -> Result<()> {
    use std::collections::HashMap;

    // =========================================================================
    // 1. RUN HEADER
    // =========================================================================
    let status = if report.success {
        "SUCCESS".green().bold().to_string()
    } else {
        "FAILED".red().bold().to_string()
    };

    let duration_s = report.duration_ms() as f64 / 1000.0;
    println!(
        "{} {} in {:.2}s",
        "Build:".bold(),
        status,
        duration_s.to_string().dimmed()
    );

    println!("  {} {}", "Workspace:".dimmed(), report.workspace_root);
    println!("  {} {}", "Profile:".dimmed(), report.profile);
    println!("  {} {}", "Target:".dimmed(), report.target_triple);

    // Show toolchains
    if let Some(ref rust_tc) = report.toolchains.rust {
        let version_str = rust_tc.version.as_deref().unwrap_or("unknown");
        let id_short = short_hex(&rust_tc.id, options.verbose);
        println!("  {} {} ({})", "Rust:".dimmed(), version_str, id_short);
    }
    if let Some(ref zig_tc) = report.toolchains.zig {
        let version_str = zig_tc.version.as_deref().unwrap_or("unknown");
        let id_short = short_hex(&zig_tc.id, options.verbose);
        println!("  {} {} ({})", "Zig:".dimmed(), version_str, id_short);
    }

    println!();

    // =========================================================================
    // 2. NODE LIST
    // =========================================================================

    // Index previous run's nodes by node_id for dep diffing
    let prev_nodes: HashMap<&str, &vx_report::NodeReport> = prev_report
        .map(|r| r.nodes.iter().map(|n| (n.node_id.as_str(), n)).collect())
        .unwrap_or_default();

    // Handle fanout-only mode
    if options.fanout_only {
        print_fanout_analysis(report, options.verbose);
        return Ok(());
    }

    // Separate nodes by outcome
    let mut failures = Vec::new();
    let mut misses = Vec::new();
    let mut hits = Vec::new();

    for node in &report.nodes {
        // Apply node filter if specified
        if let Some(ref filter) = node_filter
            && &node.node_id != filter
        {
            continue;
        }

        match &node.cache {
            CacheOutcome::Miss { .. } => {
                if node
                    .invocation
                    .as_ref()
                    .map(|i| i.exit_code != 0)
                    .unwrap_or(false)
                {
                    failures.push(node);
                } else {
                    misses.push(node);
                }
            }
            CacheOutcome::Hit { .. } => hits.push(node),
        }
    }

    // Print failures first
    if !failures.is_empty() {
        println!("{}", "Failures:".red().bold());
        for node in &failures {
            print_node(node, None, options.verbose);
        }
        println!();
    }

    // Print misses with explanations
    if !misses.is_empty() {
        println!("{}", "=== WHY REBUILT ===".yellow().bold());
        println!();
        for node in &misses {
            let prev_node = prev_nodes.get(node.node_id.as_str()).copied();
            print_node_explanation(node, prev_node, options.verbose);
        }
        println!();
    }

    // Compute and print fanout
    if !misses.is_empty() {
        print_fanout_analysis(report, options.verbose);
    }

    // Summary of hits (unless --all)
    if !options.show_all && !hits.is_empty() {
        println!("{} {} cached", "Cache hits:".green().bold(), hits.len());
    } else if options.show_all && !hits.is_empty() {
        println!("{}", "Cache hits:".green().bold());
        for node in &hits {
            print_node(node, None, options.verbose);
        }
    }

    // Error message if failed
    if let Some(error) = &report.error {
        println!();
        println!("{}", "Error:".red().bold());
        for line in error.lines().take(10) {
            println!("  {}", line);
        }
        if error.lines().count() > 10 {
            println!(
                "  {} ({} more lines)",
                "...".dimmed(),
                error.lines().count() - 10
            );
        }
    }

    Ok(())
}

/// Print a node explanation using Lane 2 analysis
fn print_node_explanation(
    node: &vx_report::NodeReport,
    prev_node: Option<&vx_report::NodeReport>,
    verbose: bool,
) {
    use vx_report::NodeRebuildExplanation;

    // Compute explanation
    let Some(explanation) = NodeRebuildExplanation::compute(node, prev_node) else {
        // Cache hit - shouldn't happen in this context
        return;
    };

    let duration_ms = node.timing.execute_ms;
    let duration_str = if duration_ms >= 1000 {
        format!("{:.2}s", duration_ms as f64 / 1000.0)
    } else {
        format!("{}ms", duration_ms)
    };

    // Print node header with observed reason
    println!(
        "  {} ({}) {}",
        node.node_id.cyan().bold(),
        explanation.observed_reason.to_string().yellow(),
        duration_str.dimmed()
    );

    // Print input changes
    if !explanation.input_changes.is_empty() {
        println!("    {}:", "Inputs".dimmed());
        for change in &explanation.input_changes {
            match (&change.old_value, &change.new_value) {
                (Some(old), Some(new)) => {
                    let old_short = short_hex(old, verbose);
                    let new_short = short_hex(new, verbose);
                    println!(
                        "      {} {} → {}",
                        change.label,
                        old_short.dimmed(),
                        new_short
                    );
                }
                (None, Some(new)) => {
                    let new_short = short_hex(new, verbose);
                    println!("      {} {} (added)", change.label, new_short);
                }
                (Some(old), None) => {
                    let old_short = short_hex(old, verbose);
                    println!("      {} {} (removed)", change.label, old_short.dimmed());
                }
                (None, None) => {}
            }
        }
    }

    // Print dependency changes
    if !explanation.dep_changes.is_empty() {
        println!("    {}:", "Dependencies".dimmed());
        for change in &explanation.dep_changes {
            use vx_report::DepChangeKind;

            let crate_id_short = short_hex(&change.crate_id, verbose);

            match &change.change {
                DepChangeKind::Added {
                    rlib_hash,
                    manifest_hash,
                } => {
                    let rlib_short = short_hex(rlib_hash, verbose);
                    let manifest_short = short_hex(manifest_hash, verbose);
                    println!(
                        "      {} (crate {}): rlib {} (manifest {})",
                        change.extern_name.cyan(),
                        crate_id_short.dimmed(),
                        rlib_short,
                        manifest_short.dimmed()
                    );
                }
                DepChangeKind::Removed {
                    rlib_hash,
                    manifest_hash,
                } => {
                    let rlib_short = short_hex(rlib_hash, verbose);
                    let manifest_short = short_hex(manifest_hash, verbose);
                    println!(
                        "      {} (crate {}): {} (removed)",
                        change.extern_name.cyan(),
                        crate_id_short.dimmed(),
                        format!("rlib {} manifest {}", rlib_short, manifest_short).dimmed()
                    );
                }
                DepChangeKind::RlibChanged {
                    old_rlib,
                    new_rlib,
                    old_manifest,
                    new_manifest,
                } => {
                    let old_rlib_short = short_hex(old_rlib, verbose);
                    let new_rlib_short = short_hex(new_rlib, verbose);
                    let old_manifest_short = short_hex(old_manifest, verbose);
                    let new_manifest_short = short_hex(new_manifest, verbose);

                    print!(
                        "      {} (crate {}): rlib {} → {}",
                        change.extern_name.cyan(),
                        crate_id_short.dimmed(),
                        old_rlib_short.dimmed(),
                        new_rlib_short
                    );
                    if verbose || old_manifest != new_manifest {
                        print!(
                            " (manifest {} → {})",
                            old_manifest_short.dimmed(),
                            new_manifest_short
                        );
                    }
                    println!();
                }
                DepChangeKind::ManifestChanged {
                    rlib_hash,
                    old_manifest,
                    new_manifest,
                } => {
                    let rlib_short = short_hex(rlib_hash, verbose);
                    let old_manifest_short = short_hex(old_manifest, verbose);
                    let new_manifest_short = short_hex(new_manifest, verbose);
                    println!(
                        "      {} (crate {}): rlib {} manifest {} → {}",
                        change.extern_name.cyan(),
                        crate_id_short.dimmed(),
                        rlib_short.dimmed(),
                        old_manifest_short.dimmed(),
                        new_manifest_short
                    );
                }
            }
        }
    }

    // If verbose, also show the reported reason from daemon
    if verbose {
        println!(
            "    {} {}",
            "Reported reason:".dimmed(),
            explanation.reported_reason.to_string().dimmed()
        );
    }

    println!();
}

/// Print the old-style node display (for cache hits and failures)
fn print_node(
    node: &vx_report::NodeReport,
    _prev_node: Option<&vx_report::NodeReport>,
    verbose: bool,
) {
    let outcome_str = match &node.cache {
        CacheOutcome::Hit { .. } => "HIT ".green().to_string(),
        CacheOutcome::Miss { reason } => format!("MISS ({})", reason).yellow().to_string(),
    };

    let duration_ms = node.timing.execute_ms;
    let duration_str = if duration_ms >= 1000 {
        format!("{:.2}s", duration_ms as f64 / 1000.0)
    } else {
        format!("{}ms", duration_ms)
    };

    println!(
        "  {} {:<15} {}",
        node.node_id,
        outcome_str,
        duration_str.dimmed()
    );

    if verbose {
        let cache_key_short = short_hex(&node.cache_key, verbose);
        println!("    cache_key: {}", cache_key_short.dimmed());
    }
}

/// Print fanout analysis showing which rebuilds triggered downstream rebuilds
fn print_fanout_analysis(report: &vx_report::BuildReport, verbose: bool) {
    use vx_report::FanoutAnalysis;

    let fanout = FanoutAnalysis::compute(report);

    // Only print if there's actual fanout
    if fanout.fanout_map.is_empty() {
        return;
    }

    println!("{}", "=== FANOUT ===".cyan().bold());
    println!();

    // Sort crate IDs for stable output
    let mut crate_ids: Vec<_> = fanout.fanout_map.keys().collect();
    crate_ids.sort();

    for crate_id in crate_ids {
        let dependents = fanout.get_dependents(crate_id);
        if dependents.is_empty() {
            continue;
        }

        let crate_id_short = short_hex(crate_id, verbose);
        println!("  crate {}:", crate_id_short.cyan());

        for dependent in dependents {
            println!("    ↳ {}", dependent.dimmed());
        }
        println!();
    }
}

fn cmd_explain_last_miss(report: &vx_report::BuildReport) -> Result<()> {
    // Find the first miss
    let miss = report
        .nodes
        .iter()
        .find(|n| matches!(n.cache, CacheOutcome::Miss { .. }));

    let Some(node) = miss else {
        println!("{} All nodes hit cache!", "Note:".green().bold());
        return Ok(());
    };

    println!("{}", "First cache miss:".bold());
    println!("  {} {}", "Node:".dimmed(), node.node_id);

    if let CacheOutcome::Miss { reason } = &node.cache {
        println!("  {} {}", "Reason:".dimmed(), reason);
    }

    println!("  {} {}", "Cache key:".dimmed(), &node.cache_key[..16]);

    println!();
    println!("{}", "Inputs:".bold());
    for input in &node.inputs {
        let value_display = if input.value.len() > 16 {
            format!("{}...", &input.value[..16])
        } else {
            input.value.clone()
        };
        println!("  {:<30} {}", input.label, value_display.dimmed());
    }

    Ok(())
}

fn cmd_explain_diff(before: &vx_report::BuildReport, after: &vx_report::BuildReport) -> Result<()> {
    let diff = RunDiff::compute(before, after);

    let before_short = if diff.before.0.len() >= 10 {
        &diff.before.0[..10]
    } else {
        &diff.before.0
    };
    let after_short = if diff.after.0.len() >= 10 {
        &diff.after.0[..10]
    } else {
        &diff.after.0
    };
    println!(
        "{} {} {} {}",
        "Comparing:".bold(),
        before_short.dimmed(),
        "→".dimmed(),
        after_short
    );

    // Toolchain changes
    if !diff.toolchain_changes.is_empty() {
        println!();
        println!("{}", "Toolchain changes:".bold());
        for tc in &diff.toolchain_changes {
            println!(
                "  {} {} → {}",
                tc.name,
                tc.old.as_deref().unwrap_or("(none)").dimmed(),
                tc.new.as_deref().unwrap_or("(none)")
            );
        }
    }

    // Nodes that flipped to miss
    if !diff.flipped_to_miss.is_empty() {
        println!();
        println!("{}", "Nodes that flipped hit → miss:".yellow().bold());
        for node_diff in &diff.flipped_to_miss {
            println!("  {}", node_diff.node_id);

            if !node_diff.changed_inputs.is_empty() {
                println!("    {}:", "Changed inputs".dimmed());
                for (label, old, new) in &node_diff.changed_inputs {
                    let old_short = if old.len() > 12 {
                        format!("{}...", &old[..12])
                    } else {
                        old.clone()
                    };
                    let new_short = if new.len() > 12 {
                        format!("{}...", &new[..12])
                    } else {
                        new.clone()
                    };
                    println!(
                        "      {}: {} → {}",
                        label,
                        old_short.dimmed(),
                        new_short.yellow()
                    );
                }
            }
        }
    }

    // Nodes that flipped to hit
    if !diff.flipped_to_hit.is_empty() {
        println!();
        println!("{}", "Nodes that flipped miss → hit:".green().bold());
        for node_id in &diff.flipped_to_hit {
            println!("  {}", node_id);
        }
    }

    // Summary
    if diff.flipped_to_miss.is_empty()
        && diff.flipped_to_hit.is_empty()
        && diff.toolchain_changes.is_empty()
    {
        println!();
        println!("{} No significant changes between runs.", "Note:".dimmed());
    }

    Ok(())
}

// JSON output types for cmd_explain_json
mod explain_json {
    use facet::Facet;
    use std::collections::HashMap;

    #[derive(Facet)]
    pub struct ExplainOutput {
        pub run_id: String,
        pub prev_run_id: Option<String>,
        pub workspace_root: String,
        pub profile: String,
        pub target_triple: String,
        pub success: bool,
        pub duration_ms: u64,
        pub nodes: Vec<NodeJson>,
        pub fanout: HashMap<String, Vec<String>>,
    }

    #[derive(Facet)]
    pub struct NodeJson {
        pub node_id: String,
        pub kind: String,
        pub cache: String,
        pub execute_ms: u64,
        #[facet(default)]
        pub reported_reason: Option<String>,
        #[facet(default)]
        pub observed_reason: Option<String>,
        #[facet(default)]
        pub input_changes: Vec<InputChangeJson>,
        #[facet(default)]
        pub dep_changes: Vec<DepChangeJson>,
    }

    #[derive(Facet)]
    pub struct InputChangeJson {
        pub label: String,
        pub old_value: Option<String>,
        pub new_value: Option<String>,
    }

    #[derive(Facet)]
    pub struct DepChangeJson {
        pub extern_name: String,
        pub crate_id: String,
        pub change: DepChangeKindJson,
    }

    #[derive(Facet)]
    #[repr(u8)]
    pub enum DepChangeKindJson {
        Added {
            rlib_hash: String,
            manifest_hash: String,
        },
        Removed {
            rlib_hash: String,
            manifest_hash: String,
        },
        RlibChanged {
            old_rlib: String,
            new_rlib: String,
            old_manifest: String,
            new_manifest: String,
        },
        ManifestChanged {
            rlib_hash: String,
            old_manifest: String,
            new_manifest: String,
        },
    }
}

/// Output explanation in JSON format
fn cmd_explain_json(
    report: &vx_report::BuildReport,
    prev_report: Option<&vx_report::BuildReport>,
    node_filter: Option<String>,
) -> Result<()> {
    use explain_json::*;
    use std::collections::HashMap;
    use vx_report::{CacheOutcome, FanoutAnalysis, NodeRebuildExplanation};

    let prev_nodes: HashMap<&str, &vx_report::NodeReport> = prev_report
        .map(|r| r.nodes.iter().map(|n| (n.node_id.as_str(), n)).collect())
        .unwrap_or_default();

    let mut nodes_json = Vec::new();

    for node in &report.nodes {
        // Apply node filter if specified
        if let Some(ref filter) = node_filter
            && &node.node_id != filter
        {
            continue;
        }

        let prev_node = prev_nodes.get(node.node_id.as_str()).copied();

        // For cache hits, include minimal info
        if matches!(node.cache, CacheOutcome::Hit { .. }) {
            nodes_json.push(NodeJson {
                node_id: node.node_id.clone(),
                kind: node.kind.clone(),
                cache: "hit".to_string(),
                execute_ms: node.timing.execute_ms,
                reported_reason: None,
                observed_reason: None,
                input_changes: vec![],
                dep_changes: vec![],
            });
            continue;
        }

        // For cache misses, compute explanation
        let explanation = NodeRebuildExplanation::compute(node, prev_node);

        if let Some(expl) = explanation {
            let input_changes: Vec<InputChangeJson> = expl
                .input_changes
                .iter()
                .map(|change| InputChangeJson {
                    label: change.label.clone(),
                    old_value: change.old_value.clone(),
                    new_value: change.new_value.clone(),
                })
                .collect();

            let dep_changes: Vec<DepChangeJson> = expl
                .dep_changes
                .iter()
                .map(|change| {
                    use vx_report::DepChangeKind;

                    let change_kind = match &change.change {
                        DepChangeKind::Added {
                            rlib_hash,
                            manifest_hash,
                        } => DepChangeKindJson::Added {
                            rlib_hash: rlib_hash.clone(),
                            manifest_hash: manifest_hash.clone(),
                        },
                        DepChangeKind::Removed {
                            rlib_hash,
                            manifest_hash,
                        } => DepChangeKindJson::Removed {
                            rlib_hash: rlib_hash.clone(),
                            manifest_hash: manifest_hash.clone(),
                        },
                        DepChangeKind::RlibChanged {
                            old_rlib,
                            new_rlib,
                            old_manifest,
                            new_manifest,
                        } => DepChangeKindJson::RlibChanged {
                            old_rlib: old_rlib.clone(),
                            new_rlib: new_rlib.clone(),
                            old_manifest: old_manifest.clone(),
                            new_manifest: new_manifest.clone(),
                        },
                        DepChangeKind::ManifestChanged {
                            rlib_hash,
                            old_manifest,
                            new_manifest,
                        } => DepChangeKindJson::ManifestChanged {
                            rlib_hash: rlib_hash.clone(),
                            old_manifest: old_manifest.clone(),
                            new_manifest: new_manifest.clone(),
                        },
                    };

                    DepChangeJson {
                        extern_name: change.extern_name.clone(),
                        crate_id: change.crate_id.clone(),
                        change: change_kind,
                    }
                })
                .collect();

            nodes_json.push(NodeJson {
                node_id: expl.node_id.clone(),
                kind: node.kind.clone(),
                cache: "miss".to_string(),
                execute_ms: node.timing.execute_ms,
                reported_reason: Some(expl.reported_reason.to_string()),
                observed_reason: Some(expl.observed_reason.to_string()),
                input_changes,
                dep_changes,
            });
        }
    }

    // Compute fanout
    let fanout = FanoutAnalysis::compute(report);

    let output = ExplainOutput {
        run_id: report.run_id.to_string(),
        prev_run_id: prev_report.map(|r| r.run_id.to_string()),
        workspace_root: report.workspace_root.clone(),
        profile: report.profile.clone(),
        target_triple: report.target_triple.clone(),
        success: report.success,
        duration_ms: report.duration_ms(),
        nodes: nodes_json,
        fanout: fanout.fanout_map.clone(),
    };

    println!(
        "{}",
        facet_json::to_string_pretty(&output)
            .map_err(|e| eyre::eyre!("failed to serialize explain output: {e}"))?
    );
    Ok(())
}

/// Open an interactive build graph in the browser.
fn cmd_explain_web(report: &vx_report::BuildReport) -> Result<()> {
    use std::io::{Read, Write};

    if report.graph.is_none() {
        println!(
            "{} This build report has no graph topology data.",
            "Error:".red().bold()
        );
        println!(
            "Graph topology was added in schema version 2. Re-run {} to generate a report with graph data.",
            "vx build".cyan()
        );
        return Ok(());
    }

    let report_json = facet_json::to_string(report)
        .map_err(|e| eyre::eyre!("failed to serialize report: {e}"))?;

    let html_template = include_str!("graph.html");
    let html = html_template.replace("__REPORT_DATA__", &report_json);

    // Start a tiny HTTP server on a random port
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let url = format!("http://127.0.0.1:{port}");

    println!("{} {}", "Build graph:".green().bold(), url.cyan());
    println!("{}", "Press Ctrl+C to stop".dimmed());

    // Open browser
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
    }

    // Serve requests until interrupted
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Read request (we only serve one page, ignore the actual request path)
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            html.len(),
            html
        );
        let _ = stream.write_all(response.as_bytes());
    }

    Ok(())
}
