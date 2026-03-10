use std::{fs, path::Path};

use rowan::TextSize;

use crate::{
    AstNode, Breadcrumb, DocumentSymbol, Expr, FoldingRangeKind, Item, LineIndex, MapEntry,
    NamespaceDecl, PathLiteralExpr, Position, SemanticTokenType, SourceFile, Stmt, SymbolKind,
    SyntaxKind, Type, document_symbols, enclosing_nodes, folding_ranges, parse, parse_source_file,
    semantic_tokens,
};

#[test]
fn parser_does_not_panic_on_if_without_else() {
    let src = r#"
fn f() {
  if x {
    { output }
  }
}
struct S { v: Fn() -> () }
"#;

    let parsed = parse(src);
    let _ = parsed.errors();
}

#[test]
fn parses_top_level_declarations_from_corpus() {
    let input = r#"
#[tag = "kind", content = "payload"]
enum Message[T] {
  #[rename = "hello"]
  struct Hello {
    body: T,
  },
  struct Goodbye,
}

newtype PathBuf of String

type Entry = Blob | Tree | Path

struct BuildInput {
  fs: Fs,
  workspace: Path,
  args: Array[String],
}

fn build[T](input: BuildInput) #fs -> T {
  input
}
"#;

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let root = parse.syntax();
    let kinds = root
        .children()
        .map(|child| child.kind())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&SyntaxKind::ENUM_DECL));
    assert!(kinds.contains(&SyntaxKind::NEWTYPE_DECL));
    assert!(kinds.contains(&SyntaxKind::TYPE_ALIAS_DECL));
    assert!(kinds.contains(&SyntaxKind::STRUCT_DECL));
    assert!(kinds.contains(&SyntaxKind::FN_DECL));
}

#[test]
fn top_level_is_a_statement_list() {
    let parse = parse(
        r#"
let seed = 1;
seed;
fn helper(x: U64) -> U64 {
  x
}
helper seed;
"#,
    );
    assert_eq!(parse.errors(), []);

    let file = SourceFile::cast_root(parse.syntax());
    let statements = file.statements().collect::<Vec<_>>();
    assert_eq!(statements.len(), 4);
    assert!(matches!(&statements[0], Stmt::Let(_)));
    assert!(matches!(&statements[1], Stmt::Expr(Expr::NameRef(_))));
    assert!(matches!(&statements[2], Stmt::FnDecl(_)));
    assert!(matches!(&statements[3], Stmt::Expr(Expr::Call(_))));
}

