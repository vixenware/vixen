use std::sync::Arc;

use arborium::advanced::{CompiledGrammar, GrammarConfig};
use arborium::{GrammarStore, Highlighter};
use arborium_highlight::spans_to_themed;
use arborium_theme::theme::builtin;
use ariadne::{Color, Config, Label, Report, ReportKind, Source};
use concolor::ColorChoice;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    concolor::set(ColorChoice::AlwaysAnsi);

    let source = r#"fn build(ws: Path) -> Tree (
  let src_tree = Tree(ws / @zoo/lua-src);
  src_tree
)"#;
    let filename = "build.vx";
    let semantic_text = "@zoo/lua-src";
    let semantic_start = source.find(semantic_text).expect("semantic label start");
    let semantic_range = semantic_start..semantic_start + semantic_text.len();

    let grammar = Arc::new(CompiledGrammar::new(GrammarConfig {
        language: tree_sitter_vixen::language().into(),
        highlights_query: tree_sitter_vixen::HIGHLIGHTS_QUERY,
        injections_query: tree_sitter_vixen::INJECTIONS_QUERY,
        locals_query: tree_sitter_vixen::LOCALS_QUERY,
    })?);

    let store = Arc::new(GrammarStore::new());
    store.insert("vixen", grammar);

    let mut highlighter = Highlighter::with_store(store);
    let raw_spans = highlighter.highlight_spans("vixen", source)?;
    let themed_spans = spans_to_themed(raw_spans);
    let theme = builtin::catppuccin_mocha();

    let mut report = Report::build(ReportKind::Error, (filename, semantic_range.clone()))
        .with_config(Config::default().with_color(true))
        .with_message("build input not found");

    for span in themed_spans {
        if span.start >= span.end {
            continue;
        }

        let Some(style) = theme.style(span.theme_index) else {
            continue;
        };
        let Some(fg) = style.fg else {
            continue;
        };

        report = report.with_label(
            Label::new((filename, span.start as usize..span.end as usize))
                .with_color(Color::Rgb(fg.r, fg.g, fg.b)),
        );
    }

    report
        .with_label(
            Label::new((filename, semantic_range))
                .with_color(Color::Red)
                .with_message("this path does not exist in the current workspace"),
        )
        .with_note("resolved path: /workspace/zoo/lua-src")
        .with_note("workspace root: /workspace")
        .with_note("os error: No such file or directory")
        .finish()
        .print((filename, Source::from(source)))?;

    Ok(())
}
