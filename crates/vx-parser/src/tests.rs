use rowan::TextSize;

use crate::{
    ArrayItem, AstNode, Expr, FieldExpr, FoldingRangeKind, InterpolatedStringExpr, Item, LineIndex,
    PathLiteralExpr, Pattern, Position, SemanticTokenType, SourceFile, Stmt, StringPart,
    SymbolKind, SyntaxKind, Type, doc_comment, document_symbols, enclosing_nodes, folding_ranges,
    parse, parse_source_file, semantic_tokens,
};

#[test]
fn parser_does_not_panic_on_if_without_else() {
    // Regression test: if-without-else followed by a struct with Fn type
    // used to panic in CompletedMarker::precede because error_and_bump
    // in atom_expr returned a CompletedMarker pointing at a Token event
    // instead of the Start event.
    let src = r#"
fn f() {
  if x {
    `hello`
  }
}
struct S { v: Fn() -> () }
"#;
    let parsed = parse(src);
    // Should produce parse errors but never panic
    let _ = parsed.errors();
}

#[test]
fn parser_does_not_panic_on_zoo_build_vx() {
    // Regression: lines 39-52 from vixen-zoo/lua/build.vx triggered a panic
    let src = r#"
fn verify_tool(class: ExecutorClass, tool: DiscoveredTool) {
  let current = discover_exec(class, ["sha256sum", tool.path]).stdout;
  if current != tool.hash {
    fail `tool at ${tool.path} changed since discovery`
  }
}
struct CToolchain {
  cc: DiscoveredTool,
  ar: DiscoveredTool,
  ranlib: DiscoveredTool,
  verifier: Fn() -> (),
}
"#;
    let parsed = parse(src);
    let _ = parsed.errors();
}

#[test]
fn parses_top_level_declarations_from_corpus() {
    let input = r#"
@tag("kind", content: "payload")
enum Message[T] {
  @rename("hello")
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
  env: Map[String, String],
  output: enum Output {
    struct Tree,
    struct Blob {
      path: String,
    },
  },
}

fn build[T](input: BuildInput, ; release: Bool = false) #fs -> T {
  input
}
"#;

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let root = parse.syntax();
    let kinds = root
        .children()
        .map(|child| child.kind())
        .filter(|kind| *kind != SyntaxKind::ATTRIBUTE_LIST)
        .collect::<Vec<_>>();
    assert!(kinds.contains(&SyntaxKind::ENUM_DECL));
    assert!(kinds.contains(&SyntaxKind::NEWTYPE_DECL));
    assert!(kinds.contains(&SyntaxKind::TYPE_ALIAS_DECL));
    assert!(kinds.contains(&SyntaxKind::STRUCT_DECL));
    assert!(kinds.contains(&SyntaxKind::FN_DECL));
}

#[test]
fn parses_zero_sized_declarations() {
    let input = r#"
struct Unit

enum Direction {
  struct North,
  struct South,
  struct East,
  struct West,
}
"#;

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let root = parse.syntax();
    let kinds = root
        .children()
        .map(|child| child.kind())
        .collect::<Vec<_>>();
    assert_eq!(kinds, [SyntaxKind::STRUCT_DECL, SyntaxKind::ENUM_DECL]);
}

#[test]
fn lexes_nested_block_comments() {
    let tokens = crate::lex("/* outer /* inner */ still outer */");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].kind, SyntaxKind::BLOCK_COMMENT);
}

