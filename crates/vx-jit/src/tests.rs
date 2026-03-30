use kajit_hir::{BinaryOp, Expr, StmtKind, Type};

use crate::{
    KernelBinaryOp, KernelExpr, KernelIntrinsic, KernelLocal, KernelLocalId, KernelStmt,
    KernelType, KernelValue, compile_source_text_u64_add_function, compile_u64_add,
    interpret_source_text_string_function, interpret_source_text_u64_add_function,
    interpret_u64_add, lower_source_text_kernel, lower_source_text_u64_add_hir,
    lower_source_text_u64_add_ir, lower_u64_add_hir, lower_u64_add_hir_text, lower_u64_add_ir,
    lower_u64_add_ir_text, lower_u64_add_kernel, run_source_text_u64_add_function, run_u64_add,
};

const SOURCE_ADD_U64: &str = r#"
fn add(lhs: U64, rhs: U64) -> U64 (
  lhs + rhs
)
"#;

const SOURCE_SPARSE_PREFIX: &str = r#"
fn sparse_prefix(name: String) -> String (
  let len = name.len();
  if len == 1 (
    "1"
  ) else (
    if len == 2 (
      "2"
    ) else (
      if len == 3 (
        `3/{name.slice(0..1)}`
      ) else (
        `{name.slice(0..2)}/{name.slice(2..4)}`
      )
    )
  )
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
        vec![KernelStmt::Return(KernelExpr::Binary {
            op: KernelBinaryOp::Add,
            lhs: Box::new(KernelExpr::Local(KernelLocalId(0))),
            rhs: Box::new(KernelExpr::Local(KernelLocalId(1))),
        })]
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
    let function = lower_source_text_kernel(SOURCE_ADD_U64, "add")
        .expect("source text u64 add should lower into kernel IR");

    assert_eq!(function.name, "add");
    assert_eq!(
        function.body,
        vec![KernelStmt::Return(KernelExpr::Binary {
            op: KernelBinaryOp::Add,
            lhs: Box::new(KernelExpr::Local(KernelLocalId(0))),
            rhs: Box::new(KernelExpr::Local(KernelLocalId(1))),
        })]
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

#[test]
fn lowers_sparse_prefix_into_kernel_ir() {
    let function = lower_source_text_kernel(SOURCE_SPARSE_PREFIX, "sparse_prefix")
        .expect("sparse_prefix should lower into kernel IR");

    assert_eq!(function.name, "sparse_prefix");
    assert_eq!(function.return_type, KernelType::String);
    assert_eq!(
        function.body,
        vec![
            KernelStmt::Let {
                local: KernelLocal {
                    local: KernelLocalId(1),
                    name: "len".to_owned(),
                    ty: KernelType::U64,
                },
                value: KernelExpr::Intrinsic {
                    intrinsic: KernelIntrinsic::StringLen,
                    args: vec![KernelExpr::Local(KernelLocalId(0))],
                },
            },
            KernelStmt::Return(KernelExpr::If {
                condition: Box::new(KernelExpr::Binary {
                    op: KernelBinaryOp::Eq,
                    lhs: Box::new(KernelExpr::Local(KernelLocalId(1))),
                    rhs: Box::new(KernelExpr::U64(1)),
                }),
                then_expr: Box::new(KernelExpr::String("1".to_owned())),
                else_expr: Box::new(KernelExpr::If {
                    condition: Box::new(KernelExpr::Binary {
                        op: KernelBinaryOp::Eq,
                        lhs: Box::new(KernelExpr::Local(KernelLocalId(1))),
                        rhs: Box::new(KernelExpr::U64(2)),
                    }),
                    then_expr: Box::new(KernelExpr::String("2".to_owned())),
                    else_expr: Box::new(KernelExpr::If {
                        condition: Box::new(KernelExpr::Binary {
                            op: KernelBinaryOp::Eq,
                            lhs: Box::new(KernelExpr::Local(KernelLocalId(1))),
                            rhs: Box::new(KernelExpr::U64(3)),
                        }),
                        then_expr: Box::new(KernelExpr::Concat(vec![
                            KernelExpr::String("3/".to_owned()),
                            KernelExpr::Intrinsic {
                                intrinsic: KernelIntrinsic::StringSlice,
                                args: vec![
                                    KernelExpr::Local(KernelLocalId(0)),
                                    KernelExpr::U64(0),
                                    KernelExpr::U64(1),
                                ],
                            },
                        ])),
                        else_expr: Box::new(KernelExpr::Concat(vec![
                            KernelExpr::Intrinsic {
                                intrinsic: KernelIntrinsic::StringSlice,
                                args: vec![
                                    KernelExpr::Local(KernelLocalId(0)),
                                    KernelExpr::U64(0),
                                    KernelExpr::U64(2),
                                ],
                            },
                            KernelExpr::String("/".to_owned()),
                            KernelExpr::Intrinsic {
                                intrinsic: KernelIntrinsic::StringSlice,
                                args: vec![
                                    KernelExpr::Local(KernelLocalId(0)),
                                    KernelExpr::U64(2),
                                    KernelExpr::U64(4),
                                ],
                            },
                        ])),
                    }),
                }),
            }),
        ]
    );
}

#[test]
fn interprets_sparse_prefix() {
    let result = interpret_source_text_string_function(
        SOURCE_SPARSE_PREFIX,
        "sparse_prefix",
        &[KernelValue::String("a".to_owned())],
    )
    .expect("sparse_prefix should interpret for len=1");
    assert_eq!(result, "1");

    let result = interpret_source_text_string_function(
        SOURCE_SPARSE_PREFIX,
        "sparse_prefix",
        &[KernelValue::String("ab".to_owned())],
    )
    .expect("sparse_prefix should interpret for len=2");
    assert_eq!(result, "2");

    let result = interpret_source_text_string_function(
        SOURCE_SPARSE_PREFIX,
        "sparse_prefix",
        &[KernelValue::String("abc".to_owned())],
    )
    .expect("sparse_prefix should interpret for len=3");
    assert_eq!(result, "3/a");

    let result = interpret_source_text_string_function(
        SOURCE_SPARSE_PREFIX,
        "sparse_prefix",
        &[KernelValue::String("serde".to_owned())],
    )
    .expect("sparse_prefix should interpret for longer strings");
    assert_eq!(result, "se/rd");
}
