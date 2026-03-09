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

#[test]
fn type_collect_structs_and_enums() {
    let file = parse_file(
        r#"
struct Point {
    x: F64,
    y: F64,
}

struct Config {
    width: U64 = 800,
    title: String = "untitled",
}

enum Shape {
    struct Circle {
        radius: F64,
    },
    struct Rectangle {
        width: F64,
        height: F64,
    },
    struct Unknown,
}

fn area(shape: Shape) -> F64 {}
"#,
    );

    let tc = crate::collect_types(&file);

    // Point should have 2 fields of type F64
    let point_ids = tc.env.lookup("Point");
    assert_eq!(point_ids.len(), 1);
    let point = tc.env.get(point_ids[0]);
    match point {
        vx_types::TypeDef::Struct(s) => {
            assert_eq!(s.fields.len(), 2);
            assert_eq!(s.fields[0].name, "x");
            assert!(matches!(
                s.fields[0].ty,
                vx_types::Ty::Scalar(vx_types::Scalar::F64)
            ));
            assert_eq!(s.fields[1].name, "y");
        }
        _ => panic!("expected struct"),
    }

    // Config should have has_default on both fields
    let config_ids = tc.env.lookup("Config");
    assert_eq!(config_ids.len(), 1);
    match tc.env.get(config_ids[0]) {
        vx_types::TypeDef::Struct(s) => {
            assert_eq!(s.fields.len(), 2);
            assert!(s.fields[0].has_default);
            assert!(s.fields[1].has_default);
        }
        _ => panic!("expected struct"),
    }

    // Shape should be an enum with 3 variants
    let shape_ids = tc.env.lookup("Shape");
    assert_eq!(shape_ids.len(), 1);
    match tc.env.get(shape_ids[0]) {
        vx_types::TypeDef::Enum(e) => {
            assert_eq!(e.variants.len(), 3);
            // Circle variant should have a radius field
            let circle = &tc.env[e.variants[0]];
            match circle {
                vx_types::TypeDef::Struct(s) => {
                    assert_eq!(s.name, "Circle");
                    assert_eq!(s.fields.len(), 1);
                    assert_eq!(s.fields[0].name, "radius");
                    assert!(s.parent_enum.is_some());
                }
                _ => panic!("expected struct variant"),
            }
            // Unknown should have no fields
            let unknown = &tc.env[e.variants[2]];
            match unknown {
                vx_types::TypeDef::Struct(s) => {
                    assert_eq!(s.name, "Unknown");
                    assert!(s.fields.is_empty());
                }
                _ => panic!("expected struct variant"),
            }
        }
        _ => panic!("expected enum"),
    }

    // Function signature should be collected
    assert_eq!(tc.range_to_fn_sig.len(), 1);
    let sig = tc.range_to_fn_sig.values().next().unwrap();
    assert_eq!(sig.name, "area");
    assert_eq!(sig.positional_params.len(), 1);
    assert_eq!(sig.positional_params[0].name, "shape");
    assert!(matches!(
        sig.ret,
        vx_types::Ty::Scalar(vx_types::Scalar::F64)
    ));
}

#[test]
fn type_collect_generics_and_newtypes() {
    let file = parse_file(
        r#"
struct Pair[A, B] {
    first: A,
    second: B,
}

newtype UserId of U64

type Name = String
"#,
    );

    let tc = crate::collect_types(&file);

    // Pair should have type params and field types referencing them
    let pair_ids = tc.env.lookup("Pair");
    assert_eq!(pair_ids.len(), 1);
    match tc.env.get(pair_ids[0]) {
        vx_types::TypeDef::Struct(s) => {
            assert_eq!(s.type_params, vec!["A", "B"]);
            assert_eq!(s.fields.len(), 2);
            assert!(
                matches!(&s.fields[0].ty, vx_types::Ty::Param(p) if p.name == "A" && p.index == 0)
            );
            assert!(
                matches!(&s.fields[1].ty, vx_types::Ty::Param(p) if p.name == "B" && p.index == 1)
            );
        }
        _ => panic!("expected struct"),
    }

    // UserId should be a newtype wrapping U64
    let uid_ids = tc.env.lookup("UserId");
    assert_eq!(uid_ids.len(), 1);
    match tc.env.get(uid_ids[0]) {
        vx_types::TypeDef::Newtype(n) => {
            assert!(matches!(
                n.inner,
                vx_types::Ty::Scalar(vx_types::Scalar::U64)
            ));
        }
        _ => panic!("expected newtype"),
    }

    // Name should be an alias for String
    let name_ids = tc.env.lookup("Name");
    assert_eq!(name_ids.len(), 1);
    match tc.env.get(name_ids[0]) {
        vx_types::TypeDef::Alias(a) => {
            assert!(matches!(
                a.target,
                vx_types::Ty::Scalar(vx_types::Scalar::String)
            ));
        }
        _ => panic!("expected alias"),
    }
}
