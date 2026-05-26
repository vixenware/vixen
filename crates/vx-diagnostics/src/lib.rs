//! Shared Vixen diagnostics rendering helpers.

mod render;

pub use render::render_compile_error;

use std::collections::BTreeSet;
use std::io::IsTerminal;
use std::sync::Arc;

use arborium::advanced::{CompiledGrammar, GrammarConfig};
use arborium::{GrammarStore, Highlighter};
use arborium_highlight::spans_to_themed;
use arborium_theme::{HIGHLIGHTS, ThemeSlot, capture_to_slot};
use eyre::{Result, eyre};
use margin::LayoutError;
use margin::{Annotation, AnnotationRole, Diagnostics, Span, SyntaxClass};
use margin_term::{
    ColorLevel, GlyphMode, HyperlinkMode, TerminalCapabilities, Theme as TerminalTheme,
};

pub fn render_diagnostics_with_default_theme(
    diagnostics: &Diagnostics,
    capabilities: TerminalCapabilities,
) -> Result<String, LayoutError> {
    let plan = margin_term::layout(diagnostics, capabilities)?;
    let theme = if use_dark_terminal() {
        arborium_theme::builtin::catppuccin_mocha()
    } else {
        arborium_theme::builtin::catppuccin_latte()
    };
    Ok(margin_term::render_plan_with_theme(
        &plan,
        capabilities,
        TerminalTheme::from_arborium(&theme),
    ))
}

/// Whether the active terminal has a dark background.
///
/// Resolution order:
///
/// 1. The `VX_THEME` environment variable (`light` or `dark`).
/// 2. An OSC 11 luma query via `terminal_light::luma()` — works on
///    iTerm2, kitty, ghostty, alacritty, wezterm, modern xterm, etc.
/// 3. Falls back to dark (the more common terminal default and the
///    safer choice if detection times out on the user's TTY).
///
/// **Cached process-wide via `OnceLock`.** This is correct for the
/// current short-lived CLI shape (vx eval, vx run, etc. — process
/// exits in seconds; the user's terminal theme doesn't change in
/// that window, and the OSC 11 query is non-trivial blocking I/O
/// we don't want to pay on every diagnostic render). If a future
/// long-lived interactive process (TUI, REPL) wants to react to a
/// mid-session theme switch, replace the OnceLock with an
/// explicit invalidation API or per-render detection.
pub fn use_dark_terminal() -> bool {
    use std::sync::OnceLock;
    static IS_DARK: OnceLock<bool> = OnceLock::new();
    *IS_DARK.get_or_init(|| {
        match std::env::var("VX_THEME").ok().as_deref() {
            Some(v) if v.eq_ignore_ascii_case("light") => return false,
            Some(v) if v.eq_ignore_ascii_case("dark") => return true,
            _ => {}
        }
        terminal_light::luma()
            .map(|luma| luma < 0.5)
            .unwrap_or(true)
    })
}

/// Render diagnostics for the terminal with Vixen syntax highlighting applied
/// to every referenced source. The single entry point for CLI/runtime
/// rendering — equivalent to:
///
/// 1. Build a `VixenSyntaxHighlighter`.
/// 2. For each `Source` in `diagnostics.sources`, generate syntax annotations.
/// 3. Attach them to each report that already has an annotation in that
///    source (so the syntax tokens appear in the report's source window).
/// 4. Lay out + render with `render_diagnostics_with_default_theme`.
///
/// Returns a fully-rendered string ready for `eprint!`. The original
/// `diagnostics` value is not mutated — a clone is augmented internally.
pub fn render_to_terminal(
    diagnostics: &Diagnostics,
    capabilities: TerminalCapabilities,
) -> Result<String> {
    let mut diagnostics = diagnostics.clone();
    let mut highlighter = VixenSyntaxHighlighter::new()?;

    for source in &diagnostics.sources {
        // Collect the set of report indices that reference this source —
        // syntax annotations only need to land on reports that actually
        // display a window into this source.
        let referencing_reports: Vec<usize> = diagnostics
            .reports
            .iter()
            .enumerate()
            .filter_map(|(idx, report)| {
                report
                    .annotations
                    .iter()
                    .any(|ann| ann.spans.iter().any(|s| s.source_id == source.id))
                    .then_some(idx)
            })
            .collect();
        if referencing_reports.is_empty() {
            continue;
        }
        let annotations = highlighter.syntax_annotations(&source.id.0, &source.text, None)?;
        for idx in referencing_reports {
            diagnostics.reports[idx]
                .annotations
                .extend(annotations.iter().cloned());
        }
    }

    render_diagnostics_with_default_theme(&diagnostics, capabilities).map_err(|e| eyre!("{e:?}"))
}

pub fn detect_terminal_capabilities() -> TerminalCapabilities {
    let stderr_is_terminal = std::io::stderr().is_terminal();
    TerminalCapabilities {
        width: terminal_width(stderr_is_terminal),
        glyph_mode: GlyphMode::Unicode,
        color_level: color_level(stderr_is_terminal),
        hyperlink_mode: hyperlink_mode(stderr_is_terminal),
        tab_width: 4,
    }
}

pub fn terminal_width(stderr_is_terminal: bool) -> usize {
    detect_terminal_width(stderr_is_terminal)
}

pub fn color_level(stderr_is_terminal: bool) -> ColorLevel {
    detect_color_level(stderr_is_terminal)
}

pub fn hyperlink_mode(stderr_is_terminal: bool) -> HyperlinkMode {
    detect_hyperlink_mode(stderr_is_terminal)
}

pub struct VixenSyntaxHighlighter {
    highlighter: Highlighter,
}

impl VixenSyntaxHighlighter {
    pub fn new() -> Result<Self> {
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

    pub fn syntax_annotations(
        &mut self,
        source_id: &str,
        source_text: &str,
        relevant_lines: Option<&BTreeSet<i32>>,
    ) -> Result<Vec<Annotation>> {
        let mut annotations = Vec::new();
        for span in spans_to_themed(self.highlighter.highlight_spans("vixen", source_text)?) {
            if span.start >= span.end {
                continue;
            }
            let start_line = line_number(source_text, span.start)?;
            if let Some(relevant_lines) = relevant_lines
                && !relevant_lines.contains(&start_line)
            {
                continue;
            }

            let start = byte_offset_to_display_boundary(source_text, span.start)?;
            let end = byte_offset_to_display_boundary(source_text, span.end)?;
            annotations.push(Annotation {
                spans: vec![Span::new(source_id, start, end)],
                role: AnnotationRole::SyntaxToken,
                syntax_class: syntax_class_for_theme_index(span.theme_index),
                message: None,
                priority: 0,
            });
        }

        Ok(annotations)
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