#[test]
fn nested_type_declarations_parse_in_statement_position() {
    let parse = parse(
        r#"
fn outer() -> U64 {
  struct Inner {
    value: U64,
  };
  enum Choice {
    struct Some,
  };
  newtype Id of U64;
  type MaybeInner = Inner?;
  fn helper(item: Inner) -> U64 {
    item.value
  };
  0
}
"#,
    );
    assert_eq!(parse.errors(), []);

    let file = SourceFile::cast_root(parse.syntax());
    let function = match file.items().next().expect("function item") {
        Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let body = function.body().expect("function body");
    let statements = body.statements().collect::<Vec<_>>();

    assert!(matches!(&statements[0], Stmt::StructDecl(_)));
    assert!(matches!(&statements[1], Stmt::EnumDecl(_)));
    assert!(matches!(&statements[2], Stmt::NewtypeDecl(_)));
    assert!(matches!(&statements[3], Stmt::TypeAliasDecl(_)));
    assert!(matches!(&statements[4], Stmt::FnDecl(_)));
    assert!(matches!(&statements[5], Stmt::Expr(Expr::Literal(_))));
}

#[test]
fn header_style_declarations_parse() {
    let parse = parse(
        r#"
type Blob;
type Stream[T];

fn exec(args: Array[String]) -> Stream[Blob];

namespace Blob {
  fn from_string(s: String) -> Blob;
  fn to_string(self) -> String;
}
"#,
    );
    assert_eq!(parse.errors(), []);

    let file = SourceFile::cast_root(parse.syntax());
    let items = file.items().collect::<Vec<_>>();
    assert_eq!(items.len(), 3);

    let opaque = match &items[0] {
        Item::TypeAliasDecl(node) => node,
        item => panic!("expected opaque type declaration, got {item:?}"),
    };
    assert_eq!(
        opaque.name().map(|t| t.text().to_string()).as_deref(),
        Some("Blob")
    );
    assert!(
        opaque.aliased_type().is_none(),
        "opaque type declarations should not have an aliased type"
    );

    let generic_opaque = match &items[1] {
        Item::TypeAliasDecl(node) => node,
        item => panic!("expected opaque type declaration, got {item:?}"),
    };
    assert_eq!(
        generic_opaque
            .name()
            .map(|t| t.text().to_string())
            .as_deref(),
        Some("Stream")
    );
    assert!(
        generic_opaque.aliased_type().is_none(),
        "generic opaque type declarations should not have an aliased type"
    );

    let header_fn = match &items[2] {
        Item::FnDecl(node) => node,
        item => panic!("expected function declaration, got {item:?}"),
    };
    assert_eq!(
        header_fn.name().map(|t| t.text().to_string()).as_deref(),
        Some("exec")
    );
    assert!(
        header_fn.body().is_none(),
        "declaration-only functions should not have a body"
    );

    let namespace = file
        .syntax()
        .children()
        .find_map(NamespaceDecl::cast)
        .expect("namespace declaration should parse");
    assert_eq!(
        namespace.name().map(|t| t.text().to_string()).as_deref(),
        Some("Blob")
    );
    let body = namespace.body().expect("namespace should have a body");
    let body_items = body.items().collect::<Vec<_>>();
    assert_eq!(body_items.len(), 2);
    assert!(matches!(&body_items[0], Item::FnDecl(_)));
    assert!(matches!(&body_items[1], Item::FnDecl(_)));
}

fn assert_stdlib_header_parses(relative: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../vx-stdlib/std");
    let path = root.join(relative);
    let source = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", path.display());
    });
    let parse = parse(&source);
    assert!(
        parse.errors().is_empty(),
        "{} should parse cleanly, got {:?}",
        path.display(),
        parse.errors()
    );
}

#[test]
fn stdlib_core_header_parses() {
    assert_stdlib_header_parses("core.vx");
}

#[test]
fn stdlib_build_header_parses() {
    assert_stdlib_header_parses("build.vx");
}

#[test]
fn function_declaration_where_blocks_parse() {
    let parse = parse(
        r#"
fn exec[T](args: Array[ExecInput]) where {
  mounts: Map[Path, Blob | Tree] = {},
  observer: Fn(Process, Tree) #sandbox -> T,
} -> T;
"#,
    );
    assert_eq!(parse.errors(), []);
}

#[test]
fn invalid_namespace_body_recovers() {
    let parse = parse(
        r#"
namespace Tree {
  Blob
}
"#,
    );
    assert!(
        !parse.errors().is_empty(),
        "invalid namespace bodies should error instead of hanging",
    );
}

#[test]
fn old_at_attributes_no_longer_parse() {
    let parse = parse(
        r#"
@rename("hello")
struct Message
"#,
    );

    assert!(
        !parse.errors().is_empty(),
        "old @attribute syntax should produce a parse error",
    );

    let kinds = parse
        .syntax()
        .children()
        .map(|child| child.kind())
        .collect::<Vec<_>>();

    assert!(!kinds.contains(&SyntaxKind::ATTRIBUTE_LIST));
    assert!(kinds.contains(&SyntaxKind::STRUCT_DECL));
}

