use crate::{SyntaxKind, parse};

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
    assert!(
        tokens
            .iter()
            .any(|token| token.kind == SyntaxKind::INTERP_STRING)
    );

    let parse = parse(input);
    assert_eq!(parse.errors(), []);

    let kinds = parse
        .syntax()
        .descendants()
        .map(|node| node.kind())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&SyntaxKind::LITERAL_EXPR));
}
