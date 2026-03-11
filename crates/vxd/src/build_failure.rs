use std::collections::{BTreeMap, BTreeSet};

use camino::Utf8Path;
use vx_eval::{DemandChain, PoisonError, PoisonKind, SourceProvenance};
use vx_parser::SyntaxError;
use vxd_proto::{
    BuildDiagnosticLabel, BuildDiagnosticLabelKind, BuildDiagnosticReport, BuildDiagnosticSeverity,
    BuildDiagnosticSource, BuildDiagnosticSpan, BuildFailure,
};

pub(crate) fn from_parse_errors(
    build_vx_path: &Utf8Path,
    source: &str,
    errors: &[SyntaxError],
) -> BuildFailure {
    let source_id = build_vx_path.to_string();
    let reports = errors
        .iter()
        .map(|error| {
            let span = span_from_range(
                &source_id,
                error.range.start().into(),
                error.range.end().into(),
            );
            BuildDiagnosticReport {
                severity: BuildDiagnosticSeverity::Error,
                message: "syntax error in build file".to_string(),
                primary_span: span.clone(),
                labels: vec![BuildDiagnosticLabel {
                    span,
                    kind: BuildDiagnosticLabelKind::Primary,
                    message: Some(error.message.clone()),
                }],
                notes: Vec::new(),
            }
        })
        .collect();

    BuildFailure {
        message: format!(
            "failed to parse {}",
            build_vx_path.file_name().unwrap_or("build.vx")
        ),
        sources: vec![BuildDiagnosticSource {
            source_id,
            language: Some("vixen".to_string()),
            text: source.to_string(),
        }],
        reports,
    }
}

pub(crate) fn from_poison_error(workspace_root: &Utf8Path, error: &PoisonError) -> BuildFailure {
    let leaves = unique_leaf_errors(error);
    let mut sources = BTreeMap::new();
    let mut reports = Vec::new();
    let mut summary = None;

    for leaf in leaves {
        let Some(primary_provenance) = primary_provenance(leaf) else {
            continue;
        };
        let presentation = classify_leaf(workspace_root, leaf);
        summary.get_or_insert_with(|| presentation.summary.clone());

        collect_source(&mut sources, &primary_provenance.source_id);
        let primary_span = span_from_provenance(&primary_provenance);
        reports.push(BuildDiagnosticReport {
            severity: BuildDiagnosticSeverity::Error,
            message: presentation.summary.clone(),
            primary_span: primary_span.clone(),
            labels: vec![BuildDiagnosticLabel {
                span: primary_span,
                kind: BuildDiagnosticLabelKind::Primary,
                message: Some(presentation.label_message),
            }],
            notes: presentation.notes,
        });

        let context_frames = context_provenances(&leaf.demand_chain);
        for (index, provenance) in context_frames.iter().enumerate() {
            collect_source(&mut sources, &provenance.source_id);
            let primary_span = span_from_provenance(provenance);
            let is_outermost = index + 1 == context_frames.len();
            reports.push(BuildDiagnosticReport {
                severity: BuildDiagnosticSeverity::Advice,
                message: if is_outermost {
                    "while evaluating build entry".to_string()
                } else {
                    "required by this expression".to_string()
                },
                primary_span: primary_span.clone(),
                labels: vec![BuildDiagnosticLabel {
                    span: primary_span,
                    kind: BuildDiagnosticLabelKind::Secondary,
                    message: Some(if is_outermost {
                        "this expression led evaluation to the failing input".to_string()
                    } else {
                        "this expression depends on the failing input".to_string()
                    }),
                }],
                notes: Vec::new(),
            });
        }
    }

    BuildFailure {
        message: summary.unwrap_or_else(|| format!("build evaluation failed: {error}")),
        sources: sources.into_values().collect(),
        reports,
    }
}

fn unique_leaf_errors(error: &PoisonError) -> Vec<&PoisonError> {
    let mut leaves = Vec::new();
    collect_leaf_errors(error, &mut leaves);

    let mut seen = BTreeSet::new();
    leaves
        .into_iter()
        .filter(|leaf| seen.insert(leaf_fingerprint(leaf)))
        .collect()
}

fn collect_leaf_errors<'a>(error: &'a PoisonError, leaves: &mut Vec<&'a PoisonError>) {
    if error.kind == PoisonKind::Multiple && !error.causes.is_empty() {
        for cause in error.causes.iter() {
            collect_leaf_errors(cause, leaves);
        }
    } else {
        leaves.push(error);
    }
}

fn leaf_fingerprint(error: &PoisonError) -> String {
    let provenance = primary_provenance(error)
        .map(|provenance| {
            format!(
                "{}:{}:{}",
                provenance.source_id, provenance.span.start, provenance.span.end
            )
        })
        .unwrap_or_default();
    format!("{:?}\n{}\n{provenance}", error.kind, error.message)
}