#[test]
fn parses_calls_struct_literals_and_with_expressions() {
    let input = r#"
fn value(x: F64, p: Point, defaults: Config) -> Point {
  render("page.html"; ..defaults, title: "Home");
  Point { :x, ..p, y: 3.0 } with { last: "Wilson" }
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
    assert!(kinds.contains(&SyntaxKind::ARG_LIST));
    assert!(kinds.contains(&SyntaxKind::SPREAD_EXPR));
    assert!(kinds.contains(&SyntaxKind::STRUCT_LITERAL_EXPR));
    assert!(kinds.contains(&SyntaxKind::WITH_EXPR));
    assert!(kinds.contains(&SyntaxKind::UPDATE_BODY));
}

#[test]
fn call_ast_exposes_keyword_arguments() {
    let input = r#"
fn build() {
  exec(["/bin/sh", "-c", "printf ok"]; mounts: %{ "src" => "ignored" }, writable: ["out"])
}
"#;

    let parse = parse(input);
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

    assert_eq!(call.args().count(), 1);

    let keyword_args = call.keyword_args().collect::<Vec<_>>();
    assert_eq!(keyword_args.len(), 2);
    assert_eq!(keyword_args[0].name().unwrap().text(), "mounts");
    assert_eq!(keyword_args[1].name().unwrap().text(), "writable");
    assert!(keyword_args.iter().all(|arg| arg.expr().is_some()));
}

#[test]
fn array_ast_exposes_spread_items() {
    let parse = parse(
        r#"
fn build(xs: Array[U64]) {
  [1, ..xs, 4]
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
    let array = match body.statements().next().expect("array statement") {
        Stmt::Expr(Expr::Array(node)) => node,
        stmt => panic!("expected array expr, got {stmt:?}"),
    };

    let items = array.items().collect::<Vec<_>>();
    assert_eq!(items.len(), 3);
    assert!(matches!(items[0], ArrayItem::Expr(_)));
    assert!(matches!(items[1], ArrayItem::Spread(_)));
    assert!(matches!(items[2], ArrayItem::Expr(_)));
}

#[test]
fn upper_ident_name_refs_expose_their_token() {
    let parse = parse(r#"fn value() { Blob.from_string("hello") }"#);
    assert_eq!(parse.errors(), []);

    let name_refs = parse
        .syntax()
        .descendants()
        .filter_map(Expr::cast)
        .filter_map(|expr| match expr {
            Expr::NameRef(name_ref) => Some(name_ref.token().unwrap().text().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(name_refs.contains(&"Blob".to_string()));
}

#[test]
fn keyword_like_field_names_parse_after_dot() {
    let parse = parse(r#"fn value() { Tree.of(%{}) }"#);
    assert_eq!(parse.errors(), []);

    let field_names = parse
        .syntax()
        .descendants()
        .filter_map(FieldExpr::cast)
        .map(|field| field.field_name().unwrap().text().to_string())
        .collect::<Vec<_>>();

    assert!(field_names.contains(&"of".to_string()));
}

#[test]
fn parses_ufcs_and_operator_precedence() {
    let input = r#"
fn value(items: Array[U64]) -> Array[U64] {
  items.map($ + 1).filter($ > 3).to_array()
}
"#;

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let kinds = parse
        .syntax()
        .descendants()
        .map(|node| node.kind())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&SyntaxKind::FIELD_EXPR));
    assert!(kinds.contains(&SyntaxKind::CALL_EXPR));
    assert!(kinds.contains(&SyntaxKind::BINARY_EXPR));
}

#[test]
fn parses_if_import_fail_and_literals() {
    let input = r#"
fn value(flag: Bool) -> String {
  let xs = [1, 2, 3];
  let ys = %{ "x" => 1, "y" => 2 };
  if flag {
    import("build.vx");
    fail "boom"
  } else {
    ys;
    xs;
    "ok"
  }
}
"#;

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let kinds = parse
        .syntax()
        .descendants()
        .map(|node| node.kind())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&SyntaxKind::ARRAY_EXPR));
    assert!(kinds.contains(&SyntaxKind::MAP_EXPR));
    assert!(kinds.contains(&SyntaxKind::IF_EXPR));
    assert!(kinds.contains(&SyntaxKind::IMPORT_EXPR));
    assert!(kinds.contains(&SyntaxKind::FAIL_EXPR));
}

#[test]
fn parses_match_with_variant_and_nested_patterns() {
    let input = r#"
fn classify(flag: Bool, value: Value) -> String {
  if flag {
    "yes"
  } else {
    match value {
      Message.Hello { body } => body,
      _ => "no",
    }
  }
}
"#;

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let kinds = parse
        .syntax()
        .descendants()
        .map(|node| node.kind())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&SyntaxKind::MATCH_EXPR));
    assert!(kinds.contains(&SyntaxKind::MATCH_ARM));
    assert!(kinds.contains(&SyntaxKind::VARIANT_PATTERN));
    assert!(kinds.contains(&SyntaxKind::WILDCARD_PATTERN));
}

#[test]
fn parses_literal_or_at_and_array_rest_patterns() {
    let input = r#"
fn value(message: Message, direction: Direction, items: Array[String], code: U64) -> String {
  match message {
    whole @ Message.Hello { body } => whole.body,
    _ => "x",
  };
  match direction {
    Direction.North | Direction.South => "ns",
    _ => "other",
  };
  match items {
    [first, ..rest] => first,
    [] => "empty",
  };
  match code {
    0 => "ok",
    _ => "error",
  }
}
"#;

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let kinds = parse
        .syntax()
        .descendants()
        .map(|node| node.kind())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&SyntaxKind::AT_PATTERN));
    assert!(kinds.contains(&SyntaxKind::OR_PATTERN));
    assert!(kinds.contains(&SyntaxKind::ARRAY_REST_PATTERN));
    assert!(kinds.contains(&SyntaxKind::LITERAL_PATTERN));
}

#[test]
fn parses_closures_type_operators_and_ranges() {
    let input = r#"
fn closures(items: Array[U64]) -> Array[U64] {
  items.map((x) => { let y = x + 1; y })
}

fn operators(a: U64) -> Bool {
  let cast = a as? U64;
  let typed = 0 :: I64;
  let checked = a is 0 | 1;
  0..=10;
  ..10;
  cast;
  typed;
  checked
}
"#;

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let kinds = parse
        .syntax()
        .descendants()
        .map(|node| node.kind())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&SyntaxKind::CLOSURE_EXPR));
    assert!(kinds.contains(&SyntaxKind::CLOSURE_PARAM_CLAUSE));
    assert!(kinds.contains(&SyntaxKind::TYPE_OP_EXPR));
    assert!(kinds.contains(&SyntaxKind::RANGE_EXPR));
}

