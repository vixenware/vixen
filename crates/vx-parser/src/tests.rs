use crate::{
    AstNode, Expr, InterpolatedStringExpr, Item, Pattern, SourceFile, Stmt, StringPart, SyntaxKind,
    Type, parse, parse_source_file,
};

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
