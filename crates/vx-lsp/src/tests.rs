use rowan::TextSize;
use tower_lsp::lsp_types::Range;
use tower_lsp::lsp_types::{CompletionItemKind, Documentation, InlayHintLabel, Position};
use vx_parser::LineIndex;

use crate::server::{DocumentState, collect_inlay_hints, completions_for_type};

fn hint_position(text: &str, needle: &str) -> Position {
    let byte_offset = text
        .find(needle)
        .unwrap_or_else(|| panic!("missing `{needle}`"));
    let line_index = LineIndex::new(text);
    let pos = line_index.position(TextSize::new(byte_offset as u32));
    Position::new(pos.line, pos.col)
}

fn text_range(text: &str, needle: &str, len: usize) -> Range {
    let byte_offset = text
        .find(needle)
        .unwrap_or_else(|| panic!("missing `{needle}`"));
    let line_index = LineIndex::new(text);
    let start = line_index.position(TextSize::new(byte_offset as u32));
    let end = line_index.position(TextSize::new((byte_offset + len) as u32));
    Range {
        start: Position::new(start.line, start.col),
        end: Position::new(end.line, end.col),
    }
}

fn definition_named<'a>(state: &'a DocumentState, name: &str) -> &'a vx_ir::Definition {
    state
        .all_definitions()
        .iter()
        .find(|def| def.name == name)
        .unwrap_or_else(|| panic!("missing definition `{name}`"))
}

