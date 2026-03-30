use kajit_hir::{BinaryOp, Expr, StmtKind, Type};

use crate::{
    KernelExpr, KernelLocalId, compile_source_text_u64_add_function, compile_u64_add,
    interpret_source_text_u64_add_function, interpret_u64_add, lower_source_text_u64_add_hir,
    lower_source_text_u64_add_ir, lower_source_text_u64_add_kernel, lower_u64_add_hir,
    lower_u64_add_hir_text, lower_u64_add_ir, lower_u64_add_ir_text, lower_u64_add_kernel,
    run_source_text_u64_add_function, run_u64_add,
};

const SOURCE_ADD_U64: &str = r#"
fn add(lhs: U64, rhs: U64) -> U64 (
  lhs + rhs
)
"#;

#[test]
fn lowers_u64_add_into_kajit_hir() {
    let module = lower_u64_add_hir().expect("u64 add should lower into Kajit HIR");
    let (_, function) = module
        .functions
        .iter()
        .next()
        .expect("module should contain one function");

    assert_eq!(function.name, "add_u64");
    assert_eq!(function.return_type, Type::u(64));

    let [stmt] = function.body.statements.as_slice() else {
        panic!("expected one return statement");
    };
    let StmtKind::Return(Some(Expr::Binary { op, .. })) = &stmt.kind else {
        panic!("expected return of a binary expression");
    };
    assert_eq!(*op, BinaryOp::Add);
}

#[test]
fn lowers_u64_add_into_kernel_ir() {
    let function = lower_u64_add_kernel();

    assert_eq!(function.name, "add_u64");
    assert_eq!(
        function.body,
        KernelExpr::Add(
            Box::new(KernelExpr::Local(KernelLocalId(0))),
            Box::new(KernelExpr::Local(KernelLocalId(1))),
        )
    );
}

#[test]
fn renders_u64_add_hir_text() {
    let text = lower_u64_add_hir_text().expect("u64 add HIR text should render");

    assert!(text.contains("function f0 \"add_u64\""));
    assert!(text.contains("stmt0: return binary add(l0, l1)"));
}

#[test]
fn lowers_u64_add_into_kajit_ir() {
    let func = lower_u64_add_ir().expect("u64 add should lower into Kajit IR");

    assert!(
        !func.lambdas.is_empty(),
        "IR should contain at least one lambda"
    );
    assert_eq!(func.root, func.lambdas[0]);
}

#[test]
fn renders_u64_add_ir_text() {
    let text = lower_u64_add_ir_text().expect("u64 add IR text should render");

    assert!(text.contains("lambda"));
    assert!(
        text.contains("add") || text.contains("Add"),
        "expected add operation in IR text:\n{text}"
    );
}

#[test]
fn compiles_u64_add_into_machine_code() {
    let compiled = compile_u64_add().expect("u64 add should compile into machine code");

    assert!(
        !compiled.code().is_empty(),
        "compiled code should not be empty"
    );
}

#[test]
fn executes_u64_add() {
    let interpreted = interpret_u64_add(30, 12).expect("u64 add should interpret");
    assert_eq!(interpreted, 42);

    let result = unsafe { run_u64_add(30, 12) }.expect("u64 add should execute");
    assert_eq!(result, 42);

    let result = unsafe { run_u64_add(0, 0) }.expect("u64 add should execute");
    assert_eq!(result, 0);

    let result = unsafe { run_u64_add(u64::MAX, 1) }.expect("u64 add should execute");
    assert_eq!(result, 0);
}

#[test]
fn lowers_source_text_u64_add_into_kernel_ir() {
    let function = lower_source_text_u64_add_kernel(SOURCE_ADD_U64, "add")
        .expect("source text u64 add should lower into kernel IR");

    assert_eq!(function.name, "add");
    assert_eq!(
        function.body,
        KernelExpr::Add(
            Box::new(KernelExpr::Local(KernelLocalId(0))),
            Box::new(KernelExpr::Local(KernelLocalId(1))),
        )
    );
}

#[test]
fn lowers_source_text_u64_add_into_kajit_hir() {
    let module = lower_source_text_u64_add_hir(SOURCE_ADD_U64, "add")
        .expect("source text u64 add should lower into Kajit HIR");
    let (_, function) = module
        .functions
        .iter()
        .next()
        .expect("module should contain one function");

    assert_eq!(function.name, "add");
    assert_eq!(function.return_type, Type::u(64));

    let [stmt] = function.body.statements.as_slice() else {
        panic!("expected one return statement");
    };
    let StmtKind::Return(Some(Expr::Binary { op, .. })) = &stmt.kind else {
        panic!("expected return of a binary expression");
    };
    assert_eq!(*op, BinaryOp::Add);
}

#[test]
fn lowers_source_text_u64_add_into_kajit_ir() {
    let func = lower_source_text_u64_add_ir(SOURCE_ADD_U64, "add")
        .expect("source text u64 add should lower into Kajit IR");

    assert!(
        !func.lambdas.is_empty(),
        "IR should contain at least one lambda"
    );
    assert_eq!(func.root, func.lambdas[0]);
}

#[test]
fn compiles_and_executes_source_text_u64_add() {
    let interpreted = interpret_source_text_u64_add_function(SOURCE_ADD_U64, "add", 30, 12)
        .expect("source text u64 add should interpret");
    assert_eq!(interpreted, 42);

    let compiled = compile_source_text_u64_add_function(SOURCE_ADD_U64, "add")
        .expect("source text u64 add should compile into machine code");
    assert!(
        !compiled.code().is_empty(),
        "compiled code should not be empty"
    );

    let result = unsafe { run_source_text_u64_add_function(SOURCE_ADD_U64, "add", 30, 12) }
        .expect("source text u64 add should execute");
    assert_eq!(result, 42);

    let result = unsafe { run_source_text_u64_add_function(SOURCE_ADD_U64, "add", u64::MAX, 1) }
        .expect("source text u64 add should execute");
    assert_eq!(result, 0);
}
