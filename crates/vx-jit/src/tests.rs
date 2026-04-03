use kajit_hir::{BinaryOp, Expr, StmtKind, Type};

use crate::{
    StrictFragmentAbiLayout, StrictFragmentBinaryOp, StrictFragmentExpr, StrictFragmentIntrinsic,
    StrictFragmentLocal, StrictFragmentLocalId, StrictFragmentStmt, StrictFragmentType,
    StrictFragmentValue, compile_source_text_u64_add_function, compile_strict_fragment,
    compile_u64_add, execute_compiled_copying_strict_fragment_strings,
    execute_compiled_unmanaged_strict_fragment, interpret_source_text_string_function,
    interpret_source_text_u64_add_function, interpret_u64_add, lower_source_text_strict_fragment,
    lower_source_text_u64_add_hir, lower_source_text_u64_add_ir, lower_u64_add_hir,
    lower_u64_add_hir_text, lower_u64_add_ir, lower_u64_add_ir_text, lower_u64_add_strict_fragment,
    run_source_text_u64_add_function, run_u64_add,
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

const SOURCE_STR_LEN: &str = r#"
fn str_len(name: String) -> U64 (
  name.len()
)
"#;

const SOURCE_STR_SLICE: &str = r#"
fn str_slice(name: String, start: U64, end: U64) -> String (
  name.slice(start..end)
)
"#;

const SOURCE_ONE: &str = r#"
fn one() -> String (
  "1"
)
"#;

const SOURCE_PICK: &str = r#"
fn pick(flag: U64) -> String (
  if flag == 1 (
    "yes"
  ) else (
    "no"
  )
)
"#;

const SOURCE_JOIN: &str = r#"
fn join(a: String, b: String) -> String (
  `{a}{b}`
)
"#;

const CARGO_RESOLVE_SOURCE: &str = include_str!("../../../zoo/cargo-resolve.vx");

fn cargo_resolve_function_source(function_name: &str) -> String {
    let needle = format!("fn {function_name}(");
    let start = CARGO_RESOLVE_SOURCE
        .find(&needle)
        .unwrap_or_else(|| panic!("missing {function_name} in cargo-resolve.vx"));
    let tail = &CARGO_RESOLVE_SOURCE[start..];
    let end = tail.find("\nfn ").unwrap_or(tail.len());
    tail[..end].trim().to_owned() + "\n"
}

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
fn lowers_u64_add_into_strict_fragment_ir() {
    let function = lower_u64_add_strict_fragment();

    assert_eq!(function.name, "add_u64");
    assert_eq!(
        function.body,
        vec![StrictFragmentStmt::Return(StrictFragmentExpr::Binary {
            op: StrictFragmentBinaryOp::Add,
            lhs: Box::new(StrictFragmentExpr::Local(StrictFragmentLocalId(0))),
            rhs: Box::new(StrictFragmentExpr::Local(StrictFragmentLocalId(1))),
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
fn lowers_source_text_u64_add_into_strict_fragment_ir() {
    let function = lower_source_text_strict_fragment(SOURCE_ADD_U64, "add")
        .expect("source text u64 add should lower into strict fragment IR");

    assert_eq!(function.name, "add");
    assert_eq!(
        function.body,
        vec![StrictFragmentStmt::Return(StrictFragmentExpr::Binary {
            op: StrictFragmentBinaryOp::Add,
            lhs: Box::new(StrictFragmentExpr::Local(StrictFragmentLocalId(0))),
            rhs: Box::new(StrictFragmentExpr::Local(StrictFragmentLocalId(1))),
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
fn lowers_sparse_prefix_into_strict_fragment_ir() {
    let function = lower_source_text_strict_fragment(SOURCE_SPARSE_PREFIX, "sparse_prefix")
        .expect("sparse_prefix should lower into strict fragment IR");

    assert_eq!(function.name, "sparse_prefix");
    assert_eq!(function.return_type, StrictFragmentType::String);
    assert_eq!(
        function.body,
        vec![
            StrictFragmentStmt::Let {
                local: StrictFragmentLocal {
                    local: StrictFragmentLocalId(1),
                    name: "len".to_owned(),
                    ty: StrictFragmentType::U64,
                },
                value: StrictFragmentExpr::Intrinsic {
                    intrinsic: StrictFragmentIntrinsic::StringLen,
                    args: vec![StrictFragmentExpr::Local(StrictFragmentLocalId(0))],
                },
            },
            StrictFragmentStmt::Return(StrictFragmentExpr::If {
                condition: Box::new(StrictFragmentExpr::Binary {
                    op: StrictFragmentBinaryOp::Eq,
                    lhs: Box::new(StrictFragmentExpr::Local(StrictFragmentLocalId(1))),
                    rhs: Box::new(StrictFragmentExpr::U64(1)),
                }),
                then_expr: Box::new(StrictFragmentExpr::String("1".to_owned())),
                else_expr: Box::new(StrictFragmentExpr::If {
                    condition: Box::new(StrictFragmentExpr::Binary {
                        op: StrictFragmentBinaryOp::Eq,
                        lhs: Box::new(StrictFragmentExpr::Local(StrictFragmentLocalId(1))),
                        rhs: Box::new(StrictFragmentExpr::U64(2)),
                    }),
                    then_expr: Box::new(StrictFragmentExpr::String("2".to_owned())),
                    else_expr: Box::new(StrictFragmentExpr::If {
                        condition: Box::new(StrictFragmentExpr::Binary {
                            op: StrictFragmentBinaryOp::Eq,
                            lhs: Box::new(StrictFragmentExpr::Local(StrictFragmentLocalId(1))),
                            rhs: Box::new(StrictFragmentExpr::U64(3)),
                        }),
                        then_expr: Box::new(StrictFragmentExpr::Concat(vec![
                            StrictFragmentExpr::String("3/".to_owned()),
                            StrictFragmentExpr::Intrinsic {
                                intrinsic: StrictFragmentIntrinsic::StringSlice,
                                args: vec![
                                    StrictFragmentExpr::Local(StrictFragmentLocalId(0)),
                                    StrictFragmentExpr::U64(0),
                                    StrictFragmentExpr::U64(1),
                                ],
                            },
                        ])),
                        else_expr: Box::new(StrictFragmentExpr::Concat(vec![
                            StrictFragmentExpr::Intrinsic {
                                intrinsic: StrictFragmentIntrinsic::StringSlice,
                                args: vec![
                                    StrictFragmentExpr::Local(StrictFragmentLocalId(0)),
                                    StrictFragmentExpr::U64(0),
                                    StrictFragmentExpr::U64(2),
                                ],
                            },
                            StrictFragmentExpr::String("/".to_owned()),
                            StrictFragmentExpr::Intrinsic {
                                intrinsic: StrictFragmentIntrinsic::StringSlice,
                                args: vec![
                                    StrictFragmentExpr::Local(StrictFragmentLocalId(0)),
                                    StrictFragmentExpr::U64(2),
                                    StrictFragmentExpr::U64(4),
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
        &[StrictFragmentValue::String("a".to_owned())],
    )
    .expect("sparse_prefix should interpret for len=1");
    assert_eq!(result, "1");

    let result = interpret_source_text_string_function(
        SOURCE_SPARSE_PREFIX,
        "sparse_prefix",
        &[StrictFragmentValue::String("ab".to_owned())],
    )
    .expect("sparse_prefix should interpret for len=2");
    assert_eq!(result, "2");

    let result = interpret_source_text_string_function(
        SOURCE_SPARSE_PREFIX,
        "sparse_prefix",
        &[StrictFragmentValue::String("abc".to_owned())],
    )
    .expect("sparse_prefix should interpret for len=3");
    assert_eq!(result, "3/a");

    let result = interpret_source_text_string_function(
        SOURCE_SPARSE_PREFIX,
        "sparse_prefix",
        &[StrictFragmentValue::String("serde".to_owned())],
    )
    .expect("sparse_prefix should interpret for longer strings");
    assert_eq!(result, "se/rd");
}

#[test]
fn compiles_and_executes_source_text_string_len() {
    let function = lower_source_text_strict_fragment(SOURCE_STR_LEN, "str_len")
        .expect("str_len should lower into strict fragment IR");

    let interpreted = crate::interpret(
        &function,
        &[StrictFragmentValue::String("serde".to_owned())],
    )
    .expect("str_len should interpret");
    assert_eq!(interpreted, StrictFragmentValue::U64(5));

    let compiled = crate::kajit_backend::compile(&function).expect("str_len should compile");
    let str_len: unsafe extern "C" fn(u64, u64) -> u64 =
        unsafe { core::mem::transmute(compiled.as_ptr()) };
    let input = "serde";
    let result = unsafe { str_len(input.as_ptr() as u64, input.len() as u64) };
    assert_eq!(result, input.len() as u64);
}

#[test]
fn compiles_and_executes_source_text_string_slice() {
    let function = lower_source_text_strict_fragment(SOURCE_STR_SLICE, "str_slice")
        .expect("str_slice should lower into strict fragment IR");

    let interpreted = crate::interpret(
        &function,
        &[
            StrictFragmentValue::String("serde".to_owned()),
            StrictFragmentValue::U64(1),
            StrictFragmentValue::U64(4),
        ],
    )
    .expect("str_slice should interpret");
    assert_eq!(interpreted, StrictFragmentValue::String("erd".to_owned()));

    let compiled = crate::kajit_backend::compile(&function).expect("str_slice should compile");
    let str_slice: unsafe extern "C" fn(u64, u64, u64, u64) -> (u64, u64) =
        unsafe { core::mem::transmute(compiled.as_ptr()) };
    let input = "serde";
    let (ptr, len) = unsafe { str_slice(input.as_ptr() as u64, input.len() as u64, 1, 4) };
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let output = std::str::from_utf8(bytes).expect("slice result should be utf-8");
    assert_eq!(output, "erd");
}

#[test]
fn compiles_and_executes_source_text_string_literal_return() {
    let function = lower_source_text_strict_fragment(SOURCE_ONE, "one")
        .expect("one should lower into strict fragment IR");

    let interpreted = crate::interpret(&function, &[]).expect("one should interpret");
    assert_eq!(interpreted, StrictFragmentValue::String("1".to_owned()));

    let compiled = crate::kajit_backend::compile(&function).expect("one should compile");
    let one: unsafe extern "C" fn() -> (u64, u64) =
        unsafe { core::mem::transmute(compiled.as_ptr()) };
    let (ptr, len) = unsafe { one() };
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let output = std::str::from_utf8(bytes).expect("literal result should be utf-8");
    assert_eq!(output, "1");
}

#[test]
fn executes_compiled_u64_add_via_abi_bridge() {
    let function = lower_source_text_strict_fragment(SOURCE_ADD_U64, "add")
        .expect("add should lower into strict fragment IR");
    let compiled = compile_strict_fragment(&function).expect("add should compile");

    let result = unsafe {
        execute_compiled_unmanaged_strict_fragment(
            &function,
            &compiled,
            &[StrictFragmentValue::U64(30), StrictFragmentValue::U64(12)],
        )
    }
    .expect("abi bridge should execute add");

    assert_eq!(
        result,
        crate::CompiledStrictFragmentOutput::Value(StrictFragmentValue::U64(42))
    );
}

#[test]
fn strict_fragment_abi_layout_matches_current_domain() {
    assert_eq!(
        StrictFragmentAbiLayout::from_type(StrictFragmentType::U64).word_count(),
        1
    );
    assert_eq!(
        StrictFragmentAbiLayout::from_type(StrictFragmentType::Bool).word_count(),
        1
    );
    assert_eq!(
        StrictFragmentAbiLayout::from_type(StrictFragmentType::String).word_count(),
        2
    );
}

#[test]
fn executes_compiled_string_literal_via_abi_bridge() {
    let function = lower_source_text_strict_fragment(SOURCE_ONE, "one")
        .expect("one should lower into strict fragment IR");
    let compiled = compile_strict_fragment(&function).expect("one should compile");

    let result = unsafe { execute_compiled_unmanaged_strict_fragment(&function, &compiled, &[]) }
        .expect("abi bridge should execute one");

    let crate::CompiledStrictFragmentOutput::UnmanagedString(result) = result else {
        panic!("expected unmanaged string return");
    };
    let output = unsafe { result.to_owned_string() }.expect("result should be utf-8");
    assert_eq!(output, "1");
}

#[test]
fn executes_compiled_string_slice_via_abi_bridge() {
    let function = lower_source_text_strict_fragment(SOURCE_STR_SLICE, "str_slice")
        .expect("str_slice should lower into strict fragment IR");
    let compiled = compile_strict_fragment(&function).expect("str_slice should compile");

    let result = unsafe {
        execute_compiled_copying_strict_fragment_strings(
            &function,
            &compiled,
            &[
                StrictFragmentValue::String("serde".to_owned()),
                StrictFragmentValue::U64(1),
                StrictFragmentValue::U64(4),
            ],
        )
    }
    .expect("abi bridge should execute str_slice");

    assert_eq!(result, StrictFragmentValue::String("erd".to_owned()));
}

#[test]
fn compiles_and_executes_source_text_string_if_return() {
    let function = lower_source_text_strict_fragment(SOURCE_PICK, "pick")
        .expect("pick should lower into strict fragment IR");

    let interpreted = crate::interpret(&function, &[StrictFragmentValue::U64(1)])
        .expect("pick should interpret for yes");
    assert_eq!(interpreted, StrictFragmentValue::String("yes".to_owned()));
    let interpreted = crate::interpret(&function, &[StrictFragmentValue::U64(0)])
        .expect("pick should interpret for no");
    assert_eq!(interpreted, StrictFragmentValue::String("no".to_owned()));

    let compiled = crate::kajit_backend::compile(&function).expect("pick should compile");
    let pick: unsafe extern "C" fn(u64) -> (u64, u64) =
        unsafe { core::mem::transmute(compiled.as_ptr()) };

    let (ptr, len) = unsafe { pick(1) };
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let output = std::str::from_utf8(bytes).expect("pick yes result should be utf-8");
    assert_eq!(output, "yes");

    let (ptr, len) = unsafe { pick(0) };
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let output = std::str::from_utf8(bytes).expect("pick no result should be utf-8");
    assert_eq!(output, "no");
}

#[test]
fn compiles_and_executes_source_text_string_concat() {
    let function = lower_source_text_strict_fragment(SOURCE_JOIN, "join")
        .expect("join should lower into strict fragment IR");

    let interpreted = crate::interpret(
        &function,
        &[
            StrictFragmentValue::String("hello".to_owned()),
            StrictFragmentValue::String(" world".to_owned()),
        ],
    )
    .expect("join should interpret");
    assert_eq!(
        interpreted,
        StrictFragmentValue::String("hello world".to_owned())
    );

    let compiled = crate::kajit_backend::compile(&function).expect("join should compile");
    let join: unsafe extern "C" fn(u64, u64, u64, u64) -> (u64, u64) =
        unsafe { core::mem::transmute(compiled.as_ptr()) };
    let a = "hello";
    let b = " world";
    let (ptr, len) = unsafe {
        join(
            a.as_ptr() as u64,
            a.len() as u64,
            b.as_ptr() as u64,
            b.len() as u64,
        )
    };
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let output = std::str::from_utf8(bytes).expect("join result should be utf-8");
    assert_eq!(output, "hello world");
    unsafe {
        std::alloc::dealloc(
            ptr as *mut u8,
            std::alloc::Layout::from_size_align(len as usize, 1).expect("join layout"),
        );
    }
}

#[test]
fn compiles_and_executes_sparse_prefix() {
    let function = lower_source_text_strict_fragment(SOURCE_SPARSE_PREFIX, "sparse_prefix")
        .expect("sparse_prefix should lower into strict fragment IR");

    let compiled = crate::kajit_backend::compile(&function).expect("sparse_prefix should compile");
    let sparse_prefix: unsafe extern "C" fn(u64, u64) -> (u64, u64) =
        unsafe { core::mem::transmute(compiled.as_ptr()) };

    let input = "abc";
    let (ptr, len) = unsafe { sparse_prefix(input.as_ptr() as u64, input.len() as u64) };
    eprintln!("abc -> ptr={ptr:#x} len={len}");
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let output = std::str::from_utf8(bytes).expect("sparse_prefix result should be utf-8");
    assert_eq!(output, "3/a");

    let input = "serde";
    let (ptr, len) = unsafe { sparse_prefix(input.as_ptr() as u64, input.len() as u64) };
    eprintln!("serde -> ptr={ptr:#x} len={len}");
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let output = std::str::from_utf8(bytes).expect("sparse_prefix result should be utf-8");
    assert_eq!(output, "se/rd");
}

#[test]
fn compiles_and_executes_registry_entry_key_from_cargo_resolve() {
    let source = cargo_resolve_function_source("registry_entry_key");
    let function = lower_source_text_strict_fragment(&source, "registry_entry_key")
        .expect("registry_entry_key should lower from cargo-resolve.vx");

    let interpreted = crate::interpret(
        &function,
        &[
            StrictFragmentValue::String("serde".to_owned()),
            StrictFragmentValue::String("1.0.228".to_owned()),
        ],
    )
    .expect("registry_entry_key should interpret");
    assert_eq!(
        interpreted,
        StrictFragmentValue::String("entry:registry:serde@1.0.228".to_owned())
    );

    let compiled = crate::kajit_backend::compile(&function)
        .expect("registry_entry_key should compile from cargo-resolve.vx");
    let registry_entry_key: unsafe extern "C" fn(u64, u64, u64, u64) -> (u64, u64) =
        unsafe { core::mem::transmute(compiled.as_ptr()) };
    let package_name = "serde";
    let version = "1.0.228";
    let (ptr, len) = unsafe {
        registry_entry_key(
            package_name.as_ptr() as u64,
            package_name.len() as u64,
            version.as_ptr() as u64,
            version.len() as u64,
        )
    };
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let output = std::str::from_utf8(bytes).expect("registry_entry_key result should be utf-8");
    assert_eq!(output, "entry:registry:serde@1.0.228");
    unsafe {
        std::alloc::dealloc(
            ptr as *mut u8,
            std::alloc::Layout::from_size_align(len as usize, 1)
                .expect("registry_entry_key layout"),
        );
    }
}
