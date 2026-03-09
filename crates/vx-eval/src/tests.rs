//! Tests for the evaluator.
//!
//! Each test parses Vixen source, lowers it, and evaluates a function call,
//! asserting the result.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use std::time::Instant;

use tempfile::TempDir;
use vx_parser::SourceFile;

use crate::lower::lower_source_file;
use crate::poison::PoisonKind;
use crate::value::{BlobValue, PathValue, StructValue, TreeValue, Value};
use crate::worker::Engine;

fn eval(source: &str, entry: &str) -> Result<Value, crate::poison::PoisonError> {
    let engine = engine(source);
    engine.call(entry, vec![])
}

fn engine(source: &str) -> Engine {
    let parse = vx_parser::parse(source);
    assert_eq!(parse.errors(), []);
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
// r[verify error.poison]
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
// r[verify eval.forcing.boundary]
// r[verify eval.pending]
fn unused_pending_arg_is_not_forced() {
    let engine = engine("fn first(x: U64, y: U64) { x }");
    let pending = engine.poison_value("unused");

    let value = engine.call("first", vec![Value::U64(1), pending]).unwrap();

    assert_eq!(value, Value::U64(1));
}

#[test]
// r[verify eval.pending]
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
// r[verify eval.memoization]
// r[verify eval.memoization.pending]
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

#[test]
// r[verify eval.pending]
// r[verify eval.enter-functions]
fn structs_can_hold_pending_children_inside_the_orchestrator() {
    let engine = engine("fn pair(x: U64, y: U64) { Pair { x: x, y: y } }");
    let call = engine
        .call_unforced(
            "pair",
            vec![Value::U64(1), engine.poison_value("pending child")],
        )
        .unwrap();

    let pair = engine.force_shallow(call).unwrap();
    let Value::Struct(pair) = pair else {
        panic!("expected struct result");
    };
    assert_eq!(pair.fields[0].0.as_ref(), "x");
    assert_eq!(pair.fields[0].1, Value::U64(1));
    assert_eq!(pair.fields[1].0.as_ref(), "y");
    assert!(matches!(pair.fields[1].1, Value::Pending(_)));

    let err = engine.force_fully(Value::Struct(pair)).unwrap_err();
    assert_eq!(err.kind, PoisonKind::UserFail);
}

#[test]
// r[verify eval.forcing]
fn binary_ops_with_pending_inputs_produce_pending_results() {
    let engine = engine(
        "fn first(x: U64, y: U64) { x }
         fn add(x: U64, y: U64) { x + y }",
    );
    let left = engine
        .call_unforced(
            "first",
            vec![Value::U64(1), engine.poison_value("left pending")],
        )
        .unwrap();
    let call = engine
        .call_unforced("add", vec![left, Value::U64(2)])
        .unwrap();

    let result = engine.force_shallow(call).unwrap();
    assert!(matches!(result, Value::Pending(_)));
    assert_eq!(engine.force_fully(result).unwrap(), Value::U64(3));
}

#[test]
// r[verify eval.demand]
fn top_level_calls_build_a_pending_graph_until_forced() {
    let engine = engine("fn main() { 1 + 2 }");
    let call = engine.call_unforced("main", vec![]).unwrap();

    assert!(matches!(call, Value::Pending(_)));
    assert_eq!(engine.force_fully(call).unwrap(), Value::U64(3));
}

#[test]
// r[verify eval.control-demand]
// r[verify eval.enter-functions]
fn demand_only_reaches_the_taken_if_branch() {
    let engine = engine(
        "fn choose(flag: Bool, unused: U64) { flag }
         fn branch(flag: Bool, poison: U64) {
           if choose(flag, poison) { 1 } else { poison }
         }",
    );
    let call = engine
        .call_unforced(
            "branch",
            vec![Value::Bool(true), engine.poison_value("dead branch")],
        )
        .unwrap();

    assert!(matches!(call, Value::Pending(_)));
    assert_eq!(engine.force_fully(call).unwrap(), Value::U64(1));
}

#[test]
// r[verify error.fail]
fn fail_poisons_when_forced() {
    let err = eval(r#"fn main() { fail "boom" }"#, "main").unwrap_err();
    assert_eq!(err.kind, PoisonKind::UserFail);
    assert_eq!(err.message, "boom");
}

#[test]
// r[verify error.fail]
// r[verify eval.control-demand]
fn untaken_fail_branch_is_not_forced() {
    let value = eval(
        r#"fn main() { if true { 1 } else { fail "boom" } }"#,
        "main",
    )
    .unwrap();
    assert_eq!(value, Value::U64(1));
}

#[test]
// r[verify error.propagation]
fn multiple_independent_poisons_are_collected_deterministically() {
    let engine = engine("fn add(x: U64, y: U64) { x + y }");
    let err = engine
        .call(
            "add",
            vec![engine.poison_value("left"), engine.poison_value("right")],
        )
        .unwrap_err();

    assert_eq!(err.kind, PoisonKind::Multiple);
    assert_eq!(err.causes.len(), 2);
    assert_eq!(err.causes[0].message, "left");
    assert_eq!(err.causes[1].message, "right");
}

#[test]
// r[verify eval.demand-depth]
fn circular_recursion_poisons_with_a_circular_dependency() {
    let err = eval(
        "fn loop_forever(n: U64) { loop_forever(n) }
         fn main() { loop_forever(1) }",
        "main",
    )
    .unwrap_err();

    assert_eq!(err.kind, PoisonKind::CircularDependency);
}

#[test]
// r[verify eval.demand-depth]
fn deep_demand_chain_respects_the_configured_limit() {
    let engine = engine(
        "fn countdown(n: U64) {
           if n == 0 { 0 } else { countdown(n - 1) }
         }",
    )
    .with_max_demand_depth(4);
    let err = engine.call("countdown", vec![Value::U64(10)]).unwrap_err();

    assert_eq!(err.kind, PoisonKind::DemandDepthExceeded);
    assert!(err.message.contains("depth limit of 4"));
}

#[test]
// r[verify eval.concurrency]
fn independent_children_are_forced_concurrently() {
    let engine = engine("fn pair(x: U64, y: U64) { Pair { left: x, right: y } }");
    let call = engine
        .call_unforced(
            "pair",
            vec![
                engine.sleep_value(200, Value::U64(1)),
                engine.sleep_value(200, Value::U64(2)),
            ],
        )
        .unwrap();

    let start = Instant::now();
    let value = engine.force_fully(call).unwrap();
    let elapsed = start.elapsed();

    assert_eq!(
        value,
        Value::Struct(StructValue {
            type_name: Arc::from("Pair"),
            fields: Arc::from([
                (Arc::from("left"), Value::U64(1)),
                (Arc::from("right"), Value::U64(2)),
            ]),
        })
    );
    assert!(elapsed.as_millis() < 350, "elapsed = {elapsed:?}");
}

#[test]
// r[verify fn.identity]
// r[verify eval.memoization]
fn transitive_callee_changes_update_the_caller_identity() {
    let source_a = "fn helper() { 1 } fn wrapper() { helper() }";
    let source_b = "fn helper() { 2 } fn wrapper() { helper() }";

    let key_a = engine(source_a).function_key("wrapper").unwrap();
    let key_b = engine(source_b).function_key("wrapper").unwrap();

    assert_ne!(key_a, key_b);
}

#[test]
// r[verify fn.identity.overloads]
fn all_visible_overloads_contribute_to_caller_identity() {
    let source_a = "fn helper(x: U64) { 1 }
                    fn helper(x: Bool) { 2 }
                    fn wrapper() { helper(1) }";
    let source_b = "fn helper(x: U64) { 1 }
                    fn helper(x: Bool) { 99 }
                    fn wrapper() { helper(1) }";

    let key_a = engine(source_a).function_key("wrapper").unwrap();
    let key_b = engine(source_b).function_key("wrapper").unwrap();

    assert_ne!(key_a, key_b);
}

#[test]
// r[verify fn.identity.stability]
fn comments_and_whitespace_do_not_change_function_identity() {
    let source_a = "fn helper() { 1 }\nfn wrapper() { helper() }";
    let source_b = r#"
// wrapper should keep the same identity
fn helper() {
  // this comment is ignored by lowering
  1
}

fn wrapper() {
  helper()
}
"#;

    let key_a = engine(source_a).function_key("wrapper").unwrap();
    let key_b = engine(source_b).function_key("wrapper").unwrap();

    assert_eq!(key_a, key_b);
}

#[test]
// r[verify fn.identity.scc]
fn mutually_recursive_functions_share_one_code_identity() {
    let source = "fn even(n: U64) { if n == 0 { true } else { odd(n - 1) } }
                  fn odd(n: U64) { if n == 0 { false } else { even(n - 1) } }";
    let engine = engine(source);

    assert_eq!(
        engine.function_key("even").unwrap(),
        engine.function_key("odd").unwrap()
    );
}

#[test]
// r[verify store.path]
fn path_values_join_with_normalized_slashes() {
    let engine = engine(r#"fn main(root: Path) { root / "src" / "./lib.rs" }"#);
    let value = engine
        .call("main", vec![Value::Path(PathValue::new("/workspace//"))])
        .unwrap();

    assert_eq!(value, Value::Path(PathValue::new("/workspace/src/lib.rs")));
}

#[test]
// r[verify store.blob]
// r[verify store.blob.to-string]
fn blob_from_string_round_trips_text() {
    let value = eval(
        r#"fn main() { Blob.from_string("hello").to_string() }"#,
        "main",
    )
    .unwrap();
    assert_eq!(value, Value::String(Arc::from("hello")));
}

#[test]
// r[verify store.tree]
// r[verify store.tree.file]
fn tree_of_and_file_only_force_the_selected_entry() {
    let engine = engine(
        r#"fn main(other: Blob) {
               Tree.of(%{
                 "a.txt" => Blob.from_string("alpha"),
                 "b.txt" => other,
               }).file("a.txt").to_string()
             }"#,
    );
    let value = engine
        .call("main", vec![engine.poison_value("unused tree entry")])
        .unwrap();

    assert_eq!(value, Value::String(Arc::from("alpha")));
}

#[test]
// r[verify store.tree]
// r[verify store.tree.file]
// r[verify store.blob.to-string]
fn fs_backed_trees_read_files_on_demand() {
    let tempdir = TempDir::new().unwrap();
    let file_path = tempdir.path().join("hello.txt");
    fs::write(&file_path, "hello from fs").unwrap();

    let engine = engine(r#"fn main(tree: Tree) { tree.file("hello.txt").to_string() }"#);
    let tree = Value::Tree(TreeValue::FsDir(PathValue::new(
        tempdir.path().to_string_lossy(),
    )));
    let value = engine.call("main", vec![tree]).unwrap();

    assert_eq!(value, Value::String(Arc::from("hello from fs")));
}

#[test]
// r[verify eval.entry]
fn call_entry_passes_workspace_as_a_path() {
    let engine = engine(
        r#"fn build(workspace: Path, args: Array[String], env: Map[String, String]) {
               Tree.of(%{
                 "workspace.txt" => Blob.from_string(workspace / "src" / "main.rs"),
               })
             }"#,
    );
    let value = engine
        .call_entry(
            "build",
            "/repo",
            vec!["build".into()],
            BTreeMap::from([(String::from("MODE"), String::from("debug"))]),
        )
        .unwrap();

    let Value::Tree(TreeValue::Inline(entries)) = value else {
        panic!("expected tree result");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0.as_ref(), "workspace.txt");
    let Value::Blob(blob) = &entries[0].1 else {
        panic!("expected blob entry");
    };
    assert_eq!(blob, &BlobValue::from_string("/repo/src/main.rs"));
}

#[test]
fn exec_runs_a_local_process_with_keyword_args_and_snapshots_outputs() {
    let tempdir = TempDir::new().unwrap();
    fs::write(tempdir.path().join("input.txt"), "hello from workspace").unwrap();

    let engine = engine(
        r#"fn observe(process: Process, tree: Tree) {
               Tree.of(%{
                 "stdout.txt" => process.stdout,
                 "output.txt" => tree.file("out/output.txt"),
               })
             }

             fn build(workspace: Path) {
               exec(
                 ["/bin/sh", "-c", "cat src/input.txt > out/output.txt; printf done"];
                 mounts: %{ "src" => workspace },
                 writable: ["out"],
                 observer: observe,
               )
             }"#,
    );
    let value = engine
        .call(
            "build",
            vec![Value::Path(PathValue::new(
                tempdir.path().to_string_lossy(),
            ))],
        )
        .unwrap();

    let Value::Tree(TreeValue::Inline(entries)) = value else {
        panic!("expected tree result");
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0.as_ref(), "stdout.txt");
    assert_eq!(entries[0].1, Value::Blob(BlobValue::from_string("done")));
    assert_eq!(entries[1].0.as_ref(), "output.txt");
    assert_eq!(
        entries[1].1,
        Value::Blob(BlobValue::from_string("hello from workspace"))
    );
}
