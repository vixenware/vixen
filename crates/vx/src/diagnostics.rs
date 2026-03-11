// d[impl integration.vx-adapter]
// d[impl integration.vx-consumer]
// d[impl integration.no-format-lockin]

use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::sync::Arc;

use arborium::advanced::{CompiledGrammar, GrammarConfig};
use arborium::{GrammarStore, Highlighter};
use arborium_highlight::spans_to_themed;
use arborium_theme::{HIGHLIGHTS, ThemeSlot, capture_to_slot};
use camino::Utf8Path;
use eyre::{Result, eyre};
use margin::{
    Annotation, AnnotationRole, Diagnostics, Note, NoteKind, Report, Severity, Source, SourceId,
    Span, SyntaxClass,
};
use margin_term::{
    ColorLevel, GlyphMode, HyperlinkMode, TerminalCapabilities, Theme as TerminalTheme,
};
use vxd_proto::{
    BuildDiagnosticLabelKind, BuildDiagnosticReport, BuildDiagnosticSeverity,
    BuildDiagnosticSource, BuildDiagnosticSpan, BuildFailure,
};

const SYNTAX_CONTEXT_RADIUS: i32 = 2;
const DEFAULT_TRIMMED_BRANCHES: usize = 2;

pub(crate) fn render_build_failure(failure: &BuildFailure, full_stack: bool) -> Result<String> {
    if failure.reports.is_empty() {
        return Ok(failure.message.clone());
    }

    render_build_failure_with_capabilities(failure, full_stack, detect_terminal_capabilities())
}

fn render_build_failure_with_capabilities(
    failure: &BuildFailure,
    full_stack: bool,
    capabilities: TerminalCapabilities,
) -> Result<String> {
    let diagnostics = build_failure_to_diagnostics(failure, full_stack)?;
    let plan = margin_term::layout(&diagnostics, capabilities)?;
    Ok(margin_term::render_plan_with_theme(
        &plan,
        capabilities,
        TerminalTheme::from_arborium(&arborium_theme::builtin::catppuccin_mocha()),
    ))
}

fn detect_terminal_capabilities() -> TerminalCapabilities {
    let stderr_is_terminal = std::io::stderr().is_terminal();
    TerminalCapabilities {
        width: detect_terminal_width(stderr_is_terminal),
        glyph_mode: GlyphMode::Unicode,
        color_level: detect_color_level(stderr_is_terminal),
        hyperlink_mode: detect_hyperlink_mode(stderr_is_terminal),
        tab_width: 4,
    }
}

fn detect_terminal_width(stderr_is_terminal: bool) -> usize {
    if let Ok(columns) = std::env::var("COLUMNS")
        && let Ok(width) = columns.parse::<usize>()
        && width > 0
    {
        return width;
    }

    if stderr_is_terminal {
        #[cfg(unix)]
        {
            let mut winsize: libc::winsize = unsafe { std::mem::zeroed() };
            let result =
                unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut winsize) };
            if result == 0 && winsize.ws_col > 0 {
                return winsize.ws_col as usize;
            }
        }
    }

    80
}

fn detect_color_level(stderr_is_terminal: bool) -> ColorLevel {
    if !stderr_is_terminal || std::env::var_os("NO_COLOR").is_some() {
        return ColorLevel::None;
    }

    let colorterm = std::env::var("COLORTERM").ok();
    let term = std::env::var("TERM").ok();
    if colorterm
        .as_deref()
        .is_some_and(|value| value.contains("truecolor") || value.contains("24bit"))
        || term.as_deref().is_some_and(|value| {
            value.contains("direct") || value.contains("kitty") || value.contains("wezterm")
        })
    {
        ColorLevel::Rgb24
    } else {
        ColorLevel::Ansi16
    }
}

fn detect_hyperlink_mode(stderr_is_terminal: bool) -> HyperlinkMode {
    if !stderr_is_terminal
        || std::env::var_os("NO_HYPERLINK").is_some()
        || std::env::var("TERM").ok().as_deref() == Some("dumb")
    {
        return HyperlinkMode::None;
    }

    let term_program = std::env::var("TERM_PROGRAM").ok();
    let vte_version = std::env::var("VTE_VERSION")
        .ok()
        .and_then(|value| value.parse::<u32>().ok());
    let term = std::env::var("TERM").ok();

    if std::env::var_os("WT_SESSION").is_some()
        || std::env::var_os("KONSOLE_VERSION").is_some()
        || std::env::var_os("DOMTERM").is_some()
        || std::env::var_os("GHOSTTY_RESOURCES_DIR").is_some()
        || term_program.as_deref().is_some_and(|program| {
            matches!(
                program,
                "iTerm.app" | "WezTerm" | "vscode" | "Hyper" | "Ghostty"
            )
        })
        || vte_version.is_some_and(|version| version >= 5000)
        || term
            .as_deref()
            .is_some_and(|value| value.contains("xterm-kitty"))
    {
        HyperlinkMode::Osc8
    } else {
        HyperlinkMode::None
    }
}

