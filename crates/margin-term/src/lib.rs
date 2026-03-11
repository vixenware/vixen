//! Terminal rendering for `margin` layout plans.
// d[impl package.primary-split]
// d[impl api.layout-render-separation]
// d[impl api.renderer-options]
// d[impl layout.ellipsis]
// d[impl label.multiline-message-alignment]
// d[impl layout.multiline-labels]
// d[impl layout.no-terminal-wrap]
// d[impl layout.notes-wrap]
// d[impl term.ansi-discipline]
// d[impl term.explicit-capabilities]
// d[impl term.plaintext-mode]
// d[impl glyph.unicode-ascii]
// d[impl theme.capability-fallback]
// d[impl theme.roles]
// d[impl test.fixture-first]
// d[impl test.width-matrix]
// d[impl test.capability-matrix]
// d[impl test.unicode]

use std::fmt::Write;

use margin::{
    AnnotationRole, Diagnostics, LayoutError, LayoutOptions, LayoutPlan, Note, NoteKind,
    PlacementMode, ResolvedSpan, Severity, SourceWindow, SyntaxClass, WindowAnnotation, plan,
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
    pub tab_width: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub severity_error: Style,
    pub severity_warning: Style,
    pub severity_advice: Style,
    pub primary_label: Style,
    pub secondary_label: Style,
    pub syntax_token: Style,
    pub note: Style,
    pub help: Style,
    pub gutter: Style,
    pub connector: Style,
    pub emphasis: Style,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    ansi16: Option<u8>,
}

impl Default for TerminalCapabilities {
    fn default() -> Self {
        Self {
            width: 80,
            glyph_mode: GlyphMode::Unicode,
            color_level: ColorLevel::None,
            tab_width: 4,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            severity_error: Style::ansi16(31),
            severity_warning: Style::ansi16(33),
            severity_advice: Style::ansi16(36),
            primary_label: Style::ansi16(31),
            secondary_label: Style::ansi16(33),
            syntax_token: Style::ansi16(36),
            note: Style::ansi16(36),
            help: Style::ansi16(32),
            gutter: Style::ansi16(90),
            connector: Style::ansi16(90),
            emphasis: Style::ansi16(35),
        }
    }
}

impl Style {
    const fn ansi16(code: u8) -> Self {
        Self { ansi16: Some(code) }
    }

    const fn plain() -> Self {
        Self { ansi16: None }
    }
}

pub fn layout(
    diagnostics: &Diagnostics,
    capabilities: TerminalCapabilities,
) -> Result<LayoutPlan, LayoutError> {
    plan(
        diagnostics,
        &LayoutOptions {
            width: capabilities.width,
            tab_width: capabilities.tab_width,
            ..LayoutOptions::default()
        },
    )
}

pub fn render(
    diagnostics: &Diagnostics,
    capabilities: TerminalCapabilities,
) -> Result<String, LayoutError> {
    let plan = layout(diagnostics, capabilities)?;
    Ok(render_plan(&plan, capabilities))
}

pub fn render_plan(plan: &LayoutPlan, capabilities: TerminalCapabilities) -> String {
    render_plan_with_theme(plan, capabilities, Theme::default())
}

pub fn render_plan_with_theme(
    plan: &LayoutPlan,
    capabilities: TerminalCapabilities,
    theme: Theme,
) -> String {
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
                theme.style_for_severity(report.severity),
                capabilities.color_level
            ),
            report.title
        );

        for window in &report.windows {
            render_window(&mut output, window, capabilities, theme);
        }

        for note in &report.notes {
            render_note(&mut output, note, capabilities, theme);
        }

        for section in &report.sections {
            let _ = writeln!(
                output,
                "{} {}",
                colorize(
                    glyphs(capabilities.glyph_mode).branch,
                    theme.connector,
                    capabilities.color_level
                ),
                section.title
            );
            for note in &section.notes {
                render_note(&mut output, note, capabilities, theme);
            }
        }
    }

    output
}

