//! Render [`vx_ir::compile::CompileError`] into structured
//! [`margin::Diagnostics`] (reports + sources).
//!
//! This is the wire-friendly bridge that the daemon uses to ship structured
//! diagnostics to clients (CLI, LSP, etc.). Clients then render the
//! `Diagnostics` however they want — through `margin-term` for ANSI output,
//! through a different formatter, or by transporting them verbatim further.

use std::collections::BTreeSet;

use margin::{Diagnostics, Note, NoteKind, Report, Severity, Source, SourceId};
use vx_ir::compile::CompileError;
use vx_parser::SourceId as ParserSourceId;
use vx_types::TypeEnv;

/// Build a [`margin::Diagnostics`] for a [`CompileError`].
///
/// `user_source_text` and `user_source_id` describe the user's input source —
/// the one that's not part of the stdlib. `user_display_name` is the
/// human-readable path shown in margin output (e.g. `"input.vx"` or a real
/// file path). Stdlib sources are looked up from [`vx_stdlib::stdlib_sources`]
/// by index — the convention from `compile_source` is that stdlib source
/// `i` gets `ParserSourceId(i as u32 + 1)`.
pub fn render_compile_error(
    err: &CompileError,
    user_source_text: &str,
    user_source_id: ParserSourceId,
    user_display_name: Option<&str>,
    type_env: &TypeEnv,
) -> Diagnostics {
    let reports = compile_error_to_reports(err, type_env);
    let sources = collect_sources(
        &reports,
        user_source_text,
        user_source_id,
        user_display_name,
    );
    Diagnostics { sources, reports }
}

fn compile_error_to_reports(err: &CompileError, fallback_env: &TypeEnv) -> Vec<Report> {
    match err {
        CompileError::Parse(diags) => diags.iter().map(|d| d.to_report()).collect(),
        CompileError::Resolve { errors, env }
        | CompileError::Type { errors, env }
        | CompileError::ErrorUnion { errors, env } => {
            errors.iter().map(|d| d.to_report(env.as_ref())).collect()
        }
        CompileError::MissingEntry(name) => {
            let _ = fallback_env;
            vec![Report {
                severity: Severity::Error,
                title: "entry function not found".to_string(),
                annotations: Vec::new(),
                notes: vec![Note {
                    kind: NoteKind::Note,
                    text: format!("no function named `{name}` in the primary source unit"),
                }],
                sections: Vec::new(),
            }]
        }
    }
}

/// Walk all reports' annotations, collect the unique [`margin::SourceId`]
/// strings, then build a [`Source`] for each one.
fn collect_sources(
    reports: &[Report],
    user_source_text: &str,
    user_source_id: ParserSourceId,
    user_display_name: Option<&str>,
) -> Vec<Source> {
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    for report in reports {
        for annotation in &report.annotations {
            for span in &annotation.spans {
                referenced.insert(span.source_id.0.clone());
            }
        }
    }

    let user_id_str = user_source_id.0.to_string();
    let stdlib = vx_stdlib::stdlib_sources();

    let mut out: Vec<Source> = Vec::new();
    for id_str in &referenced {
        if *id_str == user_id_str {
            out.push(Source {
                id: SourceId(id_str.clone()),
                name: user_display_name.unwrap_or("input.vx").to_string(),
                hyperlink: None,
                text: user_source_text.to_string(),
            });
            continue;
        }

        if let Ok(numeric) = id_str.parse::<u32>()
            && numeric >= 1
        {
            let idx = (numeric - 1) as usize;
            if let Some(stdlib_source) = stdlib.get(idx) {
                out.push(Source {
                    id: SourceId(id_str.clone()),
                    name: stdlib_source.logical_path.to_string(),
                    hyperlink: None,
                    text: stdlib_source.text.to_string(),
                });
                continue;
            }
        }

        // Unknown source id — fall back to an empty source rather than
        // dropping it (margin needs every referenced source to be present).
        out.push(Source {
            id: SourceId(id_str.clone()),
            name: format!("<unknown:{id_str}>"),
            hyperlink: None,
            text: String::new(),
        });
    }

    // Always include the user source even if no annotation referenced it.
    // This keeps the rendered output anchored and lets clients always show
    // the input file context.
    if !out.iter().any(|s| s.id.0 == user_id_str) {
        out.insert(
            0,
            Source {
                id: SourceId(user_id_str),
                name: user_display_name.unwrap_or("input.vx").to_string(),
                hyperlink: None,
                text: user_source_text.to_string(),
            },
        );
    }

    out
}