fn primary_provenance(error: &PoisonError) -> Option<SourceProvenance> {
    error
        .demand_chain
        .entries
        .iter()
        .rev()
        .find_map(|entry| entry.provenance.clone())
}

fn context_provenances(chain: &DemandChain) -> Vec<SourceProvenance> {
    let mut seen = BTreeSet::new();
    let mut frames = Vec::new();

    for provenance in chain
        .entries
        .iter()
        .rev()
        .filter_map(|entry| entry.provenance.clone())
        .skip(1)
    {
        let key = format!(
            "{}:{}:{}",
            provenance.source_id, provenance.span.start, provenance.span.end
        );
        if seen.insert(key) {
            frames.push(provenance);
        }
    }

    frames
}

fn collect_source(sources: &mut BTreeMap<String, BuildDiagnosticSource>, source_id: &str) {
    if sources.contains_key(source_id) {
        return;
    }

    let Ok(text) = std::fs::read_to_string(source_id) else {
        return;
    };

    sources.insert(
        source_id.to_string(),
        BuildDiagnosticSource {
            source_id: source_id.to_string(),
            language: source_language(source_id),
            text,
        },
    );
}

fn source_language(source_id: &str) -> Option<String> {
    source_id.ends_with(".vx").then(|| "vixen".to_string())
}

fn span_from_provenance(provenance: &SourceProvenance) -> BuildDiagnosticSpan {
    span_from_range(
        &provenance.source_id,
        provenance.span.start,
        provenance.span.end,
    )
}

fn span_from_range(source_id: &str, start: u32, end: u32) -> BuildDiagnosticSpan {
    BuildDiagnosticSpan {
        source_id: source_id.to_string(),
        start,
        end: if start == end {
            end.saturating_add(1)
        } else {
            end
        },
    }
}

struct LeafPresentation {
    summary: String,
    label_message: String,
    notes: Vec<String>,
}

fn classify_leaf(workspace_root: &Utf8Path, error: &PoisonError) -> LeafPresentation {
    if let Some(read_failure) = parse_read_failure(&error.message) {
        let missing = read_failure.os_error.contains("No such file or directory");
        let summary = if missing {
            "build input not found".to_string()
        } else {
            "build input could not be read".to_string()
        };
        let label_message = if missing {
            "this path does not exist in the current workspace".to_string()
        } else {
            "this path could not be read".to_string()
        };
        let mut notes = vec![format!("resolved path: {}", read_failure.path)];
        if read_failure.path.starts_with(workspace_root.as_str()) {
            notes.push(format!("workspace root: {workspace_root}"));
        }
        notes.push(format!("os error: {}", read_failure.os_error));
        return LeafPresentation {
            summary,
            label_message,
            notes,
        };
    }

    let summary = match error.kind {
        PoisonKind::UserFail => "build failed".to_string(),
        PoisonKind::VerificationFailure => "build verification failed".to_string(),
        PoisonKind::CircularDependency => "build evaluation found a cycle".to_string(),
        PoisonKind::DemandDepthExceeded => "build evaluation exceeded demand depth".to_string(),
        _ => "build evaluation failed".to_string(),
    };

    LeafPresentation {
        summary,
        label_message: human_label_message(&error.kind),
        notes: vec![error.message.clone()],
    }
}

fn human_label_message(kind: &PoisonKind) -> String {
    match kind {
        PoisonKind::TypeError => "this expression has the wrong type".to_string(),
        PoisonKind::UnresolvedName => "this expression references an unresolved name".to_string(),
        PoisonKind::ArithmeticError => "this expression failed during arithmetic".to_string(),
        PoisonKind::CapabilityViolation => {
            "this expression needs an unavailable capability".to_string()
        }
        PoisonKind::UserFail => "this expression explicitly failed".to_string(),
        PoisonKind::VerificationFailure => "this expression failed verification".to_string(),
        PoisonKind::CircularDependency => {
            "this expression participates in a dependency cycle".to_string()
        }
        PoisonKind::DemandDepthExceeded => {
            "this expression exceeded the demand depth limit".to_string()
        }
        PoisonKind::Multiple => "this expression triggered multiple failures".to_string(),
        PoisonKind::Internal => "this expression failed during build evaluation".to_string(),
    }
}

struct ReadFailure {
    path: String,
    os_error: String,
}

fn parse_read_failure(message: &str) -> Option<ReadFailure> {
    let rest = message.strip_prefix("failed to read ")?;
    let (path, os_error) = rest.rsplit_once(": ")?;
    Some(ReadFailure {
        path: path.to_string(),
        os_error: os_error.to_string(),
    })
}
