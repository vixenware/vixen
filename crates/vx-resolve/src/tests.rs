use vx_parser::{AstNode, SourceFile, parse};

use crate::scope::ScopeTree;
use crate::{DefKind, ScopeKind, resolve};

fn parse_file(src: &str) -> SourceFile {
    let parsed = parse(src);
    assert!(
        parsed.errors().is_empty(),
        "parse errors: {:?}",
        parsed.errors()
    );
    SourceFile::cast(parsed.syntax()).unwrap()
}

#[test]
fn module_scope_collects_all_items() {
    let file = parse_file(
        r#"
fn greet() {}
struct Point { x: U64, y: U64 }
enum Shape { struct Circle { radius: F64 }, struct Rect { w: F64, h: F64 } }
newtype Blake3 of String
type Id = U64
"#,
    );
    let tree = ScopeTree::build(&file);
    let defs = tree.definitions(crate::ScopeId::ROOT);
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, vec!["greet", "Point", "Shape", "Blake3", "Id"]);

    assert_eq!(defs[0].kind, DefKind::Function);
    assert_eq!(defs[1].kind, DefKind::Struct);
    assert_eq!(defs[2].kind, DefKind::Enum);
    assert_eq!(defs[3].kind, DefKind::Newtype);
    assert_eq!(defs[4].kind, DefKind::TypeAlias);
}

#[test]
fn fn_params_visible_in_body() {
    let file = parse_file(
        r#"
fn add(a: U64, b: U64) -> U64 {
    a
}
"#,
    );
    let res = resolve(&file);
    assert!(res.errors.is_empty(), "errors: {:?}", res.errors);

    let a_ref = res.refs.iter().find(|r| r.name == "a").unwrap();
    let def = a_ref.target.as_ref().unwrap();
    assert_eq!(def.kind, DefKind::Parameter);
    assert_eq!(def.name, "a");
}

#[test]
fn let_binding_visible_after_definition() {
    let file = parse_file(
        r#"
fn main() {
    let x = 42;
    x
}
"#,
    );
    let res = resolve(&file);
    assert!(res.errors.is_empty(), "errors: {:?}", res.errors);

    let x_ref = res.refs.iter().find(|r| r.name == "x").unwrap();
    let def = x_ref.target.as_ref().unwrap();
    assert_eq!(def.kind, DefKind::Binding);
}

#[test]
fn match_arm_bindings() {
    let file = parse_file(
        r#"
fn test(x: U64) {
    match x {
        y => y,
    }
}
"#,
    );
    let res = resolve(&file);
    // The `y` in the body of the match arm should resolve to the pattern binding
    let y_refs: Vec<_> = res.refs.iter().filter(|r| r.name == "y").collect();
    assert!(!y_refs.is_empty());
    for r in &y_refs {
        assert!(r.target.is_ok(), "y should resolve: {:?}", r.target);
    }
}

#[test]
fn closure_params() {
    let file = parse_file(
        r#"
fn main() {
    let f = (x) => x;
    f
}
"#,
    );
    let res = resolve(&file);
    // x inside the closure should resolve to the closure parameter
    let x_refs: Vec<_> = res.refs.iter().filter(|r| r.name == "x").collect();
    assert!(!x_refs.is_empty());
    for r in &x_refs {
        assert!(r.target.is_ok(), "x should resolve: {:?}", r.target);
    }
}

#[test]
fn unresolved_name() {
    let file = parse_file(
        r#"
fn main() {
    unknown_name
}
"#,
    );
    let res = resolve(&file);
    assert_eq!(res.errors.len(), 1);
    match &res.errors[0] {
        crate::ResolveError::Unresolved { name, .. } => {
            assert_eq!(name, "unknown_name");
        }
    }
}

#[test]
fn nested_scopes_shadow() {
    let file = parse_file(
        r#"
fn main() {
    let x = 1;
    {
        let x = 2;
        x
    }
}
"#,
    );
    let res = resolve(&file);
    assert!(res.errors.is_empty(), "errors: {:?}", res.errors);
    // Both `x` references should resolve (to different definitions)
    let x_refs: Vec<_> = res.refs.iter().filter(|r| r.name == "x").collect();
    assert!(!x_refs.is_empty());
}

#[test]
fn top_level_fn_visible_from_other_fn() {
    let file = parse_file(
        r#"
fn caller() {
    callee
}

fn callee() {}
"#,
    );
    let res = resolve(&file);
    assert!(
        res.errors.is_empty(),
        "top-level functions should be visible regardless of order: {:?}",
        res.errors
    );
}

#[test]
fn scope_tree_structure() {
    let file = parse_file(
        r#"
fn outer() {
    let x = 1;
    {
        let y = 2;
    }
}
"#,
    );
    let tree = ScopeTree::build(&file);
    // Module -> Function -> Block (fn body) -> Block (inner { })
    assert!(tree.scope_count() >= 4);
    assert_eq!(tree.scope_kind(crate::ScopeId::ROOT), ScopeKind::Module);
}

#[test]
fn dollar_idents_not_resolved() {
    let file = parse_file(
        r#"
fn main() {
    let f = $;
    f
}
"#,
    );
    let res = resolve(&file);
    // $ should not produce an unresolved error — it's an implicit closure param
    let dollar_errors: Vec<_> = res
        .errors
        .iter()
        .filter(|e| match e {
            crate::ResolveError::Unresolved { name, .. } => name.starts_with('$'),
        })
        .collect();
    assert!(
        dollar_errors.is_empty(),
        "$ should not be resolved through normal lookup"
    );
}
