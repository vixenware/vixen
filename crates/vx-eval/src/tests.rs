//! Tests for the evaluator.
//!
//! Each test parses Vixen source, lowers it, and evaluates a function call,
//! asserting the result.

use std::sync::Arc;

use vx_parser::SourceFile;

use crate::lower::lower_source_file;
use crate::poison::PoisonKind;
use crate::value::{StructValue, Value};
use crate::worker::Engine;

fn eval(source: &str, entry: &str) -> Result<Value, crate::poison::PoisonError> {
    let engine = engine(source);
    engine.call(entry, vec![])
}

fn engine(source: &str) -> Engine {
    let parse = vx_parser::parse(source);
    let root = SourceFile::cast_root(parse.syntax());
    let module = lower_source_file(&root);
    Engine::from_module(&module)
}

#[test]
fn integer_literal() {
    let val = eval("fn main() { 42 }", "main").unwrap();
    assert_eq!(val, Value::U64(42));
}

#[test]
fn addition() {
    let val = eval("fn main() { 1 + 2 }", "main").unwrap();
    assert_eq!(val, Value::U64(3));
}

#[test]
fn nested_arithmetic() {
    let val = eval("fn main() { 2 + 3 * 4 }", "main").unwrap();
    // Parser handles precedence: 2 + (3 * 4) = 14
    assert_eq!(val, Value::U64(14));
}

#[test]
fn boolean_literal() {
    assert_eq!(
        eval("fn main() { true }", "main").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        eval("fn main() { false }", "main").unwrap(),
        Value::Bool(false)
    );
}

#[test]
fn string_literal() {
    let val = eval(r#"fn main() { "hello" }"#, "main").unwrap();
    assert_eq!(val, Value::String(Arc::from("hello")));
}

#[test]
fn let_binding() {
    let val = eval("fn main() { let x = 10; let y = 20; x + y }", "main").unwrap();
    assert_eq!(val, Value::U64(30));
}

#[test]
fn if_true_branch() {
    let val = eval("fn main() { if true { 1 } else { 2 } }", "main").unwrap();
    assert_eq!(val, Value::U64(1));
}

#[test]
fn if_false_branch() {
    let val = eval("fn main() { if false { 1 } else { 2 } }", "main").unwrap();
    assert_eq!(val, Value::U64(2));
}

#[test]
fn if_with_condition() {
    let val = eval(
        "fn main() { let x = 5; if x > 3 { 100 } else { 0 } }",
        "main",
    )
    .unwrap();
    assert_eq!(val, Value::U64(100));
}

#[test]
fn function_call() {
    let val = eval(
        "fn double(x: U64) { x + x }
         fn main() { double(21) }",
        "main",
    )
    .unwrap();
    assert_eq!(val, Value::U64(42));
}

#[test]
fn nested_function_calls() {
    let val = eval(
        "fn add(a: U64, b: U64) { a + b }
         fn main() { add(add(1, 2), add(3, 4)) }",
        "main",
    )
    .unwrap();
    assert_eq!(val, Value::U64(10));
}

#[test]
fn recursive_function() {
    let val = eval(
        "fn factorial(n: U64) {
           if n <= 1 { 1 } else { n * factorial(n - 1) }
         }
         fn main() { factorial(5) }",
        "main",
    )
    .unwrap();
    assert_eq!(val, Value::U64(120));
}

#[test]
fn struct_literal_and_field_access() {
    let val = eval(
        "fn main() {
           let p = Point { x: 10, y: 20 };
           p.x + p.y
         }",
        "main",
    )
    .unwrap();
    assert_eq!(val, Value::U64(30));
}

#[test]
fn block_expression() {
    let val = eval(
        "fn main() {
           let result = {
             let a = 3;
             let b = 4;
             a * b
           };
           result + 1
         }",
        "main",
    )
    .unwrap();
    assert_eq!(val, Value::U64(13));
}

