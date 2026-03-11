//! Structured diagnostics core and layout planning.
//!
//! The core crate intentionally owns the reusable model and the explicit layout
//! artifact, leaving terminal emission to `margin-term`.
// d[impl package.name]
// d[impl package.no-vixen-prefix]
// d[impl package.primary-split]
// d[impl package.additional-renderers]
// d[impl api.core-model]
// d[impl api.layout-render-separation]
// d[impl api.explicit-layout-artifact]
// d[impl api.stable-fixtures]
// d[impl model.report]
// d[impl model.source]
// d[impl model.annotation]
// d[impl model.window]
// d[impl model.role-stable]
// d[impl input.structured]
// d[impl input.unified-spans]
// d[impl input.source-identity]
// d[impl input.span-bounds]
// d[impl input.multiple-sources]
// d[impl input.overlay-kinds]
// d[impl span.authoritative-source]
// d[impl span.zero-width]
// d[impl span.line-breaks]
// d[impl layout.pipeline]
// d[impl layout.pure-planning]
// d[impl layout.width-aware]
// d[impl window.primary-context]
// d[impl window.cross-file]

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::ops::Range;

use facet::Facet;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum Severity {
    Error,
    Warning,
    Advice,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Facet)]
pub struct SourceId(pub String);

impl From<&str> for SourceId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct Source {
    pub id: SourceId,
    pub name: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct Span {
    pub source_id: SourceId,
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(source_id: impl Into<SourceId>, start: usize, end: usize) -> Self {
        Self {
            source_id: source_id.into(),
            start,
            end,
        }
    }

    fn validate(&self, source: &Source) -> Result<(), LayoutError> {
        if self.start > self.end {
            return Err(LayoutError::InvalidSpan {
                source: source.id.0.clone(),
                start: self.start,
                end: self.end,
                reason: "start is greater than end",
            });
        }
        if self.end > source.text.len() {
            return Err(LayoutError::InvalidSpan {
                source: source.id.0.clone(),
                start: self.start,
                end: self.end,
                reason: "end is out of bounds",
            });
        }
        if !source.text.is_char_boundary(self.start) || !source.text.is_char_boundary(self.end) {
            return Err(LayoutError::InvalidSpan {
                source: source.id.0.clone(),
                start: self.start,
                end: self.end,
                reason: "span does not align to UTF-8 boundaries",
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Facet)]
#[repr(u8)]
pub enum AnnotationRole {
    PrimaryLabel,
    SecondaryLabel,
    RelatedLabel,
    SyntaxToken,
    SearchHighlight,
    Selection,
    Emphasis,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct Annotation {
    pub spans: Vec<Span>,
    pub role: AnnotationRole,
    pub message: Option<String>,
    pub priority: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum NoteKind {
    Note,
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct Note {
    pub kind: NoteKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct ReportSection {
    pub title: String,
    pub notes: Vec<Note>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct Report {
    pub severity: Severity,
    pub title: String,
    pub annotations: Vec<Annotation>,
    pub notes: Vec<Note>,
    pub sections: Vec<ReportSection>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Facet)]
pub struct Diagnostics {
    pub sources: Vec<Source>,
    pub reports: Vec<Report>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
pub struct LayoutOptions {
    pub width: usize,
    pub primary_context_lines: usize,
    pub secondary_context_lines: usize,
    pub tab_width: usize,
}

impl LayoutOptions {
    pub fn with_width(width: usize) -> Self {
        Self {
            width,
            ..Self::default()
        }
    }
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            width: 80,
            primary_context_lines: 2,
            secondary_context_lines: 1,
            tab_width: 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct LayoutPlan {
    pub width: usize,
    pub reports: Vec<PlannedReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct PlannedReport {
    pub severity: Severity,
    pub title: String,
    pub windows: Vec<SourceWindow>,
    pub notes: Vec<Note>,
    pub sections: Vec<ReportSection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct SourceWindow {
    pub source_id: SourceId,
    pub source_name: String,
    pub first_line_number: usize,
    pub last_line_number: usize,
    pub omitted_before: bool,
    pub omitted_after: bool,
    pub lines: Vec<WindowLine>,
    pub annotations: Vec<WindowAnnotation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct WindowLine {
    pub line_number: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct WindowAnnotation {
    pub role: AnnotationRole,
    pub message: Option<String>,
    pub priority: u16,
    pub segments: Vec<ResolvedSpan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
pub struct ResolvedSpan {
    pub line_number: usize,
    pub start_column: usize,
    pub end_column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    DuplicateSourceId(String),
    MissingSource(String),
    InvalidSpan {
        source: String,
        start: usize,
        end: usize,
        reason: &'static str,
    },
}

impl Display for LayoutError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            LayoutError::DuplicateSourceId(source) => {
                write!(f, "duplicate source id in diagnostics: {source}")
            }
            LayoutError::MissingSource(source) => {
                write!(f, "report annotation references unknown source: {source}")
            }
            LayoutError::InvalidSpan {
                source,
                start,
                end,
                reason,
            } => write!(f, "invalid span {start}..{end} for {source}: {reason}"),
        }
    }
}

impl Error for LayoutError {}

pub fn plan(diagnostics: &Diagnostics, options: &LayoutOptions) -> Result<LayoutPlan, LayoutError> {
    let sources = index_sources(&diagnostics.sources)?;
    let reports = diagnostics
        .reports
        .iter()
        .map(|report| plan_report(report, &sources, options))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(LayoutPlan {
        width: options.width,
        reports,
    })
}

fn index_sources(sources: &[Source]) -> Result<BTreeMap<&SourceId, &Source>, LayoutError> {
    let mut indexed = BTreeMap::new();
    for source in sources {
        if indexed.insert(&source.id, source).is_some() {
            return Err(LayoutError::DuplicateSourceId(source.id.0.clone()));
        }
    }
    Ok(indexed)
}

fn plan_report(
    report: &Report,
    sources: &BTreeMap<&SourceId, &Source>,
    options: &LayoutOptions,
) -> Result<PlannedReport, LayoutError> {
    let grouped = group_annotations_by_source(&report.annotations, sources, options)?;
    let windows = grouped
        .into_iter()
        .map(|(source_id, source_annotations)| {
            build_windows(source_id, source_annotations, sources, options)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();

    Ok(PlannedReport {
        severity: report.severity,
        title: report.title.clone(),
        windows,
        notes: report.notes.clone(),
        sections: report.sections.clone(),
    })
}

fn group_annotations_by_source<'a>(
    annotations: &'a [Annotation],
    sources: &BTreeMap<&SourceId, &Source>,
    options: &LayoutOptions,
) -> Result<BTreeMap<&'a SourceId, Vec<PendingAnnotation<'a>>>, LayoutError> {
    let mut grouped = BTreeMap::new();

    for annotation in annotations {
        for span in &annotation.spans {
            let source_id = &span.source_id;
            let source = sources
                .get(source_id)
                .copied()
                .ok_or_else(|| LayoutError::MissingSource(source_id.0.clone()))?;
            span.validate(source)?;
            let context = annotation_context(annotation.role, options);
            let segments = resolve_span(source, span.clone())?;
            let line_range = segments
                .iter()
                .map(|segment| segment.line_number)
                .fold(None::<(usize, usize)>, |acc, line| match acc {
                    Some((start, end)) => Some((start.min(line), end.max(line))),
                    None => Some((line, line)),
                })
                .unwrap_or((1, 1));

            grouped
                .entry(source_id)
                .or_insert_with(Vec::new)
                .push(PendingAnnotation {
                    annotation,
                    line_start: line_range.0.saturating_sub(context),
                    line_end: line_range.1 + context,
                    segments,
                });
        }
    }

    Ok(grouped)
}

fn annotation_context(role: AnnotationRole, options: &LayoutOptions) -> usize {
    match role {
        AnnotationRole::PrimaryLabel => options.primary_context_lines,
        _ => options.secondary_context_lines,
    }
}

fn build_windows(
    source_id: &SourceId,
    annotations: Vec<PendingAnnotation<'_>>,
    sources: &BTreeMap<&SourceId, &Source>,
    _options: &LayoutOptions,
) -> Result<Vec<SourceWindow>, LayoutError> {
    let source = sources
        .get(source_id)
        .copied()
        .ok_or_else(|| LayoutError::MissingSource(source_id.0.clone()))?;
    let source_lines = split_lines(&source.text);
    let ranges = merge_line_ranges(
        annotations
            .iter()
            .map(|annotation| (annotation.line_start.max(1), annotation.line_end.max(1)))
            .collect(),
    );

    ranges
        .into_iter()
        .map(|(line_start, line_end)| {
            let last_line_number = line_end.min(source_lines.len());
            let lines = source_lines
                .iter()
                .enumerate()
                .filter(|(index, _)| {
                    let line_number = index + 1;
                    (line_start..=last_line_number).contains(&line_number)
                })
                .map(|(index, line)| WindowLine {
                    line_number: index + 1,
                    text: (*line).to_string(),
                })
                .collect::<Vec<_>>();

            let annotations = annotations
                .iter()
                .filter_map(|annotation| {
                    let segments = annotation
                        .segments
                        .iter()
                        .copied()
                        .filter(|segment| {
                            (line_start..=last_line_number).contains(&segment.line_number)
                        })
                        .collect::<Vec<_>>();
                    (!segments.is_empty()).then(|| WindowAnnotation {
                        role: annotation.annotation.role,
                        message: annotation.annotation.message.clone(),
                        priority: annotation.annotation.priority,
                        segments,
                    })
                })
                .collect::<Vec<_>>();

            Ok(SourceWindow {
                source_id: source.id.clone(),
                source_name: source.name.clone(),
                first_line_number: line_start,
                last_line_number,
                omitted_before: line_start > 1,
                omitted_after: last_line_number < source_lines.len(),
                lines,
                annotations,
            })
        })
        .collect()
}

fn merge_line_ranges(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_unstable();

    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        if let Some((_, current_end)) = merged.last_mut()
            && start <= *current_end + 1
        {
            *current_end = (*current_end).max(end);
            continue;
        }
        merged.push((start, end));
    }
    merged
}

fn resolve_span(source: &Source, span: Span) -> Result<Vec<ResolvedSpan>, LayoutError> {
    span.validate(source)?;
    let line_index = LineIndex::new(&source.text);
    let start = line_index.resolve(span.start);
    let end = line_index.resolve(span.end);

    if start.line == end.line {
        return Ok(vec![ResolvedSpan {
            line_number: start.line,
            start_column: start.column,
            end_column: end
                .column
                .max(start.column + usize::from(span.start == span.end)),
        }]);
    }

    let mut segments = Vec::new();
    for line in start.line..=end.line {
        let line_range = line_index.line_range(line);
        let segment_start = if line == start.line { start.column } else { 0 };
        let segment_end = if line == end.line {
            end.column
                .max(segment_start + usize::from(span.start == span.end))
        } else {
            source.text[line_range].chars().count()
        };
        segments.push(ResolvedSpan {
            line_number: line,
            start_column: segment_start,
            end_column: segment_end,
        });
    }

    Ok(segments)
}

fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = text.lines().collect::<Vec<_>>();
    if text.ends_with('\n') {
        lines.push("");
    }
    if lines.is_empty() {
        lines.push("");
    }
    lines
}

#[derive(Debug, Clone)]
struct PendingAnnotation<'a> {
    annotation: &'a Annotation,
    line_start: usize,
    line_end: usize,
    segments: Vec<ResolvedSpan>,
}

#[derive(Debug)]
struct LineIndex<'a> {
    source: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    fn new(source: &'a str) -> Self {
        let mut starts = vec![0];
        for (index, byte) in source.bytes().enumerate() {
            if byte == b'\n' && index + 1 <= source.len() {
                starts.push(index + 1);
            }
        }
        Self { source, starts }
    }

    fn resolve(&self, offset: usize) -> LineColumn {
        let line_index = self
            .starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1);
        let line_start = self.starts[line_index];
        let column = self.source[line_start..offset].chars().count();
        LineColumn {
            line: line_index + 1,
            column,
        }
    }

    fn line_range(&self, line_number: usize) -> Range<usize> {
        let start = self.starts[line_number - 1];
        let end = self
            .starts
            .get(line_number)
            .copied()
            .map(|offset| offset.saturating_sub(1))
            .unwrap_or(self.source.len());
        start..end
    }
}

#[derive(Debug, Clone, Copy)]
struct LineColumn {
    line: usize,
    column: usize,
}