fn render_window(
    output: &mut String,
    window: &SourceWindow,
    capabilities: TerminalCapabilities,
    theme: Theme,
) {
    let glyphs = glyphs(capabilities.glyph_mode);
    let _ = writeln!(
        output,
        "{} {}",
        colorize(glyphs.source, theme.connector, capabilities.color_level),
        window.source_name
    );
    if window.omitted_before {
        let _ = writeln!(
            output,
            "{} ...",
            colorize(glyphs.separator, theme.gutter, capabilities.color_level)
        );
    }

    for line in &window.lines {
        let expanded = expand_tabs(&line.text, capabilities.tab_width);
        let clipped = clip_to_width(&expanded, window.geometry.source_columns);
        let styled = style_source_line(
            clipped.as_str(),
            line.line_number,
            &window.annotations,
            capabilities,
            theme,
        );
        let omission = if line.clipped { "..." } else { "" };
        let _ = writeln!(
            output,
            "{:>width$} {} {}{}",
            line.line_number,
            colorize(glyphs.separator, theme.gutter, capabilities.color_level),
            styled,
            omission,
            width = window.geometry.line_number_width
        );

        for annotation in annotations_for_line(&window.annotations, line.line_number)
            .into_iter()
            .filter(|annotation| annotation.role != AnnotationRole::SyntaxToken)
        {
            render_annotation(output, annotation, window, capabilities, theme);
        }
    }

    if window.omitted_after {
        let _ = writeln!(
            output,
            "{} ...",
            colorize(glyphs.separator, theme.gutter, capabilities.color_level)
        );
    }
}

fn style_source_line(
    text: &str,
    line_number: usize,
    annotations: &[WindowAnnotation],
    capabilities: TerminalCapabilities,
    theme: Theme,
) -> String {
    if capabilities.color_level == ColorLevel::None {
        return text.to_string();
    }

    let syntax_segments = annotations
        .iter()
        .filter(|annotation| annotation.role == AnnotationRole::SyntaxToken)
        .flat_map(|annotation| {
            annotation
                .segments
                .iter()
                .copied()
                .map(move |segment| (segment, annotation.syntax_class))
        })
        .filter(|(segment, _)| segment.line_number == line_number)
        .collect::<Vec<_>>();

    if syntax_segments.is_empty() {
        return text.to_string();
    }

    let mut styled = String::new();
    let mut run = String::new();
    let mut current_style = None;
    let mut column = 0;
    for ch in text.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        let style = syntax_segments
            .iter()
            .find_map(|(segment, syntax_class)| {
                let overlaps = if width == 0 {
                    segment.start_column <= column && column < segment.end_column
                } else {
                    segment.start_column < column + width && column < segment.end_column
                };
                overlaps.then(|| theme.style_for_syntax_class(*syntax_class))
            })
            .filter(|style| style.ansi16.is_some());

        if style != current_style {
            flush_styled_run(
                &mut styled,
                &mut run,
                current_style,
                capabilities.color_level,
            );
            current_style = style;
        }
        run.push(ch);

        column += width;
    }
    flush_styled_run(
        &mut styled,
        &mut run,
        current_style,
        capabilities.color_level,
    );

    styled
}

fn flush_styled_run(
    output: &mut String,
    run: &mut String,
    style: Option<Style>,
    color_level: ColorLevel,
) {
    if run.is_empty() {
        return;
    }

    match style {
        Some(style) => output.push_str(&colorize(run.as_str(), style, color_level)),
        None => output.push_str(run),
    }
    run.clear();
}

