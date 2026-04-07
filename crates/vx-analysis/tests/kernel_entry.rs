use vx_analysis::{SingleSourceBridge, build_minimal_analysis};
use vx_types::{Scalar, Ty};

/// Expression body: `fn add(x: U64) -> U64 ( x + 1 )`
/// The kernel entry expr should point at the body expression (x + 1),
/// not the first expr in the body.
#[test]
fn kernel_entry_expr_body() {
    let source = "fn add(x: U64) -> U64 ( x + 1 )";
    let bridge = SingleSourceBridge::from_source_text(source);
    assert!(bridge.parse_errors.is_empty(), "{:?}", bridge.parse_errors);

    let analysis = build_minimal_analysis(&bridge);
    let body_id = bridge.bodies.iter().find(|b| b.name == "add").unwrap().id;

    // body_result_exprs should have an entry for this body
    let body_expr_id = bridge
        .body_result_exprs
        .get(&body_id)
        .expect("missing body_result_expr for add");

    // The kernel entry should use this ExprId
    let analyzed = analysis.bodies.iter().find(|b| b.id == body_id).unwrap();
    assert_eq!(
        analyzed.kernel_entries[0].source.expr, *body_expr_id,
        "kernel entry expr should be the body expression"
    );

    // Result type should be U64
    let kernel = analysis.kernels.iter().find(|k| k.body == body_id).unwrap();
    assert_eq!(
        kernel.result_ty,
        Some(Ty::Scalar(Scalar::U64)),
        "result_ty should be U64"
    );

    // Param type should be U64
    assert_eq!(kernel.params.len(), 1);
    assert_eq!(
        kernel.params[0].ty,
        Some(Ty::Scalar(Scalar::U64)),
        "param x should have type U64"
    );
    assert_eq!(kernel.params[0].name, "x");
}

/// Block body with let: `fn main() -> U64 ( let x = 1; x + 1 )`
/// The kernel entry expr should point at the block body expression.
#[test]
fn kernel_entry_block_body() {
    let source = "fn main() -> U64 ( let x = 1; x + 1 )";
    let bridge = SingleSourceBridge::from_source_text(source);
    assert!(bridge.parse_errors.is_empty(), "{:?}", bridge.parse_errors);

    let analysis = build_minimal_analysis(&bridge);
    let body_id = bridge.bodies.iter().find(|b| b.name == "main").unwrap().id;

    let body_expr_id = bridge
        .body_result_exprs
        .get(&body_id)
        .expect("missing body_result_expr for main");

    let analyzed = analysis.bodies.iter().find(|b| b.id == body_id).unwrap();
    assert_eq!(
        analyzed.kernel_entries[0].source.expr, *body_expr_id,
        "kernel entry expr should be the body expression"
    );

    let kernel = analysis.kernels.iter().find(|k| k.body == body_id).unwrap();
    assert_eq!(
        kernel.result_ty,
        Some(Ty::Scalar(Scalar::U64)),
        "result_ty should be U64"
    );
}

/// The body_expr_id should NOT be the same as the first expr in the body
/// when there are let bindings (the first expr is the let initializer).
#[test]
fn kernel_entry_is_not_first_expr() {
    let source = "fn main() -> U64 ( let x = 1; let y = 2; x + y )";
    let bridge = SingleSourceBridge::from_source_text(source);
    assert!(bridge.parse_errors.is_empty(), "{:?}", bridge.parse_errors);

    let body_id = bridge.bodies.iter().find(|b| b.name == "main").unwrap().id;
    let body_expr_id = *bridge
        .body_result_exprs
        .get(&body_id)
        .expect("missing body_result_expr");

    let first_expr = bridge
        .exprs
        .iter()
        .find(|e| e.body == Some(body_id))
        .unwrap()
        .id;

    // The body expression should be the outer block/paren, not the first sub-expression
    assert_ne!(
        body_expr_id, first_expr,
        "body_expr_id should differ from first_expr when there are let bindings"
    );
}

#[test]
fn debug_body_expr() {
    let source = "fn add(x: U64) -> U64 ( x + 1 )";
    let bridge = SingleSourceBridge::from_source_text(source);
    let body_id = bridge.bodies.iter().find(|b| b.name == "add").unwrap().id;
    let body = bridge.bodies.iter().find(|b| b.name == "add").unwrap();

    eprintln!("body_expr_span: {:?}", body.body_expr_span);
    eprintln!(
        "body_result_expr: {:?}",
        bridge.body_result_exprs.get(&body_id)
    );

    for e in &bridge.exprs {
        if e.body == Some(body_id) {
            let ty = bridge.expr_types.get(&e.id);
            eprintln!(
                "  {:?} span={:?} kind={:?} ty={:?}",
                e.id, e.span, e.kind, ty
            );
        }
    }
}
