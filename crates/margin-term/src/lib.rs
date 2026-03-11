//! Terminal rendering for `margin` layout plans.
// d[impl package.primary-split]
// d[impl api.layout-render-separation]
// d[impl api.renderer-options]
// d[impl layout.ellipsis]
// d[impl label.multiline-message-alignment]
// d[impl layout.multiline-labels]
// d[impl layout.no-terminal-wrap]
// d[impl layout.notes-wrap]
// d[impl term.explicit-capabilities]
// d[impl term.plaintext-mode]
// d[impl test.fixture-first]
// d[impl test.width-matrix]
// d[impl test.capability-matrix]

use std::fmt::Write;

use margin::{
    AnnotationRole, Diagnostics, LayoutError, LayoutOptions, LayoutPlan, Note, NoteKind,
    PlacementMode, ResolvedSpan, Severity, SourceWindow, WindowAnnotation, plan,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphMode {
    Unicode,
    Ascii,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorLevel {
    None,
    Ansi16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCapabilities {
    pub width: usize,
    pub glyph_mode: GlyphMode,
    pub color_level: ColorLevel,
}

impl Default for TerminalCapabilities {
    fn default() -> Self {
        Self {
            width: 80,
            glyph_mode: GlyphMode::Unicode,
            color_level: ColorLevel::None,
        }
    }
}

pub fn layout(
    diagnostics: &Diagnostics,
    capabilities: TerminalCapabilities,
) -> Result<LayoutPlan, LayoutError> {
    plan(diagnostics, &LayoutOptions::with_width(capabilities.width))
}

pub fn render(
    diagnostics: &Diagnostics,
    capabilities: TerminalCapabilities,
) -> Result<String, LayoutError> {
    let plan = layout(diagnostics, capabilities)?;
    Ok(render_plan(&plan, capabilities))
}

pub fn render_plan(plan: &LayoutPlan, capabilities: TerminalCapabilities) -> String {
    let mut output = String::new();

    for (report_index, report) in plan.reports.iter().enumerate() {
        if report_index > 0 {
            output.push('\n');
        }

        let _ = writeln!(
            output,
            "{}: {}",
            colorize(
                severity_label(report.severity),
                report.severity,
                capabilities.color_level
            ),
            report.title
        );

        for window in &report.windows {
            render_window(&mut output, window, capabilities);
        }

        for note in &report.notes {
            render_note(&mut output, note, capabilities);
        }

        for section in &report.sections {
            let _ = writeln!(
                output,
                "{} {}",
                glyphs(capabilities.glyph_mode).branch,
                section.title
            );
            for note in &section.notes {
                render_note(&mut output, note, capabilities);
            }
        }
    }

    output
}

fn render_window(output: &mut String, window: &SourceWindow, capabilities: TerminalCapabilities) {
    let glyphs = glyphs(capabilities.glyph_mode);
    let _ = writeln!(output, "{} {}", glyphs.source, window.source_name);
    if window.omitted_before {
        let _ = writeln!(output, "{} ...", glyphs.separator);
    }

    for line in &window.lines {
        let clipped = clip_to_width(&line.text, window.geometry.source_columns);
        let omission = if line.clipped { "..." } else { "" };
        let _ = writeln!(
            output,
            "{:>width$} {} {}{}",
            line.line_number,
            glyphs.separator,
            clipped,
            omission,
            width = window.geometry.line_number_width
        );

        for annotation in annotations_for_line(&window.annotations, line.line_number) {
            render_annotation(output, annotation, window, capabilities);
        }
    }

    if window.omitted_after {
        let _ = writeln!(output, "{} ...", glyphs.separator);
    }
}

fn render_note(output: &mut String, note: &Note, capabilities: TerminalCapabilities) {
    let prefix = match note.kind {
        NoteKind::Note => "note",
        NoteKind::Help => "help",
    };
    let indent = format!("  = {prefix}: ");
    let wrapped = wrap_text(
        &note.text,
        capabilities.width.saturating_sub(indent_width(&indent)),
    );

    for (index, line) in wrapped.iter().enumerate() {
        if index == 0 {
            let _ = writeln!(output, "{indent}{line}");
        } else {
            let _ = writeln!(output, "{}{line}", " ".repeat(indent_width(&indent)));
        }
    }
}

fn render_annotation(
    output: &mut String,
    annotation: RenderableAnnotation<'_>,
    window: &SourceWindow,
    capabilities: TerminalCapabilities,
) {
    let glyphs = glyphs(capabilities.glyph_mode);
    let marker_line = marker_line(annotation.segment, capabilities.glyph_mode);
    let colored = colorize(
        marker_line.as_str(),
        annotation.severity,
        capabilities.color_level,
    );
    let gutter = format!(
        "{:>width$} {} ",
        "",
        glyphs.separator,
        width = window.geometry.line_number_width
    );

    match annotation.placement {
        PlacementMode::Side => {
            let message = annotation
                .message
                .map(|message| format!(" {}", message))
                .unwrap_or_default();
            let _ = writeln!(output, "{gutter}{colored}{message}");
        }
        PlacementMode::BelowSpan => {
            let _ = writeln!(output, "{gutter}{colored}");
            if let Some(message) = annotation.message {
                for line in wrap_text(message, window.geometry.source_columns.saturating_sub(2)) {
                    let _ = writeln!(output, "{gutter}{} {line}", glyphs.branch);
                }
            }
        }
        PlacementMode::Stacked => {
            let _ = writeln!(output, "{gutter}{colored}");
            if let Some(message) = annotation.message {
                for line in wrap_text(message, window.geometry.source_columns.saturating_sub(4)) {
                    let _ = writeln!(output, "{gutter}{} = {line}", glyphs.branch);
                }
            }
        }
    }
}

fn annotations_for_line<'a>(
    annotations: &'a [WindowAnnotation],
    line_number: usize,
) -> Vec<RenderableAnnotation<'a>> {
    let mut renderable = annotations
        .iter()
        .flat_map(|annotation| {
            annotation
                .segments
                .iter()
                .copied()
                .filter(move |segment| segment.line_number == line_number)
                .map(move |segment| RenderableAnnotation {
                    message: annotation.message.as_deref(),
                    placement: annotation.placement,
                    severity: severity_for_role(annotation.role),
                    priority: annotation.priority,
                    segment,
                })
        })
        .collect::<Vec<_>>();
    renderable.sort_by(|left, right| right.priority.cmp(&left.priority));
    renderable
}

fn marker_line(span: ResolvedSpan, glyph_mode: GlyphMode) -> String {
    let glyph = match glyph_mode {
        GlyphMode::Unicode => '─',
        GlyphMode::Ascii => '^',
    };
    let width = span.end_column.saturating_sub(span.start_column).max(1);
    format!(
        "{}{}",
        " ".repeat(span.start_column),
        glyph.to_string().repeat(width)
    )
}

fn clip_to_width(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let mut clipped = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + ch_width > width {
            break;
        }
        clipped.push(ch);
        used += ch_width;
    }
    clipped
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };
            if UnicodeWidthStr::width(candidate.as_str()) > width && !current.is_empty() {
                lines.push(current);
                current = word.to_string();
            } else {
                current = candidate;
            }
        }
        if current.is_empty() {
            lines.push(String::new());
        } else {
            lines.push(current);
        }
    }
    lines
}

