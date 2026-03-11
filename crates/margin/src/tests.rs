use super::{
    Annotation, AnnotationRole, Diagnostics, LayoutOptions, Note, NoteKind, Report, ReportSection,
    Severity, Source, SourceId, Span, plan,
};

fn sample_diagnostics() -> Diagnostics {
    Diagnostics {
        sources: vec![
            Source {
                id: SourceId("main".to_string()),
                name: "src/main.vx".to_string(),
                text: "let number = 1\nnumber + missing\n".to_string(),
            },
            Source {
                id: SourceId("lib".to_string()),
                name: "src/lib.vx".to_string(),
                text: "fn helper() {}\n".to_string(),
            },
        ],
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
                    spans: vec![Span::new("main", 3, 9)],
                    role: AnnotationRole::SecondaryLabel,
                    message: Some("bound here".to_string()),
                    priority: 50,
                },
                Annotation {
                    spans: vec![Span::new("lib", 0, 13)],
                    role: AnnotationRole::RelatedLabel,
                    message: Some("related declaration".to_string()),
                    priority: 20,
                },
            ],
            notes: vec![Note {
                kind: NoteKind::Help,
                text: "try importing a binding".to_string(),
            }],
            sections: vec![ReportSection {
                title: "while checking `main`".to_string(),
                notes: vec![Note {
                    kind: NoteKind::Note,
                    text: "secondary context is preserved".to_string(),
                }],
            }],
        }],
    }
}

// d[verify api.explicit-layout-artifact]
// d[verify api.stable-fixtures]
// d[verify model.report]
// d[verify model.source]
// d[verify model.annotation]
// d[verify model.window]
// d[verify input.multiple-sources]
#[test]
fn plan_preserves_structured_reports_and_explicit_windows() {
    let plan = plan(&sample_diagnostics(), &LayoutOptions::with_width(72)).unwrap();

    assert_eq!(plan.width, 72);
    assert_eq!(plan.reports.len(), 1);
    assert_eq!(plan.reports[0].windows.len(), 2);
    assert!(
        plan.reports[0]
            .windows
            .iter()
            .any(|window| window.source_name == "src/main.vx")
    );
    assert!(
        plan.reports[0]
            .windows
            .iter()
            .any(|window| window.source_name == "src/lib.vx")
    );
    assert_eq!(plan.reports[0].sections[0].title, "while checking `main`");
}

// d[verify input.span-bounds]
// d[verify span.zero-width]
// d[verify span.line-breaks]
#[test]
fn plan_keeps_zero_width_and_multiline_segments_intact() {
    let diagnostics = Diagnostics {
        sources: vec![Source {
            id: SourceId("main".to_string()),
            name: "src/main.vx".to_string(),
            text: "alpha\nbeta\ngamma\n".to_string(),
        }],
        reports: vec![Report {
            severity: Severity::Error,
            title: "bad insertion".to_string(),
            annotations: vec![
                Annotation {
                    spans: vec![Span::new("main", 2, 2)],
                    role: AnnotationRole::PrimaryLabel,
                    message: Some("insert here".to_string()),
                    priority: 100,
                },
                Annotation {
                    spans: vec![Span::new("main", 2, 8)],
                    role: AnnotationRole::SecondaryLabel,
                    message: Some("spans lines".to_string()),
                    priority: 50,
                },
            ],
            notes: Vec::new(),
            sections: Vec::new(),
        }],
    };

    let plan = plan(&diagnostics, &LayoutOptions::default()).unwrap();
    let annotations = &plan.reports[0].windows[0].annotations;

    assert_eq!(annotations[0].segments[0].start_column, 2);
    assert_eq!(annotations[0].segments[0].end_column, 3);
    assert_eq!(annotations[1].segments.len(), 2);
    assert_eq!(annotations[1].segments[0].line_number, 1);
    assert_eq!(annotations[1].segments[1].line_number, 2);
}