#[test]
fn parses_partial_application_and_assignment_statements() {
    let input = r#"
fn demo(src: String, config: Config, state: State) -> String {
  @compile(..; loudly: true);
  @compile(src; optimize: true);
  config with= {
    build.jobs: 4,
  };
  state.count = 1;
  src = "next";
  src
}
"#;

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let kinds = parse
        .syntax()
        .descendants()
        .map(|node| node.kind())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&SyntaxKind::PARTIAL_APP_EXPR));
    assert!(kinds.contains(&SyntaxKind::ARG_PLACEHOLDER));
    assert!(kinds.contains(&SyntaxKind::COMPOUND_UPDATE_STMT));
    assert!(kinds.contains(&SyntaxKind::FIELD_ASSIGN_STMT));
    assert!(kinds.contains(&SyntaxKind::ASSIGN_STMT));
}

#[test]
fn lexes_and_parses_interpolated_strings() {
    let input = r#"
fn greet(name: String) -> String {
  `hello ${name}`
}
"#;

    let tokens = crate::lex(input);
    let kinds = tokens.iter().map(|token| token.kind).collect::<Vec<_>>();
    assert!(kinds.contains(&SyntaxKind::BACKTICK));
    assert!(kinds.contains(&SyntaxKind::INTERP_TEXT));
    assert!(kinds.contains(&SyntaxKind::INTERP_START));
    assert!(kinds.contains(&SyntaxKind::INTERP_END));

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let kinds = parse
        .syntax()
        .descendants()
        .map(|node| node.kind())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&SyntaxKind::INTERPOLATED_STRING_EXPR));
    assert!(kinds.contains(&SyntaxKind::STRING_INTERPOLATION));
}

#[test]
fn parses_interpolated_strings_with_nested_braces() {
    let input = r#"
fn label(ok: Bool) -> String {
  `status: ${if ok { "pass" } else { "fail" }}`
}
"#;

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let kinds = parse
        .syntax()
        .descendants()
        .map(|node| node.kind())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&SyntaxKind::INTERPOLATED_STRING_EXPR));
    assert!(kinds.contains(&SyntaxKind::STRING_INTERPOLATION));
    assert!(kinds.contains(&SyntaxKind::IF_EXPR));
}

#[test]
fn parses_path_literals_without_confusing_partial_application() {
    let input = r#"
fn build(name: String) {
  let output = @`out/${name}.o`;
  let include = @"include/lua.h";
  @tool("--flag", ..)
}
"#;

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let kinds = parse
        .syntax()
        .descendants()
        .map(|node| node.kind())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&SyntaxKind::PATH_LITERAL_EXPR));
    assert!(kinds.contains(&SyntaxKind::PARTIAL_APP_EXPR));
}

#[test]
fn path_literal_ast_exposes_literal_and_interpolated_forms() {
    let parse = parse(
        r#"
fn build(name: String) {
  @"include/lua.h";
  @`out/${name}.o`;
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
    assert!(exprs[0].literal().is_some());
    assert!(exprs[0].interpolated().is_none());
    assert!(exprs[1].literal().is_none());
    assert!(exprs[1].interpolated().is_some());
}

#[test]
fn recovers_to_later_top_level_items() {
    let input = r#"
let bad = 1

struct Good {
  x: U64,
}

fn ok() -> U64 {
  1
}
"#;

    let parse = parse(input);
    assert!(!parse.errors().is_empty());

    let kinds = parse
        .syntax()
        .children()
        .map(|node| node.kind())
        .filter(|kind| *kind != SyntaxKind::ATTRIBUTE_LIST)
        .collect::<Vec<_>>();
    assert!(kinds.contains(&SyntaxKind::STRUCT_DECL));
    assert!(kinds.contains(&SyntaxKind::FN_DECL));
}

#[test]
fn recovers_inside_blocks_after_missing_semicolons() {
    let input = r#"
fn demo() -> U64 {
  let x = 1
  let y = 2;
  y
}
"#;

    let parse = parse(input);
    assert!(!parse.errors().is_empty());

    let kinds = parse
        .syntax()
        .descendants()
        .map(|node| node.kind())
        .collect::<Vec<_>>();
    let let_count = kinds
        .iter()
        .filter(|kind| **kind == SyntaxKind::LET_STMT)
        .count();
    assert_eq!(let_count, 2);
    assert!(kinds.contains(&SyntaxKind::NAME_REF_EXPR));
}

#[test]
fn recovers_match_arms_after_missing_commas() {
    let input = r#"
fn demo(value: Value) -> String {
  match value {
    _ => "a"
    _ => "b",
  }
}
"#;

    let parse = parse(input);
    assert!(!parse.errors().is_empty());

    let arm_count = parse
        .syntax()
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::MATCH_ARM)
        .count();
    assert_eq!(arm_count, 2);
}

#[test]
fn ast_wraps_top_level_items_for_symbols() {
    let input = r#"
struct Config {
  jobs: U64,
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

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let file = SourceFile::cast_root(parse.syntax());
    let items = file.items().collect::<Vec<_>>();
    assert_eq!(items.len(), 3);

    let names = items
        .iter()
        .map(|item| item.name().map(|token| token.text().to_string()))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            Some("Config".to_string()),
            Some("Message".to_string()),
            Some("build".to_string())
        ]
    );

    let struct_decl = match &items[0] {
        Item::StructDecl(node) => node,
        item => panic!("expected struct decl, got {item:?}"),
    };
    let field_names = struct_decl
        .fields()
        .map(|field| field.name().unwrap().text().to_string())
        .collect::<Vec<_>>();
    assert_eq!(field_names, vec!["jobs".to_string()]);

    let enum_decl = match &items[1] {
        Item::EnumDecl(node) => node,
        item => panic!("expected enum decl, got {item:?}"),
    };
    let variant_names = enum_decl
        .variants()
        .map(|variant| variant.name().unwrap().text().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        variant_names,
        vec!["Hello".to_string(), "Goodbye".to_string()]
    );
}