#[test]
fn type_contexts_stay_in_type_grammar() {
    let parse = parse(
        r#"
type LayoutMap = Map[String, Dashboard.Layout?]

fn decode(items: Array[U64], layout: Dashboard.Layout?) -> Map[String, U64] {
  items
}
"#,
    );
    assert_eq!(parse.errors(), []);

    let file = SourceFile::cast_root(parse.syntax());
    let items = file.items().collect::<Vec<_>>();
    let alias = match &items[0] {
        Item::TypeAliasDecl(node) => node,
        item => panic!("expected type alias, got {item:?}"),
    };
    assert!(matches!(alias.aliased_type(), Some(Type::Path(_))));

    let function = match &items[1] {
        Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let params = function.parameters().collect::<Vec<_>>();
    assert!(matches!(
        params[0].slot_type().and_then(|ty| ty.ty()),
        Some(Type::Path(_))
    ));
    assert!(matches!(
        params[1].slot_type().and_then(|ty| ty.ty()),
        Some(Type::Optional(_))
    ));
    assert!(matches!(
        function.return_type().and_then(|ret| ret.ty()),
        Some(Type::Path(_))
    ));
}

#[test]
fn parses_unary_application_where_and_postfix_chains() {
    let input = r#"
fn build(
  exec_fn: Fn(Array[String]) -> Stream[String],
  args: Array[String],
  defaults: Config,
  files: Array[Blob],
  process: Process
) -> String {
  exec_fn args where { ..defaults, mounts: { @bin/lua: files[0]? } };
  process.stdout!.trim!
}
"#;

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let kinds = parse
        .syntax()
        .descendants()
        .map(|node| node.kind())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&SyntaxKind::CALL_EXPR));
    assert!(kinds.contains(&SyntaxKind::WHERE_EXPR));
    assert!(kinds.contains(&SyntaxKind::RECORD_EXPR));
    assert!(kinds.contains(&SyntaxKind::MAP_EXPR));
    assert!(kinds.contains(&SyntaxKind::INDEX_EXPR));
    assert!(kinds.contains(&SyntaxKind::FALLIBLE_EXPR));
    assert!(kinds.contains(&SyntaxKind::INVOKE_EXPR));
    assert!(kinds.contains(&SyntaxKind::FIELD_EXPR));
}

