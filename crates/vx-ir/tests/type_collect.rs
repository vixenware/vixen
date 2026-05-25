//! Tests for the VIR type collection pass.
//!
//! Each test builds VIR from source, runs `collect_types_vir`, and checks the output.
//! Equivalence tests compare VIR collector output against the old AST-based collector.

use vx_parser::parse_source_file;
use vx_resolve::{SourceId, collect_types};
use vx_types::{Scalar, Ty, TypeDef};
use vx_ir::build::VirBuilder;
use vx_ir::type_collect::collect_types_vir;

/// Helper: build VIR and run type collection.
fn collect_from_source(source: &str) -> (vx_ir::arena::VirArena, vx_resolve::TypeCollection) {
    let parsed = parse_source_file(source);
    let file = parsed.tree();
    let mut program = VirBuilder::build_program(&[(SourceId(0), file)]);
    let tc = collect_types_vir(&mut program.arena);
    (program.arena, tc)
}

/// Helper: run old collector on same source for equivalence comparison.
fn collect_old(source: &str) -> vx_resolve::TypeCollection {
    let parsed = parse_source_file(source);
    let file = parsed.tree();
    collect_types(&file)
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

// =========================================================================
// Equivalence tests
// =========================================================================

/// Compare def_to_type keys and values between old and VIR collectors.
fn assert_def_to_type_equiv(old: &vx_resolve::TypeCollection, new: &vx_resolve::TypeCollection) {
    assert_eq!(
        old.def_to_type.len(),
        new.def_to_type.len(),
        "def_to_type should have same number of entries"
    );
    for (site, &old_id) in &old.def_to_type {
        let new_id = new
            .def_to_type
            .get(site)
            .copied()
            .unwrap_or_else(|| panic!("VIR collector missing def_to_type for {:?}", site));
        // TypeIds won't be numerically equal (different arenas), but the TypeDef names should match.
        let old_name = type_def_name(&old.env[old_id]);
        let new_name = type_def_name(&new.env[new_id]);
        assert_eq!(
            old_name, new_name,
            "def_to_type entry at {:?} should have same type name",
            site
        );
    }
}

fn type_def_name(td: &TypeDef) -> &str {
    match td {
        TypeDef::Struct(s) => &s.name,
        TypeDef::Enum(e) => &e.name,
        TypeDef::Newtype(n) => &n.name,
        TypeDef::Alias(a) => &a.name,
        TypeDef::Opaque(o) => &o.name,
    }
}

/// Compare fn_sig keys and field-by-field values.
fn assert_fn_sig_equiv(old: &vx_resolve::TypeCollection, new: &vx_resolve::TypeCollection) {
    assert_eq!(
        old.def_to_fn_sig.len(),
        new.def_to_fn_sig.len(),
        "def_to_fn_sig should have same number of entries\nold keys: {:?}\nnew keys: {:?}",
        old.def_to_fn_sig.keys().collect::<Vec<_>>(),
        new.def_to_fn_sig.keys().collect::<Vec<_>>(),
    );
    for (site, old_sig) in &old.def_to_fn_sig {
        let new_sig = new
            .def_to_fn_sig
            .get(site)
            .unwrap_or_else(|| panic!("VIR collector missing fn_sig for {:?}", site));
        assert_eq!(old_sig.name, new_sig.name, "fn name mismatch at {:?}", site);
        assert_eq!(
            old_sig.type_params, new_sig.type_params,
            "fn type_params mismatch for {}",
            old_sig.name
        );
        assert_eq!(
            old_sig.positional_params.len(),
            new_sig.positional_params.len(),
            "fn positional_params count mismatch for {}",
            old_sig.name
        );
        for (i, (op, np)) in old_sig
            .positional_params
            .iter()
            .zip(&new_sig.positional_params)
            .enumerate()
        {
            assert_eq!(
                op.name, np.name,
                "positional param {} name mismatch for {}",
                i, old_sig.name
            );
            assert_eq!(
                op.ty, np.ty,
                "positional param {} type mismatch for {}",
                i, old_sig.name
            );
        }
        assert_eq!(
            old_sig.keyword_params.len(),
            new_sig.keyword_params.len(),
            "fn keyword_params count mismatch for {}",
            old_sig.name
        );
        for (i, (ok, nk)) in old_sig
            .keyword_params
            .iter()
            .zip(&new_sig.keyword_params)
            .enumerate()
        {
            assert_eq!(
                ok.name, nk.name,
                "keyword param {} name mismatch for {}",
                i, old_sig.name
            );
            assert_eq!(
                ok.ty, nk.ty,
                "keyword param {} type mismatch for {}",
                i, old_sig.name
            );
            assert_eq!(
                ok.has_default, nk.has_default,
                "keyword param {} has_default mismatch for {}",
                i, old_sig.name
            );
        }
        assert_eq!(
            old_sig.ret, new_sig.ret,
            "fn return type mismatch for {}",
            old_sig.name
        );
        assert_eq!(
            old_sig.capabilities.len(),
            new_sig.capabilities.len(),
            "fn capabilities count mismatch for {}",
            old_sig.name
        );
    }
}

#[test]
fn equiv_struct() {
    let source = "struct Point { x: F64, y: F64 }";
    let old = collect_old(source);
    let (_, new) = collect_from_source(source);
    assert_def_to_type_equiv(&old, &new);
}

#[test]
fn equiv_enum() {
    let source = "enum Shape { struct Circle { r: F64 }, struct Empty }";
    let old = collect_old(source);
    let (_, new) = collect_from_source(source);
    assert_def_to_type_equiv(&old, &new);
}

#[test]
fn equiv_function() {
    let source = "fn add(x: U64, y: U64) -> U64 ( x + y )";
    let old = collect_old(source);
    let (_, new) = collect_from_source(source);
    assert_fn_sig_equiv(&old, &new);
}

#[test]
fn equiv_newtype() {
    let source = "newtype Name of String";
    let old = collect_old(source);
    let (_, new) = collect_from_source(source);
    assert_def_to_type_equiv(&old, &new);
}

#[test]
fn equiv_type_alias() {
    let source = "type Pair = (U64, U64)";
    let old = collect_old(source);
    let (_, new) = collect_from_source(source);
    assert_def_to_type_equiv(&old, &new);
}

#[test]
fn equiv_generic() {
    let source = "struct Box[T] { value: T }";
    let old = collect_old(source);
    let (_, new) = collect_from_source(source);
    assert_def_to_type_equiv(&old, &new);
}

#[test]
fn equiv_namespace_fn() {
    let source = "namespace Foo { fn bar() -> U64 ( 1 ) }";
    let old = collect_old(source);
    let (_, new) = collect_from_source(source);
    assert_fn_sig_equiv(&old, &new);
}

#[test]
fn equiv_sum_alias() {
    let source = "type Input = String | U64";
    let old = collect_old(source);
    let (_, new) = collect_from_source(source);
    assert_def_to_type_equiv(&old, &new);
    // Check alias_sum_alternatives match.
    assert_eq!(
        old.alias_sum_alternatives.len(),
        new.alias_sum_alternatives.len(),
        "alias_sum_alternatives count mismatch"
    );
}

#[test]
fn equiv_same_name_different_sources() {
    // Two different source files with items of the same name.
    let source_a = "struct Foo { x: U64 }";
    let source_b = "struct Foo { y: F64 }";
    let parsed_a = parse_source_file(source_a);
    let parsed_b = parse_source_file(source_b);
    let file_a = parsed_a.tree();
    let file_b = parsed_b.tree();

    // Old collector
    let old = vx_resolve::collect_types_for_sources(&[
        vx_resolve::SourceFileRef {
            id: SourceId(0),
            file: file_a.clone(),
        },
        vx_resolve::SourceFileRef {
            id: SourceId(1),
            file: file_b.clone(),
        },
    ]);

    // VIR collector
    let mut program = VirBuilder::build_program(&[(SourceId(0), file_a), (SourceId(1), file_b)]);
    let new = collect_types_vir(&mut program.arena);

    // Both should have def_to_type entries for both sources.
    // name_to_id will only keep the last one (flat-name collision), which is fine — parity.
    assert_eq!(
        old.def_to_type.len(),
        new.def_to_type.len(),
        "def_to_type count mismatch for multi-source same-name"
    );
}
