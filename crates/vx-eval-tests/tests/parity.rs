use vx_eval_tests::{ScalarResult, eval_cranelift, eval_interpreter};

/// Run one case through both backends and assert byte-identical results.
/// If Cranelift can't lower it (returns None), the test is skipped (not failed).
/// If the interpreter can't run it, the test fails — interpreter is the reference.
fn parity_case(
    test_name: &str,
    source: &str,
    function_name: &str,
    args: &[ScalarResult],
    expected: ScalarResult,
) {
    let interp = eval_interpreter(source, function_name, args)
        .unwrap_or_else(|e| panic!("[{test_name}] interpreter error: {e}"));

    let interp_result = match interp {
        Some(r) => r,
        None => panic!("[{test_name}] interpreter returned None (unsupported program)"),
    };

    assert_eq!(
        interp_result, expected,
        "[{test_name}] interpreter result mismatch"
    );

    let clift = eval_cranelift(source, function_name, args)
        .unwrap_or_else(|e| panic!("[{test_name}] cranelift error: {e}"));

    if let Some(clift_result) = clift {
        assert_eq!(
            clift_result, interp_result,
            "[{test_name}] PARITY FAILURE: Cranelift and interpreter produced different results"
        );
    }
    // If Cranelift returned None, the program is not strict-compatible — fallback is correct.
}

// ---------------------------------------------------------------------------
// U64 arithmetic
// ---------------------------------------------------------------------------

#[test]
fn parity_const_u64() {
    parity_case(
        "const_u64",
        "fn main() -> U64 ( 42 )",
        "main",
        &[],
        ScalarResult::U64(42),
    );
}

#[test]
fn parity_add() {
    parity_case(
        "add",
        "fn add(a: U64, b: U64) -> U64 ( a + b )",
        "add",
        &[ScalarResult::U64(10), ScalarResult::U64(32)],
        ScalarResult::U64(42),
    );
}

#[test]
fn parity_sub() {
    parity_case(
        "sub",
        "fn sub(a: U64, b: U64) -> U64 ( a - b )",
        "sub",
        &[ScalarResult::U64(50), ScalarResult::U64(8)],
        ScalarResult::U64(42),
    );
}

// ---------------------------------------------------------------------------
// Boolean
// ---------------------------------------------------------------------------

#[test]
fn parity_eq_true() {
    parity_case(
        "eq_true",
        "fn check(n: U64) -> Bool ( n == 42 )",
        "check",
        &[ScalarResult::U64(42)],
        ScalarResult::Bool(true),
    );
}

#[test]
fn parity_eq_false() {
    parity_case(
        "eq_false",
        "fn check(n: U64) -> Bool ( n == 42 )",
        "check",
        &[ScalarResult::U64(7)],
        ScalarResult::Bool(false),
    );
}

#[test]
fn parity_ne() {
    parity_case(
        "ne",
        "fn check(n: U64) -> Bool ( n != 0 )",
        "check",
        &[ScalarResult::U64(1)],
        ScalarResult::Bool(true),
    );
}

// ---------------------------------------------------------------------------
// If / select
// ---------------------------------------------------------------------------

#[test]
fn parity_if_true_branch() {
    parity_case(
        "if_true_branch",
        "fn pick(n: U64) -> U64 ( if n == 1 ( 10 ) else ( 20 ) )",
        "pick",
        &[ScalarResult::U64(1)],
        ScalarResult::U64(10),
    );
}

#[test]
fn parity_if_false_branch() {
    parity_case(
        "if_false_branch",
        "fn pick(n: U64) -> U64 ( if n == 1 ( 10 ) else ( 20 ) )",
        "pick",
        &[ScalarResult::U64(99)],
        ScalarResult::U64(20),
    );
}

#[test]
fn parity_nested_if() {
    parity_case(
        "nested_if",
        "fn classify(n: U64) -> U64 ( if n == 1 ( 10 ) else ( if n == 5 ( 50 ) else ( 0 ) ) )",
        "classify",
        &[ScalarResult::U64(5)],
        ScalarResult::U64(50),
    );
}

#[test]
fn parity_nested_if_else_arm() {
    parity_case(
        "nested_if_else_arm",
        "fn classify(n: U64) -> U64 ( if n == 1 ( 10 ) else ( if n == 5 ( 50 ) else ( 0 ) ) )",
        "classify",
        &[ScalarResult::U64(99)],
        ScalarResult::U64(0),
    );
}