#[test]
fn parameter_hints_anchor_on_effective_argument_tokens() {
    let text = r#"
fn discover_like(class: String, probe: Fn(String) -> String) {}
fn run(args: Array[String]) {}
fn executor_class(label: String) {}

fn demo(class: String, cc: String) {
  discover_like(class, (host) => host);
  run [cc, "--version"];
  executor_class("default");
}
"#;

    let state = DocumentState::new_with_uri(text.to_string(), None);
    let hints = collect_inlay_hints(&state)
        .into_iter()
        .filter_map(|hint| match hint.label {
            InlayHintLabel::String(label) => Some((label, hint.position)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(hints.contains(&(String::from("probe:"), hint_position(text, "(host)"))));
    assert!(hints.contains(&(String::from("args:"), hint_position(text, "[cc"))));
    assert!(hints.contains(&(String::from("label:"), hint_position(text, "\"default\""))));
}

#[test]
fn definition_with_origin_at_uses_reference_range_for_hovered_usages() {
    let text = r#"
fn demo() {
  let class: String = "default";
  discover(class);
}
"#;

    let state = DocumentState::new_with_uri(text.to_string(), None);
    let usage_offset = TextSize::new(text.rfind("class").unwrap() as u32);
    let (def, origin_range) = state.definition_with_origin_at(usage_offset).unwrap();

    assert_eq!(def.name, "class");
    assert_eq!(
        state.to_lsp_range_span(origin_range),
        text_range(text, "class);", 5)
    );
    assert_eq!(
        state.to_lsp_range_span(def.name_span),
        text_range(text, "class:", 5)
    );
}

#[test]
fn doc_comment_for_ignores_plain_comments() {
    let text = r#"
// Minimal cc-version build sketch for the current
// resolver/evaluator lane.
fn build(ws: Path) -> Tree {
  Tree(ws)
}
"#;

    let state = DocumentState::new_with_uri(text.to_string(), None);
    let def = definition_named(&state, "build");

    assert_eq!(state.doc_comment_for(def), None);
}

#[test]
fn doc_comment_for_returns_markdown_prose() {
    let text = r#"
/// Build the output tree.
///
/// Uses `ws` as the input workspace.
fn build(ws: Path) -> Tree {
  Tree(ws)
}
"#;

    let state = DocumentState::new_with_uri(text.to_string(), None);
    let def = definition_named(&state, "build");

    assert_eq!(
        state.doc_comment_for(def).as_deref(),
        Some("Build the output tree.\n\nUses `ws` as the input workspace.")
    );
}

#[test]
fn inferred_def_type_uses_closure_parameter_types() {
    let text = r#"
fn build(cmd: Command) {
  exec(cmd) where {
    observer: (run) => run.ok.out,
  };
}
"#;

    let state = DocumentState::new_with_uri(text.to_string(), None);
    let run = definition_named(&state, "run");

    // The observer's single parameter is typed from exec's signature: `Fn(Run) -> T`.
    assert_eq!(
        state.inferred_def_type(run).map(|ty| state
            .types()
            .env
            .get_ty(ty)
            .display_with_env(&state.types().env)
            .to_string()),
        Some(String::from("Run"))
    );
}

#[test]
fn inferred_def_type_uses_specialized_exec_return_type() {
    let text = r#"
fn build(cmd: Command) {
  let output = exec(cmd) where {
    observer: (run) => run.ok.out,
  };

  output
}
"#;

    let state = DocumentState::new_with_uri(text.to_string(), None);
    let output = definition_named(&state, "output");

    assert_eq!(
        state.inferred_def_type(output).map(|ty| state
            .types()
            .env
            .get_ty(ty)
            .display_with_env(&state.types().env)
            .to_string()),
        Some(String::from("Tree"))
    );
}

#[test]
fn inferred_def_type_uses_specialized_partial_exec_return_type() {
    let text = r#"
fn build() {
  let common = {
    observer: (run) => run.ok.out,
  };
  let cexec = partial exec where common;

  cexec
}
"#;

    let state = DocumentState::new_with_uri(text.to_string(), None);
    let cexec = definition_named(&state, "cexec");

    assert_eq!(
        state.inferred_def_type(cexec).map(|ty| state
            .types()
            .env
            .get_ty(ty)
            .display_with_env(&state.types().env)
            .to_string()),
        Some(String::from("Fn(Command) -> Tree"))
    );
}

#[test]
fn documentation_for_uses_callback_slot_docs() {
    let text = r#"
fn build(cmd: Command) {
  exec(cmd) where {
    observer: (run) => run.ok.out,
  };
}
"#;

    let state = DocumentState::new_with_uri(text.to_string(), None);
    let run = definition_named(&state, "run");

    let run_doc = state.documentation_for(run).unwrap();

    assert!(run_doc.contains("`observer` callback slot."));
    assert!(run_doc.contains("Executor-side observer evaluated with sandbox authority"));
}

#[test]
fn stdlib_reference_points_at_real_stdlib_definition() {
    let text = r#"
fn demo() {
  executor_class("default")
}
"#;

    let state = DocumentState::new_with_uri(text.to_string(), None);
    let offset = TextSize::new(text.find("executor_class").unwrap() as u32);
    let def = state.definition_at(offset).unwrap();

    assert_eq!(def.name, "executor_class");
    assert_ne!(def.source(), *state.current_source());
    assert_eq!(def.kind, vx_ir::DefKind::Function);
    assert_eq!(
        state.declaration_snippet(&def),
        "fn executor_class(label: String) -> ExecutorClass"
    );
    assert_eq!(
        state.doc_comment_for(&def).as_deref(),
        Some("Construct an executor class selector value.")
    );

    let uri = state.uri_for_source(def.source()).unwrap();
    let disk_path = uri
        .to_file_path()
        .unwrap_or_else(|_| panic!("expected file uri, got {uri}"));
    assert!(
        disk_path == vx_stdlib::source_disk_path("crates/vx-stdlib/std/build.vx"),
        "expected stdlib build.vx uri to point at real file, got {uri}"
    );
}

#[test]
fn stdlib_namespace_reference_points_at_real_namespace_definition() {
    let text = r#"
struct Config {}

fn demo(blob: Blob) {
  Toml.parse[Config](blob)
}
"#;

    let state = DocumentState::new_with_uri(text.to_string(), None);
    let offset = TextSize::new(text.find("Toml").unwrap() as u32);
    let def = state.definition_at(offset).unwrap();

    assert_eq!(def.name, "Toml");
    assert_eq!(def.kind, vx_ir::DefKind::Namespace);
    assert_ne!(def.source(), *state.current_source());
    assert_eq!(state.declaration_snippet(&def), "namespace Toml {");

    let uri = state.uri_for_source(def.source()).unwrap();
    let disk_path = uri
        .to_file_path()
        .unwrap_or_else(|_| panic!("expected file uri, got {uri}"));
    assert_eq!(
        disk_path,
        vx_stdlib::source_disk_path("crates/vx-stdlib/std/core.vx")
    );
}

#[test]
fn stdlib_method_reference_points_at_real_member_definition() {
    let text = r#"
fn demo(host: Host) {
  host.which("cc")
}
"#;

    let state = DocumentState::new_with_uri(text.to_string(), None);
    let offset = TextSize::new(text.find("which").unwrap() as u32);
    let def = state.definition_at(offset).unwrap();

    assert_eq!(def.name, "which");
    assert_eq!(def.kind, vx_ir::DefKind::Function);
    assert_ne!(def.source(), *state.current_source());
    assert_eq!(
        state.declaration_snippet(&def),
        "fn which(self, name: String) -> String"
    );
    assert_eq!(
        state.doc_comment_for(&def).as_deref(),
        Some("Resolve a tool or executable name on the executor host.")
    );
}

#[test]
fn completions_for_type_use_real_stdlib_member_definitions() {
    let text = r#"
fn demo(host: Host) {
  host.which
}
"#;

    let state = DocumentState::new_with_uri(text.to_string(), None);
    let offset = TextSize::new(text.find("host.which").unwrap() as u32);
    let host_def = *state
        .types()
        .env
        .lookup_def("Host")
        .first()
        .expect("Host type");
    let host_ty = vx_types::Ty::Opaque(vx_types::OpaqueTy {
        def: host_def,
        type_args: vec![],
    });
    let items = completions_for_type(&host_ty, &state, offset).expect("member completions");
    let which = items
        .iter()
        .find(|item| item.label == "which")
        .expect("which completion");

    assert_eq!(which.kind, Some(CompletionItemKind::METHOD));
    assert_eq!(
        which.detail.as_deref(),
        Some("fn which(self, name: String) -> String")
    );
    let documentation = which.documentation.as_ref().and_then(|doc| match doc {
        Documentation::MarkupContent(content) => Some(content.value.as_str()),
        _ => None,
    });
    assert_eq!(
        documentation,
        Some("Resolve a tool or executable name on the executor host.")
    );
}

#[test]
fn completions_for_map_type_include_stdlib_methods() {
    let text = r#"
fn demo(values: Map[String, String]) {
  values.has
}
"#;

    let state = DocumentState::new_with_uri(text.to_string(), None);
    let offset = TextSize::new(text.find("values.has").unwrap() as u32);
    // Get the actual `Map[String, String]` Ty out of the document — the binding
    // for `values` has its type populated by type checking, and that's the
    // same Ty (interned in the doc's env) we want completions for.
    let values_binding = definition_named(&state, "values");
    let map_ty_id = state
        .inferred_def_type(values_binding)
        .expect("`values` should have an inferred Map type");
    let map_ty = state.types().env.get_ty(map_ty_id).clone();
    let items = completions_for_type(&map_ty, &state, offset).expect("member completions");

    let has = items
        .iter()
        .find(|item| item.label == "has")
        .expect("has completion");
    assert_eq!(has.kind, Some(CompletionItemKind::METHOD));
    assert_eq!(has.detail.as_deref(), Some("fn has(self, key: K) -> Bool"));

    let get = items
        .iter()
        .find(|item| item.label == "get")
        .expect("get completion");
    assert_eq!(get.kind, Some(CompletionItemKind::METHOD));
    assert_eq!(get.detail.as_deref(), Some("fn get(self, key: K) -> V"));
}

#[test]
fn inlay_hints_use_stdlib_function_parameters() {
    let text = r#"
fn demo() {
  executor_class("default")
}
"#;

    let state = DocumentState::new_with_uri(text.to_string(), None);
    let hints = collect_inlay_hints(&state)
        .into_iter()
        .filter_map(|hint| match hint.label {
            InlayHintLabel::String(label) => Some((label, hint.position)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(hints.contains(&(String::from("label:"), hint_position(text, "\"default\""))));
}

#[test]
fn binding_type_inlay_hints_use_named_opaque_types() {
    let text = r#"
fn demo() {
  let class = executor_class("default");
}
"#;

    let state = DocumentState::new_with_uri(text.to_string(), None);
    let hints = collect_inlay_hints(&state)
        .into_iter()
        .filter_map(|hint| match hint.label {
            InlayHintLabel::String(label) => Some(label),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(hints.iter().any(|label| label == ": ExecutorClass"));
    assert!(!hints.iter().any(|label| label.contains("<opaque#")));
}
