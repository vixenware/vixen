use std::{fs, path::Path};

use rowan::{TextRange, TextSize};
use vx_parser::{AstNode, SourceFile, parse};

use crate::cap_check::{CapErrorKind, CapSet};
use crate::scope::ScopeTree;
use crate::{
    DefKind, ImportGraph, ScopeKind, SourceFileRef, SourceId, resolve, resolve_in_program,
};
use vx_types::{Scalar, Ty};

const TEST_SOURCE_ID: SourceId = SourceId(0);

fn parse_file(src: &str) -> SourceFile {
    let parsed = parse(src);
    assert!(
        parsed.errors().is_empty(),
        "parse errors: {:?}",
        parsed.errors()
    );
    SourceFile::cast(parsed.syntax()).unwrap()
}

fn parse_file_path(path: &Path) -> SourceFile {
    let source = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    parse_file(&source)
}

fn stdlib_program(file: &SourceFile) -> (Vec<SourceFileRef>, ImportGraph) {
    let mut sources = vec![SourceFileRef {
        id: TEST_SOURCE_ID,
        file: file.clone(),
    }];
    let mut imports = ImportGraph::new();

    for (index, stdlib) in crate::stdlib_sources().iter().enumerate() {
        let stdlib_file = parse_file(stdlib.text);
        let source_id = SourceId(index as u32 + 1);
        sources.push(SourceFileRef {
            id: source_id,
            file: stdlib_file,
        });
        imports.add_import(TEST_SOURCE_ID, source_id);
    }

    (sources, imports)
}

fn resolve_with_stdlib(file: &SourceFile) -> crate::NameResolution {
    let (sources, imports) = stdlib_program(file);
    resolve_in_program(
        SourceFileRef {
            id: TEST_SOURCE_ID,
            file: file.clone(),
        },
        &sources,
        imports,
    )
}

fn collect_types_with_stdlib(file: &SourceFile) -> crate::TypeCollection {
    let (sources, _) = stdlib_program(file);
    crate::collect_types_for_sources(&sources)
}

fn nominal_ty(types: &crate::TypeCollection, name: &str) -> Ty {
    let id = *types
        .env
        .lookup(name)
        .iter()
        .next()
        .unwrap_or_else(|| panic!("missing type `{name}`"));
    match types.env.get(id) {
        vx_types::TypeDef::Struct(_) => Ty::Struct(vx_types::StructTy {
            id,
            type_args: vec![],
        }),
        vx_types::TypeDef::Enum(_) => Ty::Enum(vx_types::EnumTy {
            id,
            type_args: vec![],
        }),
        vx_types::TypeDef::Newtype(_) => Ty::Newtype(vx_types::NewtypeTy {
            id,
            type_args: vec![],
        }),
        vx_types::TypeDef::Alias(_) => Ty::Alias(vx_types::AliasTy {
            id,
            type_args: vec![],
        }),
        vx_types::TypeDef::Opaque(_) => Ty::Opaque(vx_types::OpaqueTy {
            id,
            type_args: vec![],
        }),
    }
}

fn span_for_text(src: &str, needle: &str) -> crate::SourceSpan {
    let start = src
        .find(needle)
        .unwrap_or_else(|| panic!("missing `{needle}`"));
    let start = TextSize::new(start as u32);
    let end = start + TextSize::new(needle.len() as u32);
    crate::SourceSpan::new(TEST_SOURCE_ID, TextRange::new(start, end))
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
fn option_variants_are_visible_unqualified() {
    let file = parse_file(
        r#"
enum Option[T] {
    struct Some { value: T },
    struct None,
}
"#,
    );
    let tree = ScopeTree::build(&file);
    let defs = tree.definitions(crate::ScopeId::ROOT);
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, vec!["Some", "None", "Option"]);
    assert_eq!(defs[0].kind, DefKind::Variant);
    assert_eq!(defs[1].kind, DefKind::Variant);
    assert_eq!(defs[2].kind, DefKind::Enum);
}

#[test]
fn namespace_scope_collects_member_items() {
    let file = parse_file(
        r#"
namespace Blob {
    fn Blob(s: String) -> Self;
    fn utf8(self: Self) -> String;
}
"#,
    );
    let tree = ScopeTree::build(&file);
    let defs = tree.definitions(crate::ScopeId::ROOT);
    let namespace = defs
        .iter()
        .find(|def| def.name == "Blob")
        .expect("Blob namespace");
    assert_eq!(namespace.kind, DefKind::Namespace);

    let scope = tree.owned_scope(namespace).expect("namespace scope");
    let member_names: Vec<_> = tree
        .definitions(scope)
        .iter()
        .map(|def| (def.name.as_str(), def.kind))
        .collect();
    assert_eq!(
        member_names,
        vec![("Blob", DefKind::Function), ("utf8", DefKind::Function),]
    );
}

#[test]
fn namespace_member_function_signatures_are_collected() {
    let file = parse_file(
        r#"
type Blob;

namespace Blob {
    fn Blob(s: String) -> Self;
    fn utf8(self: Self) -> String;
}
"#,
    );
    let tree = ScopeTree::build(&file);
    let namespace = tree
        .definitions(crate::ScopeId::ROOT)
        .iter()
        .find(|def| def.name == "Blob" && def.kind == DefKind::Namespace)
        .expect("Blob namespace");
    let scope = tree.owned_scope(namespace).expect("namespace scope");
    let utf8 = tree
        .definitions(scope)
        .iter()
        .find(|def| def.name == "utf8")
        .expect("utf8 member");

    let types = crate::collect_types(&file);
    let sig = types.fn_sig_for_def(utf8).expect("utf8 signature");
    assert_eq!(sig.name, "utf8");
    assert_eq!(sig.positional_params.len(), 1);
    assert_eq!(sig.positional_params[0].name, "self");
    assert_eq!(sig.positional_params[0].ty, nominal_ty(&types, "Blob"));
    assert_eq!(sig.ret, Ty::Scalar(Scalar::String));
}

#[test]
fn generic_namespace_member_signatures_keep_namespace_type_params() {
    let file = parse_file(
        r#"
namespace Array[T] {
    fn map[U](self: Self, f: Fn(T) -> U) -> Array[U];
}
"#,
    );
    let tree = ScopeTree::build(&file);
    let array_namespace = tree
        .definitions(crate::ScopeId::ROOT)
        .iter()
        .find(|def| def.name == "Array")
        .expect("Array namespace");
    let scope = tree.owned_scope(array_namespace).expect("namespace scope");
    let map = tree
        .definitions(scope)
        .iter()
        .find(|def| def.name == "map")
        .expect("map member");

    let types = crate::collect_types(&file);
    let sig = types.fn_sig_for_def(map).expect("map signature");
    assert_eq!(sig.type_params, vec!["T".to_string(), "U".to_string()]);
    assert_eq!(
        sig.positional_params[0].ty,
        Ty::Array(Box::new(Ty::Param(vx_types::TypeParam {
            name: "T".to_string(),
            index: 0,
        })))
    );
    assert_eq!(
        sig.ret,
        Ty::Array(Box::new(Ty::Param(vx_types::TypeParam {
            name: "U".to_string(),
            index: 1,
        })))
    );
}

