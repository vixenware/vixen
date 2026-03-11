use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use arborium::advanced::{CompiledGrammar, GrammarConfig};
use arborium::{GrammarStore, Highlighter};
use arborium_highlight::spans_to_themed;
use arborium_theme::theme::builtin;
use ariadne::{Color, Config, Label, Report, ReportKind, Source, sources};
use eyre::{Result, eyre};
use vxd_proto::{
    BuildDiagnosticLabelKind, BuildDiagnosticReport, BuildDiagnosticSeverity,
    BuildDiagnosticSource, BuildDiagnosticSpan, BuildFailure,
};

const SYNTAX_CONTEXT_RADIUS: i32 = 2;

pub(crate) fn render_build_failure(failure: &BuildFailure, full_stack: bool) -> Result<String> {
    if failure.reports.is_empty() {
        return Ok(failure.message.clone());
    }

    let sources_by_id = failure
        .sources
        .iter()
        .map(|source| (source.source_id.as_str(), source))
        .collect::<BTreeMap<_, _>>();
    let mut syntax_highlighter = SyntaxHighlighter::new().ok();
    let mut out = Vec::new();

    let reports = visible_reports(failure, full_stack);

    for (index, report) in reports.iter().enumerate() {
        render_report(
            report,
            &sources_by_id,
            syntax_highlighter.as_mut(),
            &mut out,
        )?;
        if index + 1 != reports.len() {
            out.push(b'\n');
        }
    }

    String::from_utf8(out).map_err(Into::into)
}

fn visible_reports<'a>(
    failure: &'a BuildFailure,
    full_stack: bool,
) -> Vec<&'a BuildDiagnosticReport> {
    if full_stack {
        return failure.reports.iter().collect();
    }

    let mut visible = Vec::new();
    let mut index = 0usize;
    while index < failure.reports.len() {
        let report = &failure.reports[index];
        visible.push(report);
        index += 1;

        if report.severity != BuildDiagnosticSeverity::Error {
            continue;
        }

        let mut advice = Vec::new();
        while index < failure.reports.len()
            && failure.reports[index].severity == BuildDiagnosticSeverity::Advice
        {
            advice.push(&failure.reports[index]);
            index += 1;
        }

        if advice.is_empty() {
            continue;
        }

        if let Some(first_dependency) = advice
            .iter()
            .copied()
            .find(|report| report.message != "while evaluating build entry")
        {
            visible.push(first_dependency);
        }

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

    visible
}

fn render_report(
    report: &BuildDiagnosticReport,
    sources_by_id: &BTreeMap<&str, &BuildDiagnosticSource>,
    syntax_highlighter: Option<&mut SyntaxHighlighter>,
    out: &mut Vec<u8>,
) -> Result<()> {
    let primary_source = sources_by_id
        .get(report.primary_span.source_id.as_str())
        .ok_or_else(|| eyre!("missing source {}", report.primary_span.source_id))?;
    let primary_range = byte_span_to_char_range(&primary_source.text, &report.primary_span)?;

    let mut builder = Report::build(
        report_kind(report.severity),
        (report.primary_span.source_id.clone(), primary_range.clone()),
    )
    .with_config(Config::default().with_color(true))
    .with_message(&report.message);

    let referenced_sources = referenced_source_ids(report);
    let syntax_lines = relevant_syntax_lines(report, sources_by_id)?;
    if let Some(syntax_highlighter) = syntax_highlighter {
        for source_id in referenced_sources {
            let Some(source) = sources_by_id.get(source_id) else {
                continue;
            };
            for label in syntax_highlighter.syntax_labels(source, syntax_lines.get(source_id))? {
                builder = builder.with_label(label);
            }
        }
    }

    for label in &report.labels {
        let source = sources_by_id
            .get(label.span.source_id.as_str())
            .ok_or_else(|| eyre!("missing source {}", label.span.source_id))?;
        let range = byte_span_to_char_range(&source.text, &label.span)?;
        let order = line_order(&source.text, label.span.start)?;
        let mut rendered = Label::new((label.span.source_id.clone(), range))
            .with_order(order)
            .with_priority(label_priority(label.kind))
            .with_color(label_color(report.severity, label.kind));
        if let Some(message) = &label.message {
            rendered = rendered.with_message(message);
        }
        builder = builder.with_label(rendered);
    }

    for note in &report.notes {
        builder = builder.with_note(note);
    }

    let source_cache = sources(
        sources_by_id
            .values()
            .map(|source| (source.source_id.clone(), source.text.as_str()))
            .collect::<Vec<_>>(),
    );
    builder.finish().write(source_cache, out)?;
    Ok(())
}

fn referenced_source_ids(report: &BuildDiagnosticReport) -> BTreeSet<&str> {
    let mut ids = BTreeSet::from([report.primary_span.source_id.as_str()]);
    for label in &report.labels {
        ids.insert(label.span.source_id.as_str());
    }
    ids
}

fn relevant_syntax_lines(
    report: &BuildDiagnosticReport,
    sources_by_id: &BTreeMap<&str, &BuildDiagnosticSource>,
) -> Result<BTreeMap<String, BTreeSet<i32>>> {
    let mut lines = BTreeMap::new();
    add_span_lines(&mut lines, &report.primary_span, sources_by_id)?;
    for label in &report.labels {
        add_span_lines(&mut lines, &label.span, sources_by_id)?;
    }
    Ok(lines)
}