#[test]
fn call_ast_flattens_application_chains() {
    let parse = parse(
        r#"
fn build(exec_fn: Fn(Array[String]) -> Stream[String], args: Array[String], defaults: Config) {
  exec_fn args where defaults
}
"#,
    );
    assert_eq!(parse.errors(), []);

    let file = SourceFile::cast_root(parse.syntax());
    let function = match file.items().next().expect("function item") {
        Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let body = function.body().expect("function body");
    let where_expr = match body.statements().next().expect("where statement") {
        Stmt::Expr(Expr::Where(node)) => node,
        stmt => panic!("expected where expr, got {stmt:?}"),
    };

    let call = match where_expr.callee().expect("where base") {
        Expr::Call(node) => node,
        expr => panic!("expected call expr, got {expr:?}"),
    };
    let root = match call.root_callee().expect("root callee") {
        Expr::NameRef(node) => node,
        expr => panic!("expected name ref, got {expr:?}"),
    };
    assert_eq!(root.token().unwrap().text(), "exec_fn");

    let args = call.positional_args();
    assert_eq!(args.len(), 1);
    assert!(matches!(&args[0], Expr::NameRef(_)));
    assert!(matches!(where_expr.keywords(), Some(Expr::NameRef(_))));
}

#[test]
fn upper_ident_can_take_map_payload_by_unary_application() {
    let parse = parse(
        r#"
fn build(exe: Blob) {
  Tree { @bin/lua: exe }
}
"#,
    );
    assert_eq!(parse.errors(), []);

    let file = SourceFile::cast_root(parse.syntax());
    let function = match file.items().next().expect("function item") {
        Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let body = function.body().expect("function body");
    let call = match body.statements().next().expect("call statement") {
        Stmt::Expr(Expr::Call(node)) => node,
        stmt => panic!("expected call expr, got {stmt:?}"),
    };

    let callee = match call.root_callee().expect("root callee") {
        Expr::NameRef(node) => node,
        expr => panic!("expected name ref, got {expr:?}"),
    };
    assert_eq!(callee.token().unwrap().text(), "Tree");

    let args = call.positional_args();
    assert_eq!(args.len(), 1);
    assert!(matches!(&args[0], Expr::Map(_)));
}

#[test]
fn record_and_map_literals_have_distinct_ast_nodes() {
    let parse = parse(
        r#"
fn demo(verifier: Verifier, value: U64) {
  { verifier, observer: make_observer! };
  { @bin/lua: value, 80: value };
}
"#,
    );
    assert_eq!(parse.errors(), []);

    let file = SourceFile::cast_root(parse.syntax());
    let function = match file.items().next().expect("function item") {
        Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let body = function.body().expect("function body");
    let statements = body.statements().collect::<Vec<_>>();

    let record = match &statements[0] {
        Stmt::Expr(Expr::Record(node)) => node,
        stmt => panic!("expected record expr, got {stmt:?}"),
    };
    let fields = record.fields().collect::<Vec<MapEntry>>();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].name().unwrap().text(), "verifier");
    assert!(fields[0].value().is_none());
    assert_eq!(fields[1].name().unwrap().text(), "observer");
    assert!(matches!(fields[1].value(), Some(Expr::Invoke(_))));

    let map = match &statements[1] {
        Stmt::Expr(Expr::Map(node)) => node,
        stmt => panic!("expected map expr, got {stmt:?}"),
    };
    let entries = map.entries().collect::<Vec<_>>();
    assert_eq!(entries.len(), 2);
    assert!(matches!(entries[0].key(), Some(Expr::PathLiteral(_))));
    assert!(matches!(entries[0].value(), Some(Expr::NameRef(_))));
    assert!(matches!(entries[1].key(), Some(Expr::Literal(_))));
}

#[test]
fn bare_and_interpolated_path_literals_parse() {
    let parse = parse(
        r#"
fn build(name: String) {
  @bin/lua;
  @(out/{name}.o)
}
"#,
    );
    assert_eq!(parse.errors(), []);

    let file = SourceFile::cast_root(parse.syntax());
    let function = match file.items().next().expect("function item") {
        Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let body = function.body().expect("function body");
    let exprs = body
        .statements()
        .filter_map(|stmt| match stmt {
            Stmt::Expr(Expr::PathLiteral(expr)) => Some(expr),
            _ => None,
        })
        .collect::<Vec<PathLiteralExpr>>();

    assert_eq!(exprs.len(), 2);
    assert_eq!(exprs[0].syntax().text().to_string().trim(), "@bin/lua");
    assert_eq!(
        exprs[1].syntax().text().to_string().trim(),
        "@(out/{name}.o)"
    );
}

#[test]
fn formatter_preserves_unary_application_spacing() {
    let input = r#"
fn build(process: Process, args: Array[String], defaults: Config) {
  exec_fn   args   where   { mounts: { @bin/lua: process.stdout! } };
  process . stdout ! . trim !
}
"#;

    let parsed = parse_source_file(input);
    let formatted = crate::format(&parsed.tree().syntax());

    assert!(formatted.contains("exec_fn args where"));
    assert!(formatted.contains("@bin/lua: process.stdout!"));
    assert!(formatted.contains("process.stdout!.trim!"));
}

#[test]
fn ast_wraps_types_and_patterns_for_lowering() {
    let input = r#"
type Entry = Blob | Fn(String, Bool) #fs -> Result?

fn decode(input: Array[String]?, item: Message) -> Map[String, Entry?] {
  match item {
    whole @ Message.Hello { body } => body,
    Direction.North | Direction.South => "ns",
    [first, ..rest] => first,
    _ => "other",
  }
}
"#;

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let file = SourceFile::cast_root(parse.syntax());
    let items = file.items().collect::<Vec<_>>();
    let type_alias = match &items[0] {
        Item::TypeAliasDecl(node) => node,
        item => panic!("expected type alias, got {item:?}"),
    };
    let sum = match type_alias.aliased_type().expect("aliased type") {
        Type::Sum(node) => node,
        ty => panic!("expected sum type, got {ty:?}"),
    };
    assert_eq!(sum.variants().count(), 2);
}

#[test]
fn typed_parse_returns_source_file_tree() {
    let parsed = parse_source_file(
        r#"
struct Config {
  jobs: U64,
}
"#,
    );

    assert_eq!(parsed.errors(), []);

    let file = parsed.tree();
    let items = file.items().collect::<Vec<_>>();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], Item::StructDecl(_)));
}