fn render_note(output: &mut String, note: &Note, capabilities: TerminalCapabilities, theme: Theme) {
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
            let styled_indent = colorize(
                indent.as_str(),
                theme.style_for_note_kind(note.kind),
                capabilities.color_level,
            );
            let _ = writeln!(output, "{styled_indent}{line}");
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
    theme: Theme,
) {
    let glyphs = glyphs(capabilities.glyph_mode);
    let marker_line = marker_line(annotation.segment, capabilities.glyph_mode);
    let colored = colorize(
        marker_line.as_str(),
        theme.style_for_annotation_role(annotation.role, annotation.severity),
        capabilities.color_level,
    );
    let gutter = format!(
        "{:>width$} {} ",
        "",
        colorize(glyphs.separator, theme.gutter, capabilities.color_level),
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
                    let _ = writeln!(
                        output,
                        "{gutter}{} {line}",
                        colorize(glyphs.branch, theme.connector, capabilities.color_level)
                    );
                }
            }
        }
        PlacementMode::Stacked => {
            let _ = writeln!(output, "{gutter}{colored}");
            if let Some(message) = annotation.message {
                for line in wrap_text(message, window.geometry.source_columns.saturating_sub(4)) {
                    let _ = writeln!(
                        output,
                        "{gutter}{} = {line}",
                        colorize(glyphs.branch, theme.emphasis, capabilities.color_level)
                    );
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
                    role: annotation.role,
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

fn expand_tabs(text: &str, tab_width: usize) -> String {
    let mut expanded = String::new();
    let tab_width = tab_width.max(1);
    let mut col = 0;

    for ch in text.chars() {
        if ch == '\t' {
            let spaces = tab_width - (col % tab_width);
            expanded.push_str(&" ".repeat(spaces));
            col += spaces;
        } else {
            expanded.push(ch);
            col += UnicodeWidthChar::width(ch).unwrap_or(0);
        }
    }

    expanded
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

fn colorize(text: &str, style: Style, color_level: ColorLevel) -> String {
    if color_level == ColorLevel::None {
        return text.to_string();
    }

    let Some(code) = style.ansi16 else {
        return text.to_string();
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
    role: AnnotationRole,
    severity: Severity,
    priority: u16,
    segment: ResolvedSpan,
}

impl Theme {
    fn style_for_severity(self, severity: Severity) -> Style {
        match severity {
            Severity::Error => self.severity_error,
            Severity::Warning => self.severity_warning,
            Severity::Advice => self.severity_advice,
        }
    }

    fn style_for_note_kind(self, kind: NoteKind) -> Style {
        match kind {
            NoteKind::Note => self.note,
            NoteKind::Help => self.help,
        }
    }

    fn style_for_annotation_role(self, role: AnnotationRole, severity: Severity) -> Style {
        match role {
            AnnotationRole::PrimaryLabel => self.primary_label,
            AnnotationRole::SecondaryLabel => self.secondary_label,
            AnnotationRole::SyntaxToken => self.syntax_token,
            AnnotationRole::Emphasis => self.emphasis,
            AnnotationRole::RelatedLabel
            | AnnotationRole::SearchHighlight
            | AnnotationRole::Selection => self.style_for_severity(severity),
        }
    }

    fn style_for_syntax_class(self, syntax_class: Option<SyntaxClass>) -> Style {
        match syntax_class {
            Some(SyntaxClass::Keyword)
            | Some(SyntaxClass::Operator)
            | Some(SyntaxClass::Macro)
            | Some(SyntaxClass::Namespace)
            | Some(SyntaxClass::Tag)
            | Some(SyntaxClass::Title)
            | Some(SyntaxClass::Strong)
            | Some(SyntaxClass::Emphasis)
            | Some(SyntaxClass::Link)
            | Some(SyntaxClass::Literal)
            | Some(SyntaxClass::Strikethrough) => self.emphasis,
            Some(SyntaxClass::Function) | Some(SyntaxClass::Constructor) => self.syntax_token,
            Some(SyntaxClass::String) | Some(SyntaxClass::DiffAdd) => self.help,
            Some(SyntaxClass::Comment) | Some(SyntaxClass::Punctuation) => self.gutter,
            Some(SyntaxClass::Type) | Some(SyntaxClass::Attribute) => self.secondary_label,
            Some(SyntaxClass::Constant)
            | Some(SyntaxClass::Number)
            | Some(SyntaxClass::Property)
            | Some(SyntaxClass::Label)
            | Some(SyntaxClass::Embedded) => self.severity_advice,
            Some(SyntaxClass::DiffDelete) | Some(SyntaxClass::Error) => self.primary_label,
            Some(SyntaxClass::Variable) | None => Style::plain(),
        }
    }
}