fn indent_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Advice => "advice",
    }
}

fn severity_for_role(role: AnnotationRole) -> Severity {
    match role {
        AnnotationRole::PrimaryLabel => Severity::Error,
        AnnotationRole::SecondaryLabel => Severity::Warning,
        AnnotationRole::RelatedLabel
        | AnnotationRole::SyntaxToken
        | AnnotationRole::SearchHighlight
        | AnnotationRole::Selection
        | AnnotationRole::Emphasis => Severity::Advice,
    }
}

fn colorize(text: &str, severity: Severity, color_level: ColorLevel) -> String {
    if color_level == ColorLevel::None {
        return text.to_string();
    }

    let code = match severity {
        Severity::Error => 31,
        Severity::Warning => 33,
        Severity::Advice => 36,
    };
    format!("\u{1b}[{code}m{text}\u{1b}[0m")
}

fn glyphs(mode: GlyphMode) -> Glyphs {
    match mode {
        GlyphMode::Unicode => Glyphs {
            source: "╭─>",
            separator: "│",
            branch: "╰─",
        },
        GlyphMode::Ascii => Glyphs {
            source: "-->",
            separator: "|",
            branch: "\\-",
        },
    }
}

#[derive(Debug, Clone, Copy)]
struct Glyphs {
    source: &'static str,
    separator: &'static str,
    branch: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct RenderableAnnotation<'a> {
    message: Option<&'a str>,
    placement: PlacementMode,
    severity: Severity,
    priority: u16,
    segment: ResolvedSpan,
}
