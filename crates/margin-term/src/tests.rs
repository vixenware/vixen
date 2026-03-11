use margin::{
    Annotation, AnnotationRole, Diagnostics, Note, NoteKind, Report, Severity, Source, SourceId,
    Span,
};

use super::{ColorLevel, GlyphMode, TerminalCapabilities, render};

fn sample_diagnostics() -> Diagnostics {
    Diagnostics {
        sources: vec![Source {
            id: SourceId("main".to_string()),
            name: "src/main.vx".to_string(),
            text: "let number = 1\nnumber + missing\n".to_string(),
        }],
        reports: vec![Report {
            severity: Severity::Error,
            title: "unknown name `missing`".to_string(),
            annotations: vec![
                Annotation {
                    spans: vec![Span::new("main", 24, 31)],
                    role: AnnotationRole::PrimaryLabel,
                    message: Some("not found in this scope".to_string()),
                    priority: 100,
                },
                Annotation {
                    spans: vec![Span::new("main", 0, 3)],
                    role: AnnotationRole::SyntaxToken,
                    message: None,
                    priority: 1,
                },
            ],
            notes: vec![Note {
                kind: NoteKind::Help,
                text: "consider defining the binding before using it".to_string(),
            }],
            sections: Vec::new(),
        }],
    }
}

// d[verify api.renderer-options]
// d[verify term.explicit-capabilities]
// d[verify term.plaintext-mode]
// d[verify test.fixture-first]
#[test]
fn render_supports_plaintext_ascii_output() {
    let rendered = render(
        &sample_diagnostics(),
        TerminalCapabilities {
            width: 48,
            glyph_mode: GlyphMode::Ascii,
            color_level: ColorLevel::None,
        },
    )
    .unwrap();

    assert!(rendered.contains("error: unknown name `missing`"));
    assert!(rendered.contains("--> src/main.vx"));
    assert!(rendered.contains("|"));
    assert!(!rendered.contains("\u{1b}["));
}

// d[verify test.width-matrix]
// d[verify test.capability-matrix]
#[test]
fn render_changes_with_width_and_capabilities() {
    let narrow = render(
        &sample_diagnostics(),
        TerminalCapabilities {
            width: 28,
            glyph_mode: GlyphMode::Unicode,
            color_level: ColorLevel::Ansi16,
        },
    )
    .unwrap();
    let wide = render(
        &sample_diagnostics(),
        TerminalCapabilities {
            width: 72,
            glyph_mode: GlyphMode::Unicode,
            color_level: ColorLevel::Ansi16,
        },
    )
    .unwrap();

    assert!(narrow.contains("\u{1b}[31merror\u{1b}[0m"));
    assert!(narrow.contains("╭─> src/main.vx"));
    assert!(!narrow.contains("consider defining the binding before using it"));
    assert!(wide.contains("consider defining the binding before using it"));
}