fn build_failure_to_diagnostics(failure: &BuildFailure, full_stack: bool) -> Result<Diagnostics> {
    let workspace_root = workspace_root_note(failure);
    let sources = failure
        .sources
        .iter()
        .map(|source| Source {
            id: SourceId(source.source_id.clone()),
            name: source.source_id.clone(),
            hyperlink: source_hyperlink(source.source_id.as_str(), workspace_root),
            text: source.text.clone(),
        })
        .collect::<Vec<_>>();
    let source_map = failure
        .sources
        .iter()
        .map(|source| (source.source_id.as_str(), source))
        .collect::<BTreeMap<_, _>>();
    let visible_reports = visible_reports(failure, full_stack);
    let syntax_lines = visible_reports.iter().try_fold(
        BTreeMap::<String, BTreeSet<i32>>::new(),
        |mut lines, report| {
            add_span_lines(&mut lines, &report.primary_span, &source_map)?;
            for label in &report.labels {
                add_span_lines(&mut lines, &label.span, &source_map)?;
            }
            Ok::<_, eyre::Report>(lines)
        },
    )?;
    let mut syntax_highlighter = SyntaxHighlighter::new().ok();

    let reports = visible_reports
        .iter()
        .map(|report| {
            build_report(
                report,
                &source_map,
                &syntax_lines,
                syntax_highlighter.as_mut(),
            )
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(Diagnostics { sources, reports })
}

fn visible_reports(failure: &BuildFailure, full_stack: bool) -> Vec<&BuildDiagnosticReport> {
    let mut visible = Vec::new();
    let mut index = 0usize;
    while index < failure.reports.len() {
        let report = &failure.reports[index];
        if report.severity != BuildDiagnosticSeverity::Error {
            visible.push(report);
            index += 1;
            continue;
        }

        let error = report;
        index += 1;
        let mut advice = Vec::new();
        while index < failure.reports.len()
            && failure.reports[index].severity == BuildDiagnosticSeverity::Advice
        {
            advice.push(&failure.reports[index]);
            index += 1;
        }

        if full_stack {
            visible.extend(advice.iter().copied());
            visible.push(error);
            continue;
        }

        if !advice.is_empty() {
            let mut kept_dependencies: Vec<&BuildDiagnosticReport> = Vec::new();
            for dependency in advice
                .iter()
                .copied()
                .filter(|report| report.message != "while evaluating build entry")
            {
                if kept_dependencies
                    .iter()
                    .any(|existing| same_report_site(existing, dependency))
                {
                    continue;
                }
                kept_dependencies.push(dependency);
                if kept_dependencies.len() == DEFAULT_TRIMMED_BRANCHES {
                    break;
                }
            }
            visible.extend(kept_dependencies);

            if let Some(build_entry) = advice
                .iter()
                .copied()
                .rev()
                .find(|report| report.message == "while evaluating build entry")
                && visible
                    .last()
                    .is_none_or(|last| !std::ptr::eq(*last, build_entry))
            {
                visible.push(build_entry);
            }
        }

        visible.push(error);
    }

    visible
}

fn same_report_site(left: &BuildDiagnosticReport, right: &BuildDiagnosticReport) -> bool {
    left.message == right.message
        && left.primary_span.source_id == right.primary_span.source_id
        && left.primary_span.start == right.primary_span.start
        && left.primary_span.end == right.primary_span.end
}

fn workspace_root_note(failure: &BuildFailure) -> Option<&str> {
    failure
        .reports
        .iter()
        .flat_map(|report| report.notes.iter())
        .find_map(|note| note.strip_prefix("workspace root: "))
}

fn source_hyperlink(source_id: &str, workspace_root: Option<&str>) -> Option<String> {
    let path = if Utf8Path::new(source_id).is_absolute() {
        source_id.to_string()
    } else {
        let root = workspace_root?;
        Utf8Path::new(root).join(source_id).into_string()
    };

    Some(file_uri(path.as_str()))
}

fn file_uri(path: &str) -> String {
    let mut uri = String::from("file://");
    for byte in path.bytes() {
        let ch = byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '/' | '-' | '_' | '.' | '~') {
            uri.push(ch);
        } else {
            uri.push('%');
            uri.push_str(format!("{byte:02X}").as_str());
        }
    }
    uri
}

fn build_report(
    report: &BuildDiagnosticReport,
    sources_by_id: &BTreeMap<&str, &BuildDiagnosticSource>,
    syntax_lines: &BTreeMap<String, BTreeSet<i32>>,
    syntax_highlighter: Option<&mut SyntaxHighlighter>,
) -> Result<Report> {
    let mut annotations = Vec::new();
    if let Some(syntax_highlighter) = syntax_highlighter {
        for source_id in referenced_source_ids(report) {
            let Some(source) = sources_by_id.get(source_id) else {
                continue;
            };
            annotations.extend(
                syntax_highlighter.syntax_annotations(source, syntax_lines.get(source_id))?,
            );
        }
    }

    if !report
        .labels
        .iter()
        .any(|label| label.kind == BuildDiagnosticLabelKind::Primary)
    {
        annotations.push(annotation_from_span(
            &report.primary_span,
            AnnotationRole::PrimaryLabel,
            None,
            100,
            sources_by_id,
        )?);
    }

    for label in &report.labels {
        annotations.push(annotation_from_span(
            &label.span,
            label_role(label.kind),
            label.message.clone(),
            label_priority(label.kind),
            sources_by_id,
        )?);
    }

    Ok(Report {
        severity: report_severity(report.severity),
        title: report.message.clone(),
        annotations,
        notes: report
            .notes
            .iter()
            .map(|note| Note {
                kind: note_kind(note),
                text: note_text(note),
            })
            .collect(),
        sections: Vec::new(),
    })
}

fn annotation_from_span(
    span: &BuildDiagnosticSpan,
    role: AnnotationRole,
    message: Option<String>,
    priority: u16,
    sources_by_id: &BTreeMap<&str, &BuildDiagnosticSource>,
) -> Result<Annotation> {
    let source = sources_by_id
        .get(span.source_id.as_str())
        .ok_or_else(|| eyre!("missing source {}", span.source_id))?;
    let start = byte_offset_to_display_boundary(&source.text, span.start)?;
    let end = byte_offset_to_display_boundary(&source.text, span.end)?;
    Ok(Annotation {
        spans: vec![Span::new(span.source_id.as_str(), start, end)],
        role,
        syntax_class: None,
        message,
        priority,
    })
}

fn report_severity(severity: BuildDiagnosticSeverity) -> Severity {
    match severity {
        BuildDiagnosticSeverity::Error => Severity::Error,
        BuildDiagnosticSeverity::Advice => Severity::Advice,
    }
}

fn label_role(kind: BuildDiagnosticLabelKind) -> AnnotationRole {
    match kind {
        BuildDiagnosticLabelKind::Primary => AnnotationRole::PrimaryLabel,
        BuildDiagnosticLabelKind::Secondary => AnnotationRole::SecondaryLabel,
    }
}

fn label_priority(kind: BuildDiagnosticLabelKind) -> u16 {
    match kind {
        BuildDiagnosticLabelKind::Primary => 100,
        BuildDiagnosticLabelKind::Secondary => 50,
    }
}

fn note_kind(note: &str) -> NoteKind {
    if note.starts_with("help:") {
        NoteKind::Help
    } else {
        NoteKind::Note
    }
}

fn note_text(note: &str) -> String {
    note.strip_prefix("help:")
        .map(str::trim_start)
        .unwrap_or(note)
        .to_string()
}

fn referenced_source_ids(report: &BuildDiagnosticReport) -> BTreeSet<&str> {
    let mut ids = BTreeSet::from([report.primary_span.source_id.as_str()]);
    for label in &report.labels {
        ids.insert(label.span.source_id.as_str());
    }
    ids
}

fn add_span_lines(
    lines: &mut BTreeMap<String, BTreeSet<i32>>,
    span: &BuildDiagnosticSpan,
    sources_by_id: &BTreeMap<&str, &BuildDiagnosticSource>,
) -> Result<()> {
    let source = sources_by_id
        .get(span.source_id.as_str())
        .ok_or_else(|| eyre!("missing source {}", span.source_id))?;
    let start = line_number(&source.text, span.start)?;
    let end = line_number(&source.text, span.end)?;
    let entry = lines.entry(span.source_id.clone()).or_default();
    for line in (start.min(end) - SYNTAX_CONTEXT_RADIUS)..=(start.max(end) + SYNTAX_CONTEXT_RADIUS)
    {
        entry.insert(line);
    }
    Ok(())
}

fn line_number(source: &str, byte_offset: u32) -> Result<i32> {
    let byte_offset = usize::try_from(byte_offset)?;
    if byte_offset > source.len() {
        return Err(eyre!(
            "byte offset {byte_offset} is out of bounds for source of length {}",
            source.len()
        ));
    }
    if !source.is_char_boundary(byte_offset) {
        return Err(eyre!("byte offset {byte_offset} is not at a char boundary"));
    }
    Ok(source[..byte_offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count() as i32
        + 1)
}

fn byte_offset_to_display_boundary(source: &str, byte_offset: u32) -> Result<usize> {
    let byte_offset = usize::try_from(byte_offset)?;
    if byte_offset > source.len() {
        return Err(eyre!(
            "byte offset {byte_offset} is out of bounds for source of length {}",
            source.len()
        ));
    }
    if !source.is_char_boundary(byte_offset) {
        return Err(eyre!("byte offset {byte_offset} is not at a char boundary"));
    }
    Ok(byte_offset)
}

struct SyntaxHighlighter {
    highlighter: Highlighter,
}

impl SyntaxHighlighter {
    fn new() -> Result<Self> {
        let grammar = Arc::new(CompiledGrammar::new(GrammarConfig {
            language: tree_sitter_vixen::language().into(),
            highlights_query: tree_sitter_vixen::HIGHLIGHTS_QUERY,
            injections_query: tree_sitter_vixen::INJECTIONS_QUERY,
            locals_query: tree_sitter_vixen::LOCALS_QUERY,
        })?);

        let store = Arc::new(GrammarStore::new());
        store.insert("vixen", grammar);

        Ok(Self {
            highlighter: Highlighter::with_store(store),
        })
    }

    fn syntax_annotations(
        &mut self,
        source: &BuildDiagnosticSource,
        relevant_lines: Option<&BTreeSet<i32>>,
    ) -> Result<Vec<Annotation>> {
        if source.language.as_deref() != Some("vixen") {
            return Ok(Vec::new());
        }

        let mut annotations = Vec::new();
        for span in spans_to_themed(self.highlighter.highlight_spans("vixen", &source.text)?) {
            if span.start >= span.end {
                continue;
            }
            let start_line = line_number(&source.text, span.start)?;
            if let Some(relevant_lines) = relevant_lines
                && !relevant_lines.contains(&start_line)
            {
                continue;
            }

            let start = byte_offset_to_display_boundary(&source.text, span.start)?;
            let end = byte_offset_to_display_boundary(&source.text, span.end)?;
            annotations.push(Annotation {
                spans: vec![Span::new(source.source_id.as_str(), start, end)],
                role: AnnotationRole::SyntaxToken,
                syntax_class: syntax_class_for_theme_index(span.theme_index),
                message: None,
                priority: 0,
            });
        }

        Ok(annotations)
    }
}

fn syntax_class_for_theme_index(theme_index: usize) -> Option<SyntaxClass> {
    let highlight = HIGHLIGHTS.get(theme_index)?;
    let slot = capture_to_slot(highlight.name);
    Some(match slot {
        ThemeSlot::Keyword => SyntaxClass::Keyword,
        ThemeSlot::Function => SyntaxClass::Function,
        ThemeSlot::String => SyntaxClass::String,
        ThemeSlot::Comment => SyntaxClass::Comment,
        ThemeSlot::Type => SyntaxClass::Type,
        ThemeSlot::Variable => SyntaxClass::Variable,
        ThemeSlot::Constant => SyntaxClass::Constant,
        ThemeSlot::Number => SyntaxClass::Number,
        ThemeSlot::Operator => SyntaxClass::Operator,
        ThemeSlot::Punctuation => SyntaxClass::Punctuation,
        ThemeSlot::Property => SyntaxClass::Property,
        ThemeSlot::Attribute => SyntaxClass::Attribute,
        ThemeSlot::Tag => SyntaxClass::Tag,
        ThemeSlot::Macro => SyntaxClass::Macro,
        ThemeSlot::Label => SyntaxClass::Label,
        ThemeSlot::Namespace => SyntaxClass::Namespace,
        ThemeSlot::Constructor => SyntaxClass::Constructor,
        ThemeSlot::Title => SyntaxClass::Title,
        ThemeSlot::Strong => SyntaxClass::Strong,
        ThemeSlot::Emphasis => SyntaxClass::Emphasis,
        ThemeSlot::Link => SyntaxClass::Link,
        ThemeSlot::Literal => SyntaxClass::Literal,
        ThemeSlot::Strikethrough => SyntaxClass::Strikethrough,
        ThemeSlot::DiffAdd => SyntaxClass::DiffAdd,
        ThemeSlot::DiffDelete => SyntaxClass::DiffDelete,
        ThemeSlot::Embedded => SyntaxClass::Embedded,
        ThemeSlot::Error => SyntaxClass::Error,
        ThemeSlot::None => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use concolor::ColorChoice;
    use std::sync::{Mutex, MutexGuard};
    use vxd_proto::{
        BuildDiagnosticLabel, BuildDiagnosticLabelKind, BuildDiagnosticReport,
        BuildDiagnosticSeverity, BuildDiagnosticSource, BuildDiagnosticSpan, BuildFailure,
    };

    static COLOR_OVERRIDE_LOCK: Mutex<()> = Mutex::new(());
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct ColorOverrideGuard {
        _guard: MutexGuard<'static, ()>,
    }

    impl ColorOverrideGuard {
        fn always_ansi() -> Self {
            let guard = COLOR_OVERRIDE_LOCK.lock().expect("color override lock");
            concolor::set(ColorChoice::AlwaysAnsi);
            Self { _guard: guard }
        }
    }

    impl Drop for ColorOverrideGuard {
        fn drop(&mut self) {
            concolor::set(ColorChoice::Auto);
        }
    }

    fn strip_osc8_hyperlinks(text: &str) -> String {
        let mut stripped = String::with_capacity(text.len());
        let mut remainder = text;
        let open = "\u{1b}]8;;";
        let st = "\u{1b}\\";

        while let Some(start) = remainder.find(open) {
            stripped.push_str(&remainder[..start]);
            let after_open = &remainder[start + open.len()..];
            let Some(uri_end) = after_open.find(st) else {
                stripped.push_str(&remainder[start..]);
                return stripped;
            };
            let visible = &after_open[uri_end + st.len()..];
            let Some(close) = visible.find(open) else {
                stripped.push_str(visible);
                return stripped;
            };
            stripped.push_str(&visible[..close]);
            let after_close = &visible[close + open.len()..];
            let Some(close_end) = after_close.find(st) else {
                return stripped;
            };
            remainder = &after_close[close_end + st.len()..];
        }

        stripped.push_str(remainder);
        stripped
    }

    fn test_terminal_capabilities() -> TerminalCapabilities {
        TerminalCapabilities {
            width: 80,
            glyph_mode: GlyphMode::Unicode,
            color_level: ColorLevel::Rgb24,
            hyperlink_mode: HyperlinkMode::Osc8,
            tab_width: 4,
        }
    }

    fn render_test_build_failure(failure: &BuildFailure, full_stack: bool) -> Result<String> {
        render_build_failure_with_capabilities(failure, full_stack, test_terminal_capabilities())
    }

    fn with_env_var<T>(key: &str, value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let previous = std::env::var_os(key);
        match value {
            Some(value) => unsafe { std::env::set_var(key, value) },
            None => unsafe { std::env::remove_var(key) },
        }
        let result = f();
        match previous {
            Some(previous) => unsafe { std::env::set_var(key, previous) },
            None => unsafe { std::env::remove_var(key) },
        }
        result
    }

    // d[impl test.vixen-acceptance-fixtures]
    // d[verify product.compiler-grade]
    // d[verify integration.vx-adapter]
    // d[verify integration.vx-consumer]
    // d[verify integration.no-format-lockin]
    // d[verify test.vixen-acceptance-fixtures]
    #[test]
    fn render_build_failure_keeps_one_section_and_error_priority() {
        let _color = ColorOverrideGuard::always_ansi();
        let source = r#"fn build(ws: Path) -> Tree (
  let src_tree = Tree(ws / @zoo/lua-src);
  src_tree
)"#;
        let source_id = "build.vx";
        let label_text = "@zoo/lua-src";
        let start = source.find(label_text).expect("label start");
        let end = start + label_text.len();

        let rendered = render_test_build_failure(
            &BuildFailure {
                message: "build input not found".to_string(),
                sources: vec![BuildDiagnosticSource {
                    source_id: source_id.to_string(),
                    language: Some("vixen".to_string()),
                    text: source.to_string(),
                }],
                reports: vec![BuildDiagnosticReport {
                    severity: BuildDiagnosticSeverity::Error,
                    message: "build input not found".to_string(),
                    primary_span: BuildDiagnosticSpan {
                        source_id: source_id.to_string(),
                        start: start as u32,
                        end: end as u32,
                    },
                    labels: vec![BuildDiagnosticLabel {
                        span: BuildDiagnosticSpan {
                            source_id: source_id.to_string(),
                            start: start as u32,
                            end: end as u32,
                        },
                        kind: BuildDiagnosticLabelKind::Primary,
                        message: Some(
                            "this path does not exist in the current workspace".to_string(),
                        ),
                    }],
                    notes: vec![
                        "resolved path: /workspace/zoo/lua-src".to_string(),
                        "workspace root: /workspace".to_string(),
                        "os error: No such file or directory".to_string(),
                    ],
                }],
            },
            false,
        )
        .expect("render");
        let visible = strip_osc8_hyperlinks(&rendered);
        let plain = strip_ansi_escapes::strip_str(&visible);

        println!("{rendered}");
        assert!(rendered.contains("build input not found"), "{rendered}");
        assert!(
            rendered.contains("\u{1b}]8;;file:///workspace/build.vx\u{1b}\\"),
            "{rendered}"
        );
        assert!(
            plain.contains("this path does not exist in the current"),
            "{rendered}"
        );
        assert!(plain.contains("workspace"), "{rendered}");
        assert!(
            plain.contains("resolved path: /workspace/zoo/lua-src"),
            "{rendered}"
        );
        assert_eq!(plain.matches("build.vx").count(), 1, "{rendered}");
        assert!(rendered.contains("\u{1b}["), "{rendered:?}");
    }

    // d[verify integration.vx-consumer]
    // d[verify test.vixen-acceptance-fixtures]
    #[test]
    fn render_build_failure_does_not_expand_to_the_entire_file() {
        let _color = ColorOverrideGuard::always_ansi();
        let source = indoc::indoc! {r#"
            fn build(ws: Path) -> Tree (
              let this_line_should_not_show_up = "sentinel";
              let objects = Tree(ws / @ok/input);
              let toolchain = Toolchain("stable");
              let src_tree = Tree(ws / @zoo/lua-src);
              src_tree
            )
        "#};
        let source_id = "build.vx";
        let label_text = "@zoo/lua-src";
        let start = source.find(label_text).expect("label start");
        let end = start + label_text.len();

        let rendered = render_test_build_failure(
            &BuildFailure {
                message: "build input not found".to_string(),
                sources: vec![BuildDiagnosticSource {
                    source_id: source_id.to_string(),
                    language: Some("vixen".to_string()),
                    text: source.to_string(),
                }],
                reports: vec![BuildDiagnosticReport {
                    severity: BuildDiagnosticSeverity::Error,
                    message: "build input not found".to_string(),
                    primary_span: BuildDiagnosticSpan {
                        source_id: source_id.to_string(),
                        start: start as u32,
                        end: end as u32,
                    },
                    labels: vec![BuildDiagnosticLabel {
                        span: BuildDiagnosticSpan {
                            source_id: source_id.to_string(),
                            start: start as u32,
                            end: end as u32,
                        },
                        kind: BuildDiagnosticLabelKind::Primary,
                        message: Some(
                            "this path does not exist in the current workspace".to_string(),
                        ),
                    }],
                    notes: Vec::new(),
                }],
            },
            false,
        )
        .expect("render");
        let plain = strip_ansi_escapes::strip_str(&rendered);

        println!("{rendered}");
        assert!(
            !plain.contains("this_line_should_not_show_up"),
            "{rendered}"
        );
        assert!(plain.contains("let src_tree = Tree"), "{rendered}");
        assert!(
            plain.contains("this path does not exist in the current"),
            "{rendered}"
        );
        assert!(plain.contains("workspace"), "{rendered}");
    }

    #[test]
    fn render_build_failure_trims_default_stack() {
        let source = indoc::indoc! {r#"
            fn build() {
              first();
              second();
              third();
            }
        "#};
        let source_id = "build.vx";
        let source_start = source.find("first()").expect("first");
        let entry_start = source.find("third()").expect("third");

        let failure = BuildFailure {
            message: "build failed".to_string(),
            sources: vec![BuildDiagnosticSource {
                source_id: source_id.to_string(),
                language: Some("vixen".to_string()),
                text: source.to_string(),
            }],
            reports: vec![
                BuildDiagnosticReport {
                    severity: BuildDiagnosticSeverity::Error,
                    message: "build failed".to_string(),
                    primary_span: BuildDiagnosticSpan {
                        source_id: source_id.to_string(),
                        start: source_start as u32,
                        end: source_start as u32 + 5,
                    },
                    labels: vec![BuildDiagnosticLabel {
                        span: BuildDiagnosticSpan {
                            source_id: source_id.to_string(),
                            start: source_start as u32,
                            end: source_start as u32 + 5,
                        },
                        kind: BuildDiagnosticLabelKind::Primary,
                        message: Some("this expression failed".to_string()),
                    }],
                    notes: Vec::new(),
                },
                BuildDiagnosticReport {
                    severity: BuildDiagnosticSeverity::Advice,
                    message: "required by this expression".to_string(),
                    primary_span: BuildDiagnosticSpan {
                        source_id: source_id.to_string(),
                        start: source_start as u32,
                        end: source_start as u32 + 5,
                    },
                    labels: vec![],
                    notes: Vec::new(),
                },
                BuildDiagnosticReport {
                    severity: BuildDiagnosticSeverity::Advice,
                    message: "required by this expression".to_string(),
                    primary_span: BuildDiagnosticSpan {
                        source_id: source_id.to_string(),
                        start: source_start as u32 + 9,
                        end: source_start as u32 + 15,
                    },
                    labels: vec![],
                    notes: Vec::new(),
                },
                BuildDiagnosticReport {
                    severity: BuildDiagnosticSeverity::Advice,
                    message: "while evaluating build entry".to_string(),
                    primary_span: BuildDiagnosticSpan {
                        source_id: source_id.to_string(),
                        start: entry_start as u32,
                        end: entry_start as u32 + 5,
                    },
                    labels: vec![],
                    notes: Vec::new(),
                },
            ],
        };

        let trimmed = strip_ansi_escapes::strip_str(
            render_test_build_failure(&failure, false).expect("render trimmed"),
        );
        assert_eq!(trimmed.matches("advice:").count(), 3, "{trimmed}");
        assert_eq!(
            trimmed.matches("required by this expression").count(),
            2,
            "{trimmed}"
        );
        assert_eq!(
            trimmed.matches("while evaluating build entry").count(),
            1,
            "{trimmed}"
        );

        let full = strip_ansi_escapes::strip_str(
            render_test_build_failure(&failure, true).expect("render full"),
        );
        assert_eq!(full.matches("advice:").count(), 3, "{full}");
        assert_eq!(
            full.matches("required by this expression").count(),
            2,
            "{full}"
        );
    }

    #[test]
    fn render_build_failure_places_terminal_error_after_context() {
        let source = indoc::indoc! {r#"
            fn build() {
              first();
              second();
              third();
            }
        "#};
        let source_id = "build.vx";
        let source_start = source.find("first()").expect("first");
        let entry_start = source.find("third()").expect("third");

        let failure = BuildFailure {
            message: "build failed".to_string(),
            sources: vec![BuildDiagnosticSource {
                source_id: source_id.to_string(),
                language: Some("vixen".to_string()),
                text: source.to_string(),
            }],
            reports: vec![
                BuildDiagnosticReport {
                    severity: BuildDiagnosticSeverity::Error,
                    message: "build failed".to_string(),
                    primary_span: BuildDiagnosticSpan {
                        source_id: source_id.to_string(),
                        start: source_start as u32,
                        end: source_start as u32 + 5,
                    },
                    labels: vec![BuildDiagnosticLabel {
                        span: BuildDiagnosticSpan {
                            source_id: source_id.to_string(),
                            start: source_start as u32,
                            end: source_start as u32 + 5,
                        },
                        kind: BuildDiagnosticLabelKind::Primary,
                        message: Some("this expression failed".to_string()),
                    }],
                    notes: Vec::new(),
                },
                BuildDiagnosticReport {
                    severity: BuildDiagnosticSeverity::Advice,
                    message: "required by this expression".to_string(),
                    primary_span: BuildDiagnosticSpan {
                        source_id: source_id.to_string(),
                        start: source_start as u32,
                        end: source_start as u32 + 5,
                    },
                    labels: vec![],
                    notes: Vec::new(),
                },
                BuildDiagnosticReport {
                    severity: BuildDiagnosticSeverity::Advice,
                    message: "while evaluating build entry".to_string(),
                    primary_span: BuildDiagnosticSpan {
                        source_id: source_id.to_string(),
                        start: entry_start as u32,
                        end: entry_start as u32 + 5,
                    },
                    labels: vec![],
                    notes: Vec::new(),
                },
            ],
        };

        let trimmed = strip_ansi_escapes::strip_str(
            render_test_build_failure(&failure, false).expect("render trimmed"),
        );
        let advice_pos = trimmed.find("advice:").expect("advice position");
        let error_pos = trimmed.rfind("error:").expect("error position");
        assert!(advice_pos < error_pos, "{trimmed}");

        let full = strip_ansi_escapes::strip_str(
            render_test_build_failure(&failure, true).expect("render full"),
        );
        let advice_pos = full.find("advice:").expect("advice position");
        let error_pos = full.rfind("error:").expect("error position");
        assert!(advice_pos < error_pos, "{full}");
    }

    #[test]
    fn render_build_failure_multiline_primary_message_only_once() {
        let _color = ColorOverrideGuard::always_ansi();
        let source = indoc::indoc! {r#"
            fn compile(file: Path) -> Tree (
              cexec [
                cc,
                @(src/{file}),
              ]
            )
        "#};
        let source_id = "build.vx";
        let start = source.find("fn compile").expect("compile start");
        let end = source.rfind(")").expect("compile end") + 1;

        let rendered = render_test_build_failure(
            &BuildFailure {
                message: "build command inputs not found".to_string(),
                sources: vec![BuildDiagnosticSource {
                    source_id: source_id.to_string(),
                    language: Some("vixen".to_string()),
                    text: source.to_string(),
                }],
                reports: vec![BuildDiagnosticReport {
                    severity: BuildDiagnosticSeverity::Error,
                    message: "build command inputs not found".to_string(),
                    primary_span: BuildDiagnosticSpan {
                        source_id: source_id.to_string(),
                        start: start as u32,
                        end: end as u32,
                    },
                    labels: vec![BuildDiagnosticLabel {
                        span: BuildDiagnosticSpan {
                            source_id: source_id.to_string(),
                            start: start as u32,
                            end: end as u32,
                        },
                        kind: BuildDiagnosticLabelKind::Primary,
                        message: Some(
                            "this source site produced input paths that were not present in the sandbox"
                                .to_string(),
                        ),
                    }],
                    notes: Vec::new(),
                }],
            },
            false,
        )
        .expect("render");
        let plain = strip_ansi_escapes::strip_str(strip_osc8_hyperlinks(&rendered));

        println!("{rendered}");
        assert_eq!(
            plain
                .matches("this source site produced input paths")
                .count(),
            1,
            "{rendered}"
        );
    }

    #[test]
    fn terminal_width_prefers_columns_env_var() {
        let _env = ENV_LOCK.lock().expect("env lock");
        with_env_var("COLUMNS", Some("123"), || {
            assert_eq!(detect_terminal_width(false), 123);
        });
    }

    #[test]
    fn detected_terminal_capabilities_respect_env() {
        let _env = ENV_LOCK.lock().expect("env lock");
        with_env_var("NO_COLOR", None, || {
            with_env_var("NO_HYPERLINK", None, || {
                with_env_var("TERM", Some("xterm-256color"), || {
                    with_env_var("COLORTERM", Some("truecolor"), || {
                        with_env_var("WT_SESSION", Some("1"), || {
                            assert_eq!(detect_color_level(true), ColorLevel::Rgb24);
                            assert_eq!(detect_hyperlink_mode(true), HyperlinkMode::Osc8);
                        })
                    })
                })
            })
        });

        with_env_var("NO_COLOR", Some("1"), || {
            assert_eq!(detect_color_level(true), ColorLevel::None);
        });

        with_env_var("NO_HYPERLINK", Some("1"), || {
            assert_eq!(detect_hyperlink_mode(true), HyperlinkMode::None);
        });
    }

    #[test]
    fn render_build_failure_keeps_grouped_compile_branches_visible() {
        let _color = ColorOverrideGuard::always_ansi();
        let source = indoc::indoc! {r#"
            fn compile(file: Path) -> Tree (
              cexec [
                cc,
                @(src/{file}),
              ]
            )

            fn build() -> Tree (
              let liblua = archive(objects);
              let lua_exe = link(Tree [compile @lua.c, liblua]);
              Tree {
                @bin/lua: lua_exe.file(@bin/lua).executable(),
                @lib/liblua.a: liblua.file(@liblua.a),
              }
            )
        "#};
        let source_id = "build.vx";
        let compile_start = source.find("compile(file: Path)").expect("compile");
        let objects_start = source.find("archive(objects)").expect("objects");
        let lua_exe_start = source
            .find("link(Tree [compile @lua.c, liblua])")
            .expect("lua_exe");

        let rendered = render_test_build_failure(
            &BuildFailure {
                message: "build command inputs not found".to_string(),
                sources: vec![BuildDiagnosticSource {
                    source_id: source_id.to_string(),
                    language: Some("vixen".to_string()),
                    text: source.to_string(),
                }],
                reports: vec![
                    BuildDiagnosticReport {
                        severity: BuildDiagnosticSeverity::Advice,
                        message: "required by `objects`".to_string(),
                        primary_span: BuildDiagnosticSpan {
                            source_id: source_id.to_string(),
                            start: objects_start as u32,
                            end: (objects_start + "archive(objects)".len()) as u32,
                        },
                        labels: vec![],
                        notes: Vec::new(),
                    },
                    BuildDiagnosticReport {
                        severity: BuildDiagnosticSeverity::Advice,
                        message: "required by `lua_exe`".to_string(),
                        primary_span: BuildDiagnosticSpan {
                            source_id: source_id.to_string(),
                            start: lua_exe_start as u32,
                            end: (lua_exe_start + "link(Tree [compile @lua.c, liblua])".len())
                                as u32,
                        },
                        labels: vec![],
                        notes: Vec::new(),
                    },
                    BuildDiagnosticReport {
                        severity: BuildDiagnosticSeverity::Error,
                        message: "build command inputs not found".to_string(),
                        primary_span: BuildDiagnosticSpan {
                            source_id: source_id.to_string(),
                            start: compile_start as u32,
                            end: (compile_start + "compile(file: Path)".len()) as u32,
                        },
                        labels: vec![BuildDiagnosticLabel {
                            span: BuildDiagnosticSpan {
                                source_id: source_id.to_string(),
                                start: compile_start as u32,
                                end: (compile_start + "compile(file: Path)".len()) as u32,
                            },
                            kind: BuildDiagnosticLabelKind::Primary,
                            message: Some(
                                "this source site produced input paths that were not present in the sandbox"
                                    .to_string(),
                            ),
                        }],
                        notes: vec![
                            "2 commands failed with the same missing-input pattern".to_string(),
                            "missing process inputs: src/lapi.c, src/lua.c".to_string(),
                            "similar sandbox paths: src/src/lapi.c, src/src/lua.c".to_string(),
                            "workspace root: /workspace".to_string(),
                        ],
                    },
                ],
            },
            false,
        )
        .expect("render");
        let plain = strip_ansi_escapes::strip_str(strip_osc8_hyperlinks(&rendered));

        println!("{rendered}");
        assert!(plain.contains("required by `objects`"), "{rendered}");
        assert!(plain.contains("required by `lua_exe`"), "{rendered}");
        assert!(
            plain.contains("build command inputs not found"),
            "{rendered}"
        );
        assert!(plain.contains("compile(file: Path)"), "{rendered}");
        assert!(
            plain.contains("missing process inputs: src/lapi.c, src/lua.c"),
            "{rendered}"
        );
    }
}
