use std::cmp::Ordering;
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
    let leaf_groups = group_leaf_errors(workspace_root, &leaves);
    let mut sources = BTreeMap::new();
    let mut reports = Vec::new();
    let mut summary = None;

    for group in leaf_groups {
        let Some(primary_provenance) = group_primary_provenance(&group) else {
            continue;
        };
        let presentation = &group.presentation;
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
                message: Some(presentation.label_message.clone()),
            }],
            notes: presentation.notes.clone(),
        });

        let context_frames = group_context_frames(&group, &primary_provenance);
        for frame in context_frames {
            collect_source(&mut sources, &frame.provenance.source_id);
            let primary_span = span_from_provenance(&frame.provenance);
            reports.push(BuildDiagnosticReport {
                severity: BuildDiagnosticSeverity::Advice,
                message: if frame.is_build_entry {
                    "while evaluating build entry".to_string()
                } else {
                    "required by this expression".to_string()
                },
                primary_span: primary_span.clone(),
                labels: vec![BuildDiagnosticLabel {
                    span: primary_span,
                    kind: BuildDiagnosticLabelKind::Secondary,
                    message: Some(if frame.is_build_entry {
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

fn group_leaf_errors<'a>(
    workspace_root: &Utf8Path,
    leaves: &[&'a PoisonError],
) -> Vec<LeafGroup<'a>> {
    let mut groups = Vec::<LeafGroup<'a>>::new();
    let mut indices = BTreeMap::<String, usize>::new();

    for leaf in leaves {
        let presentation = classify_leaf(workspace_root, leaf);
        let key = leaf_group_key(leaf, &presentation);
        if let Some(index) = indices.get(&key).copied() {
            groups[index].leaves.push(*leaf);
        } else {
            indices.insert(key, groups.len());
            groups.push(LeafGroup {
                presentation,
                leaves: vec![*leaf],
            });
        }
    }

    groups
}

fn leaf_group_key(error: &PoisonError, presentation: &LeafPresentation) -> String {
    if let Some(read_failure) = parse_read_failure(&error.message) {
        return format!(
            "read:{}:{}:{}",
            presentation.summary, read_failure.path, read_failure.os_error
        );
    }

    format!("{:?}:{}", error.kind, error.message)
}

fn group_primary_provenance(group: &LeafGroup<'_>) -> Option<SourceProvenance> {
    if group.presentation.focus == ProvenanceFocus::PathRead
        && !group
            .leaves
            .iter()
            .flat_map(|leaf| provenance_candidates(&leaf.demand_chain))
            .any(|(_, provenance)| provenance.label == "path template")
        && let Some(provenance) = grouped_path_read_primary(group)
    {
        return Some(provenance);
    }

    let mut best = None;

    for (leaf_index, leaf) in group.leaves.iter().enumerate() {
        let chain_len = provenance_candidates(&leaf.demand_chain).len();
        for (depth, provenance) in primary_candidates(&leaf.demand_chain, group.presentation.focus)
        {
            if best.as_ref().is_none_or(
                |(best_leaf_index, best_depth, best_chain_len, best_provenance)| {
                    better_group_primary_candidate(
                        group.presentation.focus,
                        &provenance,
                        leaf_index,
                        depth,
                        chain_len,
                        best_provenance,
                        *best_leaf_index,
                        *best_depth,
                        *best_chain_len,
                    )
                },
            ) {
                best = Some((leaf_index, depth, chain_len, provenance));
            }
        }
    }

    best.map(|(_, _, _, provenance)| provenance)
}

fn grouped_path_read_primary(group: &LeafGroup<'_>) -> Option<SourceProvenance> {
    let mut best = None;

    for (leaf_index, leaf) in group.leaves.iter().enumerate() {
        let candidates = provenance_candidates(&leaf.demand_chain);
        if candidates.len() < 2 {
            continue;
        }

        let chain_len = candidates.len();
        let (depth, provenance) = candidates[candidates.len() - 2].clone();
        if best.as_ref().is_none_or(
            |(best_leaf_index, best_depth, best_chain_len, best_provenance): &(
                usize,
                usize,
                usize,
                SourceProvenance,
            )| {
                chain_len
                    .cmp(best_chain_len)
                    .then_with(|| depth.cmp(best_depth))
                    .then_with(|| {
                        provenance_span_width(best_provenance)
                            .cmp(&provenance_span_width(&provenance))
                    })
                    .then_with(|| best_leaf_index.cmp(&leaf_index))
                    .is_gt()
            },
        ) {
            best = Some((leaf_index, depth, chain_len, provenance));
        }
    }

    best.map(|(_, _, _, provenance)| provenance)
}

fn better_group_primary_candidate(
    focus: ProvenanceFocus,
    candidate: &SourceProvenance,
    candidate_leaf_index: usize,
    candidate_depth: usize,
    candidate_chain_len: usize,
    best: &SourceProvenance,
    best_leaf_index: usize,
    best_depth: usize,
    best_chain_len: usize,
) -> bool {
    let ordering = primary_rank(focus, candidate)
        .cmp(&primary_rank(focus, best))
        .then_with(|| {
            if focus == ProvenanceFocus::PathRead {
                candidate_chain_len.cmp(&best_chain_len)
            } else {
                Ordering::Equal
            }
        })
        .then_with(|| provenance_span_width(best).cmp(&provenance_span_width(candidate)))
        .then_with(|| candidate_depth.cmp(&best_depth))
        .then_with(|| best_leaf_index.cmp(&candidate_leaf_index));

    ordering.is_gt()
}

fn group_context_frames(
    group: &LeafGroup<'_>,
    primary_provenance: &SourceProvenance,
) -> Vec<ContextFrame> {
    let mut seen = BTreeSet::new();
    let mut line_cache = BTreeMap::new();
    let mut frames = BTreeMap::<ContextClusterKey, (usize, usize, SourceProvenance, bool)>::new();

    for (leaf_index, leaf) in group.leaves.iter().enumerate() {
        for frame in context_frame_candidates(&leaf.demand_chain, primary_provenance) {
            let key = provenance_key(&frame.provenance);
            if !seen.insert((leaf_index, key)) {
                continue;
            }

            let cluster_key = context_cluster_key(&frame.provenance, &mut line_cache);
            match frames.entry(cluster_key) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert((
                        leaf_index,
                        frame.depth,
                        frame.provenance,
                        frame.is_build_entry,
                    ));
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    let (best_leaf_index, best_depth, best_provenance, best_is_build_entry) =
                        entry.get();
                    if better_context_candidate(
                        &frame.provenance,
                        frame.depth,
                        best_provenance,
                        *best_depth,
                    ) {
                        entry.insert((
                            leaf_index,
                            frame.depth,
                            frame.provenance,
                            *best_is_build_entry || frame.is_build_entry,
                        ));
                    } else if frame.is_build_entry {
                        entry.insert((
                            *best_leaf_index,
                            *best_depth,
                            best_provenance.clone(),
                            true,
                        ));
                    }
                }
            }
        }
    }

    let mut frames = frames.into_values().collect::<Vec<_>>();
    let mut line_cache = BTreeMap::new();
    frames.sort_by(|(_, _, left_provenance, _), (_, _, right_provenance, _)| {
        source_line(
            &left_provenance.source_id,
            left_provenance.span.start,
            &mut line_cache,
        )
        .unwrap_or(left_provenance.span.start)
        .cmp(
            &source_line(
                &right_provenance.source_id,
                right_provenance.span.start,
                &mut line_cache,
            )
            .unwrap_or(right_provenance.span.start),
        )
        .then_with(|| left_provenance.span.start.cmp(&right_provenance.span.start))
    });
    frames
        .into_iter()
        .map(|(_, _, provenance, is_build_entry)| ContextFrame {
            provenance,
            is_build_entry,
        })
        .collect()
}

struct ContextFrame {
    provenance: SourceProvenance,
    is_build_entry: bool,
}

struct ContextFrameCandidate {
    provenance: SourceProvenance,
    depth: usize,
    is_build_entry: bool,
}

fn context_frame_candidates(
    chain: &DemandChain,
    primary_provenance: &SourceProvenance,
) -> Vec<ContextFrameCandidate> {
    let mut seen = BTreeSet::new();
    let mut frames = BTreeMap::<ContextClusterKey, (usize, SourceProvenance, bool)>::new();
    let mut line_cache = BTreeMap::new();

    let candidates = provenance_candidates(chain)
        .into_iter()
        .rev()
        .filter(|(_, provenance)| !same_provenance_span(provenance, primary_provenance))
        .collect::<Vec<_>>();

    for (index, (depth, provenance)) in candidates.iter().enumerate() {
        let key = provenance_key(provenance);
        if seen.insert(key) {
            let cluster_key = context_cluster_key(provenance, &mut line_cache);
            let is_build_entry = index + 1 == candidates.len();
            match frames.entry(cluster_key) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert((*depth, provenance.clone(), is_build_entry));
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    let (best_depth, best_provenance, best_is_build_entry) = entry.get();
                    if better_context_candidate(provenance, *depth, best_provenance, *best_depth) {
                        entry.insert((
                            *depth,
                            provenance.clone(),
                            *best_is_build_entry || is_build_entry,
                        ));
                    } else if is_build_entry {
                        entry.insert((*best_depth, best_provenance.clone(), true));
                    }
                }
            }
        }
    }

    let mut frames = frames.into_values().collect::<Vec<_>>();
    frames.sort_by(|(left_depth, _, _), (right_depth, _, _)| right_depth.cmp(left_depth));
    frames
        .into_iter()
        .map(
            |(depth, provenance, is_build_entry)| ContextFrameCandidate {
                provenance,
                depth,
                is_build_entry,
            },
        )
        .collect()
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
    let provenance = primary_provenance(&error.demand_chain, leaf_focus(error))
        .map(|provenance| {
            format!(
                "{}:{}:{}",
                provenance.source_id, provenance.span.start, provenance.span.end
            )
        })
        .unwrap_or_default();
    format!("{:?}\n{}\n{provenance}", error.kind, error.message)
}

fn primary_provenance(chain: &DemandChain, focus: ProvenanceFocus) -> Option<SourceProvenance> {
    let mut best = None;

    for (depth, provenance) in primary_candidates(chain, focus) {
        if best.as_ref().is_none_or(|(best_depth, best_provenance)| {
            better_primary_candidate(focus, &provenance, depth, best_provenance, *best_depth)
        }) {
            best = Some((depth, provenance));
        }
    }

    best.map(|(_, provenance)| provenance)
}

fn primary_candidates(
    chain: &DemandChain,
    focus: ProvenanceFocus,
) -> Vec<(usize, SourceProvenance)> {
    let candidates = provenance_candidates(chain);
    if focus == ProvenanceFocus::PathRead
        && !candidates
            .iter()
            .any(|(_, provenance)| provenance.label == "path template")
        && candidates.len() > 1
    {
        return candidates[..candidates.len() - 1].to_vec();
    }

    candidates
}

fn provenance_candidates(chain: &DemandChain) -> Vec<(usize, SourceProvenance)> {
    chain
        .entries
        .iter()
        .enumerate()
        .filter_map(|(depth, entry)| {
            entry
                .provenance
                .clone()
                .map(|provenance| (depth, provenance))
        })
        .collect()
}

fn better_primary_candidate(
    focus: ProvenanceFocus,
    candidate: &SourceProvenance,
    candidate_depth: usize,
    best: &SourceProvenance,
    best_depth: usize,
) -> bool {
    compare_primary_candidates(focus, candidate, candidate_depth, best, best_depth).is_gt()
}

fn compare_primary_candidates(
    focus: ProvenanceFocus,
    left: &SourceProvenance,
    left_depth: usize,
    right: &SourceProvenance,
    right_depth: usize,
) -> Ordering {
    primary_rank(focus, left)
        .cmp(&primary_rank(focus, right))
        .then_with(|| provenance_span_width(right).cmp(&provenance_span_width(left)))
        .then_with(|| left_depth.cmp(&right_depth))
}

fn better_context_candidate(
    candidate: &SourceProvenance,
    candidate_depth: usize,
    best: &SourceProvenance,
    best_depth: usize,
) -> bool {
    context_rank(candidate)
        .cmp(&context_rank(best))
        .then_with(|| provenance_span_width(candidate).cmp(&provenance_span_width(best)))
        .then_with(|| best_depth.cmp(&candidate_depth))
        .is_gt()
}

fn primary_rank(focus: ProvenanceFocus, provenance: &SourceProvenance) -> u8 {
    match focus {
        ProvenanceFocus::PathRead => match provenance.label.as_str() {
            "path template" => 5,
            label if label.starts_with("field(") => 4,
            "call(..)" | "invoke(..)" => 3,
            label if label.starts_with("binary(") => 2,
            _ => 1,
        },
        ProvenanceFocus::Generic => match provenance.label.as_str() {
            "call(..)" | "invoke(..)" => 4,
            label if label.starts_with("field(") => 3,
            label if label.starts_with("binary(") || label.starts_with("unary(") => 2,
            "path template" => 2,
            _ => 1,
        },
    }
}

fn context_rank(provenance: &SourceProvenance) -> u8 {
    match provenance.label.as_str() {
        "call(..)" | "invoke(..)" => 5,
        label if label.starts_with("field(") => 4,
        label if label.starts_with("binary(") || label.starts_with("unary(") => 3,
        label if label.starts_with("if(") || label.starts_with("fail(") => 2,
        "path template" => 1,
        _ => 0,
    }
}

fn provenance_span_width(provenance: &SourceProvenance) -> u32 {
    provenance.span.end.saturating_sub(provenance.span.start)
}

fn provenance_key(provenance: &SourceProvenance) -> String {
    format!(
        "{}:{}:{}",
        provenance.source_id, provenance.span.start, provenance.span.end
    )
}

fn same_provenance_span(left: &SourceProvenance, right: &SourceProvenance) -> bool {
    left.source_id == right.source_id
        && left.span.start == right.span.start
        && left.span.end == right.span.end
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProvenanceFocus {
    PathRead,
    Generic,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct ContextClusterKey {
    source_id: String,
    line: u32,
}

fn context_cluster_key(
    provenance: &SourceProvenance,
    line_cache: &mut BTreeMap<String, Option<String>>,
) -> ContextClusterKey {
    let line = source_line(&provenance.source_id, provenance.span.start, line_cache)
        .unwrap_or(provenance.span.start);
    ContextClusterKey {
        source_id: provenance.source_id.to_string(),
        line,
    }
}

fn source_line(
    source_id: &str,
    byte_offset: u32,
    line_cache: &mut BTreeMap<String, Option<String>>,
) -> Option<u32> {
    let source = line_cache
        .entry(source_id.to_string())
        .or_insert_with(|| std::fs::read_to_string(source_id).ok())
        .as_deref()?;
    let end = usize::try_from(byte_offset).ok()?.min(source.len());
    Some(
        source.as_bytes()[..end]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count() as u32,
    )
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
    focus: ProvenanceFocus,
}

struct LeafGroup<'a> {
    presentation: LeafPresentation,
    leaves: Vec<&'a PoisonError>,
}

fn classify_leaf(workspace_root: &Utf8Path, error: &PoisonError) -> LeafPresentation {
    if let Some(exec_missing_input) = parse_exec_missing_input_failure(&error.message) {
        let mut notes = vec![format!(
            "missing process input: {}",
            exec_missing_input.path
        )];
        if let Some(similar_path) = &exec_missing_input.similar_path {
            notes.push(format!("similar sandbox path: {similar_path}"));
        }
        notes.push(format!("process stderr:\n{}", exec_missing_input.stderr));
        return LeafPresentation {
            summary: "build command input not found".to_string(),
            label_message:
                "this command depends on an input file that was not present in the sandbox"
                    .to_string(),
            notes,
            focus: ProvenanceFocus::Generic,
        };
    }

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
            focus: ProvenanceFocus::PathRead,
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
        focus: ProvenanceFocus::Generic,
    }
}

fn leaf_focus(error: &PoisonError) -> ProvenanceFocus {
    parse_read_failure(&error.message)
        .map(|_| ProvenanceFocus::PathRead)
        .unwrap_or(ProvenanceFocus::Generic)
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

struct ExecMissingInputFailure {
    path: String,
    similar_path: Option<String>,
    stderr: String,
}

fn parse_read_failure(message: &str) -> Option<ReadFailure> {
    let rest = message.strip_prefix("failed to read ")?;
    let (path, os_error) = rest.rsplit_once(": ")?;
    Some(ReadFailure {
        path: path.to_string(),
        os_error: os_error.to_string(),
    })
}

fn parse_exec_missing_input_failure(message: &str) -> Option<ExecMissingInputFailure> {
    let (_, stderr) = message.split_once("\nprocess stderr:\n")?;
    let stderr = stderr
        .split_once("\nprocess stdout:\n")
        .map(|(stderr, _)| stderr)
        .unwrap_or(stderr)
        .split_once("\nsimilar sandbox path: ")
        .map(|(stderr, _)| stderr)
        .unwrap_or(stderr)
        .trim_end();
    let similar_path = message.lines().find_map(|line| {
        line.strip_prefix("similar sandbox path: ")
            .map(ToOwned::to_owned)
    });
    let path = stderr.lines().find_map(parse_missing_process_input_path)?;
    Some(ExecMissingInputFailure {
        path,
        similar_path,
        stderr: stderr.to_string(),
    })
}

fn parse_missing_process_input_path(line: &str) -> Option<String> {
    let (_, rest) = line.split_once("no such file or directory: ")?;
    parse_quoted_path(rest.trim())
}

fn parse_quoted_path(value: &str) -> Option<String> {
    for quote in ['\'', '"'] {
        let stripped = value.strip_prefix(quote)?;
        let (path, _) = stripped.split_once(quote)?;
        return Some(path.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use tempfile::TempDir;
    use vx_eval::{DemandChain, NodeId, SourceSpan};

    use super::*;

    #[test]
    fn read_failures_anchor_to_path_template_and_collapse_same_line_context() {
        let temp_dir = TempDir::new().expect("temp dir");
        let workspace_root = Utf8Path::from_path(temp_dir.path()).expect("utf8 workspace root");
        let source_path = workspace_root.join("build.vx");
        let source = r#"fn build(ws: Path) -> Tree (
  let src_tree = Tree(ws / @zoo/lua-src);
  let lua_exe = link(Tree [compile @lua.c, liblua]);
  Tree {
    @bin/lua: lua_exe.file(@bin/lua).executable(),
  }
)
"#;
        fs::write(&source_path, source).expect("write source");

        let missing_path = workspace_root.join("zoo/lua-src");
        let mut demand_chain = DemandChain::new();
        demand_chain.push(
            NodeId::next(),
            "call(..)".to_string(),
            Some(provenance(
                &source_path,
                source,
                "lua_exe.file(@bin/lua).executable()",
                "call(..)",
            )),
        );
        demand_chain.push(
            NodeId::next(),
            "call(..)".to_string(),
            Some(provenance(
                &source_path,
                source,
                "Tree [compile @lua.c, liblua]",
                "call(..)",
            )),
        );
        demand_chain.push(
            NodeId::next(),
            "call(..)".to_string(),
            Some(provenance(
                &source_path,
                source,
                "compile @lua.c",
                "call(..)",
            )),
        );
        demand_chain.push(
            NodeId::next(),
            "template(Path)".to_string(),
            Some(provenance(
                &source_path,
                source,
                "@zoo/lua-src",
                "path template",
            )),
        );

        let error = PoisonError::new(
            PoisonKind::Internal,
            format!("failed to read {missing_path}: No such file or directory (os error 2)"),
            demand_chain,
        );

        let failure = from_poison_error(workspace_root, &error);

        assert_eq!(failure.reports.len(), 3, "{failure:#?}");
        assert_eq!(failure.reports[0].message, "build input not found");
        assert_eq!(
            span_text(source, &failure.reports[0].primary_span),
            "@zoo/lua-src"
        );
        assert_eq!(
            span_text(source, &failure.reports[1].primary_span),
            "Tree [compile @lua.c, liblua]"
        );
        assert_eq!(
            span_text(source, &failure.reports[2].primary_span),
            "lua_exe.file(@bin/lua).executable()"
        );
        assert_eq!(
            failure.reports[2].message,
            "while evaluating build entry".to_string()
        );
    }

    #[test]
    fn observer_process_missing_input_failures_are_rephrased() {
        let temp_dir = TempDir::new().expect("temp dir");
        let workspace_root = Utf8Path::from_path(temp_dir.path()).expect("utf8 workspace root");
        let source_path = workspace_root.join("build.vx");
        let source = r#"fn build(ws: Path) -> Tree (
  let lua_exe = link(Tree [compile @lua.c, liblua]);
  lua_exe
)
"#;
        fs::write(&source_path, source).expect("write source");

        let mut demand_chain = DemandChain::new();
        demand_chain.push(
            NodeId::next(),
            "call(..)".to_string(),
            Some(provenance(
                &source_path,
                source,
                "Tree [compile @lua.c, liblua]",
                "call(..)",
            )),
        );

        let error = PoisonError::new(
            PoisonKind::UserFail,
            "failed to evaluate shipped observer: UserFail: process failed\n  demand chain:\n    -> fail(..)\nsimilar sandbox path: src/src/lua.c\nprocess stderr:\nclang: error: no such file or directory: 'src/lua.c'\nclang: error: no input files\n".to_string(),
            demand_chain,
        );

        let failure = from_poison_error(workspace_root, &error);

        assert_eq!(failure.message, "build command input not found");
        assert_eq!(failure.reports.len(), 1, "{failure:#?}");
        assert_eq!(failure.reports[0].message, "build command input not found");
        assert_eq!(
            failure.reports[0].labels[0].message.as_deref(),
            Some("this command depends on an input file that was not present in the sandbox")
        );
        assert_eq!(
            failure.reports[0].notes.first().map(String::as_str),
            Some("missing process input: src/lua.c")
        );
        assert!(
            failure.reports[0]
                .notes
                .iter()
                .any(|note| note == "similar sandbox path: src/src/lua.c"),
            "{failure:#?}"
        );
        assert!(
            failure.reports[0]
                .notes
                .iter()
                .any(|note| note.contains("clang: error: no such file or directory: 'src/lua.c'")),
            "{failure:#?}"
        );
        assert!(
            failure.reports[0]
                .notes
                .iter()
                .all(|note| !note.contains("process stderr:\nclang: error: no such file or directory: 'src/lua.c'\nclang: error: no input files\n\nsimilar sandbox path: src/src/lua.c")),
            "{failure:#?}"
        );
    }

    fn provenance(
        source_path: &Utf8Path,
        source: &str,
        needle: &str,
        label: &str,
    ) -> SourceProvenance {
        let start = source.find(needle).expect("needle start") as u32;
        let end = start + u32::try_from(needle.len()).expect("needle length");
        SourceProvenance {
            source_id: Arc::from(source_path.as_str()),
            span: SourceSpan { start, end },
            label: label.to_string(),
        }
    }

    fn span_text<'a>(source: &'a str, span: &BuildDiagnosticSpan) -> &'a str {
        &source[span.start as usize..span.end as usize]
    }
}