#[test]
fn comparison_operators() {
    assert_eq!(
        eval("fn main() { 1 == 1 }", "main").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        eval("fn main() { 1 != 2 }", "main").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        eval("fn main() { 1 < 2 }", "main").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        eval("fn main() { 2 >= 2 }", "main").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        eval("fn main() { 3 > 2 }", "main").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        eval("fn main() { 1 <= 1 }", "main").unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn logical_operators() {
    assert_eq!(
        eval("fn main() { true && false }", "main").unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        eval("fn main() { true || false }", "main").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        eval("fn main() { !true }", "main").unwrap(),
        Value::Bool(false)
    );
}

#[test]
fn division_by_zero_poisons() {
    let err = eval("fn main() { 10 / 0 }", "main").unwrap_err();
    assert_eq!(err.kind, PoisonKind::ArithmeticError);
}

#[test]
fn type_error_poisons() {
    let err = eval(r#"fn main() { 1 + "hello" }"#, "main").unwrap_err();
    assert_eq!(err.kind, PoisonKind::TypeError);
}

#[test]
fn unresolved_name_poisons() {
    let err = eval("fn main() { nonexistent }", "main").unwrap_err();
    assert_eq!(err.kind, PoisonKind::UnresolvedName);
}

#[test]
fn empty_function_returns_unit() {
    let val = eval("fn main() { }", "main").unwrap();
    assert_eq!(val, Value::Unit);
}

#[test]
fn function_as_value() {
    let val = eval(
        "fn add(a: U64, b: U64) { a + b }
         fn apply(f: Fn(U64, U64) -> U64, x: U64, y: U64) { f(x, y) }
         fn main() { apply(add, 3, 4) }",
        "main",
    )
    .unwrap();
    assert_eq!(val, Value::U64(7));
}

#[test]
fn shadowing() {
    let val = eval(
        "fn main() {
           let x = 1;
           let x = x + 10;
           x
         }",
        "main",
    )
    .unwrap();
    assert_eq!(val, Value::U64(11));
}

#[test]
fn early_return() {
    let val = eval(
        "fn check(n: U64) {
           if n > 10 { return 999 } else { () };
           n + 1
         }
         fn main() { check(20) }",
        "main",
    )
    .unwrap();
    assert_eq!(val, Value::U64(999));
}

#[test]
fn fibonacci() {
    let val = eval(
        "fn fib(n: U64) {
           if n <= 1 { n } else { fib(n - 1) + fib(n - 2) }
         }
         fn main() { fib(10) }",
        "main",
    )
    .unwrap();
    assert_eq!(val, Value::U64(55));
}

#[test]
fn unused_pending_arg_is_not_forced() {
    let engine = engine("fn first(x: U64, y: U64) { x }");
    let pending = engine.poison_value("unused");

    let value = engine.call("first", vec![Value::U64(1), pending]).unwrap();

    assert_eq!(value, Value::U64(1));
}

#[test]
fn field_access_does_not_force_other_fields() {
    let engine = engine("fn get_x(pair: Pair) { pair.x }");
    let value = engine
        .call(
            "get_x",
            vec![Value::Struct(StructValue {
                type_name: Arc::from("Pair"),
                fields: Arc::from([
                    (Arc::from("x"), Value::U64(7)),
                    (Arc::from("y"), engine.poison_value("unused field")),
                ]),
            })],
        )
        .unwrap();

    assert_eq!(value, Value::U64(7));
}

#[test]
fn resolved_calls_are_memoized() {
    let engine = engine("fn double(x: U64) { x + x }");

    assert_eq!(engine.memoized_call_count(), 0);
    assert_eq!(
        engine.call("double", vec![Value::U64(21)]).unwrap(),
        Value::U64(42)
    );
    assert_eq!(engine.memoized_call_count(), 1);

    assert_eq!(
        engine.call("double", vec![Value::U64(21)]).unwrap(),
        Value::U64(42)
    );
    assert_eq!(engine.memoized_call_count(), 1);
}
