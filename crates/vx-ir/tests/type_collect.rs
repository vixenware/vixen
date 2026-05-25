//! Tests for the VIR type collection pass.
//!
//! Each test builds VIR from source, runs `collect_types_vir`, and checks the output.

use vx_ir::build::VirBuilder;
use vx_ir::type_collect::collect_types_vir;
use vx_parser::{SourceId, parse_source_file};
use vx_types::{Scalar, Ty, TypeDef};

/// Helper: build VIR and run type collection.
fn collect_from_source(source: &str) -> (vx_ir::arena::VirArena, vx_ir::TypeCollection) {
    let parsed = parse_source_file(source);
    let file = parsed.tree();
    let mut program = VirBuilder::build_program(&[(SourceId(0), file)]);
    let tc = collect_types_vir(&mut program.arena);
    (program.arena, tc)
}

// =========================================================================
// 1. Struct with typed fields
// =========================================================================

#[test]
fn struct_with_typed_fields() {
    let (_, tc) = collect_from_source("struct Point { x: F64, y: F64 }");
    let (id, _) = tc.name_to_id["Point"];
    if let TypeDef::Struct(s) = &tc.env[id] {
        assert_eq!(s.name, "Point");
        assert_eq!(s.fields.len(), 2);
        assert_eq!(s.fields[0].name, "x");
        assert_eq!(s.fields[0].ty, Ty::Scalar(Scalar::F64));
        assert_eq!(s.fields[1].name, "y");
        assert_eq!(s.fields[1].ty, Ty::Scalar(Scalar::F64));
    } else {
        panic!("expected struct TypeDef for Point");
    }
}

// =========================================================================
// 2. Enum with variants
// =========================================================================

#[test]
fn enum_with_variants() {
    let (_, tc) = collect_from_source("enum Shape { struct Circle { r: F64 }, struct Empty }");
    let (enum_id, _) = tc.name_to_id["Shape"];
    if let TypeDef::Enum(e) = &tc.env[enum_id] {
        assert_eq!(e.name, "Shape");
        assert_eq!(e.variants.len(), 2);
        // First variant: Circle with one field
        if let TypeDef::Struct(circle) = &tc.env[e.variants[0]] {
            assert_eq!(circle.name, "Circle");
            assert_eq!(circle.fields.len(), 1);
            assert_eq!(circle.fields[0].name, "r");
            assert_eq!(circle.fields[0].ty, Ty::Scalar(Scalar::F64));
            assert_eq!(circle.parent_enum, Some(enum_id));
        } else {
            panic!("expected struct TypeDef for Circle");
        }
        // Second variant: Empty with no fields
        if let TypeDef::Struct(empty) = &tc.env[e.variants[1]] {
            assert_eq!(empty.name, "Empty");
            assert_eq!(empty.fields.len(), 0);
        } else {
            panic!("expected struct TypeDef for Empty");
        }
    } else {
        panic!("expected enum TypeDef for Shape");
    }
}

// =========================================================================
// 3. Function signature
// =========================================================================

#[test]
fn function_signature() {
    let (_, tc) = collect_from_source("fn add(x: U64, y: U64) -> U64 ( x + y )");
    // Should have exactly one FnSig
    assert_eq!(tc.def_to_fn_sig.len(), 1);
    let sig = tc.def_to_fn_sig.values().next().unwrap();
    assert_eq!(sig.name, "add");
    assert_eq!(sig.positional_params.len(), 2);
    assert_eq!(sig.positional_params[0].name, "x");
    assert_eq!(sig.positional_params[0].ty, Ty::Scalar(Scalar::U64));
    assert_eq!(sig.positional_params[1].name, "y");
    assert_eq!(sig.positional_params[1].ty, Ty::Scalar(Scalar::U64));
    assert_eq!(sig.ret, Ty::Scalar(Scalar::U64));
}

// =========================================================================
// 4. Newtype
// =========================================================================

#[test]
fn newtype_wrapping() {
    let (_, tc) = collect_from_source("newtype Name of String");
    let (id, _) = tc.name_to_id["Name"];
    if let TypeDef::Newtype(n) = &tc.env[id] {
        assert_eq!(n.name, "Name");
        assert_eq!(n.inner, Ty::Scalar(Scalar::String));
    } else {
        panic!("expected newtype TypeDef for Name");
    }
}

// =========================================================================
// 5. Type alias
// =========================================================================

#[test]
fn type_alias_tuple() {
    let (_, tc) = collect_from_source("type Pair = (U64, U64)");
    let (id, _) = tc.name_to_id["Pair"];
    if let TypeDef::Alias(a) = &tc.env[id] {
        assert_eq!(a.name, "Pair");
        if let Ty::Tuple(tup) = &a.target {
            assert_eq!(tup.elements.len(), 2);
            assert_eq!(tup.elements[0], Ty::Scalar(Scalar::U64));
            assert_eq!(tup.elements[1], Ty::Scalar(Scalar::U64));
        } else {
            panic!("expected tuple alias target, got {:?}", a.target);
        }
    } else {
        panic!("expected alias TypeDef for Pair");
    }
}

// =========================================================================
// 6. Generic type
// =========================================================================

#[test]
fn generic_struct() {
    let (_, tc) = collect_from_source("struct Box[T] { value: T }");
    let (id, _) = tc.name_to_id["Box"];
    if let TypeDef::Struct(s) = &tc.env[id] {
        assert_eq!(s.type_params, vec!["T"]);
        assert_eq!(s.fields.len(), 1);
        assert_eq!(s.fields[0].name, "value");
        // T should be resolved as type param at index 0.
        assert_eq!(
            s.fields[0].ty,
            Ty::Param(vx_types::TypeParam {
                name: "T".to_string(),
                index: 0,
            })
        );
    } else {
        panic!("expected struct TypeDef for Box");
    }
}

// =========================================================================
// 7. Namespace function
// =========================================================================

#[test]
fn namespace_function_sig() {
    let (_, tc) = collect_from_source("namespace Foo { fn bar() -> U64 ( 1 ) }");
    assert_eq!(tc.def_to_fn_sig.len(), 1);
    let sig = tc.def_to_fn_sig.values().next().unwrap();
    assert_eq!(sig.name, "bar");
    assert_eq!(sig.ret, Ty::Scalar(Scalar::U64));
}

// =========================================================================
// 8. Sum alias
// =========================================================================

#[test]
fn sum_alias() {
    let (_, tc) = collect_from_source("type Input = String | U64");
    let (id, _) = tc.name_to_id["Input"];
    assert!(
        tc.alias_sum_alternatives.contains_key(&id),
        "sum alias should have alternatives"
    );
    let alts = &tc.alias_sum_alternatives[&id];
    assert_eq!(alts.len(), 2);
    assert_eq!(alts[0], Ty::Scalar(Scalar::String));
    assert_eq!(alts[1], Ty::Scalar(Scalar::U64));
}

// =========================================================================
// 9. Opaque type (alias without target)
// =========================================================================

#[test]
fn opaque_type_alias() {
    let (_, tc) = collect_from_source("type Opaque");
    let (id, _) = tc.name_to_id["Opaque"];
    assert!(matches!(tc.env[id], TypeDef::Opaque(_)));
}

// =========================================================================
// 10. Function with no return type defaults to Unit
// =========================================================================

#[test]
fn function_no_return_type() {
    let (_, tc) = collect_from_source("fn noop() ( 1 )");
    let sig = tc.def_to_fn_sig.values().next().unwrap();
    assert_eq!(sig.ret, Ty::Unit);
}
