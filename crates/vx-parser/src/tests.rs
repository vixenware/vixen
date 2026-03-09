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