#[test]
fn ast_wraps_key_expressions() {
    let input = r#"
fn demo(value: Value) -> String {
  if value.flag {
    `hello ${value.name}`
  } else {
    match value {
      _ => greet(value.name),
    }
  }
}
"#;

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let file = SourceFile::cast_root(parse.syntax());
    let function = match file.items().next().expect("function item") {
        Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let body = function.body().expect("function body");
    let mut exprs = body.exprs();

    let if_expr = match exprs.next().expect("if expr") {
        Expr::If(node) => node,
        expr => panic!("expected if expr, got {expr:?}"),
    };
    assert!(if_expr.condition().is_some());

    let interpolated = body
        .syntax()
        .descendants()
        .find_map(InterpolatedStringExpr::cast)
        .expect("interpolated string");
    let parts = interpolated.parts().collect::<Vec<_>>();
    assert!(matches!(&parts[0], StringPart::Text(token) if token.text() == "hello "));
    assert!(matches!(&parts[1], StringPart::Interpolation(_)));

    let match_expr = body
        .syntax()
        .descendants()
        .find_map(crate::MatchExpr::cast)
        .expect("match expr");
    let arm = match_expr.arms().next().expect("match arm");
    match arm.expr().expect("arm expr") {
        Expr::Call(_) => {}
        expr => panic!("expected call expr, got {expr:?}"),
    }
}

#[test]
fn ast_wraps_block_statements_and_common_lowering_nodes() {
    let input = r#"
fn demo(input: Config, state: State) -> String {
  let next = Config { jobs: input.jobs + 1 };
  state.count = next.jobs;
  return next.jobs
}
"#;

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let file = SourceFile::cast_root(parse.syntax());
    let function = match file.items().next().expect("function item") {
        Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let body = function.body().expect("function body");
    let statements = body.statements().collect::<Vec<_>>();
    assert_eq!(statements.len(), 3);

    let let_stmt = match &statements[0] {
        Stmt::Let(node) => node,
        stmt => panic!("expected let stmt, got {stmt:?}"),
    };
    assert_eq!(let_stmt.name().unwrap().text(), "next");
    let struct_literal = match let_stmt.expr().expect("let value") {
        Expr::StructLiteral(node) => node,
        expr => panic!("expected struct literal, got {expr:?}"),
    };
    assert_eq!(struct_literal.name().unwrap().text(), "Config");
    let field = struct_literal
        .fields()
        .next()
        .expect("struct literal field");
    assert_eq!(field.name().unwrap().text(), "jobs");
    let binary = match field.expr().expect("field expr") {
        Expr::Binary(node) => node,
        expr => panic!("expected binary expr, got {expr:?}"),
    };
    assert!(matches!(binary.lhs(), Some(Expr::Field(_))));
    assert!(matches!(binary.rhs(), Some(Expr::Literal(_))));

    let field_assign = match &statements[1] {
        Stmt::FieldAssign(node) => node,
        stmt => panic!("expected field assign stmt, got {stmt:?}"),
    };
    let segments = field_assign
        .target()
        .expect("field assign target")
        .segments()
        .map(|token| token.text().to_string())
        .collect::<Vec<_>>();
    assert_eq!(segments, vec!["state".to_string(), "count".to_string()]);
    assert!(matches!(field_assign.expr(), Some(Expr::Field(_))));

    let return_stmt = match &statements[2] {
        Stmt::Return(node) => node,
        stmt => panic!("expected return stmt, got {stmt:?}"),
    };
    let return_expr = match return_stmt.expr().expect("return expr") {
        Expr::Field(node) => node,
        expr => panic!("expected field expr, got {expr:?}"),
    };
    assert_eq!(return_expr.field_name().unwrap().text(), "jobs");
}

#[test]
fn ast_wraps_types_and_patterns_for_lowering() {
    let input = r#"
type Entry = Blob | Fn(String, Bool) #fs -> Result?

fn decode(input: Array[String]?, item: Message, ; loudly: Bool = false) -> Map[String, Entry?] {
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
    let variants = sum.variants().collect::<Vec<_>>();
    assert_eq!(variants.len(), 2);
    assert!(matches!(&variants[0], Type::Optional(_)));
    let function = match &variants[1] {
        Type::Optional(node) => match node.inner().expect("inner function type") {
            Type::Function(node) => node,
            ty => panic!("expected function type, got {ty:?}"),
        },
        ty => panic!("expected function type, got {ty:?}"),
    };
    let params = function.parameter_types().collect::<Vec<_>>();
    assert_eq!(params.len(), 2);
    assert!(matches!(&params[0], Type::Optional(_)));
    assert!(matches!(function.return_type(), Some(Type::Optional(_))));

    let function = match &items[1] {
        Item::FnDecl(node) => node,
        item => panic!("expected fn decl, got {item:?}"),
    };
    let input_param = function.parameters().next().expect("input parameter");
    let input_type = input_param
        .slot_type()
        .and_then(|slot| slot.ty())
        .expect("input type");
    let input_type = match input_type {
        Type::Optional(node) => node,
        ty => panic!("expected optional input type, got {ty:?}"),
    };
    let input_path = match input_type.inner().expect("inner input type") {
        Type::Path(node) => node,
        ty => panic!("expected path type, got {ty:?}"),
    };
    let input_segments = input_path
        .segments()
        .map(|token| token.text().to_string())
        .collect::<Vec<_>>();
    assert_eq!(input_segments, vec!["Array".to_string()]);
    let input_args = input_path.type_args().expect("type args");
    let arg_types = input_args.types().collect::<Vec<_>>();
    assert_eq!(arg_types.len(), 1);

    let return_type = function
        .return_type()
        .and_then(|ret| ret.ty())
        .expect("return type");
    let return_path = match return_type {
        Type::Optional(node) => match node.inner().expect("map inner type") {
            Type::Path(node) => node,
            ty => panic!("expected path type, got {ty:?}"),
        },
        ty => panic!("expected optional return wrapper, got {ty:?}"),
    };
    assert_eq!(
        return_path
            .segments()
            .map(|token| token.text().to_string())
            .collect::<Vec<_>>(),
        vec!["Map".to_string()]
    );

    let body = function.body().expect("function body");
    let match_expr = body
        .syntax()
        .descendants()
        .find_map(crate::MatchExpr::cast)
        .expect("match expr");
    let arms = match_expr.arms().collect::<Vec<_>>();
    assert_eq!(arms.len(), 4);

    let at_pattern = match arms[0].pattern().expect("at pattern") {
        Pattern::At(node) => node,
        pattern => panic!("expected at pattern, got {pattern:?}"),
    };
    assert_eq!(at_pattern.binding_name().unwrap().text(), "whole");
    let variant = match at_pattern.pattern().expect("variant pattern") {
        Pattern::Variant(node) => node,
        pattern => panic!("expected variant pattern, got {pattern:?}"),
    };
    assert_eq!(
        variant
            .segments()
            .map(|token| token.text().to_string())
            .collect::<Vec<_>>(),
        vec!["Message".to_string(), "Hello".to_string()]
    );

    let or_pattern = match arms[1].pattern().expect("or pattern") {
        Pattern::Or(node) => node,
        pattern => panic!("expected or pattern, got {pattern:?}"),
    };
    assert_eq!(or_pattern.patterns().count(), 2);

    let array_pattern = match arms[2].pattern().expect("array pattern") {
        Pattern::Array(node) => node,
        pattern => panic!("expected array pattern, got {pattern:?}"),
    };
    let items = array_pattern.items().collect::<Vec<_>>();
    assert_eq!(items.len(), 2);
    assert!(matches!(&items[0], Pattern::Binding(_)));
    let rest = match &items[1] {
        Pattern::ArrayRest(node) => node,
        pattern => panic!("expected array rest pattern, got {pattern:?}"),
    };
    assert_eq!(rest.name().unwrap().text(), "rest");
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
fn typed_parse_preserves_errors_and_tree_access() {
    let parsed = parse_source_file(
        r#"
let bad = 1

fn ok() -> U64 {
  1
}
"#,
    );

    assert!(!parsed.errors().is_empty());

    let file = parsed.tree();
    let items = file.items().collect::<Vec<_>>();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], Item::FnDecl(_)));
}

#[test]
fn ast_wraps_remaining_operator_and_postfix_expressions() {
    let input = r#"
fn demo(xs: Array[String], value: String) -> String {
  @compile(xs[0]);
  let closure = (item) => item?;
  let casted = xs[0] as String;
  let typed = 0 :: I64;
  let checked = value is Message.Hello { body };
  let ranged = ..10;
  let negated = -1;
  closure(value)
}
"#;

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let file = SourceFile::cast_root(parse.syntax());
    let function = match file.items().next().expect("function item") {
        Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let body = function.body().expect("function body");
    let statements = body.statements().collect::<Vec<_>>();

    let partial_app = match &statements[0] {
        Stmt::Expr(Expr::PartialApp(node)) => node,
        stmt => panic!("expected partial app expr, got {stmt:?}"),
    };
    assert!(matches!(partial_app.callee(), Some(Expr::NameRef(_))));

    let closure_stmt = match &statements[1] {
        Stmt::Let(node) => node,
        stmt => panic!("expected let stmt, got {stmt:?}"),
    };
    let closure = match closure_stmt.expr().expect("closure expr") {
        Expr::Closure(node) => node,
        expr => panic!("expected closure expr, got {expr:?}"),
    };
    assert_eq!(closure.parameters().count(), 1);
    assert!(matches!(closure.body(), Some(Expr::Fallible(_))));

    let cast_stmt = match &statements[2] {
        Stmt::Let(node) => node,
        stmt => panic!("expected cast let stmt, got {stmt:?}"),
    };
    let cast = match cast_stmt.expr().expect("cast expr") {
        Expr::TypeOp(node) => node,
        expr => panic!("expected type op expr, got {expr:?}"),
    };
    assert_eq!(cast.operator_token().unwrap().text(), "as");
    assert!(matches!(cast.expr(), Some(Expr::Index(_))));
    assert!(matches!(cast.ty(), Some(Type::Optional(_))));

    let typed_stmt = match &statements[3] {
        Stmt::Let(node) => node,
        stmt => panic!("expected typed let stmt, got {stmt:?}"),
    };
    let typed = match typed_stmt.expr().expect("typed expr") {
        Expr::TypeOp(node) => node,
        expr => panic!("expected type op expr, got {expr:?}"),
    };
    assert_eq!(typed.operator_token().unwrap().text(), "::");
    assert!(matches!(typed.ty(), Some(Type::Optional(_))));

    let checked_stmt = match &statements[4] {
        Stmt::Let(node) => node,
        stmt => panic!("expected checked let stmt, got {stmt:?}"),
    };
    let checked = match checked_stmt.expr().expect("checked expr") {
        Expr::TypeOp(node) => node,
        expr => panic!("expected type op expr, got {expr:?}"),
    };
    assert_eq!(checked.operator_token().unwrap().text(), "is");
    assert!(matches!(checked.pattern(), Some(Pattern::Variant(_))));

    let ranged_stmt = match &statements[5] {
        Stmt::Let(node) => node,
        stmt => panic!("expected range let stmt, got {stmt:?}"),
    };
    let range = match ranged_stmt.expr().expect("range expr") {
        Expr::Range(node) => node,
        expr => panic!("expected range expr, got {expr:?}"),
    };
    assert_eq!(range.operands().count(), 1);

    let negated_stmt = match &statements[6] {
        Stmt::Let(node) => node,
        stmt => panic!("expected prefix let stmt, got {stmt:?}"),
    };
    let prefix = match negated_stmt.expr().expect("prefix expr") {
        Expr::Prefix(node) => node,
        expr => panic!("expected prefix expr, got {expr:?}"),
    };
    assert_eq!(prefix.operator_token().unwrap().text(), "-");
    assert!(matches!(prefix.expr(), Some(Expr::Literal(_))));

    let call = match &statements[7] {
        Stmt::Expr(Expr::Call(node)) => node,
        stmt => panic!("expected call expr, got {stmt:?}"),
    };
    assert!(matches!(call.callee(), Some(Expr::NameRef(_))));
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
        idx.position(TextSize::from(5)),
        Position { line: 1, col: 1 }
    );
    assert_eq!(
        idx.position(TextSize::from(8)),
        Position { line: 2, col: 0 }
    );

    assert_eq!(
        idx.offset(Position { line: 1, col: 0 }),
        Some(TextSize::from(4))
    );
    assert_eq!(
        idx.offset(Position { line: 2, col: 2 }),
        Some(TextSize::from(10))
    );
}

#[test]
fn document_symbols_extracts_items_with_children() {
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

newtype PathBuf of String

type Entry = Blob | Tree

fn build(src: String) -> String {
  src
}
"#;

    let parsed = parse_source_file(input);
    assert_eq!(parsed.errors(), []);

    let symbols = document_symbols(&parsed.tree());
    assert_eq!(symbols.len(), 5);

    assert_eq!(symbols[0].name, "Config");
    assert_eq!(symbols[0].kind, SymbolKind::Struct);
    assert_eq!(symbols[0].children.len(), 2);
    assert_eq!(symbols[0].children[0].name, "jobs");
    assert_eq!(symbols[0].children[0].kind, SymbolKind::Field);
    assert_eq!(symbols[0].children[1].name, "name");

    assert_eq!(symbols[1].name, "Message");
    assert_eq!(symbols[1].kind, SymbolKind::Enum);
    assert_eq!(symbols[1].children.len(), 2);
    assert_eq!(symbols[1].children[0].name, "Hello");
    assert_eq!(symbols[1].children[0].kind, SymbolKind::Variant);
    assert_eq!(symbols[1].children[0].children.len(), 1);
    assert_eq!(symbols[1].children[0].children[0].name, "body");
    assert_eq!(symbols[1].children[1].name, "Goodbye");
    assert_eq!(symbols[1].children[1].children.len(), 0);

    assert_eq!(symbols[2].name, "PathBuf");
    assert_eq!(symbols[2].kind, SymbolKind::Newtype);

    assert_eq!(symbols[3].name, "Entry");
    assert_eq!(symbols[3].kind, SymbolKind::TypeAlias);

    assert_eq!(symbols[4].name, "build");
    assert_eq!(symbols[4].kind, SymbolKind::Function);
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

    // Offset inside the "42" literal within the match arm body block
    let offset = TextSize::from(input.find("42").unwrap() as u32);
    let crumbs = enclosing_nodes(&parsed.tree().syntax(), offset);

    let kinds: Vec<_> = crumbs.iter().map(|b| b.kind).collect();
    assert!(kinds.contains(&SyntaxKind::SOURCE_FILE));
    assert!(kinds.contains(&SyntaxKind::FN_DECL));
    assert!(kinds.contains(&SyntaxKind::MATCH_EXPR));
    assert!(kinds.contains(&SyntaxKind::MATCH_ARM));
    assert!(kinds.contains(&SyntaxKind::BLOCK_EXPR));

    let fn_crumb = crumbs
        .iter()
        .find(|b| b.kind == SyntaxKind::FN_DECL)
        .unwrap();
    assert_eq!(fn_crumb.name.as_deref(), Some("outer"));
}

#[test]
fn folding_ranges_finds_blocks_and_items() {
    let input = r#"
struct Config {
  jobs: U64,
}

fn build() -> U64 {
  if true {
    1
  } else {
    0
  }
}
"#;

    let parsed = parse_source_file(input);
    assert_eq!(parsed.errors(), []);

    let ranges = folding_ranges(&parsed.tree().syntax());
    let kinds: Vec<_> = ranges.iter().map(|r| r.kind).collect();
    assert!(kinds.iter().all(|k| *k == FoldingRangeKind::Block));
    // At minimum: struct decl, struct body, fn decl, fn body block, if expr
    assert!(ranges.len() >= 5);
}

#[test]
fn semantic_tokens_classifies_all_token_kinds() {
    let input = r#"
struct Point {
  x: F64,
  y: F64,
}

fn distance(a: Point, b: Point) -> F64 {
  let dx = a.x - b.x;
  let dy = a.y - b.y;
  dx * dy
}
"#;

    let parsed = parse_source_file(input);
    assert_eq!(parsed.errors(), []);

    let tokens = semantic_tokens(&parsed.tree().syntax());

    // Collect (text, type) pairs using the source
    let classified: Vec<(&str, SemanticTokenType)> = tokens
        .iter()
        .map(|t| {
            let start = u32::from(t.range.start()) as usize;
            let end = u32::from(t.range.end()) as usize;
            (&input[start..end], t.token_type)
        })
        .collect();

    use SemanticTokenType as T;

    // Check keywords
    assert!(classified.contains(&("struct", T::Keyword)));
    assert!(classified.contains(&("fn", T::Keyword)));
    assert!(classified.contains(&("let", T::Keyword)));

    // Check type names
    assert!(classified.contains(&("Point", T::Type)));
    assert!(classified.contains(&("F64", T::Type)));

    // Check function name
    assert!(classified.contains(&("distance", T::Function)));

    // Check parameters
    assert!(classified.contains(&("a", T::Parameter)));
    assert!(classified.contains(&("b", T::Parameter)));

    // Check field names
    assert!(classified.contains(&("x", T::Property)));
    assert!(classified.contains(&("y", T::Property)));

    // Check variables
    assert!(classified.contains(&("dx", T::Variable)));
    assert!(classified.contains(&("dy", T::Variable)));

    // Check operators
    assert!(classified.contains(&("-", T::Operator)));
    assert!(classified.contains(&("*", T::Operator)));
}

#[test]
fn semantic_tokens_handles_strings_and_interpolation() {
    let input = r#"
fn greet(name: String) -> String {
  let greeting = "hello";
  `${greeting}, ${name}!`
}
"#;

    let parsed = parse_source_file(input);
    assert_eq!(parsed.errors(), []);

    let tokens = semantic_tokens(&parsed.tree().syntax());
    let classified: Vec<(&str, SemanticTokenType)> = tokens
        .iter()
        .map(|t| {
            let start = u32::from(t.range.start()) as usize;
            let end = u32::from(t.range.end()) as usize;
            (&input[start..end], t.token_type)
        })
        .collect();

    use SemanticTokenType as T;

    assert!(classified.contains(&("\"hello\"", T::String)));
    assert!(classified.contains(&("greet", T::Function)));
    assert!(classified.contains(&("name", T::Parameter)));
    assert!(classified.contains(&("greeting", T::Variable)));
    // Backtick delimiters
    assert!(classified.iter().filter(|(_, ty)| *ty == T::String).count() >= 2);
    // Interpolation delimiters
    assert!(classified.contains(&("${", T::Operator)));
}

#[test]
fn semantic_tokens_handles_match_and_enum_patterns() {
    let input = r#"
enum Shape {
  struct Circle { radius: F64 },
  struct Rect { w: F64, h: F64 },
}

fn area(s: Shape) -> F64 {
  match s {
    Circle { radius } => radius * radius,
    Rect { w, h } => w * h,
  }
}
"#;

    let parsed = parse_source_file(input);
    assert_eq!(parsed.errors(), []);

    let tokens = semantic_tokens(&parsed.tree().syntax());
    let classified: Vec<(&str, SemanticTokenType)> = tokens
        .iter()
        .map(|t| {
            let start = u32::from(t.range.start()) as usize;
            let end = u32::from(t.range.end()) as usize;
            (&input[start..end], t.token_type)
        })
        .collect();

    use SemanticTokenType as T;

    assert!(classified.contains(&("match", T::Keyword)));
    assert!(classified.contains(&("enum", T::Keyword)));
    assert!(classified.contains(&("Shape", T::Type)));
    assert!(classified.contains(&("area", T::Function)));
    assert!(classified.contains(&("radius", T::Property)));
}

#[test]
fn doc_comments_lexed_as_distinct_kind() {
    let input = "/// doc\n// regular\n//// not doc\n";
    let tokens = crate::lex(input);
    let kinds: Vec<_> = tokens
        .iter()
        .filter(|t| !matches!(t.kind, SyntaxKind::WHITESPACE))
        .map(|t| t.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![
            SyntaxKind::DOC_COMMENT,
            SyntaxKind::LINE_COMMENT,
            SyntaxKind::LINE_COMMENT,
        ]
    );
}

#[test]
fn doc_comment_extracted_from_fn_decl() {
    let input = r#"
/// Adds two numbers.
/// Returns their sum.
fn add(a: U64, b: U64) -> U64 {
    a
}
"#;
    let parsed = parse_source_file(input);
    assert_eq!(parsed.errors(), []);

    let items: Vec<_> = parsed.tree().items().collect();
    assert_eq!(items.len(), 1);
    let doc = doc_comment(items[0].syntax()).expect("should have doc comment");
    assert_eq!(doc, "Adds two numbers.\nReturns their sum.");
}

#[test]
fn doc_comment_none_without_doc_lines() {
    let input = r#"
// Just a regular comment
fn foo() {}
"#;
    let parsed = parse_source_file(input);
    assert_eq!(parsed.errors(), []);

    let items: Vec<_> = parsed.tree().items().collect();
    let doc = doc_comment(items[0].syntax());
    assert!(doc.is_none(), "regular comments should not be doc comments");
}

#[test]
fn doc_comment_on_struct() {
    let input = r#"
/// A 2D point.
struct Point {
    x: F64,
    y: F64,
}
"#;
    let parsed = parse_source_file(input);
    assert_eq!(parsed.errors(), []);

    let items: Vec<_> = parsed.tree().items().collect();
    let doc = doc_comment(items[0].syntax()).expect("should have doc comment");
    assert_eq!(doc, "A 2D point.");
}

// ── Formatter tests ──────────────────────────────────────────────────

use crate::format;

#[test]
fn fmt_simple_struct() {
    let input = "struct  Point{x:F64,  y:F64}";
    let parsed = parse_source_file(input);
    let out = format(&parsed.tree().syntax());
    assert_eq!(out, "struct Point {\n    x: F64,\n    y: F64,\n}\n");
}

#[test]
fn fmt_enum_with_variants() {
    let input = "enum Direction{struct North,struct South}";
    let parsed = parse_source_file(input);
    let out = format(&parsed.tree().syntax());
    assert_eq!(
        out,
        "enum Direction {\n    struct North,\n    struct South,\n}\n"
    );
}

#[test]
fn fmt_function() {
    let input = "fn  add(a:I64,b:I64)->I64{a+b}";
    let parsed = parse_source_file(input);
    let out = format(&parsed.tree().syntax());
    assert_eq!(out, "fn add(a: I64, b: I64) -> I64 {\n    a + b\n}\n");
}

#[test]
fn fmt_preserves_comments() {
    let input = "// A point\nstruct Point {\n    // x coord\n    x: F64,\n}";
    let parsed = parse_source_file(input);
    let out = format(&parsed.tree().syntax());
    assert!(out.contains("// A point"), "should preserve top comment");
    assert!(out.contains("// x coord"), "should preserve field comment");
}

#[test]
fn fmt_idempotent_struct() {
    let input = "struct Point {\n    x: F64,\n    y: F64,\n}\n";
    let parsed = parse_source_file(input);
    let pass1 = format(&parsed.tree().syntax());
    let parsed2 = parse_source_file(&pass1);
    let pass2 = format(&parsed2.tree().syntax());
    assert_eq!(pass1, pass2, "formatter should be idempotent");
}

#[test]
fn fmt_idempotent_type_decl_vx() {
    let input = include_str!("../../../docs/content/spec/type-decl.vx");
    let parsed = parse_source_file(input);
    let pass1 = format(&parsed.tree().syntax());
    let parsed2 = parse_source_file(&pass1);
    let pass2 = format(&parsed2.tree().syntax());
    assert_eq!(
        pass1, pass2,
        "formatter should be idempotent on type-decl.vx"
    );
}

#[test]
fn fmt_idempotent_sample_vx() {
    let input = include_str!("../../../docs/content/spec/sample.vx");
    let parsed = parse_source_file(input);
    let pass1 = format(&parsed.tree().syntax());
    let parsed2 = parse_source_file(&pass1);
    let pass2 = format(&parsed2.tree().syntax());
    assert_eq!(pass1, pass2, "formatter should be idempotent on sample.vx");
}