fn add_span_lines(
    lines: &mut BTreeMap<String, BTreeSet<i32>>,
    span: &BuildDiagnosticSpan,
    sources_by_id: &BTreeMap<&str, &BuildDiagnosticSource>,
) -> Result<()> {
    let source = sources_by_id
        .get(span.source_id.as_str())
        .ok_or_else(|| eyre!("missing source {}", span.source_id))?;
    let start = line_order(&source.text, span.start)?;
    let end = line_order(&source.text, span.end)?;
    let entry = lines.entry(span.source_id.clone()).or_default();
    for line in (start.min(end) - SYNTAX_CONTEXT_RADIUS)..=(start.max(end) + SYNTAX_CONTEXT_RADIUS)
    {
        entry.insert(line);
    }
    Ok(())
}

fn report_kind(severity: BuildDiagnosticSeverity) -> ReportKind<'static> {
    match severity {
        BuildDiagnosticSeverity::Error => ReportKind::Error,
        BuildDiagnosticSeverity::Advice => ReportKind::Advice,
    }
}

fn label_priority(kind: BuildDiagnosticLabelKind) -> i32 {
    match kind {
        BuildDiagnosticLabelKind::Primary => 10,
        BuildDiagnosticLabelKind::Secondary => 5,
    }
}

fn label_color(severity: BuildDiagnosticSeverity, kind: BuildDiagnosticLabelKind) -> Color {
    match (severity, kind) {
        (BuildDiagnosticSeverity::Error, BuildDiagnosticLabelKind::Primary) => Color::Red,
        (BuildDiagnosticSeverity::Error, BuildDiagnosticLabelKind::Secondary) => Color::Yellow,
        (BuildDiagnosticSeverity::Advice, BuildDiagnosticLabelKind::Primary) => Color::Cyan,
        (BuildDiagnosticSeverity::Advice, BuildDiagnosticLabelKind::Secondary) => Color::Blue,
    }
}

fn line_order(source: &str, byte_offset: u32) -> Result<i32> {
    let char_offset = byte_offset_to_char_offset(source, byte_offset)?;
    Source::from(source)
        .get_offset_line(char_offset)
        .map(|(_, line, _)| line as i32)
        .ok_or_else(|| eyre!("offset {byte_offset} does not map to a source line"))
}

fn byte_span_to_char_range(
    source: &str,
    span: &BuildDiagnosticSpan,
) -> Result<std::ops::Range<usize>> {
    let start = byte_offset_to_char_offset(source, span.start)?;
    let end = byte_offset_to_char_offset(source, span.end)?;
    Ok(start..end.max(start + usize::from(start == end && !source.is_empty())))
}

fn byte_offset_to_char_offset(source: &str, byte_offset: u32) -> Result<usize> {
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
    Ok(source[..byte_offset].chars().count())
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

    fn syntax_labels(
        &mut self,
        source: &BuildDiagnosticSource,
        relevant_lines: Option<&BTreeSet<i32>>,
    ) -> Result<Vec<Label<(String, std::ops::Range<usize>)>>> {
        if source.language.as_deref() != Some("vixen") {
            return Ok(Vec::new());
        }

        let theme = builtin::catppuccin_mocha();
        let mut labels = Vec::new();
        for span in spans_to_themed(self.highlighter.highlight_spans("vixen", &source.text)?) {
            if span.start >= span.end {
                continue;
            }

            let Some(style) = theme.style(span.theme_index) else {
                continue;
            };
            let Some(fg) = style.fg else {
                continue;
            };

            let start = u32::try_from(span.start)?;
            let end = u32::try_from(span.end)?;
            let order = line_order(&source.text, start)?;
            if let Some(relevant_lines) = relevant_lines
                && !relevant_lines.contains(&order)
            {
                continue;
            }
            let range = byte_span_to_char_range(
                &source.text,
                &BuildDiagnosticSpan {
                    source_id: source.source_id.clone(),
                    start,
                    end,
                },
            )?;
            labels.push(
                Label::new((source.source_id.clone(), range))
                    .with_order(order)
                    .with_priority(0)
                    .with_color(Color::Rgb(fg.r, fg.g, fg.b)),
            );
        }

        Ok(labels)
    }
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

        let rendered = render_build_failure(
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

        println!("{rendered}");
        assert!(rendered.contains("build input not found"), "{rendered}");
        assert!(
            rendered.contains("this path does not exist in the current workspace"),
            "{rendered}"
        );
        assert!(
            rendered.contains("resolved path: /workspace/zoo/lua-src"),
            "{rendered}"
        );
        assert_eq!(rendered.matches("build.vx:").count(), 1, "{rendered}");
        assert!(rendered.contains("\u{1b}["), "{rendered:?}");
    }

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

        let rendered = render_build_failure(
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

        assert!(
            !plain.contains("this_line_should_not_show_up"),
            "{rendered}"
        );
        assert!(plain.contains("let src_tree = Tree"), "{rendered}");
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
            &render_build_failure(&failure, false).expect("render trimmed"),
        );
        assert_eq!(trimmed.matches("Advice:").count(), 2, "{trimmed}");
        assert_eq!(
            trimmed.matches("required by this expression").count(),
            1,
            "{trimmed}"
        );
        assert_eq!(
            trimmed.matches("while evaluating build entry").count(),
            1,
            "{trimmed}"
        );

        let full = strip_ansi_escapes::strip_str(
            &render_build_failure(&failure, true).expect("render full"),
        );
        assert_eq!(full.matches("Advice:").count(), 3, "{full}");
        assert_eq!(
            full.matches("required by this expression").count(),
            2,
            "{full}"
        );
    }
}