#[test]
fn line_index_basics() {
    let idx = LineIndex::new("abc\ndef\nghi");
    assert_eq!(idx.line_count(), 3);
    assert_eq!(
        idx.position(TextSize::from(0)),
        Position { line: 0, col: 0 }
    );
    assert_eq!(
        idx.position(TextSize::from(4)),
        Position { line: 1, col: 0 }
    );
    assert_eq!(
        idx.offset(Position { line: 2, col: 2 }),
        Some(TextSize::from(10))
    );
}

#[test]
fn document_symbols_extract_items_with_children() {
    let input = r#"
struct Config {
  jobs: U64,
  name: String,
}

enum Message {
  struct Hello {
    body: String,
  },
  struct Goodbye,
}

fn build(src: String) -> String {
  src
}
"#;

    let parsed = parse_source_file(input);
    assert_eq!(parsed.errors(), []);

    let symbols = document_symbols(&parsed.tree());
    assert_eq!(symbols.len(), 3);
    assert_symbol(&symbols[0], "Config", SymbolKind::Struct);
    assert_eq!(symbols[0].children.len(), 2);
    assert_symbol(&symbols[1], "Message", SymbolKind::Enum);
    assert_eq!(symbols[1].children.len(), 2);
    assert_symbol(&symbols[2], "build", SymbolKind::Function);
}

#[test]
fn enclosing_nodes_finds_breadcrumb_trail() {
    let input = r#"fn outer() -> U64 {
  match 1 {
    _ => {
      42
    },
  }
}"#;

    let parsed = parse_source_file(input);
    assert_eq!(parsed.errors(), []);

    let offset = TextSize::from(input.find("42").unwrap() as u32);
    let crumbs = enclosing_nodes(&parsed.tree().syntax(), offset);

    assert_has_breadcrumb(&crumbs, SyntaxKind::SOURCE_FILE);
    assert_has_breadcrumb(&crumbs, SyntaxKind::FN_DECL);
    assert_has_breadcrumb(&crumbs, SyntaxKind::MATCH_EXPR);
    assert_has_breadcrumb(&crumbs, SyntaxKind::MATCH_ARM);
    assert_has_breadcrumb(&crumbs, SyntaxKind::BLOCK_EXPR);
}

#[test]
fn folding_ranges_finds_blocks_and_items() {
    let input = r#"
struct Config {
  jobs: U64,
}

fn build(flag: Bool) -> U64 {
  if flag {
    1
  } else {
    0
  }
}
"#;

    let parsed = parse_source_file(input);
    assert_eq!(parsed.errors(), []);

    let ranges = folding_ranges(&parsed.tree().syntax());
    assert!(
        ranges
            .iter()
            .all(|range| range.kind == FoldingRangeKind::Block)
    );
    assert!(ranges.len() >= 5);
}

#[test]
fn semantic_tokens_classify_functions_keywords_and_properties() {
    let input = r#"
fn build(process: Process, args: Array[String], defaults: Config) -> String {
  exec args where { mounts: { @bin/lua: process.stdout! } }
}
"#;

    let parsed = parse_source_file(input);
    assert_eq!(parsed.errors(), []);

    let tokens = semantic_tokens(&parsed.tree().syntax());
    let classified = tokens
        .iter()
        .map(|token| {
            let start = u32::from(token.range.start()) as usize;
            let end = u32::from(token.range.end()) as usize;
            (&input[start..end], token.token_type)
        })
        .collect::<Vec<_>>();

    assert!(classified.contains(&("fn", SemanticTokenType::Keyword)));
    assert!(classified.contains(&("where", SemanticTokenType::Keyword)));
    assert!(classified.contains(&("exec", SemanticTokenType::Function)));
    assert!(classified.contains(&("mounts", SemanticTokenType::Property)));
    assert!(classified.contains(&("stdout", SemanticTokenType::Property)));
}

fn assert_symbol(symbol: &DocumentSymbol, name: &str, kind: SymbolKind) {
    assert_eq!(symbol.name, name);
    assert_eq!(symbol.kind, kind);
}

fn assert_has_breadcrumb(crumbs: &[Breadcrumb], kind: SyntaxKind) {
    assert!(crumbs.iter().any(|crumb| crumb.kind == kind));
}