#[test]
fn implicit_self_uses_namespace_self_type() {
    let file = parse_file(
        r#"
namespace Array[T] {
    fn flatten(self) -> T;
}
"#,
    );
    let tree = ScopeTree::build(&file);
    let array_namespace = tree
        .definitions(crate::ScopeId::ROOT)
        .iter()
        .find(|def| def.name == "Array")
        .expect("Array namespace");
    let scope = tree.owned_scope(array_namespace).expect("namespace scope");
    let flatten = tree
        .definitions(scope)
        .iter()
        .find(|def| def.name == "flatten")
        .expect("flatten member");

    let types = crate::collect_types(&file);
    let sig = types.fn_sig_for_def(flatten).expect("flatten signature");
    assert_eq!(
        sig.positional_params[0].ty,
        Ty::Array(Box::new(Ty::Param(vx_types::TypeParam {
            name: "T".to_string(),
            index: 0,
        })))
    );
    assert_eq!(
        sig.ret,
        Ty::Param(vx_types::TypeParam {
            name: "T".to_string(),
            index: 0,
        })
    );
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
fn nested_function_calls_use_collected_signature() {
    let src = r#"
fn main(ws: Path) {
    fn compile(path: Path) -> Tree (
        Tree(path)
    )

    compile(ws)
}
"#;
    let (result, types) = type_check_with_types(src);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let ty = find_expr_type(&result, src, "compile(ws)");
    assert_eq!(ty, Some(nominal_ty(&types, "Tree")));
}

#[test]
fn top_level_let_visible_from_function_body() {
    let file = parse_file(
        r#"
let greeting = "hello"

fn main() {
    greeting
}
"#,
    );
    let res = resolve(&file);
    assert!(res.errors.is_empty(), "errors: {:?}", res.errors);

    let greeting_ref = res.refs.iter().find(|r| r.name == "greeting").unwrap();
    let def = greeting_ref.target.as_ref().unwrap();
    assert_eq!(def.kind, DefKind::Binding);
    assert_eq!(def.name, "greeting");
}

#[test]
fn top_level_let_binding_type_is_visible_from_function_body() {
    let src = r#"
let paths = ["a.c", "b.c"]

fn blobify(path: String) -> Blob {
    Blob(path)
}

fn main() {
    paths.map(blobify)
}
"#;
    let result = type_check(src);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let ty = find_expr_type(&result, src, "paths.map(blobify)");
    assert_eq!(
        ty,
        Some(Ty::Array(Box::new(nominal_ty(
            &collect_types_with_stdlib(&parse_file(src)),
            "Blob",
        ))))
    );
}

#[test]
fn top_level_path_array_binding_type_is_visible_from_function_body() {
    let src = r#"
let paths = [@a.c, @b.c]

fn compile(path: Path) -> Tree {
    Tree(path)
}

fn main() {
    paths.map(compile)
}
"#;
    let (result, types) = type_check_with_types(src);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let ty = find_expr_type(&result, src, "paths.map(compile)");
    assert_eq!(ty, Some(Ty::Array(Box::new(nominal_ty(&types, "Tree")))));
}

#[test]
fn top_level_let_is_not_visible_before_definition() {
    let file = parse_file(
        r#"
let first = second
let second = "hello"
"#,
    );
    let res = resolve(&file);
    assert_eq!(res.errors.len(), 1, "errors: {:?}", res.errors);
    match &res.errors[0] {
        crate::ResolveError::Unresolved { name, .. } => assert_eq!(name, "second"),
    }
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
fn nested_type_declarations_are_visible_in_block_scope() {
    let file = parse_file(
        r#"
fn main() {
    struct Inner {
        value: U64,
    };
    enum Choice {
        struct Some,
    };
    newtype Id of U64;
    type MaybeInner = Inner?;
    Inner { value: 1 };
}
"#,
    );
    let tree = ScopeTree::build(&file);

    let inner = tree
        .all_definitions()
        .find(|def| def.name == "Inner")
        .expect("nested struct should be collected");
    let choice = tree
        .all_definitions()
        .find(|def| def.name == "Choice")
        .expect("nested enum should be collected");
    let id = tree
        .all_definitions()
        .find(|def| def.name == "Id")
        .expect("nested newtype should be collected");
    let maybe_inner = tree
        .all_definitions()
        .find(|def| def.name == "MaybeInner")
        .expect("nested type alias should be collected");

    assert_eq!(inner.kind, DefKind::Struct);
    assert_eq!(choice.kind, DefKind::Enum);
    assert_eq!(id.kind, DefKind::Newtype);
    assert_eq!(maybe_inner.kind, DefKind::TypeAlias);
    assert_eq!(tree.scope_kind(inner.scope), ScopeKind::Block);
    assert_eq!(inner.scope, choice.scope);
    assert_eq!(inner.scope, id.scope);
    assert_eq!(inner.scope, maybe_inner.scope);
}

#[test]
fn old_at_attribute_syntax_is_rejected() {
    let parsed = parse(
        r#"
@rename("hello")
struct Message
"#,
    );

    assert!(
        !parsed.errors().is_empty(),
        "old @attribute syntax should produce a parse error",
    );
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
    assert_eq!(tc.def_to_fn_sig.len(), 1);
    let sig = tc.def_to_fn_sig.values().next().unwrap();
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

#[test]
fn type_collect_opaque_types() {
    let file = parse_file(
        r#"
type Token;

fn id(token: Token) -> Token {
    token
}
"#,
    );

    let tc = crate::collect_types(&file);

    let token_ids = tc.env.lookup("Token");
    assert_eq!(token_ids.len(), 1);
    match tc.env.get(token_ids[0]) {
        vx_types::TypeDef::Opaque(o) => {
            assert!(o.type_params.is_empty());
        }
        other => panic!("expected opaque type, got {other:?}"),
    }

    let sig = tc
        .def_to_fn_sig
        .values()
        .find(|sig| sig.name == "id")
        .expect("id signature");
    assert_eq!(
        sig.positional_params[0].ty,
        vx_types::Ty::Opaque(vx_types::OpaqueTy {
            id: token_ids[0],
            type_args: vec![],
        })
    );
    assert_eq!(
        sig.ret,
        vx_types::Ty::Opaque(vx_types::OpaqueTy {
            id: token_ids[0],
            type_args: vec![],
        })
    );
}

fn check_caps(src: &str) -> crate::CapCheckResult {
    let file = parse_file(src);
    let resolution = resolve_with_stdlib(&file);
    let types = collect_types_with_stdlib(&file);
    let type_check = crate::type_check(&file, &resolution, &types);
    crate::check_capabilities(&file, &resolution, &types, &type_check)
}

// r[cap.mask] — pure closure satisfies any callback mask
#[test]
fn pure_closure_satisfies_any_mask() {
    let result = check_caps(
        r#"
fn main() {
    build((path, args, env) => path);
    stabilize(() => "w", (w) => "r");
    exec(["echo"]) where { observer: (proc, tree) => [] };
    discover(executor_class("default"), (host) => ());
}
"#,
    );
    assert!(
        result.errors.is_empty(),
        "pure closures should satisfy any mask: {:?}",
        result.errors
    );
}

// r[cap.mask] — observer that uses #host authority is rejected
#[test]
fn observer_using_host_api_rejected() {
    let result = check_caps(
        r#"
fn probe() #host {}

fn main() {
    exec(["gcc"]) where {
        observer: (proc, tree) => {
            probe();
            []
        },
    };
}
"#,
    );
    assert_eq!(result.errors.len(), 1, "errors: {:?}", result.errors);
    match &result.errors[0].kind {
        CapErrorKind::CapabilityViolation {
            slot_name,
            allowed,
            excess,
            ..
        } => {
            assert_eq!(slot_name, "exec observer");
            assert_eq!(*allowed, CapSet::SANDBOX);
            assert!(!excess.is_empty());
            assert!(
                excess.names().contains(&"host"),
                "excess should contain #host: {excess}"
            );
        }
        other => panic!("expected CapabilityViolation, got: {other:?}"),
    }
}

// r[cap.mask] — observer that uses discover is rejected
#[test]
fn observer_using_discover_rejected() {
    let result = check_caps(
        r#"
fn main() {
    exec(["gcc"]) where {
        observer: (proc, tree) => {
            discover(executor_class("default"), (host) => host.which("gcc"));
            []
        },
    };
}
"#,
    );
    assert_eq!(result.errors.len(), 1, "errors: {:?}", result.errors);
    match &result.errors[0].kind {
        CapErrorKind::CapabilityViolation {
            slot_name, excess, ..
        } => {
            assert_eq!(slot_name, "exec observer");
            assert!(
                excess.names().contains(&"discover"),
                "excess should contain #discover: {excess}"
            );
        }
        other => panic!("expected CapabilityViolation, got: {other:?}"),
    }
}

// r[cap.propagation] — calling a #discover function from a stabilize witness is rejected
#[test]
fn stabilize_witness_cannot_discover() {
    let result = check_caps(
        r#"
fn probe() #discover {}

fn main() {
    stabilize(() => probe(), (w) => w);
}
"#,
    );
    assert_eq!(result.errors.len(), 1, "errors: {:?}", result.errors);
    match &result.errors[0].kind {
        CapErrorKind::CapabilityViolation {
            slot_name, excess, ..
        } => {
            assert_eq!(slot_name, "stabilize witness");
            assert!(
                excess.names().contains(&"discover"),
                "excess should contain #discover: {excess}"
            );
        }
        other => panic!("expected CapabilityViolation, got: {other:?}"),
    }
}

// r[cap.mask] — stabilize resolve with #fs is rejected (resolve only allows #discover)
#[test]
fn stabilize_resolve_cannot_fs() {
    let result = check_caps(
        r#"
fn main() {
    stabilize(() => "w", (w) => read_file("foo"));
}
"#,
    );
    assert_eq!(result.errors.len(), 1, "errors: {:?}", result.errors);
    match &result.errors[0].kind {
        CapErrorKind::CapabilityViolation {
            slot_name, excess, ..
        } => {
            assert_eq!(slot_name, "stabilize resolve");
            assert!(excess.names().contains(&"fs"), "excess: {excess}");
        }
        other => panic!("expected CapabilityViolation, got: {other:?}"),
    }
}

// r[cap.tags] — unknown capability tag produces error
#[test]
fn unknown_capability_tag() {
    let result = check_caps(
        r#"
fn foo() #network {}
"#,
    );
    assert_eq!(result.errors.len(), 1, "errors: {:?}", result.errors);
    match &result.errors[0].kind {
        CapErrorKind::UnknownCapability { name } => {
            assert_eq!(name, "network");
        }
        other => panic!("expected UnknownCapability, got: {other:?}"),
    }
}

// r[cap.propagation] — calling a function with capabilities propagates to closure
#[test]
fn cap_propagation_through_call() {
    let result = check_caps(
        r#"
fn main() {
    exec(["echo"]) where {
        observer: (proc, tree) => {
            read_file("vixen.toml");
            []
        },
    };
}
"#,
    );
    assert_eq!(result.errors.len(), 1, "errors: {:?}", result.errors);
    match &result.errors[0].kind {
        CapErrorKind::CapabilityViolation {
            slot_name, excess, ..
        } => {
            assert_eq!(slot_name, "exec observer");
            assert!(excess.names().contains(&"fs"), "excess: {excess}");
        }
        other => panic!("expected CapabilityViolation, got: {other:?}"),
    }
}

// r[cap.kinds] — valid capability tags are accepted
#[test]
fn valid_capability_tags() {
    let result = check_caps(
        r#"
fn read_stuff() #fs {}
fn probe() #discover {}
fn observe() #sandbox {}
fn verify() #host {}
fn multi() #fs #discover {}
"#,
    );
    assert!(
        result.errors.is_empty(),
        "valid caps should be accepted: {:?}",
        result.errors
    );
}

// r[cap.mask] — build entry allows #discover alongside #fs
#[test]
fn build_entry_can_discover() {
    let result = check_caps(
        r#"
fn main() {
    build((path, args, env) => {
        discover(executor_class("default"), (host) => host.which("gcc"));
        path
    });
}
"#,
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
}

// Unresolved method names do not imply host authority.
#[test]
fn unresolved_method_name_does_not_imply_host_capability() {
    let result = check_caps(
        r#"
fn main() {
    exec(["gcc"]) where {
        observer: (proc, tree) => {
            host.which("gcc");
            []
        },
    };
}
"#,
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
}

// FnSig collects capabilities from declarations
#[test]
fn fn_sig_collects_capabilities() {
    let file = parse_file(
        r#"
fn read_stuff() #fs -> String {}
fn probe() #discover -> String {}
fn both() #fs #discover -> String {}
fn pure() -> String {}
"#,
    );
    let tc = crate::collect_types(&file);

    let sigs: Vec<_> = tc.def_to_fn_sig.values().collect();

    let read_sig = sigs.iter().find(|s| s.name == "read_stuff").unwrap();
    assert_eq!(read_sig.capabilities.len(), 1);
    assert_eq!(read_sig.capabilities[0].name, "fs");

    let probe_sig = sigs.iter().find(|s| s.name == "probe").unwrap();
    assert_eq!(probe_sig.capabilities.len(), 1);
    assert_eq!(probe_sig.capabilities[0].name, "discover");

    let both_sig = sigs.iter().find(|s| s.name == "both").unwrap();
    assert_eq!(both_sig.capabilities.len(), 2);

    let pure_sig = sigs.iter().find(|s| s.name == "pure").unwrap();
    assert!(pure_sig.capabilities.is_empty());
}

fn type_check(src: &str) -> crate::TypeCheckResult {
    type_check_with_types(src).0
}

fn type_check_with_types(src: &str) -> (crate::TypeCheckResult, crate::TypeCollection) {
    let file = parse_file(src);
    let resolution = resolve_with_stdlib(&file);
    let types = collect_types_with_stdlib(&file);
    let checked = crate::type_check(&file, &resolution, &types);
    (checked, types)
}

fn find_expr_type(result: &crate::TypeCheckResult, src: &str, needle: &str) -> Option<Ty> {
    let offset = src.find(needle)? as u32;
    find_expr_type_at_offset(result, offset, needle.len() as u32)
}

fn find_last_expr_type(result: &crate::TypeCheckResult, src: &str, needle: &str) -> Option<Ty> {
    let offset = src.rfind(needle)? as u32;
    find_expr_type_at_offset(result, offset, needle.len() as u32)
}

fn find_expr_type_at_offset(
    result: &crate::TypeCheckResult,
    offset: u32,
    needle_len: u32,
) -> Option<Ty> {
    // Find the smallest expr_type whose range covers this needle
    result
        .expr_types
        .iter()
        .filter(|(range, _)| {
            let start: u32 = range.start().into();
            let end: u32 = range.end().into();
            start <= offset && end >= offset + needle_len
        })
        .min_by_key(|(range, _)| {
            let len: u32 = range.range.len().into();
            len
        })
        .map(|(_, ty)| ty.clone())
}

// r[exec.discover] — discover returns the probe body type
#[test]
fn discover_returns_probe_type() {
    let src = r#"
fn probe() #discover {
    discover(executor_class("default"), (host) => host.which("gcc"))
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, r#"discover(executor_class("default"),"#);
    assert_eq!(ty, Some(Ty::Scalar(vx_types::Scalar::String)));
}

// r[exec.verifier.host] — host.read(...) returns Blob
#[test]
fn host_read_returns_blob() {
    let src = r#"
fn main() {
    discover(executor_class("default"), (host) => {
        host.read("foo")
    })
}
"#;
    let (result, types) = type_check_with_types(src);
    let ty = find_expr_type(&result, src, r#"host.read("foo")"#);
    assert_eq!(ty, Some(nominal_ty(&types, "Blob")));
}

#[test]
fn host_os_returns_string() {
    let src = r#"
fn main() {
    discover(executor_class("default"), (host) => {
        host.os()
    })
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, "host.os()");
    assert_eq!(ty, Some(Ty::Scalar(vx_types::Scalar::String)));
}

#[test]
fn host_arch_returns_string() {
    let src = r#"
fn main() {
    discover(executor_class("default"), (host) => {
        host.arch()
    })
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, "host.arch()");
    assert_eq!(ty, Some(Ty::Scalar(vx_types::Scalar::String)));
}

// r[exec.verifier.host] — host.hash(...) returns Blob
#[test]
fn host_hash_returns_blob() {
    let src = r#"
fn main() {
    discover(executor_class("default"), (host) => {
        host.hash("foo")
    })
}
"#;
    let (result, types) = type_check_with_types(src);
    let ty = find_expr_type(&result, src, r#"host.hash("foo")"#);
    assert_eq!(ty, Some(nominal_ty(&types, "Blob")));
}

// r[exec.verifier.host] — host.which(...) returns String
#[test]
fn host_which_returns_string() {
    let src = r#"
fn main() {
    discover(executor_class("default"), (host) => {
        host.which("gcc")
    })
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, r#"host.which("gcc")"#);
    assert_eq!(ty, Some(Ty::Scalar(vx_types::Scalar::String)));
}

// r[exec.verifier.host] — host.which(...)? returns String?
#[test]
fn host_which_call_site_fallible_returns_optional_string() {
    let src = r#"
fn main() {
    discover(executor_class("default"), (host) => {
        host.which("gcc")?
    })
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, r#"host.which("gcc")?"#);
    assert_eq!(
        ty,
        Some(Ty::Optional(Box::new(Ty::Scalar(vx_types::Scalar::String))))
    );
}

#[test]
fn process_exit_code_field_returns_u64() {
    let src = r#"
fn main() {
    exec(["echo"]) where {
        observer: (proc, tree) => {
            proc.exit_code
        },
    };
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, "proc.exit_code");
    assert_eq!(ty, Some(Ty::Scalar(vx_types::Scalar::U64)));
}

#[test]
fn process_stdout_field_returns_stream_bytes() {
    let src = r#"
fn main() {
    exec(["echo"]) where {
        observer: (proc, tree) => {
            proc.stdout
        },
    };
}
"#;
    let (result, types) = type_check_with_types(src);
    let ty = find_expr_type(&result, src, "proc.stdout");
    assert_eq!(ty, Some(Ty::Stream(Box::new(nominal_ty(&types, "Bytes")))));
}

#[test]
fn process_stderr_field_returns_stream_bytes() {
    let src = r#"
fn main() {
    exec(["echo"]) where {
        observer: (proc, tree) => {
            proc.stderr
        },
    };
}
"#;
    let (result, types) = type_check_with_types(src);
    let ty = find_expr_type(&result, src, "proc.stderr");
    assert_eq!(ty, Some(Ty::Stream(Box::new(nominal_ty(&types, "Bytes")))));
}

// r[type.builtin.blob] — blob.utf8() returns String
#[test]
fn blob_utf8_returns_string() {
    let src = r#"
fn main() {
    build((path, args, env) => {
        read_file("hello.txt").utf8()
    });
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, r#"read_file("hello.txt").utf8()"#);
    assert_eq!(ty, Some(Ty::Scalar(vx_types::Scalar::String)));
}

// r[exec.observer] — observer callback params are typed
#[test]
fn observer_params_typed() {
    let src = r#"
fn main() {
    exec(["echo"]) where {
        observer: (proc, tree) => {
            proc.exit_code
        },
    };
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, "proc.exit_code");
    assert_eq!(ty, Some(Ty::Scalar(vx_types::Scalar::U64)));
}

#[test]
fn partial_exec_where_binding_types_observer_params() {
    let src = r#"
fn main() {
    let common = {
        observer: (proc, tree) => tree.subtree(@out),
    };

    partial exec where common
}
"#;
    let (result, types) = type_check_with_types(src);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let ty = find_expr_type(&result, src, "tree.subtree(@out)");
    assert_eq!(ty, Some(nominal_ty(&types, "Tree")));
}

#[test]
fn exec_observer_specializes_return_type() {
    let src = r#"
fn main() {
    let output = exec(["echo"]) where {
        observer: (proc, tree) => tree.subtree(@out),
    };

    output
}
"#;
    let (result, types) = type_check_with_types(src);
    let ty = find_last_expr_type(&result, src, "output");
    assert_eq!(ty, Some(nominal_ty(&types, "Tree")));
}

#[test]
fn partial_exec_where_binding_specializes_return_type() {
    let src = r#"
fn main() {
    let common = {
        observer: (proc, tree) => tree.subtree(@out),
    };
    let cexec = partial exec where common;

    cexec
}
"#;
    let (result, types) = type_check_with_types(src);
    let ty = find_last_expr_type(&result, src, "cexec");
    let Some(Ty::Function(function)) = ty else {
        panic!("expected function type, got {ty:?}");
    };
    assert_eq!(*function.ret, nominal_ty(&types, "Tree"));
}

#[test]
fn observer_process_param_uses_stdlib_struct_type() {
    let src = r#"
fn main() {
    exec(["echo"]) where {
        observer: (proc, tree) => {
            proc.exit_code
        },
    };
}
"#;
    let file = parse_file(src);
    let resolution = resolve_with_stdlib(&file);
    let types = collect_types_with_stdlib(&file);
    let result = crate::type_check(&file, &resolution, &types);
    let ty = result
        .closure_param_types
        .get(&span_for_text(src, "proc"))
        .cloned()
        .expect("proc closure param type");

    let Ty::Struct(struct_ty) = ty else {
        panic!("expected Process to resolve as a stdlib struct, got {ty:?}");
    };
    assert_eq!(types.env.get(struct_ty.id).name(), "Process");
}

#[test]
fn cc_version_sketch_resolves_and_typechecks() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cc-version.vx");
    let file = parse_file_path(&path);

    let resolution = resolve_with_stdlib(&file);
    assert!(
        resolution.errors.is_empty(),
        "resolve errors in {}: {:?}",
        path.display(),
        resolution.errors
    );

    let types = collect_types_with_stdlib(&file);
    let checked = crate::type_check(&file, &resolution, &types);
    assert!(
        checked.errors.is_empty(),
        "type errors in {}: {:?}",
        path.display(),
        checked.errors
    );
}

#[test]
fn lua_sketch_resolves_and_typechecks() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/lua.vx");
    let file = parse_file_path(&path);

    let resolution = resolve_with_stdlib(&file);
    assert!(
        resolution.errors.is_empty(),
        "resolve errors in {}: {:?}",
        path.display(),
        resolution.errors
    );

    let types = collect_types_with_stdlib(&file);
    let checked = crate::type_check(&file, &resolution, &types);
    assert!(
        checked.errors.is_empty(),
        "type errors in {}: {:?}",
        path.display(),
        checked.errors
    );
}

#[test]
fn cargo_resolve_sketch_resolves_and_typechecks() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx");
    let file = parse_file_path(&path);

    let resolution = resolve_with_stdlib(&file);
    assert!(
        resolution.errors.is_empty(),
        "resolve errors in {}: {:?}",
        path.display(),
        resolution.errors
    );

    let types = collect_types_with_stdlib(&file);
    let checked = crate::type_check(&file, &resolution, &types);
    assert!(
        checked.errors.is_empty(),
        "type errors in {}: {:?}",
        path.display(),
        checked.errors
    );
}

#[test]
fn libpng_sketch_resolves_and_typechecks() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/libpng.vx");
    let file = parse_file_path(&path);

    let resolution = resolve_with_stdlib(&file);
    assert!(
        resolution.errors.is_empty(),
        "resolve errors in {}: {:?}",
        path.display(),
        resolution.errors
    );

    let types = collect_types_with_stdlib(&file);
    let checked = crate::type_check(&file, &resolution, &types);
    assert!(
        checked.errors.is_empty(),
        "type errors in {}: {:?}",
        path.display(),
        checked.errors
    );
}

#[test]
fn remote_libpng_sketch_resolves_and_typechecks() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/remote-libpng.vx");
    let file = parse_file_path(&path);

    let resolution = resolve_with_stdlib(&file);
    assert!(
        resolution.errors.is_empty(),
        "resolve errors in {}: {:?}",
        path.display(),
        resolution.errors
    );

    let types = collect_types_with_stdlib(&file);
    let checked = crate::type_check(&file, &resolution, &types);
    assert!(
        checked.errors.is_empty(),
        "type errors in {}: {:?}",
        path.display(),
        checked.errors
    );
}

#[test]
fn pipe_placeholder_does_not_produce_resolve_errors() {
    let file = parse_file(
        r#"
fn demo(value: Blob) {
    value |> _.utf8()
}
"#,
    );
    let resolution = resolve_with_stdlib(&file);
    assert!(
        resolution.errors.is_empty(),
        "expected no resolve errors, got: {:?}",
        resolution.errors
    );
}

#[test]
fn pipe_placeholder_stage_infers_receiver_type() {
    let src = r#"
fn demo(value: Blob) {
    value |> _.utf8()
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, "value |> _.utf8()");
    assert_eq!(ty, Some(Ty::Scalar(vx_types::Scalar::String)));
}

#[test]
fn dollar_placeholder_predicate_adapter_typechecks() {
    let src = r#"
fn main(lines: Array[String]) {
    lines.filter(!$.is_empty()).to_array()
}
"#;
    let result = type_check(src);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let ty = find_expr_type(&result, src, r#"lines.filter(!$.is_empty())"#);
    assert_eq!(
        ty,
        Some(Ty::Array(Box::new(Ty::Scalar(vx_types::Scalar::String))))
    );
}

#[test]
fn dollar_placeholder_mapping_adapter_typechecks() {
    let src = r#"
struct Request {
    name: String,
    kind: String?,
}

fn request_key(req: Request) -> String (
    req.name
)

fn main(lines: Array[String], requests: Array[Request]) {
    let blobs = lines.map(Blob($)).to_array();
    let grouped = requests.group_by(request_key($));
    let filtered = requests.filter($.kind.unwrap_or("normal") == "normal").to_array();
    (blobs, grouped, filtered)
}
"#;
    let (result, types) = type_check_with_types(src);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let grouped_ty = find_expr_type(&result, src, "requests.group_by(request_key($))");
    assert_eq!(
        grouped_ty,
        Some(Ty::Map(
            Box::new(Ty::Scalar(vx_types::Scalar::String)),
            Box::new(Ty::Array(Box::new(nominal_ty(&types, "Request")))),
        ))
    );

    let filtered_ty = find_expr_type(
        &result,
        src,
        r#"requests.filter($.kind.unwrap_or("normal") == "normal")"#,
    );
    assert_eq!(
        filtered_ty,
        Some(Ty::Array(Box::new(nominal_ty(&types, "Request"))))
    );
}

#[test]
fn tree_constructor_from_stdlib_returns_tree() {
    let src = r#"
fn main(ws: Path) {
    Tree(ws)
}
"#;
    let (result, types) = type_check_with_types(src);
    let ty = find_expr_type(&result, src, "Tree(ws)");
    assert_eq!(ty, Some(nominal_ty(&types, "Tree")));
}

#[test]
fn tree_array_constructor_from_stdlib_returns_tree() {
    let src = r#"
fn main(other: Tree) {
    Tree([other])
}
"#;
    let (result, types) = type_check_with_types(src);
    let ty = find_expr_type(&result, src, "Tree([other])");
    assert_eq!(ty, Some(nominal_ty(&types, "Tree")));
}

#[test]
fn http_get_static_namespace_call_returns_blob() {
    let src = r#"
fn main() {
    Http.get("https://example.com")
}
"#;
    let (result, types) = type_check_with_types(src);
    let ty = find_expr_type(&result, src, r#"Http.get("https://example.com")"#);
    assert_eq!(ty, Some(nominal_ty(&types, "Blob")));
}

#[test]
fn toml_parse_typed_static_namespace_call_returns_concrete_type() {
    let src = r#"
struct CargoLock {}

fn main(blob: Blob) {
    Toml.parse[CargoLock](blob)
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, "Toml.parse[CargoLock](blob)");
    assert!(matches!(ty, Some(Ty::Struct(_))), "got: {ty:?}");
}

#[test]
fn string_split_once_method_returns_optional_tuple() {
    let src = r#"
fn main(s: String) {
    s.split_once("/")
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, r#"s.split_once("/")"#);
    assert!(matches!(ty, Some(Ty::Optional(_))), "got: {ty:?}");
}

#[test]
fn string_len_method_returns_u64() {
    let src = r#"
fn main(s: String) {
    s.len()
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, "s.len()");
    assert_eq!(ty, Some(Ty::Scalar(vx_types::Scalar::U64)));
}

#[test]
fn function_name_ref_is_a_function_value() {
    let src = r#"
fn compile(path: Path) -> Tree {
    Tree(path)
}

fn main() {
    let f = compile;
    f
}
"#;
    let result = type_check(src);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let ty = find_last_expr_type(&result, src, "compile");
    assert!(matches!(ty, Some(Ty::Function(_))), "got: {ty:?}");
}

#[test]
fn array_map_with_named_function_returns_mapped_array() {
    let src = r#"
fn compile(path: Path) -> Tree {
    Tree(path)
}

fn main(paths: Array[Path]) {
    paths.map(compile)
}
"#;
    let (result, types) = type_check_with_types(src);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let compile_ty = find_last_expr_type(&result, src, "compile");
    assert!(
        matches!(compile_ty, Some(Ty::Function(_))),
        "got: {compile_ty:?}"
    );
    let ty = find_expr_type(&result, src, "paths.map(compile)");
    assert_eq!(ty, Some(Ty::Array(Box::new(nominal_ty(&types, "Tree")))));
}

#[test]
fn array_to_map_accepts_bound_method_value() {
    let src = r#"
fn main() {
    discover(executor_class("default"), (host) => {
        ["gcc"].to_map(host.which)
    })
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, r#"["gcc"].to_map(host.which)"#);
    assert_eq!(
        ty,
        Some(Ty::Map(
            Box::new(Ty::Scalar(vx_types::Scalar::String)),
            Box::new(Ty::Scalar(vx_types::Scalar::String)),
        ))
    );
}

#[test]
fn map_values_closure_param_is_typed_from_value_type() {
    let src = r#"
struct Tool {
    len: U64,
}

fn main(values: Map[String, String]) {
    values.map_values((path) => Tool { len: path.len() })
}
"#;
    let result = type_check(src);
    let path_len_ty = find_expr_type(&result, src, "path.len()");
    assert_eq!(path_len_ty, Some(Ty::Scalar(vx_types::Scalar::U64)));

    let map_ty = find_expr_type(
        &result,
        src,
        "values.map_values((path) => Tool { len: path.len() })",
    );
    assert!(matches!(map_ty, Some(Ty::Map(_, _))), "got: {map_ty:?}");
}

#[test]
fn map_map_closure_params_are_typed_from_key_and_value_types() {
    let src = r#"
struct DependencyRequest {
    name: String,
    dep_len: U64,
}

struct CargoManifest {
    dependencies: Map[String, String],
}

fn request_from_dep(name: String, dep: String, kind: String) -> DependencyRequest {
    let _kind = kind;
    DependencyRequest { name, dep_len: dep.len() }
}

fn main(manifest: CargoManifest) {
    manifest.dependencies
        .map((name, dep) => request_from_dep(name, dep, "normal"))
        .to_array()
}
"#;
    let result = type_check(src);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let dep_len_ty = find_expr_type(&result, src, "dep.len()");
    assert_eq!(dep_len_ty, Some(Ty::Scalar(vx_types::Scalar::U64)));

    let (result, types) = type_check_with_types(src);
    let ty = find_expr_type(
        &result,
        src,
        r#"manifest.dependencies
        .map((name, dep) => request_from_dep(name, dep, "normal"))
        .to_array()"#,
    );
    assert_eq!(
        ty,
        Some(Ty::Array(Box::new(nominal_ty(&types, "DependencyRequest"))))
    );
}

#[test]
fn array_filter_and_find_closure_params_are_typed() {
    let src = r#"
struct IndexEntry {
    vers: String,
    yanked: Bool,
}

fn main(entries: Array[IndexEntry]) {
    let filtered =
        entries
            .filter((entry) => !entry.yanked)
            .to_array();
    filtered.find((entry) => entry.vers == "1.0.0")
}
"#;
    let result = type_check(src);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let yanked_ty = find_expr_type(&result, src, "entry.yanked");
    assert_eq!(yanked_ty, Some(Ty::Scalar(vx_types::Scalar::Bool)));

    let vers_ty = find_expr_type(&result, src, "entry.vers");
    assert_eq!(vers_ty, Some(Ty::Scalar(vx_types::Scalar::String)));

    let found_ty = find_last_expr_type(
        &result,
        src,
        r#"filtered.find((entry) => entry.vers == "1.0.0")"#,
    );
    assert!(
        matches!(found_ty, Some(Ty::Optional(_))),
        "got: {found_ty:?}"
    );
}

#[test]
fn array_flatten_returns_inner_array_type() {
    let src = r#"
fn main(parts: Array[Array[String]]) {
    parts.flatten()
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, "parts.flatten()");
    assert_eq!(
        ty,
        Some(Ty::Array(Box::new(Ty::Scalar(vx_types::Scalar::String))))
    );
}

#[test]
fn array_merge_returns_tree() {
    let src = r#"
fn main(parts: Array[Tree]) {
    parts.merge()
}
"#;
    let (result, types) = type_check_with_types(src);
    let ty = find_expr_type(&result, src, "parts.merge()");
    assert_eq!(ty, Some(nominal_ty(&types, "Tree")));
}

#[test]
fn array_concat_returns_array_type() {
    let src = r#"
fn main(left: Array[String], right: Array[String]) {
    left ++ right
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, "left ++ right");
    assert_eq!(
        ty,
        Some(Ty::Array(Box::new(Ty::Scalar(vx_types::Scalar::String))))
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
}

#[test]
fn array_concat_requires_matching_element_types() {
    let src = r#"
fn main(left: Array[String], right: Array[U64]) {
    left ++ right
}
"#;
    let result = type_check(src);
    assert_eq!(result.errors.len(), 1, "errors: {:?}", result.errors);
    match &result.errors[0].kind {
        crate::TypeErrorKind::BinaryOpMismatch { op, left, right } => {
            assert_eq!(op, "++");
            assert_eq!(
                left,
                &Ty::Array(Box::new(Ty::Scalar(vx_types::Scalar::String)))
            );
            assert_eq!(
                right,
                &Ty::Array(Box::new(Ty::Scalar(vx_types::Scalar::U64)))
            );
        }
        other => panic!("expected BinaryOpMismatch, got: {other:?}"),
    }
}

#[test]
fn bare_none_resolves_and_typechecks_in_optional_context() {
    let src = r#"
fn main() {
    let value: String? = None;
    value
}
"#;
    let file = parse_file(src);
    let resolution = resolve_with_stdlib(&file);
    assert!(
        resolution.errors.is_empty(),
        "errors: {:?}",
        resolution.errors
    );

    let types = collect_types_with_stdlib(&file);
    let checked = crate::type_check(&file, &resolution, &types);
    assert!(checked.errors.is_empty(), "errors: {:?}", checked.errors);
}

#[test]
fn some_variant_constructor_typechecks_in_optional_context() {
    let src = r#"
fn main(s: String) {
    let value: String? = Some(s);
    value
}
"#;
    let file = parse_file(src);
    let resolution = resolve_with_stdlib(&file);
    assert!(
        resolution.errors.is_empty(),
        "errors: {:?}",
        resolution.errors
    );

    let types = collect_types_with_stdlib(&file);
    let checked = crate::type_check(&file, &resolution, &types);
    assert!(checked.errors.is_empty(), "errors: {:?}", checked.errors);
    assert_eq!(
        find_expr_type(&checked, src, "Some(s)"),
        Some(Ty::Optional(Box::new(Ty::Scalar(vx_types::Scalar::String))))
    );
}

#[test]
fn option_unwrap_or_returns_inner_type() {
    let src = r#"
fn main(name: String?) {
    name.unwrap_or("fallback")
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, r#"name.unwrap_or("fallback")"#);
    assert_eq!(ty, Some(Ty::Scalar(vx_types::Scalar::String)));
}

#[test]
fn option_variant_pattern_binds_inner_value_type() {
    let src = r#"
fn main(name: String?) {
    match name (
        Some { value } => value.ends_with("?"),
        None => false,
    )
}
"#;
    let result = type_check(src);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let ty = find_expr_type(&result, src, r#"value.ends_with("?")"#);
    assert_eq!(ty, Some(Ty::Scalar(vx_types::Scalar::Bool)));
}

#[test]
fn string_slice_accepts_range_argument() {
    let src = r#"
fn main(name: String) {
    name.slice(0..1)
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, "name.slice(0..1)");
    assert_eq!(ty, Some(Ty::Scalar(vx_types::Scalar::String)));
}

#[test]
fn tuple_destructuring_let_binds_element_types() {
    let src = r#"
fn main(pair: (String, String)) {
    let (dep, feat) = pair;
    (dep.ends_with("?"), feat.len())
}
"#;
    let result = type_check(src);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let ends_with_ty = find_expr_type(&result, src, r#"dep.ends_with("?")"#);
    assert_eq!(ends_with_ty, Some(Ty::Scalar(vx_types::Scalar::Bool)));

    let feat_len_ty = find_expr_type(&result, src, "feat.len()");
    assert_eq!(feat_len_ty, Some(Ty::Scalar(vx_types::Scalar::U64)));
}

#[test]
fn array_filter_map_any_group_by_and_reduce_typecheck() {
    let src = r#"
struct Item {
    key: String,
    value: String,
}

fn main(items: Array[Item]) {
    let filtered = items.filter_map((item) => Some(item.value)).to_array();
    let present = items.any((item) => item.key == "dep");
    let grouped = items.group_by((item) => item.key);
    let reduced = grouped.map((key, group) => group.reduce((left, right) => left)).to_array();
    (filtered, present, reduced)
}
"#;
    let result = type_check(src);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
}

#[test]
fn top_level_expression_statement_is_a_type_error() {
    let src = "42";
    let result = type_check(src);
    assert_eq!(result.errors.len(), 1, "errors: {:?}", result.errors);
    assert_eq!(
        result.errors[0].kind,
        crate::TypeErrorKind::UnusedExprStatement
    );
}

#[test]
fn tail_expression_with_trailing_semicolon_is_a_type_error() {
    let src = r#"
fn main() {
    42;
}
"#;
    let result = type_check(src);
    assert_eq!(result.errors.len(), 1, "errors: {:?}", result.errors);
    assert_eq!(
        result.errors[0].kind,
        crate::TypeErrorKind::UnusedExprStatement
    );
}

#[test]
fn discarded_discovery_expression_is_a_type_error() {
    let src = r#"
fn probe() #discover {
    discover(executor_class("default"), (host) => {
        host.read("/tmp/tool");
        host.which("/tmp/tool")
    })
}
"#;
    let result = type_check(src);
    assert_eq!(result.errors.len(), 1, "errors: {:?}", result.errors);
    assert_eq!(
        result.errors[0].kind,
        crate::TypeErrorKind::UnusedExprStatement
    );
}

// r[type.error.arg_mismatch] — host.read(123) gets a type diagnostic
#[test]
fn host_read_int_arg_type_error() {
    let src = r#"
fn main() {
    discover(executor_class("default"), (host) => {
        host.read(123)
    })
}
"#;
    let result = type_check(src);
    assert_eq!(result.errors.len(), 1, "errors: {:?}", result.errors);
    match &result.errors[0].kind {
        crate::TypeErrorKind::ArgTypeMismatch {
            param_name,
            expected,
            found,
        } => {
            assert_eq!(param_name, "path");
            assert_eq!(*expected, Ty::Scalar(vx_types::Scalar::String));
            assert_eq!(*found, Ty::Scalar(vx_types::Scalar::U64));
        }
        other => panic!("expected ArgTypeMismatch, got: {other:?}"),
    }
}

// r[type.scalar.literals] — literal types are inferred
#[test]
fn literal_types() {
    let src = r#"
fn main() {
    42
}
"#;
    let result = type_check(src);
    let ty = find_expr_type(&result, src, "42");
    assert_eq!(ty, Some(Ty::Scalar(vx_types::Scalar::U64)));
}

#[test]
fn path_literal_is_nominal_path() {
    let src = r#"
fn main() {
    @out
}
"#;
    let (result, types) = type_check_with_types(src);
    let ty = find_expr_type(&result, src, "@out");
    assert_eq!(ty, Some(nominal_ty(&types, "Path")));
}

// r[type.builtin.read_file] — read_file returns Blob
#[test]
fn read_file_returns_blob() {
    let src = r#"
fn main() #fs {
    read_file("hello.txt")
}
"#;
    let (result, types) = type_check_with_types(src);
    let ty = find_expr_type(&result, src, r#"read_file("hello.txt")"#);
    assert_eq!(ty, Some(nominal_ty(&types, "Blob")));
}

// r[type.builtin.executor_class] — executor_class returns ExecutorClass
#[test]
fn executor_class_returns_executor_class() {
    let src = r#"
fn main() {
    executor_class("default")
}
"#;
    let (result, types) = type_check_with_types(src);
    let ty = find_expr_type(&result, src, r#"executor_class("default")"#);
    assert_eq!(ty, Some(nominal_ty(&types, "ExecutorClass")));
}

// r[type.discover.host_param] — discover callback host param is typed Host
#[test]
fn discover_probe_host_param_typed() {
    let src = r#"
fn main() {
    discover(executor_class("default"), (host) => host)
}
"#;
    let (result, types) = type_check_with_types(src);
    let host_ty = nominal_ty(&types, "Host");
    // The name ref "host" in the body should resolve to the stdlib Host type.
    let _ty = find_expr_type(&result, src, "host\n");
    // The inner "host" expression (in the body, not the param declaration)
    // Find the last occurrence of "host" which is the expression use
    assert!(
        result.expr_types.values().any(|ty| *ty == host_ty),
        "expected Host type in expr_types, got: {:?}",
        result.expr_types
    );
}

// --- Step 1: cap_check derives from real stdlib signatures ---

#[test]
fn cap_check_derives_fs_from_stdlib_sig() {
    // read_file has #fs in std/build.vx — cap_check should derive FS from it
    let result = check_caps(
        r#"
fn main() {
    build((path, args, env) => {
        read_file("foo");
    });
}
"#,
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
}

#[test]
fn cap_check_derives_discover_from_stdlib_sig() {
    // build's entry slot now allows #discover as well as #fs
    let result = check_caps(
        r#"
fn main() {
    build((path, args, env) => {
        discover(executor_class("gcc"), (host) => host.which("gcc"));
    });
}
"#,
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
}

#[test]
fn cap_check_method_caps_from_stdlib_method_sig() {
    // host.which has #host capability from std/core.vx
    let result = check_caps(
        r#"
fn probe() #discover {
    discover(executor_class("default"), (host) => {
            host.which("gcc");
        });
}
"#,
    );
    // discover probe allows #host, so this should pass
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
}

// --- Step 2: let-binding type propagation ---

fn type_check_src(src: &str) -> crate::TypeCheckResult {
    let file = parse_file(src);
    let resolution = resolve_with_stdlib(&file);
    let types = collect_types_with_stdlib(&file);
    crate::type_check(&file, &resolution, &types)
}

#[test]
fn let_binding_propagates_type() {
    let (result, types) = type_check_with_types(
        r#"
fn main() {
    build((path, args, env) => {
        let b = read_file("foo");
        b.utf8()
    })
}
"#,
    );
    let blob_ty = nominal_ty(&types, "Blob");
    assert!(
        result.errors.is_empty(),
        "expected no type errors, got: {:?}",
        result.errors
    );
    // Check that binding_types has an entry for b
    assert!(
        result.binding_types.values().any(|ty| *ty == blob_ty),
        "expected Blob binding type, got: {:?}",
        result.binding_types
    );
}

#[test]
fn let_binding_chain_propagates() {
    let result = type_check_src(
        r#"
fn main() {
    build((path, args, env) => {
        let b = read_file("foo");
        let s = b.utf8();
    });
}
"#,
    );
    // s should have type String
    assert!(
        result
            .binding_types
            .values()
            .any(|ty| matches!(ty, Ty::Scalar(vx_types::Scalar::String))),
        "expected String binding type, got: {:?}",
        result.binding_types
    );
}

// --- Step 4: builtins resolve properly ---

#[test]
fn stdlib_names_resolve_without_error() {
    let file = parse_file(
        r#"
fn main() {
    let class = executor_class("default");
    let cc = discover(class, (host) => host.which("cc"));
    exec(["echo"]) where {
        observer: (proc, tree) => tree.subtree(@out),
    };
    cc
}
"#,
    );
    let resolution = resolve_with_stdlib(&file);
    assert!(
        resolution.errors.is_empty(),
        "expected no resolve errors, got: {:?}",
        resolution.errors
    );
    let stdlib_refs: Vec<_> = resolution
        .refs
        .iter()
        .filter_map(|r| r.target.as_ref().ok())
        .filter(|def| def.kind == DefKind::Function && def.source() != TEST_SOURCE_ID)
        .collect();
    assert!(
        stdlib_refs.len() >= 3,
        "expected at least 3 stdlib refs (executor_class, discover, exec), got: {stdlib_refs:?}"
    );
}

#[test]
fn stdlib_namespace_names_resolve_to_real_definitions() {
    let file = parse_file(
        r#"
struct Config {}

fn main(blob: Blob) {
    Toml.parse[Config](blob)
}
"#,
    );
    let resolution = resolve_with_stdlib(&file);
    assert!(
        resolution.errors.is_empty(),
        "expected no resolve errors, got: {:?}",
        resolution.errors
    );

    let toml = resolution
        .refs
        .iter()
        .find(|r| r.name == "Toml")
        .and_then(|r| r.target.as_ref().ok())
        .expect("Toml should resolve");
    assert_eq!(toml.kind, DefKind::Namespace);
    assert_ne!(toml.source(), TEST_SOURCE_ID);
}

#[test]
fn static_namespace_member_refs_point_to_real_definitions() {
    let src = r#"
struct Config {}

fn main(blob: Blob) {
    Toml.parse[Config](blob)
}
"#;
    let file = parse_file(src);
    let resolution = resolve_with_stdlib(&file);
    let types = collect_types_with_stdlib(&file);
    let checked = crate::type_check(&file, &resolution, &types);

    let parse_def = checked
        .member_defs
        .get(&span_for_text(src, "parse"))
        .expect("parse member def");
    assert_eq!(parse_def.name, "parse");
    assert_eq!(parse_def.kind, DefKind::Function);
    assert_ne!(parse_def.source(), TEST_SOURCE_ID);
    assert_eq!(
        types.fn_sig_for_def(parse_def).expect("parse sig").name,
        "parse"
    );
}

#[test]
fn builtin_method_refs_point_to_real_definitions() {
    let src = r#"
fn main(host: Host) {
    host.which("cc")
}
"#;
    let file = parse_file(src);
    let resolution = resolve_with_stdlib(&file);
    let types = collect_types_with_stdlib(&file);
    let checked = crate::type_check(&file, &resolution, &types);

    let which_def = checked
        .member_defs
        .get(&span_for_text(src, "which"))
        .expect("which member def");
    assert_eq!(which_def.name, "which");
    assert_eq!(which_def.kind, DefKind::Function);
    assert_ne!(which_def.source(), TEST_SOURCE_ID);
    assert_eq!(
        types.fn_sig_for_def(which_def).expect("which sig").name,
        "which"
    );
}

#[test]
fn generic_method_refs_point_to_real_definitions() {
    let src = r#"
fn compile(path: String) -> Blob {
    Blob(path)
}

fn main(paths: Array[String]) {
    paths.map(compile)
}
"#;
    let file = parse_file(src);
    let resolution = resolve_with_stdlib(&file);
    let types = collect_types_with_stdlib(&file);
    let checked = crate::type_check(&file, &resolution, &types);

    let map_def = checked
        .member_defs
        .get(&span_for_text(src, "map"))
        .expect("map member def");
    assert_eq!(map_def.name, "map");
    assert_eq!(map_def.kind, DefKind::Function);
    assert_ne!(map_def.source(), TEST_SOURCE_ID);
}

// --- Function parameter types from declared signatures ---

#[test]
fn fn_param_type_from_signature() {
    let result = type_check_src(
        r#"
fn process(x: String) {
    let len = x;
}
"#,
    );
    // x should have type String from the declared signature
    assert!(
        result
            .expr_types
            .values()
            .any(|ty| matches!(ty, Ty::Scalar(vx_types::Scalar::String))),
        "expected String type for x reference, got: {:?}",
        result.expr_types
    );
}

#[test]
fn fn_keyword_param_type_from_signature() {
    let result = type_check_src(
        r#"
fn process(; name: String) {
    let n = name;
}
"#,
    );
    assert!(
        result
            .binding_types
            .values()
            .any(|ty| matches!(ty, Ty::Scalar(vx_types::Scalar::String))),
        "expected String binding type for n, got: {:?}",
        result.binding_types
    );
}

// --- Interpolated string typing ---

#[test]
fn interpolated_string_is_string() {
    let result = type_check_src(
        r#"
fn main() {
    let s = `hello ${"world"}`;
}
"#,
    );
    assert!(
        result
            .binding_types
            .values()
            .any(|ty| matches!(ty, Ty::Scalar(vx_types::Scalar::String))),
        "expected String binding type for interpolated string, got: {:?}",
        result.binding_types
    );
}

// --- Array element type inference ---

#[test]
fn array_infers_element_type() {
    let result = type_check_src(
        r#"
fn main() {
    let arr = [1, 2, 3];
}
"#,
    );
    assert!(
        result
            .binding_types
            .values()
            .any(|ty| matches!(ty, Ty::Array(inner) if matches!(inner.as_ref(), Ty::Scalar(vx_types::Scalar::U64)))),
        "expected Array[U64] binding type, got: {:?}",
        result.binding_types
    );
}

#[test]
fn array_of_strings() {
    let result = type_check_src(
        r#"
fn main() {
    let arr = ["a", "b"];
}
"#,
    );
    assert!(
        result
            .binding_types
            .values()
            .any(|ty| matches!(ty, Ty::Array(inner) if matches!(inner.as_ref(), Ty::Scalar(vx_types::Scalar::String)))),
        "expected Array[String] binding type, got: {:?}",
        result.binding_types
    );
}

#[test]
fn array_of_nominal_blobs() {
    let (result, types) = type_check_with_types(
        r#"
fn main() {
    build((path, args, env) => {
        let blobs = [read_file("a"), read_file("b")];
    });
}
"#,
    );
    let blob_ty = nominal_ty(&types, "Blob");
    assert!(
        result
            .binding_types
            .values()
            .any(|ty| matches!(ty, Ty::Array(inner) if inner.as_ref() == &blob_ty)),
        "expected Array[Blob] binding type, got: {:?}",
        result.binding_types
    );
}