// ---------------------------------------------------------------------------
// String operations
// ---------------------------------------------------------------------------

#[test]
fn parity_const_string() {
    parity_case(
        "const_string",
        r#"fn greeting() -> String ( "hello" )"#,
        "greeting",
        &[],
        ScalarResult::String("hello".to_owned()),
    );
}

#[test]
fn parity_string_len() {
    parity_case(
        "string_len",
        "fn slen(s: String) -> U64 ( s.len() )",
        "slen",
        &[ScalarResult::String("serde".to_owned())],
        ScalarResult::U64(5),
    );
}

#[test]
fn parity_string_len_empty() {
    parity_case(
        "string_len_empty",
        "fn slen(s: String) -> U64 ( s.len() )",
        "slen",
        &[ScalarResult::String(String::new())],
        ScalarResult::U64(0),
    );
}

#[test]
fn parity_string_concat() {
    parity_case(
        "string_concat",
        r#"fn join(a: String, b: String) -> String ( `{a}{b}` )"#,
        "join",
        &[
            ScalarResult::String("hello".to_owned()),
            ScalarResult::String(" world".to_owned()),
        ],
        ScalarResult::String("hello world".to_owned()),
    );
}

#[test]
fn parity_string_if_select() {
    parity_case(
        "string_if_select",
        r#"fn pick(flag: U64) -> String ( if flag == 1 ( "yes" ) else ( "no" ) )"#,
        "pick",
        &[ScalarResult::U64(1)],
        ScalarResult::String("yes".to_owned()),
    );
}

#[test]
fn parity_string_if_select_no() {
    parity_case(
        "string_if_select_no",
        r#"fn pick(flag: U64) -> String ( if flag == 1 ( "yes" ) else ( "no" ) )"#,
        "pick",
        &[ScalarResult::U64(0)],
        ScalarResult::String("no".to_owned()),
    );
}

// ---------------------------------------------------------------------------
// Lazy programs (not strict-compatible) — interpreter only, Cranelift skips
// ---------------------------------------------------------------------------

#[test]
fn non_strict_product_interpreter_only() {
    // This creates a product (pair) — not strict-compatible.
    // Interpreter returns a non-scalar (Pair), which eval_interpreter returns
    // as an error. We just verify it doesn't panic.
    let source = r#"
fn make_pair() -> (U64, U64) ( (1, 2) )
fn main() -> U64 ( make_pair().0 )
"#;
    let result = eval_interpreter(source, "main", &[]);
    // May succeed (if interpreter handles it) or fail — either is fine here.
    // The key invariant is: if interpreter succeeds, Cranelift result == interpreter result.
    if let Ok(Some(interp_val)) = &result {
        if let Ok(Some(clift_val)) = eval_cranelift(source, "main", &[]) {
            assert_eq!(
                clift_val, *interp_val,
                "PARITY FAILURE: non_strict_product_interpreter_only"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Regression: fibonacci (recursive — not strict-lowerable, falls back to interpreter)
// ---------------------------------------------------------------------------

#[test]
fn parity_fibonacci_interpreter_only() {
    let source = r#"
fn fib(n: U64) -> U64 (
  if n == 0 ( 0 )
  else if n == 1 ( 1 )
  else ( fib(n - 1) + fib(n - 2) )
)
fn main() -> U64 ( fib(7) )
"#;
    // fib calls itself recursively — VIR lowers this as CallBody, which the
    // strict lowerer doesn't support (None). Interpreter handles it fine.
    let interp = eval_interpreter(source, "main", &[])
        .expect("interpreter should not error")
        .expect("interpreter should produce a value");
    assert_eq!(interp, ScalarResult::U64(13));

    // Cranelift cannot lower recursive calls — should return None (no parity failure).
    let clift = eval_cranelift(source, "main", &[]);
    match clift {
        Ok(None) => { /* expected: strict lowering fails, interpreter fallback is correct */ }
        Ok(Some(v)) => assert_eq!(
            v,
            ScalarResult::U64(13),
            "PARITY FAILURE: fibonacci cranelift vs interpreter"
        ),
        Err(e) => panic!("cranelift error on fibonacci: {e}"),
    }
}
