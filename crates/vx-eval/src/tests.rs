//! Tests for the evaluator.
//!
//! Each test parses Vixen source, lowers it, and evaluates a function call,
//! asserting the result.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use blake3::hash as blake3_hash;
use indoc::indoc;
use tempfile::TempDir;
use vx_parser::{AstNode, Pattern as AstPattern, SourceFile, Stmt as AstStmt};
use vx_store_proto::{Blake3Hash, BlobRef, ChunkHash, NodeHash};

use crate::exec::{AcquireRequest, ExecArgValue, ImportRequest, RenderRequest, StabilizeRequest};
use crate::instr::{Block, FnDef};
use crate::lower::{lower_source_file, lower_source_file_with_source_id};
use crate::node::{NodeId, NodeJob, TemplateKind, TemplatePartValue};
use crate::poison::{PoisonError, PoisonKind};
use crate::transport::TransportValue;
use crate::value::{
    BlobValue, BuiltinMethod, BuiltinValue, DiscoveredValue, ExecPathValue, FunctionValue,
    HostObservation, HostValue, MapValue, PartialValue, PathValue, StructValue, TreeValue, Value,
};
use crate::worker::Engine;

fn eval(source: &str, entry: &str) -> Result<Value, crate::poison::PoisonError> {
    let engine = engine(source);
    engine.call(entry, vec![])
}

fn engine(source: &str) -> Engine {
    engine_with_source_id(source, "<test>")
}

fn engine_with_source_id(source: &str, source_id: &str) -> Engine {
    let parse = vx_parser::parse(source);
    assert_eq!(parse.errors(), []);
    let root = SourceFile::cast_root(parse.syntax());
    let module = lower_source_file_with_source_id(&root, source_id);
    Engine::from_module(&module)
}

static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

const LUA_SKETCH_TAIL: &str = "  let objects = (all_lib_sources.map(compile)).merge();\n  let liblua = archive(objects);\n\n  let lua_exe = link(Tree [compile @lua.c, liblua]);\n\n  Tree %{\n    @bin/lua => lua_exe.file(@bin/lua).executable(),\n    @lib/liblua.a => liblua.file(@liblua.a),\n  }\n";

fn collect_tree_leaf_paths(value: &Value) -> Vec<String> {
    fn walk_value(value: &Value, prefix: &str, out: &mut Vec<String>) {
        match value {
            Value::Tree(TreeValue::Inline(entries)) => {
                for (name, child) in entries.iter() {
                    let next = if prefix.is_empty() {
                        name.to_string()
                    } else {
                        format!("{prefix}/{name}")
                    };
                    walk_value(child, &next, out);
                }
            }
            Value::Tree(TreeValue::FsDir(root)) => walk_fs_dir(root.as_str(), prefix, out),
            Value::Blob(_) => out.push(prefix.to_string()),
            other => panic!("expected tree or blob while walking leaves, got {other:?}"),
        }
    }

    fn walk_fs_dir(root: &str, prefix: &str, out: &mut Vec<String>) {
        fn walk(
            current: &std::path::Path,
            root: &std::path::Path,
            prefix: &str,
            out: &mut Vec<String>,
        ) {
            for entry in fs::read_dir(current).expect("read_dir") {
                let entry = entry.expect("dir entry");
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, root, prefix, out);
                } else {
                    let relative = path
                        .strip_prefix(root)
                        .expect("path under root")
                        .to_string_lossy()
                        .replace('\\', "/");
                    let full = if prefix.is_empty() {
                        relative
                    } else {
                        format!("{prefix}/{relative}")
                    };
                    out.push(full);
                }
            }
        }

        let root = std::path::Path::new(root);
        walk(root, root, prefix, out);
    }

    let mut out = Vec::new();
    walk_value(value, "", &mut out);
    out.sort();
    out.dedup();
    out
}

fn inline_tree_with_blob(path: &str, contents: &str) -> Value {
    let mut value = Value::Blob(BlobValue::from_string(contents));
    for segment in path.split('/').rev() {
        value = Value::Tree(TreeValue::Inline(Arc::from([(Arc::from(segment), value)])));
    }
    value
}

fn treeish_type_name(value: &Value) -> String {
    match value {
        Value::Tree(_) => "Tree".into(),
        Value::Pending(_) => "Pending".into(),
        Value::Discovered(discovered) => {
            format!(
                "Discovered({})",
                treeish_type_name(discovered.value.as_ref())
            )
        }
        other => other.type_name().to_string(),
    }
}

fn is_treeish(value: &Value) -> bool {
    match value {
        Value::Tree(_) | Value::Pending(_) => true,
        Value::Discovered(discovered) => is_treeish(discovered.value.as_ref()),
        _ => false,
    }
}

fn sorted_node_ids(ids: Vec<NodeId>) -> Vec<u64> {
    let mut ids = ids.into_iter().map(|id| id.0).collect::<Vec<_>>();
    ids.sort_unstable();
    ids
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
fn array_concat() {
    let val = eval("fn main() { [1, 2] ++ [3, 4] }", "main").unwrap();
    assert_eq!(
        val,
        Value::Array(Arc::from([
            Value::U64(1),
            Value::U64(2),
            Value::U64(3),
            Value::U64(4),
        ]))
    );
}

#[test]
fn string_methods_work() {
    let value = eval(
        r#"
fn main() {
  %{
    "starts" => "arm64-apple-darwin".starts_with("arm64"),
    "contains" => "x86_64-unknown-linux-gnu".contains("linux"),
    "split" => "a,b,c".split(","),
    "lines" => "a\nb\n".lines(),
    "trim" => "  hi \n".trim(),
    "lower" => "PNG_ARM_NEON".to_lower(),
  }
}
"#,
        "main",
    )
    .unwrap();

    let Value::Map(map) = value else {
        panic!("expected map result");
    };

    let get = |name: &str| {
        map.entries
            .iter()
            .find_map(|(key, value)| match key {
                Value::String(key) if key.as_ref() == name => Some(value.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing key {name}"))
    };

    assert_eq!(get("starts"), Value::Bool(true));
    assert_eq!(get("contains"), Value::Bool(true));
    assert_eq!(
        get("split"),
        Value::Array(Arc::from([
            Value::String(Arc::from("a")),
            Value::String(Arc::from("b")),
            Value::String(Arc::from("c")),
        ]))
    );
    assert_eq!(
        get("lines"),
        Value::Array(Arc::from([
            Value::String(Arc::from("a")),
            Value::String(Arc::from("b")),
        ]))
    );
    assert_eq!(get("trim"), Value::String(Arc::from("hi")));
    assert_eq!(get("lower"), Value::String(Arc::from("png_arm_neon")));
}

#[test]
fn pending_string_contains_resolves() {
    let engine = engine(indoc! {r#"
        fn main() {
          let target =
            exec ["tool", "-dumpmachine"] where {
              observer: (proc, _vfs) => proc.stdout.collect().utf8().trim(),
            };
          if target.contains("x86_64") (
            "x86_64"
          ) else (
            "other"
          )
        }
    "#})
    .with_exec_handler(Arc::new(|request| {
        assert_eq!(
            request.args.as_slice(),
            &[
                ExecArgValue::Literal("tool".to_string()),
                ExecArgValue::Literal("-dumpmachine".to_string()),
            ]
        );
        Ok(Value::String(Arc::from("x86_64-unknown-linux-gnu")))
    }));

    assert_eq!(
        engine.call("main", vec![]).unwrap(),
        Value::String(Arc::from("x86_64"))
    );
}

#[test]
fn pending_array_map_receiver_resolves() {
    let engine = engine(indoc! {r#"
        fn main() {
          let target =
            exec ["tool", "-dumpmachine"] where {
              observer: (proc, _vfs) => proc.stdout.collect().utf8().trim(),
            };
          let defines =
            if target.contains("x86_64") (
              ["PNG_INTEL_SSE_OPT=1"]
            ) else (
              []
            );
          defines.map((define) => `-D${define}`)
        }
    "#})
    .with_exec_handler(Arc::new(|request| {
        assert_eq!(
            request.args.as_slice(),
            &[
                ExecArgValue::Literal("tool".to_string()),
                ExecArgValue::Literal("-dumpmachine".to_string()),
            ]
        );
        Ok(Value::String(Arc::from("x86_64-unknown-linux-gnu")))
    }));

    assert_eq!(
        engine.call("main", vec![]).unwrap(),
        Value::Array(Arc::from([Value::String(Arc::from(
            "-DPNG_INTEL_SSE_OPT=1",
        ))]))
    );
}

#[test]
fn pending_kwarg_array_map_receiver_resolves() {
    let engine = engine(indoc! {r#"
        fn render_defines() where { defines: Array[String] = [] } {
          defines.map((define) => `-D${define}`)
        }

        fn main() {
          let target =
            exec ["tool", "-dumpmachine"] where {
              observer: (proc, _vfs) => proc.stdout.collect().utf8().trim(),
            };
          let defines =
            if target.contains("x86_64") (
              ["PNG_INTEL_SSE_OPT=1"]
            ) else (
              []
            );
          render_defines() where { defines }
        }
    "#})
    .with_exec_handler(Arc::new(|request| {
        assert_eq!(
            request.args.as_slice(),
            &[
                ExecArgValue::Literal("tool".to_string()),
                ExecArgValue::Literal("-dumpmachine".to_string()),
            ]
        );
        Ok(Value::String(Arc::from("x86_64-unknown-linux-gnu")))
    }));

    assert_eq!(
        engine.call("main", vec![]).unwrap(),
        Value::Array(Arc::from([Value::String(Arc::from(
            "-DPNG_INTEL_SSE_OPT=1",
        ))]))
    );
}

#[test]
fn pending_kwarg_array_map_spread_into_exec_resolves() {
    let engine = engine(indoc! {r#"
        fn observe(_proc: Process, tree: Tree) {
          tree
        }

        fn compile() where { defines: Array[String] = [] } {
          let cexec = partial exec where { observer: observe };
          cexec [
            "tool",
            ..defines.map((define) => `-D${define}`),
            "-o",
            @out/result,
          ]
        }

        fn main() {
          let target_triple =
            exec ["tool", "-dumpmachine"] where {
              observer: (proc, _vfs) => proc.stdout.collect().utf8().trim(),
            };
          let target =
            if target_triple.contains("x86_64") (
              "x86_64"
            ) else (
              "generic"
            );
          let defines =
            match target (
              "x86_64" => ["PNG_INTEL_SSE_OPT=1"],
              _ => [],
            );
          compile() where { defines }
        }
    "#})
    .with_exec_handler(Arc::new(|request| {
        if matches!(
            request.args.as_slice(),
            [
                ExecArgValue::Literal(program),
                ExecArgValue::Literal(flag),
            ] if program == "tool" && flag == "-dumpmachine"
        ) {
            return Ok(Value::String(Arc::from("x86_64-unknown-linux-gnu")));
        }

        Ok(inline_tree_with_blob("out/result", "artifact"))
    }));

    assert_eq!(
        collect_tree_leaf_paths(&engine.call("main", vec![]).unwrap()),
        vec!["out/result".to_string()]
    );
}

#[test]
fn pending_kwarg_array_map_spread_shared_across_exec_callbacks_resolves() {
    let engine = engine(indoc! {r#"
        fn observe(_proc: Process, tree: Tree) {
          tree
        }

        fn compile_all(files: Array[Path]) where { defines: Array[String] = [] } {
          let cexec = partial exec where { observer: observe };
          files
            .map((file) =>
              cexec [
                "tool",
                ..defines.map((define) => `-D${define}`),
                "-c",
                file,
                "-o",
                @(out/{file}.o),
              ]
            )
            .merge()
        }

        fn main() {
          let target_triple =
            exec ["tool", "-dumpmachine"] where {
              observer: (proc, _vfs) => proc.stdout.collect().utf8().trim(),
            };
          let target =
            if target_triple.contains("x86_64") (
              "x86_64"
            ) else (
              "generic"
            );
          let defines =
            match target (
              "x86_64" => ["PNG_INTEL_SSE_OPT=1"],
              _ => [],
            );
          compile_all([@a.c, @b.c]) where { defines }
        }
    "#})
    .with_exec_handler(Arc::new(|request| {
        if matches!(
            request.args.as_slice(),
            [
                ExecArgValue::Literal(program),
                ExecArgValue::Literal(flag),
            ] if program == "tool" && flag == "-dumpmachine"
        ) {
            return Ok(Value::String(Arc::from("x86_64-unknown-linux-gnu")));
        }

        let output = request
            .args
            .iter()
            .zip(request.args.iter().skip(1))
            .find_map(|(arg, next)| match (arg, next) {
                (ExecArgValue::Literal(flag), ExecArgValue::Path(path)) if flag == "-o" => {
                    Some(path.clone())
                }
                _ => None,
            })
            .expect("expected -o output path");
        let file = output
            .as_str()
            .rsplit('/')
            .next()
            .expect("output path should have a filename")
            .to_string();
        Ok(Value::Tree(TreeValue::Inline(Arc::from([(
            Arc::from("out"),
            Value::Tree(TreeValue::Inline(Arc::from([(
                Arc::from(file),
                Value::Blob(BlobValue::from_string("ok")),
            )]))),
        )]))))
    }));

    assert_eq!(
        collect_tree_leaf_paths(&engine.call("main", vec![]).unwrap()),
        vec!["out/a.c.o".to_string(), "out/b.c.o".to_string()]
    );
}

#[test]
fn match_expression_selects_literal_arms() {
    let source = r#"
fn main(target: String) {
  match target (
    "linux-x86_64" => "intel",
    _ => "generic",
  )
}
"#;
    let engine = engine(source);
    assert_eq!(
        engine
            .call("main", vec![Value::String(Arc::from("linux-x86_64"))])
            .unwrap(),
        Value::String(Arc::from("intel"))
    );
    assert_eq!(
        engine
            .call("main", vec![Value::String(Arc::from("linux-riscv64"))])
            .unwrap(),
        Value::String(Arc::from("generic"))
    );
}

#[test]
fn match_expression_with_binding_pattern_returns_scrutinee() {
    let source = r#"
fn main(target: String) {
  match target (
    value => value,
  )
}
"#;
    let engine = engine(source);
    let value = engine
        .call("main", vec![Value::String(Arc::from("linux-x86_64"))])
        .unwrap();
    assert_eq!(value, Value::String(Arc::from("linux-x86_64")));
}

#[test]
fn ordinary_functions_accept_keyword_args_via_where_records() {
    let source = r#"
fn cc_probe(source: String) where {
  flags: Array[String] = [],
  link: Bool = false,
} {
  if link (
    [source] ++ flags ++ ["link"]
  ) else (
    [source] ++ flags ++ ["compile"]
  )
}

fn main() {
  let common = { flags: ["-Wall"] };
  cc_probe("probe.c") where {
    ..common,
    link: true,
  }
}
"#;
    let value = eval(source, "main").unwrap();
    assert_eq!(
        value,
        Value::Array(Arc::from([
            Value::String(Arc::from("probe.c")),
            Value::String(Arc::from("-Wall")),
            Value::String(Arc::from("link")),
        ]))
    );
}

#[test]
fn keyword_defaults_can_reference_prior_bindings() {
    let source = r#"
fn banner(name: String) where {
  prefix: String = name,
  suffix: String = `${prefix}!`,
} {
  `${prefix}:${suffix}`
}

fn main() {
  banner("png")
}
"#;
    let value = eval(source, "main").unwrap();
    assert_eq!(value, Value::String(Arc::from("png:png!")));
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
fn option_unwrap_or_returns_the_inner_value() {
    let val = eval(r#"fn main() { (Some { value: 7 }).unwrap_or(9) }"#, "main").unwrap();
    assert_eq!(val, Value::U64(7));
}

#[test]
fn option_unwrap_or_returns_the_default_for_none() {
    let val = eval(r#"fn main() { None.unwrap_or(9) }"#, "main").unwrap();
    assert_eq!(val, Value::U64(9));
}

#[test]
fn option_unwrap_or_does_not_force_the_default_when_some() {
    let val = eval(
        r#"fn main() { (Some { value: 7 }).unwrap_or((fail "should not be forced")) }"#,
        "main",
    )
    .unwrap();
    assert_eq!(val, Value::U64(7));
}

#[test]
fn fallible_suffix_invokes_zero_arg_callables() {
    let value = eval(r#"fn main() { Blob("hello").utf8? }"#, "main").unwrap();
    assert_eq!(
        value,
        Value::Struct(StructValue {
            type_name: Arc::from("Some"),
            fields: Arc::from([(Arc::from("value"), Value::String(Arc::from("hello")),)]),
        })
    );
}

#[test]
// r[verify expr.literal.interpolation]
fn interpolated_string_literal() {
    let val = eval(r#"fn main() { `hello ${"world"}` }"#, "main").unwrap();
    assert_eq!(val, Value::String(Arc::from("hello world")));
}

#[test]
fn path_literal_normalizes_slashes() {
    let engine = engine(r#"fn main(root: Path) { @({root}/./artifact.o) }"#);
    let val = engine
        .call("main", vec![Value::Path(PathValue::new("out//"))])
        .unwrap();
    assert_eq!(val, Value::Path(PathValue::new("out/artifact.o")));
}

#[test]
fn symbol_keyed_toolchain_pipeline_supports_method_values_and_destructuring() {
    let source = r#"
fn build(host: Host) (
  struct Tool {
    path: String,
    hash: Blob,
  }

  namespace Tool {
    fn to_string(self) -> String self.path;
  }

  let toolchain = ([:sh, :cc] |> _.to_map(host.which) |> _.map_values((path) => Tool { path, hash: host.hash(path) }));

  let { sh, cc } = toolchain;
  [sh.to_string(), cc.to_string()]
)
"#;
    let parse = vx_parser::parse(source);
    assert_eq!(parse.errors(), []);
    let root = SourceFile::cast_root(parse.syntax());
    let build_fn = match root.items().find(|item| match item {
        vx_parser::Item::FnDecl(node) => node.name().is_some_and(|name| name.text() == "build"),
        _ => false,
    }) {
        Some(vx_parser::Item::FnDecl(node)) => node,
        other => panic!("expected build fn item, got {other:?}"),
    };
    let parsed_body = build_fn.body().expect("build body");
    let parsed_stmts = parsed_body.statements().collect::<Vec<_>>();
    assert_eq!(parsed_stmts.len(), 5);
    let toolchain_let = match &parsed_stmts[2] {
        AstStmt::Let(stmt) => stmt,
        other => panic!("expected toolchain let stmt, got {other:?}"),
    };
    assert!(
        matches!(toolchain_let.pattern(), Some(AstPattern::Binding(_))),
        "expected binding pattern in toolchain let: {:?}",
        toolchain_let.pattern()
    );
    let destructuring_let = match &parsed_stmts[3] {
        AstStmt::Let(stmt) => stmt,
        other => panic!("expected destructuring let stmt, got {other:?}"),
    };
    assert!(
        matches!(destructuring_let.pattern(), Some(AstPattern::Struct(_))),
        "expected struct destructuring pattern in final let: {:?}",
        destructuring_let.pattern()
    );
    let module = lower_source_file(&root);
    assert!(
        module
            .functions
            .iter()
            .any(|def| def.name == "Tool.to_string"),
        "expected lowered namespace method in module functions: {:?}",
        module
            .functions
            .iter()
            .map(|def| def.name.as_str())
            .collect::<Vec<_>>()
    );
    let build = module
        .functions
        .iter()
        .find(|def| def.name == "build")
        .expect("build fn");
    assert!(
        matches!(
            build.body.stmts.as_slice(),
            [
                crate::instr::Stmt::StructDecl(_),
                crate::instr::Stmt::Let { name, .. },
                crate::instr::Stmt::LetPattern { .. },
                crate::instr::Stmt::Expr(_)
            ]
            if name == "toolchain"
        ),
        "unexpected lowered build body: {:?}",
        build.body.stmts
    );

    let engine = Engine::from_module(&module);

    let value = engine.call("build", vec![Value::Host(HostValue)]).unwrap();
    let Value::Array(items) = value else {
        panic!("expected array result");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], Value::String(Arc::from("/bin/sh")));
    assert_eq!(items[1], Value::String(Arc::from("/usr/bin/cc")));
}

#[test]
fn lua_sketch_runs_with_discovered_tools_and_fake_sources() {
    let sketch_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/lua.vx");
    let source = fs::read_to_string(&sketch_path).expect("read lua sketch");
    let engine = engine(&source);

    let tempdir = TempDir::new().unwrap();
    let workspace = tempdir.path();
    let src_dir = workspace.join("zoo/lua-src");
    fs::create_dir_all(&src_dir).unwrap();

    let sources = [
        "lapi.c",
        "lcode.c",
        "lctype.c",
        "ldebug.c",
        "ldo.c",
        "ldump.c",
        "lfunc.c",
        "lgc.c",
        "llex.c",
        "lmem.c",
        "lobject.c",
        "lopcodes.c",
        "lparser.c",
        "lstate.c",
        "lstring.c",
        "ltable.c",
        "ltm.c",
        "lundump.c",
        "lvm.c",
        "lzio.c",
        "lauxlib.c",
        "lbaselib.c",
        "ldblib.c",
        "liolib.c",
        "lmathlib.c",
        "loslib.c",
        "ltablib.c",
        "lstrlib.c",
        "lutf8lib.c",
        "loadlib.c",
        "lcorolib.c",
        "linit.c",
        "lua.c",
    ];
    for file in sources {
        let stem = file.trim_end_matches(".c");
        let contents = if file == "lua.c" {
            "int main(void) { return 0; }\n".to_string()
        } else {
            format!("int {stem}_dummy = 0;\n")
        };
        fs::write(src_dir.join(file), contents).unwrap();
    }

    let tools_dir = workspace.join("tools");
    fs::create_dir_all(&tools_dir).unwrap();
    let cc_script = tools_dir.join("cc");
    let libtool_script = tools_dir.join("libtool");
    fs::write(
        &cc_script,
        r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    shift
    out="$1"
  fi
  shift
done
mkdir -p "$(dirname "$out")"
printf 'artifact\n' > "$out"
"#,
    )
    .unwrap();
    fs::write(
        &libtool_script,
        r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    shift
    out="$1"
  fi
  shift
done
mkdir -p "$(dirname "$out")"
printf 'archive\n' > "$out"
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for script in [&cc_script, &libtool_script] {
            let mut perms = fs::metadata(script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(script, perms).unwrap();
        }
    }

    let _env_guard = ENV_MUTEX.lock().unwrap();
    let old_path = std::env::var_os("PATH");
    let mut path_entries = vec![tools_dir.as_os_str().to_os_string()];
    if let Some(existing) = &old_path {
        path_entries.extend(std::env::split_paths(existing).map(|p| p.into_os_string()));
    }
    let joined = std::env::join_paths(path_entries).unwrap();
    unsafe {
        std::env::set_var("PATH", &joined);
    }

    let result = engine.call(
        "build",
        vec![
            Value::Path(PathValue::new(workspace.to_string_lossy())),
            Value::Array(Arc::from([Value::String(Arc::from("build"))])),
            Value::Map(MapValue {
                entries: Arc::from([]),
            }),
        ],
    );

    match old_path {
        Some(path) => unsafe {
            std::env::set_var("PATH", path);
        },
        None => unsafe {
            std::env::remove_var("PATH");
        },
    }

    let value = result.unwrap();
    let leafs = collect_tree_leaf_paths(&value);
    assert!(
        leafs.iter().any(|path| path.ends_with("/lua")),
        "expected a lua executable leaf, got {leafs:?}"
    );
    assert!(
        leafs.iter().any(|path| path.ends_with("liblua.a")),
        "expected a liblua archive leaf, got {leafs:?}"
    );
}

#[test]
fn lua_sketch_compile_single_file_returns_tree() {
    let sketch_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/lua.vx");
    let source = fs::read_to_string(&sketch_path).expect("read lua sketch");
    let source = source.replace(LUA_SKETCH_TAIL, "  compile(@lapi.c)\n");
    let engine = engine(&source);

    let tempdir = TempDir::new().unwrap();
    let workspace = tempdir.path();
    let src_dir = workspace.join("zoo/lua-src");
    fs::create_dir_all(&src_dir).unwrap();

    let files = ["lapi.c"];
    for file in files {
        let stem = file.trim_end_matches(".c");
        fs::write(src_dir.join(file), format!("int {stem}_dummy = 0;\n")).unwrap();
    }

    let tools_dir = workspace.join("tools");
    fs::create_dir_all(&tools_dir).unwrap();
    let cc_script = tools_dir.join("cc");
    let ar_script = tools_dir.join("ar");
    let ranlib_script = tools_dir.join("ranlib");
    fs::write(
        &cc_script,
        r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    shift
    out="$1"
  fi
  shift
done
mkdir -p "$(dirname "$out")"
printf 'artifact\n' > "$out"
"#,
    )
    .unwrap();
    fs::write(&ar_script, "#!/bin/sh\nexit 0\n").unwrap();
    fs::write(&ranlib_script, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for script in [&cc_script, &ar_script, &ranlib_script] {
            let mut perms = fs::metadata(script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(script, perms).unwrap();
        }
    }

    let _env_guard = ENV_MUTEX.lock().unwrap();
    let old_path = std::env::var_os("PATH");
    let mut path_entries = vec![tools_dir.as_os_str().to_os_string()];
    if let Some(existing) = &old_path {
        path_entries.extend(std::env::split_paths(existing).map(|p| p.into_os_string()));
    }
    let joined = std::env::join_paths(path_entries).unwrap();
    unsafe {
        std::env::set_var("PATH", &joined);
    }

    let result = engine.call(
        "build",
        vec![
            Value::Path(PathValue::new(workspace.to_string_lossy())),
            Value::Array(Arc::from([Value::String(Arc::from("build"))])),
            Value::Map(MapValue {
                entries: Arc::from([]),
            }),
        ],
    );

    match old_path {
        Some(path) => unsafe {
            std::env::set_var("PATH", path);
        },
        None => unsafe {
            std::env::remove_var("PATH");
        },
    }

    let value = result.unwrap();
    assert!(
        matches!(value, Value::Tree(_)),
        "expected Tree, got {value:?}"
    );
}

#[test]
fn lua_sketch_compile_map_returns_tree_items() {
    let sketch_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/lua.vx");
    let source = fs::read_to_string(&sketch_path).expect("read lua sketch");
    let source = source.replace(LUA_SKETCH_TAIL, "  all_lib_sources.map(compile)\n");
    let engine = engine(&source);

    let tempdir = TempDir::new().unwrap();
    let workspace = tempdir.path();
    let src_dir = workspace.join("zoo/lua-src");
    fs::create_dir_all(&src_dir).unwrap();

    let sources = [
        "lapi.c",
        "lcode.c",
        "lctype.c",
        "ldebug.c",
        "ldo.c",
        "ldump.c",
        "lfunc.c",
        "lgc.c",
        "llex.c",
        "lmem.c",
        "lobject.c",
        "lopcodes.c",
        "lparser.c",
        "lstate.c",
        "lstring.c",
        "ltable.c",
        "ltm.c",
        "lundump.c",
        "lvm.c",
        "lzio.c",
        "lauxlib.c",
        "lbaselib.c",
        "ldblib.c",
        "liolib.c",
        "lmathlib.c",
        "loslib.c",
        "ltablib.c",
        "lstrlib.c",
        "lutf8lib.c",
        "loadlib.c",
        "lcorolib.c",
        "linit.c",
    ];
    for file in sources {
        let stem = file.trim_end_matches(".c");
        fs::write(src_dir.join(file), format!("int {stem}_dummy = 0;\n")).unwrap();
    }

    let tools_dir = workspace.join("tools");
    fs::create_dir_all(&tools_dir).unwrap();
    let cc_script = tools_dir.join("cc");
    let libtool_script = tools_dir.join("libtool");
    fs::write(
        &cc_script,
        r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    shift
    out="$1"
  fi
  shift
done
mkdir -p "$(dirname "$out")"
printf 'artifact\n' > "$out"
"#,
    )
    .unwrap();
    fs::write(&libtool_script, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for script in [&cc_script, &libtool_script] {
            let mut perms = fs::metadata(script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(script, perms).unwrap();
        }
    }

    let _env_guard = ENV_MUTEX.lock().unwrap();
    let old_path = std::env::var_os("PATH");
    let mut path_entries = vec![tools_dir.as_os_str().to_os_string()];
    if let Some(existing) = &old_path {
        path_entries.extend(std::env::split_paths(existing).map(|p| p.into_os_string()));
    }
    let joined = std::env::join_paths(path_entries).unwrap();
    unsafe {
        std::env::set_var("PATH", &joined);
    }

    let result = engine.call(
        "build",
        vec![
            Value::Path(PathValue::new(workspace.to_string_lossy())),
            Value::Array(Arc::from([Value::String(Arc::from("build"))])),
            Value::Map(MapValue {
                entries: Arc::from([]),
            }),
        ],
    );

    match old_path {
        Some(path) => unsafe {
            std::env::set_var("PATH", path);
        },
        None => unsafe {
            std::env::remove_var("PATH");
        },
    }

    let value = result.unwrap();
    let Value::Array(items) = value else {
        panic!("expected Array, got {value:?}");
    };
    let type_names = items.iter().map(treeish_type_name).collect::<Vec<_>>();
    assert!(
        items.iter().all(is_treeish),
        "expected tree-like items, got {:?}",
        type_names
    );
}

#[test]
fn cc_version_sketch_returns_stdout_blob_in_output_tree() {
    let sketch_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cc-version.vx");
    let source = fs::read_to_string(&sketch_path).expect("read cc-version sketch");
    let engine = engine(&source);

    let tempdir = TempDir::new().unwrap();
    let workspace = tempdir.path();
    let tools_dir = workspace.join("tools");
    fs::create_dir_all(&tools_dir).unwrap();
    let cc_script = tools_dir.join("cc");
    fs::write(
        &cc_script,
        r##"#!/bin/sh
printf 'fake cc 1.0\n'
"##,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = fs::metadata(&cc_script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&cc_script, perms).unwrap();
    }

    let _env_guard = ENV_MUTEX.lock().unwrap();
    let old_path = std::env::var_os("PATH");
    let mut path_entries = vec![tools_dir.as_os_str().to_os_string()];
    if let Some(existing) = &old_path {
        path_entries.extend(std::env::split_paths(existing).map(|p| p.into_os_string()));
    }
    let joined = std::env::join_paths(path_entries).unwrap();
    unsafe {
        std::env::set_var("PATH", &joined);
    }

    let value = engine
        .call_entry(
            "build",
            workspace.to_string_lossy(),
            vec!["build".into()],
            BTreeMap::new(),
        )
        .unwrap();

    match old_path {
        Some(path) => unsafe {
            std::env::set_var("PATH", path);
        },
        None => unsafe {
            std::env::remove_var("PATH");
        },
    }

    let Value::Tree(TreeValue::Inline(entries)) = value else {
        panic!("expected tree result");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0.as_ref(), "cc-version.txt");
    assert_eq!(
        entries[0].1,
        Value::Blob(BlobValue::from_string("fake cc 1.0\n"))
    );
}

#[test]
fn lua_sketch_object_merge_returns_tree() {
    let sketch_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/lua.vx");
    let source = fs::read_to_string(&sketch_path).expect("read lua sketch");
    let source = source.replace(
        LUA_SKETCH_TAIL,
        "  let objects = (all_lib_sources.map(compile)).merge();\n  objects\n",
    );
    let engine = engine(&source);

    let tempdir = TempDir::new().unwrap();
    let workspace = tempdir.path();
    let src_dir = workspace.join("zoo/lua-src");
    fs::create_dir_all(&src_dir).unwrap();

    let sources = [
        "lapi.c",
        "lcode.c",
        "lctype.c",
        "ldebug.c",
        "ldo.c",
        "ldump.c",
        "lfunc.c",
        "lgc.c",
        "llex.c",
        "lmem.c",
        "lobject.c",
        "lopcodes.c",
        "lparser.c",
        "lstate.c",
        "lstring.c",
        "ltable.c",
        "ltm.c",
        "lundump.c",
        "lvm.c",
        "lzio.c",
        "lauxlib.c",
        "lbaselib.c",
        "ldblib.c",
        "liolib.c",
        "lmathlib.c",
        "loslib.c",
        "ltablib.c",
        "lstrlib.c",
        "lutf8lib.c",
        "loadlib.c",
        "lcorolib.c",
        "linit.c",
    ];
    for file in sources {
        let stem = file.trim_end_matches(".c");
        fs::write(src_dir.join(file), format!("int {stem}_dummy = 0;\n")).unwrap();
    }

    let tools_dir = workspace.join("tools");
    fs::create_dir_all(&tools_dir).unwrap();
    let cc_script = tools_dir.join("cc");
    let libtool_script = tools_dir.join("libtool");
    fs::write(
        &cc_script,
        r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    shift
    out="$1"
  fi
  shift
done
mkdir -p "$(dirname "$out")"
printf 'artifact\n' > "$out"
"#,
    )
    .unwrap();
    fs::write(&libtool_script, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for script in [&cc_script, &libtool_script] {
            let mut perms = fs::metadata(script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(script, perms).unwrap();
        }
    }

    let _env_guard = ENV_MUTEX.lock().unwrap();
    let old_path = std::env::var_os("PATH");
    let mut path_entries = vec![tools_dir.as_os_str().to_os_string()];
    if let Some(existing) = &old_path {
        path_entries.extend(std::env::split_paths(existing).map(|p| p.into_os_string()));
    }
    let joined = std::env::join_paths(path_entries).unwrap();
    unsafe {
        std::env::set_var("PATH", &joined);
    }

    let result = engine.call(
        "build",
        vec![
            Value::Path(PathValue::new(workspace.to_string_lossy())),
            Value::Array(Arc::from([Value::String(Arc::from("build"))])),
            Value::Map(MapValue {
                entries: Arc::from([]),
            }),
        ],
    );

    match old_path {
        Some(path) => unsafe {
            std::env::set_var("PATH", path);
        },
        None => unsafe {
            std::env::remove_var("PATH");
        },
    }

    let value = result.unwrap();
    assert!(
        matches!(value, Value::Tree(_)),
        "expected Tree, got {value:?}"
    );
}

#[test]
fn lua_sketch_archive_returns_tree() {
    let sketch_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/lua.vx");
    let source = fs::read_to_string(&sketch_path).expect("read lua sketch");
    let source = source.replace(
        LUA_SKETCH_TAIL,
        "  let objects = (all_lib_sources.map(compile)).merge();\n  let liblua = archive(objects);\n  liblua\n",
    );
    let engine = engine(&source);

    let tempdir = TempDir::new().unwrap();
    let workspace = tempdir.path();
    let src_dir = workspace.join("zoo/lua-src");
    fs::create_dir_all(&src_dir).unwrap();

    let sources = [
        "lapi.c",
        "lcode.c",
        "lctype.c",
        "ldebug.c",
        "ldo.c",
        "ldump.c",
        "lfunc.c",
        "lgc.c",
        "llex.c",
        "lmem.c",
        "lobject.c",
        "lopcodes.c",
        "lparser.c",
        "lstate.c",
        "lstring.c",
        "ltable.c",
        "ltm.c",
        "lundump.c",
        "lvm.c",
        "lzio.c",
        "lauxlib.c",
        "lbaselib.c",
        "ldblib.c",
        "liolib.c",
        "lmathlib.c",
        "loslib.c",
        "ltablib.c",
        "lstrlib.c",
        "lutf8lib.c",
        "loadlib.c",
        "lcorolib.c",
        "linit.c",
    ];
    for file in sources {
        let stem = file.trim_end_matches(".c");
        fs::write(src_dir.join(file), format!("int {stem}_dummy = 0;\n")).unwrap();
    }

    let tools_dir = workspace.join("tools");
    fs::create_dir_all(&tools_dir).unwrap();
    let cc_script = tools_dir.join("cc");
    let libtool_script = tools_dir.join("libtool");
    fs::write(
        &cc_script,
        r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    shift
    out="$1"
  fi
  shift
done
mkdir -p "$(dirname "$out")"
printf 'artifact\n' > "$out"
"#,
    )
    .unwrap();
    fs::write(
        &libtool_script,
        r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    shift
    out="$1"
  fi
  shift
done
mkdir -p "$(dirname "$out")"
printf 'archive\n' > "$out"
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for script in [&cc_script, &libtool_script] {
            let mut perms = fs::metadata(script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(script, perms).unwrap();
        }
    }

    let _env_guard = ENV_MUTEX.lock().unwrap();
    let old_path = std::env::var_os("PATH");
    let mut path_entries = vec![tools_dir.as_os_str().to_os_string()];
    if let Some(existing) = &old_path {
        path_entries.extend(std::env::split_paths(existing).map(|p| p.into_os_string()));
    }
    let joined = std::env::join_paths(path_entries).unwrap();
    unsafe {
        std::env::set_var("PATH", &joined);
    }

    let result = engine.call(
        "build",
        vec![
            Value::Path(PathValue::new(workspace.to_string_lossy())),
            Value::Array(Arc::from([Value::String(Arc::from("build"))])),
            Value::Map(MapValue {
                entries: Arc::from([]),
            }),
        ],
    );

    match old_path {
        Some(path) => unsafe {
            std::env::set_var("PATH", path);
        },
        None => unsafe {
            std::env::remove_var("PATH");
        },
    }

    let value = result.unwrap();
    assert!(
        matches!(value, Value::Tree(_)),
        "expected Tree, got {value:?}"
    );
}

#[test]
fn libpng_sketch_zlib_archive_returns_tree_with_mocked_boundaries() {
    let sketch_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/libpng.vx");
    let source = fs::read_to_string(&sketch_path).expect("read libpng sketch");
    let marker = "  let zlib_static = static_archive(@lib/libz.a, zlib_objects);\n";
    let (prefix, _) = source
        .split_once(marker)
        .expect("libpng sketch should contain zlib archive marker");
    let source = format!("{prefix}{marker}  zlib_static\n)\n");

    let tempdir = TempDir::new().unwrap();
    let workspace = tempdir.path();
    let zlib_src_dir = workspace.join("mock-zlib");
    fs::create_dir_all(&zlib_src_dir).unwrap();

    let engine = engine(&source)
        .with_acquire_handler(Arc::new({
            let zlib_src_dir = zlib_src_dir.clone();
            move |_request| {
                Ok(Value::Tree(TreeValue::FsDir(PathValue::new(
                    zlib_src_dir.to_string_lossy(),
                ))))
            }
        }))
        .with_render_handler(Arc::new(|_request| {
            Ok(Value::Blob(BlobValue::from_string("generated header")))
        }))
        .with_discover_handler(Arc::new(|_request| {
            Ok(Value::Map(MapValue {
                entries: Arc::from([
                    (
                        Value::String(Arc::from("cc")),
                        Value::String(Arc::from("/usr/bin/cc")),
                    ),
                    (
                        Value::String(Arc::from("ar")),
                        Value::String(Arc::from("/usr/bin/ar")),
                    ),
                ]),
            }))
        }))
        .with_exec_handler(Arc::new(|request| {
            if matches!(
                request.args.as_slice(),
                [ExecArgValue::Literal(program), ExecArgValue::Literal(flag)]
                    if program == "/usr/bin/cc" && flag == "-dumpmachine"
            ) {
                return Ok(Value::String(Arc::from("x86_64-unknown-linux-gnu")));
            }
            let output_arg = request
                .args
                .windows(2)
                .find_map(|window| match window {
                    [ExecArgValue::Literal(flag), output] if flag == "-o" => Some(output.as_str()),
                    _ => None,
                })
                .or_else(|| request.args.get(2).map(ExecArgValue::as_str))
                .expect("mock exec should receive an output path");
            if output_arg == "out/probe" || output_arg == "out/probe.o" {
                return Ok(Value::Bool(true));
            }
            let relative = output_arg.strip_prefix("out/").unwrap_or(output_arg);
            Ok(inline_tree_with_blob(relative, "artifact"))
        }));

    let value = engine
        .call(
            "build",
            vec![
                Value::Path(PathValue::new(workspace.to_string_lossy())),
                Value::Array(Arc::from([])),
                Value::Map(MapValue {
                    entries: Arc::from([]),
                }),
            ],
        )
        .unwrap();

    assert!(
        matches!(value, Value::Tree(_)),
        "expected Tree result, got {value:?}"
    );
    assert_eq!(
        collect_tree_leaf_paths(&value),
        vec!["lib/libz.a".to_string()]
    );
}

#[test]
fn libpng_sketch_full_build_returns_tree_with_mocked_boundaries() {
    let sketch_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/libpng.vx");
    let source = fs::read_to_string(&sketch_path).expect("read libpng sketch");

    let tempdir = TempDir::new().unwrap();
    let workspace = tempdir.path();
    let zlib_src_dir = workspace.join("mock-zlib");
    let libpng_src_dir = workspace.join("mock-libpng");
    fs::create_dir_all(&zlib_src_dir).unwrap();
    fs::create_dir_all(&libpng_src_dir).unwrap();
    fs::create_dir_all(libpng_src_dir.join("scripts")).unwrap();
    fs::write(zlib_src_dir.join("zlib.h"), "/* mock zlib.h */\n").unwrap();
    fs::write(zlib_src_dir.join("zconf.h"), "/* mock zconf.h */\n").unwrap();
    fs::write(libpng_src_dir.join("png.h"), "/* mock png.h */\n").unwrap();
    fs::write(libpng_src_dir.join("pngconf.h"), "/* mock pngconf.h */\n").unwrap();
    fs::write(
        libpng_src_dir.join("scripts/pnglibconf.h.prebuilt"),
        "/* mock pnglibconf.h */\n",
    )
    .unwrap();
    fs::write(libpng_src_dir.join("libpng.vers"), "{ global: *; };\n").unwrap();

    let zlib_root = NodeHash(Blake3Hash::from_bytes(b"mock-zlib-root"));
    let libpng_root = NodeHash(Blake3Hash::from_bytes(b"mock-libpng-root"));
    let engine = engine(&source)
        .with_store_hydration_handler(Arc::new({
            let zlib_src_dir = zlib_src_dir.clone();
            let libpng_src_dir = libpng_src_dir.clone();
            move |request| match request.value {
                Value::Tree(TreeValue::Store(root)) if root == zlib_root => Ok(Value::Tree(
                    TreeValue::FsDir(PathValue::new(zlib_src_dir.to_string_lossy())),
                )),
                Value::Tree(TreeValue::Store(root)) if root == libpng_root => Ok(Value::Tree(
                    TreeValue::FsDir(PathValue::new(libpng_src_dir.to_string_lossy())),
                )),
                other => panic!("unexpected hydration request: {other:?}"),
            }
        }))
        .with_acquire_handler(Arc::new(move |request| match request.url.as_ref() {
            "https://zlib.net/fossils/zlib-1.3.1.tar.gz" => {
                Ok(Value::Tree(TreeValue::Store(zlib_root)))
            }
            "https://download.sourceforge.net/libpng/libpng-1.6.43.tar.xz" => {
                Ok(Value::Tree(TreeValue::Store(libpng_root)))
            }
            other => panic!("unexpected acquire url: {other}"),
        }))
        .with_render_handler(Arc::new(|_request| {
            Ok(Value::Blob(BlobValue::from_string("generated header")))
        }))
        .with_discover_handler(Arc::new(|_request| {
            Ok(Value::Map(MapValue {
                entries: Arc::from([
                    (
                        Value::String(Arc::from("cc")),
                        Value::String(Arc::from("/usr/bin/cc")),
                    ),
                    (
                        Value::String(Arc::from("ar")),
                        Value::String(Arc::from("/usr/bin/ar")),
                    ),
                ]),
            }))
        }))
        .with_exec_handler(Arc::new(|request| {
            if matches!(
                request.args.as_slice(),
                [ExecArgValue::Literal(program), ExecArgValue::Literal(flag)]
                    if program == "/usr/bin/cc" && flag == "-dumpmachine"
            ) {
                return Ok(Value::String(Arc::from("x86_64-unknown-linux-gnu")));
            }
            let output_arg = request
                .args
                .windows(2)
                .find_map(|window| match window {
                    [ExecArgValue::Literal(flag), output] if flag == "-o" => Some(output.as_str()),
                    _ => None,
                })
                .or_else(|| request.args.get(2).map(ExecArgValue::as_str))
                .expect("mock exec should receive an output path");
            if output_arg == "out/probe" || output_arg == "out/probe.o" {
                return Ok(Value::Bool(true));
            }
            let relative = output_arg.strip_prefix("out/").unwrap_or(output_arg);
            Ok(inline_tree_with_blob(relative, "artifact"))
        }));

    let value = engine
        .call(
            "build",
            vec![
                Value::Path(PathValue::new(workspace.to_string_lossy())),
                Value::Array(Arc::from([])),
                Value::Map(MapValue {
                    entries: Arc::from([]),
                }),
            ],
        )
        .unwrap();

    assert!(
        matches!(value, Value::Tree(_)),
        "expected Tree result, got {value:?}"
    );
    assert_eq!(
        collect_tree_leaf_paths(&value),
        vec![
            "libpng/include/png.h".to_string(),
            "libpng/include/pngconf.h".to_string(),
            "libpng/include/pnglibconf.h".to_string(),
            "libpng/lib/libpng16.a".to_string(),
            "libpng/lib/libpng16.so".to_string(),
            "zlib/include/zconf.h".to_string(),
            "zlib/include/zlib.h".to_string(),
            "zlib/lib/libz.a".to_string(),
            "zlib/lib/libz.so".to_string(),
        ]
    );
}

#[test]
fn libpng_sketch_full_build_with_discovered_tools_returns_tree_with_mocked_boundaries() {
    let sketch_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/libpng.vx");
    let source = fs::read_to_string(&sketch_path).expect("read libpng sketch");

    let tempdir = TempDir::new().unwrap();
    let workspace = tempdir.path();
    let zlib_src_dir = workspace.join("mock-zlib");
    let libpng_src_dir = workspace.join("mock-libpng");
    fs::create_dir_all(&zlib_src_dir).unwrap();
    fs::create_dir_all(&libpng_src_dir).unwrap();
    fs::create_dir_all(libpng_src_dir.join("scripts")).unwrap();
    fs::write(zlib_src_dir.join("zlib.h"), "/* mock zlib.h */\n").unwrap();
    fs::write(zlib_src_dir.join("zconf.h"), "/* mock zconf.h */\n").unwrap();
    fs::write(libpng_src_dir.join("png.h"), "/* mock png.h */\n").unwrap();
    fs::write(libpng_src_dir.join("pngconf.h"), "/* mock pngconf.h */\n").unwrap();
    fs::write(
        libpng_src_dir.join("scripts/pnglibconf.h.prebuilt"),
        "/* mock pnglibconf.h */\n",
    )
    .unwrap();
    fs::write(libpng_src_dir.join("libpng.vers"), "{ global: *; };\n").unwrap();

    let zlib_root = NodeHash(Blake3Hash::from_bytes(b"mock-zlib-root"));
    let libpng_root = NodeHash(Blake3Hash::from_bytes(b"mock-libpng-root"));
    let engine = engine(&source)
        .with_store_hydration_handler(Arc::new({
            let zlib_src_dir = zlib_src_dir.clone();
            let libpng_src_dir = libpng_src_dir.clone();
            move |request| match request.value {
                Value::Tree(TreeValue::Store(root)) if root == zlib_root => Ok(Value::Tree(
                    TreeValue::FsDir(PathValue::new(zlib_src_dir.to_string_lossy())),
                )),
                Value::Tree(TreeValue::Store(root)) if root == libpng_root => Ok(Value::Tree(
                    TreeValue::FsDir(PathValue::new(libpng_src_dir.to_string_lossy())),
                )),
                other => panic!("unexpected hydration request: {other:?}"),
            }
        }))
        .with_acquire_handler(Arc::new(move |request| match request.url.as_ref() {
            "https://zlib.net/fossils/zlib-1.3.1.tar.gz" => {
                Ok(Value::Tree(TreeValue::Store(zlib_root)))
            }
            "https://download.sourceforge.net/libpng/libpng-1.6.43.tar.xz" => {
                Ok(Value::Tree(TreeValue::Store(libpng_root)))
            }
            other => panic!("unexpected acquire url: {other}"),
        }))
        .with_render_handler(Arc::new(|_request| {
            Ok(Value::Blob(BlobValue::from_string("generated header")))
        }))
        .with_discover_handler(Arc::new(|_request| {
            let cc_tool = Value::Struct(StructValue {
                type_name: Arc::from("Tool"),
                fields: Arc::from([
                    (Arc::from("path"), Value::String(Arc::from("/usr/bin/cc"))),
                    (
                        Arc::from("hash"),
                        Value::Blob(BlobValue::from_string("cc-hash")),
                    ),
                ]),
            });
            let ar_tool = Value::Struct(StructValue {
                type_name: Arc::from("Tool"),
                fields: Arc::from([
                    (Arc::from("path"), Value::String(Arc::from("/usr/bin/ar"))),
                    (
                        Arc::from("hash"),
                        Value::Blob(BlobValue::from_string("ar-hash")),
                    ),
                ]),
            });
            Ok(Value::Discovered(DiscoveredValue {
                value: Box::new(Value::Map(MapValue {
                    entries: Arc::from([
                        (Value::String(Arc::from("cc")), cc_tool),
                        (Value::String(Arc::from("ar")), ar_tool),
                    ]),
                })),
                obligations: Arc::from([
                    HostObservation::Which {
                        name: "cc".to_string(),
                        expected_path: Some("/usr/bin/cc".to_string()),
                    },
                    HostObservation::Hash {
                        path: "/usr/bin/cc".to_string(),
                        content_hash: "cc-hash".to_string(),
                    },
                    HostObservation::Which {
                        name: "ar".to_string(),
                        expected_path: Some("/usr/bin/ar".to_string()),
                    },
                    HostObservation::Hash {
                        path: "/usr/bin/ar".to_string(),
                        content_hash: "ar-hash".to_string(),
                    },
                ]),
            }))
        }))
        .with_exec_handler(Arc::new(|request| {
            if matches!(
                request.args.as_slice(),
                [ExecArgValue::Literal(program), ExecArgValue::Literal(flag)]
                    if program == "/usr/bin/cc" && flag == "-dumpmachine"
            ) {
                return Ok(Value::String(Arc::from("x86_64-unknown-linux-gnu")));
            }
            let output_arg = request
                .args
                .windows(2)
                .find_map(|window| match window {
                    [ExecArgValue::Literal(flag), output] if flag == "-o" => Some(output.as_str()),
                    _ => None,
                })
                .or_else(|| request.args.get(2).map(ExecArgValue::as_str))
                .expect("mock exec should receive an output path");
            if output_arg == "out/probe" || output_arg == "out/probe.o" {
                return Ok(Value::Bool(true));
            }
            let relative = output_arg.strip_prefix("out/").unwrap_or(output_arg);
            Ok(inline_tree_with_blob(relative, "artifact"))
        }));

    let value = engine
        .call(
            "build",
            vec![
                Value::Path(PathValue::new(workspace.to_string_lossy())),
                Value::Array(Arc::from([])),
                Value::Map(MapValue {
                    entries: Arc::from([]),
                }),
            ],
        )
        .unwrap();

    assert_eq!(
        collect_tree_leaf_paths(&value),
        vec![
            "libpng/include/png.h".to_string(),
            "libpng/include/pngconf.h".to_string(),
            "libpng/include/pnglibconf.h".to_string(),
            "libpng/lib/libpng16.a".to_string(),
            "libpng/lib/libpng16.so".to_string(),
            "zlib/include/zconf.h".to_string(),
            "zlib/include/zlib.h".to_string(),
            "zlib/lib/libz.a".to_string(),
            "zlib/lib/libz.so".to_string(),
        ]
    );
}

#[test]
fn libpng_sketch_surfaces_archive_failures_from_exec_boundaries() {
    let sketch_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/libpng.vx");
    let source = fs::read_to_string(&sketch_path).expect("read libpng sketch");

    let tempdir = TempDir::new().unwrap();
    let workspace = tempdir.path();
    let zlib_src_dir = workspace.join("mock-zlib");
    let libpng_src_dir = workspace.join("mock-libpng");
    fs::create_dir_all(&zlib_src_dir).unwrap();
    fs::create_dir_all(&libpng_src_dir).unwrap();
    fs::create_dir_all(libpng_src_dir.join("scripts")).unwrap();
    fs::write(zlib_src_dir.join("zlib.h"), "/* mock zlib.h */\n").unwrap();
    fs::write(zlib_src_dir.join("zconf.h"), "/* mock zconf.h */\n").unwrap();
    fs::write(libpng_src_dir.join("png.h"), "/* mock png.h */\n").unwrap();
    fs::write(libpng_src_dir.join("pngconf.h"), "/* mock pngconf.h */\n").unwrap();
    fs::write(
        libpng_src_dir.join("scripts/pnglibconf.h.prebuilt"),
        "/* mock pnglibconf.h */\n",
    )
    .unwrap();
    fs::write(libpng_src_dir.join("libpng.vers"), "{ global: *; };\n").unwrap();

    let zlib_root = NodeHash(Blake3Hash::from_bytes(b"mock-zlib-root"));
    let libpng_root = NodeHash(Blake3Hash::from_bytes(b"mock-libpng-root"));
    let engine = engine(&source)
        .with_store_hydration_handler(Arc::new({
            let zlib_src_dir = zlib_src_dir.clone();
            let libpng_src_dir = libpng_src_dir.clone();
            move |request| match request.value {
                Value::Tree(TreeValue::Store(root)) if root == zlib_root => Ok(Value::Tree(
                    TreeValue::FsDir(PathValue::new(zlib_src_dir.to_string_lossy())),
                )),
                Value::Tree(TreeValue::Store(root)) if root == libpng_root => Ok(Value::Tree(
                    TreeValue::FsDir(PathValue::new(libpng_src_dir.to_string_lossy())),
                )),
                other => panic!("unexpected hydration request: {other:?}"),
            }
        }))
        .with_acquire_handler(Arc::new(move |request| match request.url.as_ref() {
            "https://zlib.net/fossils/zlib-1.3.1.tar.gz" => {
                Ok(Value::Tree(TreeValue::Store(zlib_root)))
            }
            "https://download.sourceforge.net/libpng/libpng-1.6.43.tar.xz" => {
                Ok(Value::Tree(TreeValue::Store(libpng_root)))
            }
            other => panic!("unexpected acquire url: {other}"),
        }))
        .with_render_handler(Arc::new(|_request| {
            Ok(Value::Blob(BlobValue::from_string("generated header")))
        }))
        .with_discover_handler(Arc::new(|_request| {
            Ok(Value::Map(MapValue {
                entries: Arc::from([
                    (
                        Value::String(Arc::from("cc")),
                        Value::String(Arc::from("/usr/bin/cc")),
                    ),
                    (
                        Value::String(Arc::from("ar")),
                        Value::String(Arc::from("/usr/bin/ar")),
                    ),
                ]),
            }))
        }))
        .with_exec_handler(Arc::new(|request| {
            let program = request
                .args
                .first()
                .expect("mock exec should receive a program")
                .as_str();
            if matches!(
                request.args.as_slice(),
                [ExecArgValue::Literal(program), ExecArgValue::Literal(flag)]
                    if program == "/usr/bin/cc" && flag == "-dumpmachine"
            ) {
                return Ok(Value::String(Arc::from("x86_64-unknown-linux-gnu")));
            }
            if program == "/usr/bin/ar" {
                return Err(PoisonError::new(
                    PoisonKind::UserFail,
                    "archive failed",
                    request.demand_chain.clone(),
                ));
            }
            let output_arg = request
                .args
                .windows(2)
                .find_map(|window| match window {
                    [ExecArgValue::Literal(flag), output] if flag == "-o" => Some(output.as_str()),
                    _ => None,
                })
                .or_else(|| request.args.get(2).map(ExecArgValue::as_str))
                .expect("mock exec should receive an output path");
            if output_arg == "out/probe" || output_arg == "out/probe.o" {
                return Ok(Value::Bool(true));
            }
            let relative = output_arg.strip_prefix("out/").unwrap_or(output_arg);
            Ok(inline_tree_with_blob(relative, "artifact"))
        }));

    let error = engine
        .call(
            "build",
            vec![
                Value::Path(PathValue::new(workspace.to_string_lossy())),
                Value::Array(Arc::from([])),
                Value::Map(MapValue {
                    entries: Arc::from([]),
                }),
            ],
        )
        .expect_err("archive failure should poison the build");

    assert_eq!(error.kind, PoisonKind::UserFail);
    assert!(
        error.message.contains("archive failed"),
        "unexpected error: {error:?}"
    );
}

#[test]
fn libpng_compile_objects_body_survives_lowering() {
    let sketch_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/libpng.vx");
    let source = fs::read_to_string(&sketch_path).expect("read libpng sketch");
    let parse = vx_parser::parse(&source);
    assert_eq!(parse.errors(), []);
    let root = SourceFile::cast_root(parse.syntax());
    let module = lower_source_file(&root);
    let build = module
        .functions
        .iter()
        .find(|function| function.name == "build")
        .expect("build function");
    let compile_objects = build
        .body
        .stmts
        .iter()
        .find_map(|stmt| match stmt {
            crate::instr::Stmt::FnDecl(def) if def.name == "compile_objects" => Some(def.clone()),
            _ => None,
        })
        .expect("compile_objects function");

    assert!(
        !compile_objects.body.stmts.is_empty(),
        "compile_objects body should not be dropped by lowering"
    );
    assert!(
        matches!(
            compile_objects.body.stmts.last(),
            Some(crate::instr::Stmt::Expr(_))
        ),
        "compile_objects should end with an expression, got {:?}",
        compile_objects.body.stmts
    );
}

#[test]
fn lowering_preserves_includes_map_flatten_expression() {
    let source = r#"
fn main(includes: Array[Path]) {
  includes.map((include) => ["-I", include]).flatten()
}
"#;
    let parse = vx_parser::parse(source);
    assert_eq!(parse.errors(), []);
    let root = SourceFile::cast_root(parse.syntax());
    let module = lower_source_file(&root);
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main function");

    assert!(
        matches!(main.body.stmts.last(), Some(crate::instr::Stmt::Expr(_))),
        "main should end with an expression, got {:?}",
        main.body.stmts
    );
}

#[test]
fn lowering_preserves_exec_where_expression_with_spreads() {
    let source = r#"
fn main(
  cexec: Fn(Array[ExecInput]) -> Tree,
  cc: String,
  src: Tree,
  file: Path,
  includes: Array[Path],
  defines: Array[String],
  common_cflags: Array[String],
  flags: Array[String],
  mounts: Map[Path, TreeEntry],
  generated: Map[Path, Blob]
) {
  cexec [
    cc,
    "-c",
    @(src/{file}),
    "-o",
    @(out/{file}.o),
    "-I", @src,
    ..includes.map((include) => ["-I", include]).flatten(),
    ..defines.map((define) => `-D${define}`),
    ..common_cflags,
    ..flags,
  ] where {
    mounts: %{ @src => src, @gen => Tree(generated), ..mounts },
  }
}
"#;
    let parse = vx_parser::parse(source);
    assert_eq!(parse.errors(), []);
    let root = SourceFile::cast_root(parse.syntax());
    let module = lower_source_file(&root);
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main function");

    assert!(
        matches!(main.body.stmts.last(), Some(crate::instr::Stmt::Expr(_))),
        "main should end with an expression, got {:?}",
        main.body.stmts
    );
}

#[test]
fn lua_sketch_link_input_array_contains_tree_items() {
    let sketch_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/lua.vx");
    let source = fs::read_to_string(&sketch_path).expect("read lua sketch");
    let source = source.replace(
        LUA_SKETCH_TAIL,
        "  let objects = (all_lib_sources.map(compile)).merge();\n  let liblua = archive(objects);\n  [compile @lua.c, liblua]\n",
    );
    let engine = engine(&source);

    let tempdir = TempDir::new().unwrap();
    let workspace = tempdir.path();
    let src_dir = workspace.join("zoo/lua-src");
    fs::create_dir_all(&src_dir).unwrap();

    let sources = [
        "lapi.c",
        "lcode.c",
        "lctype.c",
        "ldebug.c",
        "ldo.c",
        "ldump.c",
        "lfunc.c",
        "lgc.c",
        "llex.c",
        "lmem.c",
        "lobject.c",
        "lopcodes.c",
        "lparser.c",
        "lstate.c",
        "lstring.c",
        "ltable.c",
        "ltm.c",
        "lundump.c",
        "lvm.c",
        "lzio.c",
        "lauxlib.c",
        "lbaselib.c",
        "ldblib.c",
        "liolib.c",
        "lmathlib.c",
        "loslib.c",
        "ltablib.c",
        "lstrlib.c",
        "lutf8lib.c",
        "loadlib.c",
        "lcorolib.c",
        "linit.c",
        "lua.c",
    ];
    for file in sources {
        let stem = file.trim_end_matches(".c");
        fs::write(src_dir.join(file), format!("int {stem}_dummy = 0;\n")).unwrap();
    }

    let tools_dir = workspace.join("tools");
    fs::create_dir_all(&tools_dir).unwrap();
    let cc_script = tools_dir.join("cc");
    let libtool_script = tools_dir.join("libtool");
    fs::write(
        &cc_script,
        r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    shift
    out="$1"
  fi
  shift
done
mkdir -p "$(dirname "$out")"
printf 'artifact\n' > "$out"
"#,
    )
    .unwrap();
    fs::write(
        &libtool_script,
        r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    shift
    out="$1"
  fi
  shift
done
mkdir -p "$(dirname "$out")"
printf 'archive\n' > "$out"
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for script in [&cc_script, &libtool_script] {
            let mut perms = fs::metadata(script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(script, perms).unwrap();
        }
    }

    let _env_guard = ENV_MUTEX.lock().unwrap();
    let old_path = std::env::var_os("PATH");
    let mut path_entries = vec![tools_dir.as_os_str().to_os_string()];
    if let Some(existing) = &old_path {
        path_entries.extend(std::env::split_paths(existing).map(|p| p.into_os_string()));
    }
    let joined = std::env::join_paths(path_entries).unwrap();
    unsafe {
        std::env::set_var("PATH", &joined);
    }

    let result = engine.call(
        "build",
        vec![
            Value::Path(PathValue::new(workspace.to_string_lossy())),
            Value::Array(Arc::from([Value::String(Arc::from("build"))])),
            Value::Map(MapValue {
                entries: Arc::from([]),
            }),
        ],
    );

    match old_path {
        Some(path) => unsafe {
            std::env::set_var("PATH", path);
        },
        None => unsafe {
            std::env::remove_var("PATH");
        },
    }

    let value = result.unwrap();
    let Value::Array(items) = value else {
        panic!("expected Array, got {value:?}");
    };
    assert!(
        items.iter().all(is_treeish),
        "expected tree-like items, got {:?}",
        items.iter().map(treeish_type_name).collect::<Vec<_>>()
    );
}

#[test]
fn lua_sketch_tree_constructor_over_link_inputs_returns_tree() {
    let sketch_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/lua.vx");
    let source = fs::read_to_string(&sketch_path).expect("read lua sketch");
    let source = source.replace(
        LUA_SKETCH_TAIL,
        "  let objects = (all_lib_sources.map(compile)).merge();\n  let liblua = archive(objects);\n  Tree [compile @lua.c, liblua]\n",
    );
    let engine = engine(&source);

    let tempdir = TempDir::new().unwrap();
    let workspace = tempdir.path();
    let src_dir = workspace.join("zoo/lua-src");
    fs::create_dir_all(&src_dir).unwrap();

    let sources = [
        "lapi.c",
        "lcode.c",
        "lctype.c",
        "ldebug.c",
        "ldo.c",
        "ldump.c",
        "lfunc.c",
        "lgc.c",
        "llex.c",
        "lmem.c",
        "lobject.c",
        "lopcodes.c",
        "lparser.c",
        "lstate.c",
        "lstring.c",
        "ltable.c",
        "ltm.c",
        "lundump.c",
        "lvm.c",
        "lzio.c",
        "lauxlib.c",
        "lbaselib.c",
        "ldblib.c",
        "liolib.c",
        "lmathlib.c",
        "loslib.c",
        "ltablib.c",
        "lstrlib.c",
        "lutf8lib.c",
        "loadlib.c",
        "lcorolib.c",
        "linit.c",
        "lua.c",
    ];
    for file in sources {
        let stem = file.trim_end_matches(".c");
        fs::write(src_dir.join(file), format!("int {stem}_dummy = 0;\n")).unwrap();
    }

    let tools_dir = workspace.join("tools");
    fs::create_dir_all(&tools_dir).unwrap();
    let cc_script = tools_dir.join("cc");
    let libtool_script = tools_dir.join("libtool");
    fs::write(
        &cc_script,
        r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    shift
    out="$1"
  fi
  shift
done
mkdir -p "$(dirname "$out")"
printf 'artifact\n' > "$out"
"#,
    )
    .unwrap();
    fs::write(
        &libtool_script,
        r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    shift
    out="$1"
  fi
  shift
done
mkdir -p "$(dirname "$out")"
printf 'archive\n' > "$out"
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for script in [&cc_script, &libtool_script] {
            let mut perms = fs::metadata(script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(script, perms).unwrap();
        }
    }

    let _env_guard = ENV_MUTEX.lock().unwrap();
    let old_path = std::env::var_os("PATH");
    let mut path_entries = vec![tools_dir.as_os_str().to_os_string()];
    if let Some(existing) = &old_path {
        path_entries.extend(std::env::split_paths(existing).map(|p| p.into_os_string()));
    }
    let joined = std::env::join_paths(path_entries).unwrap();
    unsafe {
        std::env::set_var("PATH", &joined);
    }

    let result = engine.call(
        "build",
        vec![
            Value::Path(PathValue::new(workspace.to_string_lossy())),
            Value::Array(Arc::from([Value::String(Arc::from("build"))])),
            Value::Map(MapValue {
                entries: Arc::from([]),
            }),
        ],
    );

    match old_path {
        Some(path) => unsafe {
            std::env::set_var("PATH", path);
        },
        None => unsafe {
            std::env::remove_var("PATH");
        },
    }

    let value = result.unwrap();
    assert!(
        matches!(value, Value::Tree(_)),
        "expected Tree, got {value:?}"
    );
}

#[test]
fn lua_sketch_link_returns_tree() {
    let sketch_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../zoo/lua.vx");
    let source = fs::read_to_string(&sketch_path).expect("read lua sketch");
    let source = source.replace(
        LUA_SKETCH_TAIL,
        "  let objects = (all_lib_sources.map(compile)).merge();\n  let liblua = archive(objects);\n  let lua_exe = link(Tree [compile @lua.c, liblua]);\n  lua_exe\n",
    );
    let engine = engine(&source);

    let tempdir = TempDir::new().unwrap();
    let workspace = tempdir.path();
    let src_dir = workspace.join("zoo/lua-src");
    fs::create_dir_all(&src_dir).unwrap();

    let sources = [
        "lapi.c",
        "lcode.c",
        "lctype.c",
        "ldebug.c",
        "ldo.c",
        "ldump.c",
        "lfunc.c",
        "lgc.c",
        "llex.c",
        "lmem.c",
        "lobject.c",
        "lopcodes.c",
        "lparser.c",
        "lstate.c",
        "lstring.c",
        "ltable.c",
        "ltm.c",
        "lundump.c",
        "lvm.c",
        "lzio.c",
        "lauxlib.c",
        "lbaselib.c",
        "ldblib.c",
        "liolib.c",
        "lmathlib.c",
        "loslib.c",
        "ltablib.c",
        "lstrlib.c",
        "lutf8lib.c",
        "loadlib.c",
        "lcorolib.c",
        "linit.c",
        "lua.c",
    ];
    for file in sources {
        let stem = file.trim_end_matches(".c");
        fs::write(src_dir.join(file), format!("int {stem}_dummy = 0;\n")).unwrap();
    }

    let tools_dir = workspace.join("tools");
    fs::create_dir_all(&tools_dir).unwrap();
    let cc_script = tools_dir.join("cc");
    let libtool_script = tools_dir.join("libtool");
    fs::write(
        &cc_script,
        r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    shift
    out="$1"
  fi
  shift
done
mkdir -p "$(dirname "$out")"
printf 'artifact\n' > "$out"
"#,
    )
    .unwrap();
    fs::write(
        &libtool_script,
        r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    shift
    out="$1"
  fi
  shift
done
mkdir -p "$(dirname "$out")"
printf 'archive\n' > "$out"
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for script in [&cc_script, &libtool_script] {
            let mut perms = fs::metadata(script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(script, perms).unwrap();
        }
    }

    let _env_guard = ENV_MUTEX.lock().unwrap();
    let old_path = std::env::var_os("PATH");
    let mut path_entries = vec![tools_dir.as_os_str().to_os_string()];
    if let Some(existing) = &old_path {
        path_entries.extend(std::env::split_paths(existing).map(|p| p.into_os_string()));
    }
    let joined = std::env::join_paths(path_entries).unwrap();
    unsafe {
        std::env::set_var("PATH", &joined);
    }

    let result = engine.call(
        "build",
        vec![
            Value::Path(PathValue::new(workspace.to_string_lossy())),
            Value::Array(Arc::from([Value::String(Arc::from("build"))])),
            Value::Map(MapValue {
                entries: Arc::from([]),
            }),
        ],
    );

    match old_path {
        Some(path) => unsafe {
            std::env::set_var("PATH", path);
        },
        None => unsafe {
            std::env::remove_var("PATH");
        },
    }

    let value = result.unwrap();
    assert!(
        matches!(value, Value::Tree(_)),
        "expected Tree, got {value:?}"
    );
}

#[test]
fn exec_stringifies_custom_values_via_to_string_method() {
    let captured = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&captured);
    let engine = engine(
        r#"
fn build() (
  struct Tool {
    path: String,
    hash: Blob,
  }

  namespace Tool {
    fn to_string(self) -> String self.path;
  }

  let tool = Tool { path: "/bin/sh", hash: Blob("x") };
  exec [tool, "-c", "exit 0"] where {
    observer: (proc, tree) => proc.exit_code,
  }
)
"#,
    )
    .with_exec_handler(Arc::new(move |request| {
        *sink.lock().unwrap() = Some(request);
        Ok(Value::U64(0))
    }));

    let value = engine.call("build", vec![]).unwrap();
    assert_eq!(value, Value::U64(0));

    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("exec request should be captured");
    assert_eq!(
        request.args,
        vec![
            ExecArgValue::Literal("/bin/sh".into()),
            ExecArgValue::Literal("-c".into()),
            ExecArgValue::Literal("exit 0".into()),
        ]
    );
}

#[test]
fn partial_exec_where_common_preserves_keyword_records() {
    let captured = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&captured);
    let engine = engine(
        r#"
fn observe(proc: Process, tree: Tree) (
  proc.exit_code
)

fn build(input: Blob) (
  let common = {
    observer: observe,
    mounts: %{ @src/input.txt => input },
  };

  let cexec = partial exec where common;
  cexec(["/bin/cat", @src/input.txt])
)
"#,
    )
    .with_exec_handler(Arc::new(move |request| {
        *sink.lock().unwrap() = Some(request);
        Ok(Value::U64(0))
    }));

    let value = engine
        .call("build", vec![Value::Blob(BlobValue::from_string("hello"))])
        .unwrap();
    assert_eq!(value, Value::U64(0));

    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("exec request should be captured");
    assert_eq!(request.args.len(), 2);
    assert_eq!(request.mounts.len(), 1);
    assert_eq!(request.mounts[0].path, PathValue::new("src/input.txt"));
}

#[test]
fn array_merge_combines_tree_results_in_current_syntax() {
    let value = eval(
        r#"
fn build(a: Tree, b: Tree) (
  [a, b].merge()
)
"#,
        "build",
    );
    assert!(value.is_err(), "direct eval() should require arguments");

    let engine = engine(
        r#"
fn build(a: Tree, b: Tree) (
  [a, b].merge()
)
"#,
    );
    let left = Value::Tree(TreeValue::Inline(Arc::from([(
        Arc::from("a.o"),
        Value::Blob(BlobValue::from_string("left")),
    )])));
    let right = Value::Tree(TreeValue::Inline(Arc::from([(
        Arc::from("b.o"),
        Value::Blob(BlobValue::from_string("right")),
    )])));
    let value = engine.call("build", vec![left, right]).unwrap();
    let Value::Tree(TreeValue::Inline(entries)) = value else {
        panic!("expected merged tree");
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0.as_ref(), "a.o");
    assert_eq!(entries[1].0.as_ref(), "b.o");
}

#[test]
fn interpolated_path_literal_builds_plain_paths() {
    let engine = engine(r#"fn main(name: String) { @(out/{name}.o) }"#);
    let value = engine
        .call("main", vec![Value::String(Arc::from("lua"))])
        .unwrap();
    assert_eq!(value, Value::Path(PathValue::new("out/lua.o")));
}

#[test]
fn lowering_keeps_interpolated_path_literals_in_function_bodies() {
    let parse = vx_parser::parse(indoc! {r#"
        fn main(root: Path) {
          @({root}/src/./lib.rs)
        }
    "#});
    assert_eq!(parse.errors(), []);
    let file = SourceFile::cast_root(parse.syntax());
    let function = match file.items().next().expect("function item") {
        vx_parser::Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let body = function.body().expect("function body");
    match body.statements().next().expect("path statement") {
        vx_parser::Stmt::Expr(vx_parser::Expr::PathLiteral(_)) => {}
        stmt => panic!("expected parsed path literal, got {stmt:?}"),
    }

    let module = lower_source_file(&file);
    let body = &module.functions[0].body;

    match body.stmts.as_slice() {
        [crate::instr::Stmt::Expr(crate::instr::Expr::PathTemplate { .. })] => {}
        other => panic!("unexpected lowered body: {other:?}"),
    }
}

#[test]
fn lowering_keeps_import_expressions_in_function_bodies() {
    let parse = vx_parser::parse(indoc! {r#"
        fn main() {
          import("./c-rules")
        }
    "#});
    assert_eq!(parse.errors(), []);
    let file = SourceFile::cast_root(parse.syntax());
    let function = match file.items().next().expect("function item") {
        vx_parser::Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let body = function.body().expect("function body");
    match body.statements().next().expect("import statement") {
        vx_parser::Stmt::Expr(vx_parser::Expr::Import(_)) => {}
        stmt => panic!("expected parsed import expr, got {stmt:?}"),
    }

    let module = lower_source_file_with_source_id(&file, "/workspace/build.vx");
    let body = &module.functions[0].body;
    match body.stmts.as_slice() {
        [crate::instr::Stmt::Expr(crate::instr::Expr::Import { path, provenance })] => {
            assert_eq!(path, "./c-rules");
            let provenance = provenance.as_ref().expect("import provenance");
            assert_eq!(provenance.source_id.as_ref(), "/workspace/build.vx");
        }
        other => panic!("unexpected lowered body: {other:?}"),
    }
}

#[test]
fn import_handler_returns_module_namespace_values() {
    let captured = Arc::new(Mutex::new(None::<ImportRequest>));
    let sink = Arc::clone(&captured);
    let imported: crate::instr::Module = engine_with_source_id(
        indoc! {r#"
            let answer = 42;
        "#},
        "/workspace/c-rules.vx",
    )
    .transport_module()
    .into();
    let engine = engine_with_source_id(
        indoc! {r#"
            fn build() {
              let rules = import("./c-rules");
              rules.answer
            }
        "#},
        "/workspace/build.vx",
    )
    .with_import_handler(Arc::new(move |request| {
        *sink.lock().unwrap() = Some(request);
        Ok(imported.clone())
    }));

    let value = engine.call("build", vec![]).unwrap();
    assert_eq!(value, Value::U64(42));

    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("import request should be captured");
    assert_eq!(request.path, "./c-rules");
    assert_eq!(request.from_source_id, "/workspace/build.vx");
}

#[test]
fn lowering_keeps_call_provenance_with_source_spans() {
    let source = indoc! {r#"
        fn main(tree: Tree) {
          tree.file("missing.txt").utf8()
        }
    "#};
    let parse = vx_parser::parse(source);
    assert_eq!(parse.errors(), []);
    let file = SourceFile::cast_root(parse.syntax());
    let function = match file.items().next().expect("function item") {
        vx_parser::Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let parsed_call = match function.body().expect("function body").statements().next() {
        Some(vx_parser::Stmt::Expr(vx_parser::Expr::Call(call))) => call,
        other => panic!("expected parsed call, got {other:?}"),
    };
    let expected_range = parsed_call.syntax().text_range();
    let module = lower_source_file_with_source_id(&file, "fixtures/build.vx");
    let body = &module.functions[0].body;

    let crate::instr::Stmt::Expr(crate::instr::Expr::Call { provenance, .. }) = &body.stmts[0]
    else {
        panic!("expected lowered call expression");
    };
    let provenance = provenance.as_ref().expect("call provenance");
    assert_eq!(provenance.source_id.as_ref(), "fixtures/build.vx");
    assert_eq!(provenance.label, "call(..)");
    assert_eq!(provenance.span.start, u32::from(expected_range.start()));
    assert_eq!(provenance.span.end, u32::from(expected_range.end()));
}

#[test]
fn lowering_keeps_source_ids_for_local_function_provenance() {
    let source = indoc! {r#"
        fn build(ws: Path) {
          fn compile() {
            Tree(ws / @zoo/lua-src)
          }

          compile()
        }
    "#};
    let parse = vx_parser::parse(source);
    assert_eq!(parse.errors(), []);
    let file = SourceFile::cast_root(parse.syntax());
    let module = lower_source_file_with_source_id(&file, "fixtures/build.vx");
    let build_fn = &module.functions[0];

    let crate::instr::Stmt::FnDecl(local_fn) = &build_fn.body.stmts[0] else {
        panic!("expected local function declaration");
    };
    let crate::instr::Stmt::Expr(crate::instr::Expr::Call { provenance, .. }) =
        &local_fn.body.stmts[0]
    else {
        panic!("expected local function body to contain a call");
    };
    let provenance = provenance.as_ref().expect("call provenance");
    assert_eq!(provenance.source_id.as_ref(), "fixtures/build.vx");
}

#[test]
fn lowering_keeps_map_payloads_for_tree_constructor_calls() {
    let parse = vx_parser::parse(indoc! {r#"
        fn main(other: Blob) {
          Tree(%{
            @a.txt => Blob("alpha"),
            @b.txt => other,
          })
        }
    "#});
    assert_eq!(parse.errors(), []);
    let file = SourceFile::cast_root(parse.syntax());
    let function = match file.items().next().expect("function item") {
        vx_parser::Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let body = function.body().expect("function body");
    let parsed_call = match body.statements().next().expect("tree statement") {
        vx_parser::Stmt::Expr(vx_parser::Expr::Call(node)) => node,
        stmt => panic!("expected parsed call, got {stmt:?}"),
    };
    let parsed_args = parsed_call.positional_args();
    assert_eq!(parsed_args.len(), 1);
    match &parsed_args[0] {
        vx_parser::Expr::Paren(paren) => match paren.expr() {
            Some(vx_parser::Expr::Map(map)) => {
                let entries = map.entries().collect::<Vec<_>>();
                assert_eq!(entries.len(), 2);
                assert!(entries.iter().all(|entry| entry.key().is_some()));
                assert!(entries.iter().all(|entry| entry.value().is_some()));
                assert!(matches!(
                    entries[0].key(),
                    Some(vx_parser::Expr::PathLiteral(_))
                ));
                assert!(matches!(
                    entries[1].key(),
                    Some(vx_parser::Expr::PathLiteral(_))
                ));
            }
            other => panic!("expected map payload inside parens, got {other:?}"),
        },
        vx_parser::Expr::Map(map) => {
            let entries = map.entries().collect::<Vec<_>>();
            assert_eq!(entries.len(), 2);
            assert!(entries.iter().all(|entry| entry.key().is_some()));
            assert!(entries.iter().all(|entry| entry.value().is_some()));
            assert!(matches!(
                entries[0].key(),
                Some(vx_parser::Expr::PathLiteral(_))
            ));
            assert!(matches!(
                entries[1].key(),
                Some(vx_parser::Expr::PathLiteral(_))
            ));
        }
        expr => panic!("expected parsed map payload, got {expr:?}"),
    }

    let module = lower_source_file(&file);
    let body = &module.functions[0].body;

    match body.stmts.as_slice() {
        [crate::instr::Stmt::Expr(crate::instr::Expr::Call { args, .. })] => {
            assert_eq!(args.len(), 1);
            match &args[0] {
                crate::instr::Expr::Map(entries) => assert_eq!(entries.len(), 2),
                other => panic!("expected map payload, got {other:?}"),
            }
        }
        other => panic!("expected one call expression, got {other:?}"),
    }
}

#[test]
fn lowering_keeps_where_calls_as_final_expression_statements() {
    let parse = vx_parser::parse(indoc! {r#"
        fn observe(process: Process, tree: Tree) {
          process.exit_code
        }

        fn build() {
          exec(["/bin/true"]) where {
            observer: observe,
          }
        }
    "#});
    assert_eq!(parse.errors(), []);
    let file = SourceFile::cast_root(parse.syntax());
    let build_fn = match file.items().find(|item| match item {
        vx_parser::Item::FnDecl(node) => node.name().is_some_and(|name| name.text() == "build"),
        _ => false,
    }) {
        Some(vx_parser::Item::FnDecl(node)) => node,
        other => panic!("expected build fn, got {other:?}"),
    };
    let parsed_body = build_fn.body().expect("build body");
    match parsed_body.statements().next().expect("where stmt") {
        vx_parser::Stmt::Expr(vx_parser::Expr::Where(_)) => {}
        stmt => panic!("expected parsed where expr, got {stmt:?}"),
    }

    let module = lower_source_file(&file);
    let build = module
        .functions
        .iter()
        .find(|def| def.name == "build")
        .expect("build function");

    match build.body.stmts.as_slice() {
        [
            crate::instr::Stmt::Expr(crate::instr::Expr::Call {
                args,
                kwargs,
                kwargs_expr,
                ..
            }),
        ] => {
            assert_eq!(args.len(), 1);
            assert!(kwargs.is_empty());
            match kwargs_expr.as_deref() {
                Some(crate::instr::Expr::Record(items)) => match items.as_slice() {
                    [crate::instr::RecordItem::Field { name, .. }] => {
                        assert_eq!(name, "observer");
                    }
                    other => panic!("unexpected kwargs record items: {other:?}"),
                },
                other => panic!("unexpected kwargs expr: {other:?}"),
            }
        }
        other => panic!("unexpected lowered body: {other:?}"),
    }
}

#[test]
fn top_level_let_bindings_are_visible_to_exported_functions() {
    let value = eval(
        r#"
let greeting = "hello";
fn main() { greeting }
"#,
        "main",
    )
    .unwrap();

    assert_eq!(value, Value::String(Arc::from("hello")));
}

#[test]
fn let_binding() {
    let val = eval("fn main() { let x = 10; let y = 20; x + y }", "main").unwrap();
    assert_eq!(val, Value::U64(30));
}

#[test]
fn array_spread_flattens_arrays() {
    let value = eval(r#"fn main() { [1, ..[2, 3], 4] }"#, "main").unwrap();
    assert_eq!(
        value,
        Value::Array(Arc::from([
            Value::U64(1),
            Value::U64(2),
            Value::U64(3),
            Value::U64(4),
        ]))
    );
}

#[test]
fn array_spread_forces_pending_trees_shallowly() {
    let value = eval(
        r#"
fn objects() {
  Tree %{ @artifact.o => Blob("x") }
}

fn main() {
  ["ar", ..objects()]
}
"#,
        "main",
    )
    .unwrap();

    let Value::Array(items) = value else {
        panic!("expected array, got {value:?}");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], Value::String(Arc::from("ar")));
    assert!(
        matches!(items[1], Value::ExecPath(_)),
        "expected exec path spread"
    );
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
fn function_parameters_can_omit_type_annotations() {
    let val = eval(
        "fn double(x) { x + x }
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
// r[verify expr.closure]
// r[verify fn.closure]
fn closures_capture_by_value_and_support_block_bodies() {
    let val = eval(
        "fn apply(f: Fn(U64) -> U64, x: U64) { f(x) }
         fn main() {
           let offset = 40;
           let add = (x) => {
             let y = offset + x;
             y
           };
           let offset = 1;
           apply(add, 2)
         }",
        "main",
    )
    .unwrap();
    assert_eq!(val, Value::U64(42));
}

#[test]
// r[verify fn.identity.closure]
fn closure_identity_includes_the_capture_set() {
    let engine = engine("fn make(offset: U64) { (x) => offset + x }");

    let first = engine.call("make", vec![Value::U64(1)]).unwrap();
    let same = engine.call("make", vec![Value::U64(1)]).unwrap();
    let different = engine.call("make", vec![Value::U64(2)]).unwrap();

    let Value::Function(first) = first else {
        panic!("expected closure value");
    };
    let Value::Function(same) = same else {
        panic!("expected closure value");
    };
    let Value::Function(different) = different else {
        panic!("expected closure value");
    };

    assert_eq!(first.identity(), same.identity());
    assert_eq!(first.key(), same.key());
    assert_ne!(first.key(), different.key());
}

#[test]
fn shipped_functions_preserve_global_calls_and_captures() {
    let engine = engine(
        "fn helper(x: U64) { x + 1 }
         fn make(offset: U64) {
           let base = 40;
           (x) => helper(base + offset + x)
         }",
    );

    let module = engine.transport_module();
    let shipped = engine
        .ship_function(engine.call("make", vec![Value::U64(1)]).unwrap())
        .unwrap();
    let value = Engine::invoke_shipped(&module, &shipped, vec![Value::U64(1)]).unwrap();

    assert_eq!(value, Value::U64(43));
}

#[test]
fn inline_values_round_trip_through_transport() {
    let engine = engine(indoc! {r#"
        fn pack(root: Path) {
          %{
            "root" => root,
            "tree" => Tree(%{
              @hello.txt => Blob("hello"),
            }),
            "values" => [41, 42],
          }
        }
    "#});
    let value = engine
        .call("pack", vec![Value::Path(PathValue::new("src"))])
        .unwrap();

    let shipped = engine.ship_value(value.clone()).unwrap();
    let round_trip = Value::try_from(shipped).unwrap();

    assert_eq!(round_trip, value);
    assert!(matches!(
        engine.ship_value(value).unwrap(),
        TransportValue::Map(_)
    ));
}

#[test]
fn store_backed_values_round_trip_through_transport() {
    let engine = engine("fn main() { () }");
    let blob = Value::Blob(BlobValue::Store {
        blob_ref: BlobRef::Small {
            chunk: ChunkHash(Blake3Hash([0x11; 32])),
            size: 3,
        },
        executable: false,
    });
    let tree = Value::Tree(TreeValue::Store(NodeHash(Blake3Hash([0x22; 32]))));

    let shipped_blob = engine.ship_value(blob.clone()).unwrap();
    let shipped_tree = engine.ship_value(tree.clone()).unwrap();

    assert_eq!(Value::try_from(shipped_blob).unwrap(), blob);
    assert_eq!(Value::try_from(shipped_tree).unwrap(), tree);
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
fn user_bindings_can_shadow_exec() {
    let val = eval(
        "fn exec() { 42 }
         fn main() { exec() }",
        "main",
    )
    .unwrap();
    assert_eq!(val, Value::U64(42));
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
fn collect_pending_walks_nested_runtime_values() {
    let value = Value::Array(Arc::from([
        Value::Pending(NodeId(1)),
        Value::ExecPath(ExecPathValue::InTree {
            tree: TreeValue::Inline(Arc::from([(Arc::from("bin"), Value::Pending(NodeId(2)))])),
            suffix: PathValue::new("tool"),
        }),
        Value::Discovered(DiscoveredValue {
            value: Box::new(Value::Map(MapValue {
                entries: Arc::from([(
                    Value::Pending(NodeId(3)),
                    Value::Builtin(BuiltinValue::Method(BuiltinMethod::ArrayMerge {
                        receiver: Box::new(Value::Pending(NodeId(4))),
                    })),
                )]),
            })),
            obligations: Arc::from([]),
        }),
        Value::Partial(Arc::new(PartialValue {
            function: Value::Pending(NodeId(5)),
            args: Arc::from([Value::Function(FunctionValue {
                module_id: Arc::from("/tests/capture.vx"),
                def: Arc::new(FnDef {
                    name: "capture".into(),
                    params: vec![],
                    keyword_params: vec![],
                    body: Block { stmts: Vec::new() },
                }),
                code_identity: Arc::from("capture"),
                captures: Arc::from([(Arc::from("value"), Value::Pending(NodeId(6)))]),
            })]),
            kwargs: Arc::from([(Arc::from("extra"), Value::Pending(NodeId(7)))]),
            kwargs_expr: None,
        })),
    ]));

    let mut deps = Vec::new();
    value.collect_pending(&mut deps);

    assert_eq!(sorted_node_ids(deps), vec![1, 2, 3, 4, 5, 6, 7]);
}

#[test]
fn node_job_pending_deps_cover_templates_calls_and_invokes() {
    let template = NodeJob::Template {
        kind: TemplateKind::String,
        parts: Arc::from([
            TemplatePartValue::Text(Arc::from("hello ")),
            TemplatePartValue::Expr(Value::Pending(NodeId(8))),
            TemplatePartValue::Expr(Value::Array(Arc::from([Value::Pending(NodeId(9))]))),
        ]),
    };
    assert_eq!(sorted_node_ids(template.pending_deps()), vec![8, 9]);

    let call = NodeJob::Call {
        function: Value::Builtin(BuiltinValue::Method(BuiltinMethod::ArrayMap {
            receiver: Box::new(Value::Pending(NodeId(10))),
        })),
        args: Arc::from([
            Value::Pending(NodeId(11)),
            Value::Array(Arc::from([Value::Pending(NodeId(12))])),
        ]),
        kwargs: Arc::from([(Arc::from("named"), Value::Pending(NodeId(13)))]),
        kwargs_expr: Some(Value::Pending(NodeId(14))),
    };
    assert_eq!(
        sorted_node_ids(call.pending_deps()),
        vec![10, 11, 12, 13, 14]
    );

    let invoke = NodeJob::Invoke {
        function: FunctionValue {
            module_id: Arc::from("/tests/invoke.vx"),
            def: Arc::new(FnDef {
                name: "invoke".into(),
                params: vec![],
                keyword_params: vec![],
                body: Block { stmts: Vec::new() },
            }),
            code_identity: Arc::from("invoke"),
            captures: Arc::from([(Arc::from("captured"), Value::Pending(NodeId(15)))]),
        },
        args: Arc::from([Value::Pending(NodeId(16))]),
    };
    assert_eq!(sorted_node_ids(invoke.pending_deps()), vec![15, 16]);
}

#[test]
fn node_job_if_only_depends_on_the_condition() {
    let job = NodeJob::If {
        cond: Value::Pending(NodeId(17)),
        then_branch: Block { stmts: Vec::new() },
        else_branch: Block { stmts: Vec::new() },
        bindings: Arc::new(HashMap::from([(
            "ignored".into(),
            Value::Pending(NodeId(18)),
        )])),
        scope_module_id: Arc::from("/tests/if.vx"),
        scope_identity: Arc::from("<test>"),
    };

    assert_eq!(sorted_node_ids(job.pending_deps()), vec![17]);
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
fn array_concat_keeps_pending_children_lazy() {
    let engine = engine("fn append(xs: Array[U64], ys: Array[U64]) { xs ++ ys }");
    let left = Value::Array(Arc::from([
        Value::U64(1),
        engine.poison_value("pending child"),
    ]));
    let right = Value::Array(Arc::from([Value::U64(3)]));
    let call = engine.call_unforced("append", vec![left, right]).unwrap();

    let mut result = engine.force_shallow(call).unwrap();
    while matches!(result, Value::Pending(_)) {
        result = engine.force_shallow(result).unwrap();
    }
    let Value::Array(items) = &result else {
        panic!("expected concatenation to produce an array");
    };
    assert_eq!(items.len(), 3);
    assert_eq!(items[0], Value::U64(1));
    assert!(matches!(items[1], Value::Pending(_)));
    assert_eq!(items[2], Value::U64(3));

    let err = engine.force_fully(result).unwrap_err();
    assert_eq!(err.kind, PoisonKind::UserFail);
}

#[test]
fn array_concat_map_merge_resolves_pending_tree_items() {
    let engine = engine(
        r#"
fn object(file: Path) {
  Tree %{ @(out/{file}.o) => Blob("ok") }
}

fn main(extra: Array[Path]) {
  let sources = [@core1.c, @core2.c] ++ extra;
  sources.map(object).merge()
}
"#,
    );

    let call = engine
        .call_unforced(
            "main",
            vec![Value::Array(Arc::from([Value::Path(PathValue::new(
                "arch1.c",
            ))]))],
        )
        .unwrap();

    let value = engine.force_fully(call).unwrap();
    let Value::Tree(TreeValue::Inline(entries)) = value else {
        panic!("expected merged tree");
    };

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0.as_ref(), "out");
    let Value::Tree(TreeValue::Inline(out_entries)) = &entries[0].1 else {
        panic!("expected merged out subtree");
    };
    assert_eq!(out_entries.len(), 3);
    assert_eq!(out_entries[0].0.as_ref(), "arch1.c.o");
    assert_eq!(out_entries[1].0.as_ref(), "core1.c.o");
    assert_eq!(out_entries[2].0.as_ref(), "core2.c.o");
}

#[test]
fn array_concat_map_merge_resolves_exec_tree_results() {
    let engine = engine(
        r#"
fn observe(process: Process, tree: Tree) {
  tree
}

fn compile(file: Path) {
  exec ["tool", file] where { observer: observe }
}

fn main(extra: Array[Path]) {
  let sources = [@core1.c, @core2.c] ++ extra;
  sources.map(compile).merge()
}
"#,
    )
    .with_exec_handler(Arc::new(|request| {
        let [ExecArgValue::Literal(program), ExecArgValue::Path(path)] = request.args.as_slice()
        else {
            panic!("expected tool plus one path arg");
        };
        assert_eq!(program, "tool");
        let file = path
            .as_str()
            .rsplit('/')
            .next()
            .expect("path argument should have a filename")
            .to_string();
        Ok(Value::Tree(TreeValue::Inline(Arc::from([(
            Arc::from("out"),
            Value::Tree(TreeValue::Inline(Arc::from([(
                Arc::from(format!("{file}.o")),
                Value::Blob(BlobValue::from_string("ok")),
            )]))),
        )]))))
    }));

    let call = engine
        .call_unforced(
            "main",
            vec![Value::Array(Arc::from([Value::Path(PathValue::new(
                "arch1.c",
            ))]))],
        )
        .unwrap();

    let value = engine.force_fully(call).unwrap();
    let Value::Tree(TreeValue::Inline(entries)) = value else {
        panic!("expected merged tree");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0.as_ref(), "out");
    let Value::Tree(TreeValue::Inline(out_entries)) = &entries[0].1 else {
        panic!("expected merged out subtree");
    };
    assert_eq!(out_entries.len(), 3);
    assert_eq!(out_entries[0].0.as_ref(), "arch1.c.o");
    assert_eq!(out_entries[1].0.as_ref(), "core1.c.o");
    assert_eq!(out_entries[2].0.as_ref(), "core2.c.o");
}

#[test]
fn array_concat_map_merge_resolves_partial_exec_tree_results() {
    let engine = engine(
        r#"
fn observe(process: Process, tree: Tree) {
  tree
}

fn main(extra: Array[Path]) {
  let cexec = partial exec where { observer: observe };
  let compile = (file) => cexec ["tool", file];
  let sources = [@core1.c, @core2.c] ++ extra;
  sources.map(compile).merge()
}
"#,
    )
    .with_exec_handler(Arc::new(|request| {
        let [ExecArgValue::Literal(program), ExecArgValue::Path(path)] = request.args.as_slice()
        else {
            panic!("expected tool plus one path arg");
        };
        assert_eq!(program, "tool");
        let file = path
            .as_str()
            .rsplit('/')
            .next()
            .expect("path argument should have a filename")
            .to_string();
        Ok(Value::Tree(TreeValue::Inline(Arc::from([(
            Arc::from("out"),
            Value::Tree(TreeValue::Inline(Arc::from([(
                Arc::from(format!("{file}.o")),
                Value::Blob(BlobValue::from_string("ok")),
            )]))),
        )]))))
    }));

    let call = engine
        .call_unforced(
            "main",
            vec![Value::Array(Arc::from([Value::Path(PathValue::new(
                "arch1.c",
            ))]))],
        )
        .unwrap();

    let value = engine.force_fully(call).unwrap();
    let Value::Tree(TreeValue::Inline(entries)) = value else {
        panic!("expected merged tree");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0.as_ref(), "out");
    let Value::Tree(TreeValue::Inline(out_entries)) = &entries[0].1 else {
        panic!("expected merged out subtree");
    };
    assert_eq!(out_entries.len(), 3);
    assert_eq!(out_entries[0].0.as_ref(), "arch1.c.o");
    assert_eq!(out_entries[1].0.as_ref(), "core1.c.o");
    assert_eq!(out_entries[2].0.as_ref(), "core2.c.o");
}

#[test]
fn array_concat_map_merge_with_discovered_tool_exec_results_resolves() {
    let engine = engine(
        r#"
struct Tool {
  path: String,
  hash: Blob,
}

namespace Tool {
  fn to_string(self) -> String self.path;
}

fn observe(process: Process, tree: Tree) {
  tree
}

fn main(extra: Array[Path], cc: Tool) {
  let cexec = partial exec where { observer: observe };
  let compile = (file) => cexec [cc, "-c", file, "-o", @(out/{file}.o)];
  let sources = [@core1.c, @core2.c] ++ extra;
  sources.map(compile).merge()
}
"#,
    )
    .with_exec_handler(Arc::new(|request| {
        let output = request
            .args
            .windows(2)
            .find_map(|window| match window {
                [ExecArgValue::Literal(flag), ExecArgValue::Path(path)] if flag == "-o" => {
                    Some(path.clone())
                }
                _ => None,
            })
            .expect("expected -o output path");
        let file = output
            .as_str()
            .rsplit('/')
            .next()
            .expect("output path should have a filename")
            .to_string();
        Ok(Value::Tree(TreeValue::Inline(Arc::from([(
            Arc::from("out"),
            Value::Tree(TreeValue::Inline(Arc::from([(
                Arc::from(file),
                Value::Blob(BlobValue::from_string("ok")),
            )]))),
        )]))))
    }));

    let cc = Value::Discovered(DiscoveredValue {
        value: Box::new(Value::Struct(StructValue {
            type_name: Arc::from("Tool"),
            fields: Arc::from([
                (Arc::from("path"), Value::String(Arc::from("/usr/bin/cc"))),
                (
                    Arc::from("hash"),
                    Value::Blob(BlobValue::from_string("cc-hash")),
                ),
            ]),
        })),
        obligations: Arc::from([
            HostObservation::Which {
                name: "cc".to_string(),
                expected_path: Some("/usr/bin/cc".to_string()),
            },
            HostObservation::Hash {
                path: "/usr/bin/cc".to_string(),
                content_hash: "cc-hash".to_string(),
            },
        ]),
    });

    let value = engine
        .call(
            "main",
            vec![
                Value::Array(Arc::from([Value::Path(PathValue::new("arch1.c"))])),
                cc,
            ],
        )
        .unwrap();
    let Value::Tree(TreeValue::Inline(entries)) = value else {
        panic!("expected merged tree");
    };
    assert_eq!(entries.len(), 1);
}

#[test]
fn compile_objects_shape_with_concat_exec_and_mount_spreads_resolves() {
    let engine = engine(
        r#"
let common_cflags = ["-O2", "-fPIC"];

fn observe(process: Process, tree: Tree) {
  tree
}

fn compile_objects(src: Tree, sources: Array[Path]) where {
  generated: Map[Path, Blob] = {},
  mounts: Map[Path, TreeEntry] = {},
  includes: Array[Path] = [],
  defines: Array[String] = [],
  flags: Array[String] = [],
} -> Tree (
  let cexec = partial exec where { observer: observe };
  sources
    .map((file) =>
      cexec [
        "tool",
        "-c",
        @(src/{file}),
        "-o",
        @(out/{file}.o),
        ..includes.map((include) => ["-I", include]).flatten(),
        ..defines.map((define) => `-D${define}`),
        ..common_cflags,
        ..flags,
      ] where {
        mounts: %{ @src => src, @gen => Tree(generated), ..mounts },
      }
    )
    .merge()
);

fn main(extra: Array[Path]) {
  let src = Tree %{
    @core1.c => Blob("1"),
    @core2.c => Blob("2"),
    @arch1.c => Blob("3"),
  };

  compile_objects(src, [@core1.c, @core2.c] ++ extra) where {
    generated: %{ @config.h => Blob("generated") },
    includes: [@gen, @src],
    defines: ["FOO=1"],
  }
}
"#,
    )
    .with_exec_handler(Arc::new(|request| {
        let output = request
            .args
            .iter()
            .zip(request.args.iter().skip(1))
            .find_map(|(arg, next)| match (arg, next) {
                (ExecArgValue::Literal(flag), ExecArgValue::Path(path)) if flag == "-o" => {
                    Some(path.clone())
                }
                _ => None,
            })
            .expect("expected -o output path");
        let file = output
            .as_str()
            .rsplit('/')
            .next()
            .expect("output path should have a filename")
            .to_string();
        Ok(Value::Tree(TreeValue::Inline(Arc::from([(
            Arc::from("out"),
            Value::Tree(TreeValue::Inline(Arc::from([(
                Arc::from(file),
                Value::Blob(BlobValue::from_string("ok")),
            )]))),
        )]))))
    }));

    let call = engine
        .call_unforced(
            "main",
            vec![Value::Array(Arc::from([Value::Path(PathValue::new(
                "arch1.c",
            ))]))],
        )
        .unwrap();

    let value = engine.force_fully(call).unwrap();
    let Value::Tree(TreeValue::Inline(entries)) = value else {
        panic!("expected merged tree");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0.as_ref(), "out");
    let Value::Tree(TreeValue::Inline(out_entries)) = &entries[0].1 else {
        panic!("expected merged out subtree");
    };
    assert_eq!(out_entries.len(), 3);
    assert_eq!(out_entries[0].0.as_ref(), "arch1.c.o");
    assert_eq!(out_entries[1].0.as_ref(), "core1.c.o");
    assert_eq!(out_entries[2].0.as_ref(), "core2.c.o");
}

#[test]
fn compile_objects_shape_with_discovered_tool_and_mount_spreads_resolves() {
    let engine = engine(
        r#"
struct Tool {
  path: String,
  hash: Blob,
}

namespace Tool {
  fn to_string(self) -> String self.path;
}

let common_cflags = ["-O2", "-fPIC"];

fn observe(process: Process, tree: Tree) {
  tree
}

fn compile_objects(cc: Tool, src: Tree, sources: Array[Path]) where {
  generated: Map[Path, Blob] = {},
  mounts: Map[Path, TreeEntry] = {},
  includes: Array[Path] = [],
  defines: Array[String] = [],
  flags: Array[String] = [],
} -> Tree (
  let cexec = partial exec where { observer: observe };
  sources
    .map((file) =>
      cexec [
        cc,
        "-c",
        @(src/{file}),
        "-o",
        @(out/{file}.o),
        ..includes.map((include) => ["-I", include]).flatten(),
        ..defines.map((define) => `-D${define}`),
        ..common_cflags,
        ..flags,
      ] where {
        mounts: %{ @src => src, @gen => Tree(generated), ..mounts },
      }
    )
    .merge()
);

fn main(extra: Array[Path], cc: Tool) {
  let src = Tree %{
    @core1.c => Blob("1"),
    @core2.c => Blob("2"),
    @arch1.c => Blob("3"),
  };

  compile_objects(cc, src, [@core1.c, @core2.c] ++ extra) where {
    generated: %{ @config.h => Blob("generated") },
    includes: [@gen, @src],
    defines: ["FOO=1"],
  }
}
"#,
    )
    .with_exec_handler(Arc::new(|request| {
        let output = request
            .args
            .iter()
            .zip(request.args.iter().skip(1))
            .find_map(|(arg, next)| match (arg, next) {
                (ExecArgValue::Literal(flag), ExecArgValue::Path(path)) if flag == "-o" => {
                    Some(path.clone())
                }
                _ => None,
            })
            .expect("expected -o output path");
        let file = output
            .as_str()
            .rsplit('/')
            .next()
            .expect("output path should have a filename")
            .to_string();
        Ok(Value::Tree(TreeValue::Inline(Arc::from([(
            Arc::from("out"),
            Value::Tree(TreeValue::Inline(Arc::from([(
                Arc::from(file),
                Value::Blob(BlobValue::from_string("ok")),
            )]))),
        )]))))
    }));

    let cc = Value::Discovered(DiscoveredValue {
        value: Box::new(Value::Struct(StructValue {
            type_name: Arc::from("Tool"),
            fields: Arc::from([
                (Arc::from("path"), Value::String(Arc::from("/usr/bin/cc"))),
                (
                    Arc::from("hash"),
                    Value::Blob(BlobValue::from_string("cc-hash")),
                ),
            ]),
        })),
        obligations: Arc::from([
            HostObservation::Which {
                name: "cc".to_string(),
                expected_path: Some("/usr/bin/cc".to_string()),
            },
            HostObservation::Hash {
                path: "/usr/bin/cc".to_string(),
                content_hash: "cc-hash".to_string(),
            },
        ]),
    });

    let value = engine
        .call(
            "main",
            vec![
                Value::Array(Arc::from([Value::Path(PathValue::new("arch1.c"))])),
                cc,
            ],
        )
        .unwrap();
    let Value::Tree(TreeValue::Inline(entries)) = value else {
        panic!("expected merged tree");
    };
    assert_eq!(entries.len(), 1);
}

#[test]
fn compile_objects_shape_with_match_rhs_concat_resolves() {
    let engine = engine(
        r#"
let common_cflags = ["-O2", "-fPIC"];

fn observe(process: Process, tree: Tree) {
  tree
}

fn compile_objects(src: Tree, sources: Array[Path]) where {
  includes: Array[Path] = [],
} -> Tree (
  let cexec = partial exec where { observer: observe };
  sources
    .map((file) =>
      cexec [
        "tool",
        "-c",
        @(src/{file}),
        "-o",
        @(out/{file}.o),
        ..includes.map((include) => ["-I", include]).flatten(),
        ..common_cflags,
      ] where {
        mounts: %{ @src => src },
      }
    )
    .merge()
);

fn main() {
  let target =
    exec ["tool", "-dumpmachine"] where {
      observer: (proc, _vfs) => proc.stdout.collect().utf8().trim(),
    };
  let src = Tree %{
    @core1.c => Blob("1"),
    @core2.c => Blob("2"),
    @arch1.c => Blob("3"),
  };
  let extra =
    match target (
      "x86_64-unknown-linux-gnu" => [@arch1.c],
      _ => [],
    );

  compile_objects(src, [@core1.c, @core2.c] ++ extra) where {
    includes: [@src],
  }
}
"#,
    )
    .with_exec_handler(Arc::new(|request| {
        if matches!(
            request.args.as_slice(),
            [ExecArgValue::Literal(program), ExecArgValue::Literal(flag)]
                if program == "tool" && flag == "-dumpmachine"
        ) {
            return Ok(Value::String(Arc::from("x86_64-unknown-linux-gnu")));
        }
        let output = request
            .args
            .iter()
            .zip(request.args.iter().skip(1))
            .find_map(|(arg, next)| match (arg, next) {
                (ExecArgValue::Literal(flag), ExecArgValue::Path(path)) if flag == "-o" => {
                    Some(path.clone())
                }
                _ => None,
            })
            .expect("expected -o output path");
        let file = output
            .as_str()
            .rsplit('/')
            .next()
            .expect("output path should have a filename")
            .to_string();
        Ok(Value::Tree(TreeValue::Inline(Arc::from([(
            Arc::from("out"),
            Value::Tree(TreeValue::Inline(Arc::from([(
                Arc::from(file),
                Value::Blob(BlobValue::from_string("ok")),
            )]))),
        )]))))
    }));

    let value = engine.call("main", vec![]).unwrap();
    let Value::Tree(TreeValue::Inline(entries)) = value else {
        panic!("expected merged tree");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0.as_ref(), "out");
    let Value::Tree(TreeValue::Inline(out_entries)) = &entries[0].1 else {
        panic!("expected merged out subtree");
    };
    assert_eq!(out_entries.len(), 3);
    assert_eq!(out_entries[0].0.as_ref(), "arch1.c.o");
    assert_eq!(out_entries[1].0.as_ref(), "core1.c.o");
    assert_eq!(out_entries[2].0.as_ref(), "core2.c.o");
}

#[test]
fn array_merge_over_nested_subtree_exec_results_resolves() {
    let engine = engine(
        r#"
fn observe(process: Process, tree: Tree) {
  tree.subtree(@out)
}

fn compile(file: Path) {
  let cexec = partial exec where { observer: observe };
  cexec ["tool", "-c", file, "-o", @(out/{file}.o)]
}

fn main() {
  let sources = [@core1.c, @intel/intel_init.c];
  sources.map(compile).merge()
}
"#,
    )
    .with_exec_handler(Arc::new(|request| {
        let output = request
            .args
            .iter()
            .zip(request.args.iter().skip(1))
            .find_map(|(arg, next)| match (arg, next) {
                (ExecArgValue::Literal(flag), ExecArgValue::Path(path)) if flag == "-o" => {
                    Some(path.clone())
                }
                _ => None,
            })
            .expect("expected -o output path");
        let relative = output
            .as_str()
            .strip_prefix("out/")
            .expect("output path should be rooted in out/")
            .to_string();
        Ok(inline_tree_with_blob(&relative, "ok"))
    }));

    let call = engine.call_unforced("main", vec![]).unwrap();
    let value = engine.force_fully(call).unwrap();

    assert_eq!(
        collect_tree_leaf_paths(&value),
        vec!["core1.c.o".to_string(), "intel/intel_init.c.o".to_string()]
    );
}

#[test]
fn static_archive_shape_with_tool_to_string_and_tree_spread_resolves() {
    let engine = engine(
        r#"
struct Tool {
  path: String,
}

namespace Tool {
  fn to_string(self) -> String self.path;
}

fn observe(process: Process, tree: Tree) {
  tree
}

fn compile(file: Path) {
  exec ["compile", file] where { observer: observe }
}

fn static_archive(ar: Tool, objects: Tree) {
  exec [ar, "rcs", @out/lib.a, ..objects] where { observer: observe }
}

fn main(extra: Array[Path]) {
  let ar = Tool { path: "archive-tool" };
  let sources = [@core1.c, @core2.c] ++ extra;
  let objects = sources.map(compile).merge();
  let archive = static_archive(ar, objects);
  archive.file(@lib.a)
}
"#,
    )
    .with_exec_handler(Arc::new(|request| {
        let program = match request.args.first() {
            Some(ExecArgValue::Literal(program)) => program.as_str(),
            other => panic!("expected literal program, got {other:?}"),
        };
        match program {
            "compile" => {
                let [_, ExecArgValue::Path(path)] = request.args.as_slice() else {
                    panic!("expected compile plus one path arg");
                };
                let file = path
                    .as_str()
                    .rsplit('/')
                    .next()
                    .expect("path argument should have a filename")
                    .to_string();
                Ok(Value::Tree(TreeValue::Inline(Arc::from([(
                    Arc::from("obj"),
                    Value::Tree(TreeValue::Inline(Arc::from([(
                        Arc::from(format!("{file}.o")),
                        Value::Blob(BlobValue::from_string("obj")),
                    )]))),
                )]))))
            }
            "archive-tool" => Ok(Value::Tree(TreeValue::Inline(Arc::from([(
                Arc::from("lib.a"),
                Value::Blob(BlobValue::from_string("archive")),
            )])))),
            other => panic!("unexpected program {other}"),
        }
    }));

    let value = engine
        .call(
            "main",
            vec![Value::Array(Arc::from([Value::Path(PathValue::new(
                "arch1.c",
            ))]))],
        )
        .unwrap();

    let Value::Blob(BlobValue::Inline { bytes, executable }) = value else {
        panic!("expected hydrated archive blob, got {:?}", value);
    };
    assert!(!executable);
    assert_eq!(bytes.as_ref(), b"archive");
}

#[test]
fn static_archive_shape_with_store_backed_exec_results_resolves() {
    let stored_trees = Arc::new(Mutex::new(HashMap::<NodeHash, Value>::new()));
    let store_sink = Arc::clone(&stored_trees);
    let engine = engine(
        r#"
struct Tool {
  path: String,
}

namespace Tool {
  fn to_string(self) -> String self.path;
}

fn observe(process: Process, tree: Tree) {
  tree
}

fn compile(file: Path) {
  exec ["compile", file] where { observer: observe }
}

fn static_archive(ar: Tool, objects: Tree) {
  exec [ar, "rcs", @out/lib.a, ..objects] where { observer: observe }
}

fn main(extra: Array[Path]) {
  let ar = Tool { path: "archive-tool" };
  let sources = [@core1.c, @core2.c] ++ extra;
  let objects = sources.map(compile).merge();
  let archive = static_archive(ar, objects);
  archive.file(@lib.a)
}
"#,
    )
    .with_store_hydration_handler(Arc::new(move |request| {
        let Value::Tree(TreeValue::Store(root)) = request.value else {
            panic!("unexpected hydration request: {:?}", request.value);
        };
        store_sink
            .lock()
            .unwrap()
            .get(&root)
            .cloned()
            .ok_or_else(|| {
                PoisonError::new(
                    PoisonKind::Internal,
                    format!("missing stored tree {root}"),
                    request.demand_chain,
                )
            })
    }))
    .with_exec_handler(Arc::new({
        let stored_trees = Arc::clone(&stored_trees);
        move |request| {
            let program = match request.args.first() {
                Some(ExecArgValue::Literal(program)) => program.as_str(),
                other => panic!("expected literal program, got {other:?}"),
            };
            let root = match program {
                "compile" => {
                    let [_, ExecArgValue::Path(path)] = request.args.as_slice() else {
                        panic!("expected compile plus one path arg");
                    };
                    let file = path
                        .as_str()
                        .rsplit('/')
                        .next()
                        .expect("path argument should have a filename")
                        .to_string();
                    let root = NodeHash(Blake3Hash::from_bytes(file.as_bytes()));
                    stored_trees.lock().unwrap().insert(
                        root,
                        Value::Tree(TreeValue::Inline(Arc::from([(
                            Arc::from("obj"),
                            Value::Tree(TreeValue::Inline(Arc::from([(
                                Arc::from(format!("{file}.o")),
                                Value::Blob(BlobValue::from_string("obj")),
                            )]))),
                        )]))),
                    );
                    root
                }
                "archive-tool" => {
                    let root = NodeHash(Blake3Hash::from_bytes(b"archive-root"));
                    stored_trees.lock().unwrap().insert(
                        root,
                        Value::Tree(TreeValue::Inline(Arc::from([(
                            Arc::from("lib.a"),
                            Value::Blob(BlobValue::from_string("archive")),
                        )]))),
                    );
                    root
                }
                other => panic!("unexpected program {other}"),
            };
            Ok(Value::Tree(TreeValue::Store(root)))
        }
    }));

    let value = engine
        .call(
            "main",
            vec![Value::Array(Arc::from([Value::Path(PathValue::new(
                "arch1.c",
            ))]))],
        )
        .unwrap();

    let Value::Blob(BlobValue::Inline { bytes, executable }) = value else {
        panic!("expected hydrated archive blob, got {:?}", value);
    };
    assert!(!executable);
    assert_eq!(bytes.as_ref(), b"archive");
}

#[test]
fn local_exec_partial_archive_shape_with_concat_resolves() {
    let engine = engine(
        r#"
struct Tool {
  path: String,
}

namespace Tool {
  fn to_string(self) -> String self.path;
}

fn observe(process: Process, tree: Tree) {
  tree
}

fn compile(file: Path) {
  exec [
    "/bin/sh",
    "-c",
    "mkdir -p out && printf obj > \"out/$(basename \"$1\").o\"",
    "sh",
    file,
  ] where { observer: observe }
}

fn static_archive(ar: Tool, objects: Tree) {
  exec [
    ar,
    "-c",
    "mkdir -p out && printf archive > out/lib.a",
    "sh",
    ..objects,
  ] where { observer: observe }
}

fn main(extra: Array[Path]) {
  let ar = Tool { path: "/bin/sh" };
  let sources = [@core1.c, @core2.c] ++ extra;
  let objects = sources.map(compile).merge();
  static_archive(ar, objects)
}
"#,
    );

    let value = engine
        .call(
            "main",
            vec![Value::Array(Arc::from([Value::Path(PathValue::new(
                "arch1.c",
            ))]))],
        )
        .unwrap();

    let paths = collect_tree_leaf_paths(&value);
    assert_eq!(paths.len(), 4);
    assert!(paths.iter().any(|path| path == "out/lib.a"));
    assert!(paths.iter().any(|path| path.ends_with("/out/core1.c.o")));
    assert!(paths.iter().any(|path| path.ends_with("/out/core2.c.o")));
    assert!(paths.iter().any(|path| path.ends_with("/out/arch1.c.o")));
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
fn missing_file_failures_keep_source_provenance_in_demand_chain() {
    let source = r#"fn main(tree: Tree) { tree.file("missing.txt").utf8() }"#;
    let parse = vx_parser::parse(source);
    assert_eq!(parse.errors(), []);
    let file = SourceFile::cast_root(parse.syntax());
    let function = match file.items().next().expect("function item") {
        vx_parser::Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let parsed_outer_call = match function.body().expect("function body").statements().next() {
        Some(vx_parser::Stmt::Expr(vx_parser::Expr::Call(call))) => call,
        other => panic!("expected parsed call, got {other:?}"),
    };
    let outer_range = parsed_outer_call.syntax().text_range();
    let engine = engine_with_source_id(source, "/workspace/build.vx");
    let tree = Value::Tree(TreeValue::Inline(Arc::from([])));
    let err = engine.call("main", vec![tree]).unwrap_err();

    assert!(err.message.contains("missing file"));
    assert!(!err.demand_chain.entries.is_empty());

    let call_entry = err
        .demand_chain
        .entries
        .iter()
        .find(|entry| entry.description == "call(..)")
        .expect("call demand entry");
    let provenance = call_entry.provenance.as_ref().expect("call provenance");
    assert_eq!(provenance.source_id.as_ref(), "/workspace/build.vx");
    assert_eq!(provenance.label, "call(..)");
    assert_eq!(provenance.span.start, u32::from(outer_range.start()));
    assert_eq!(provenance.span.end, u32::from(outer_range.end()));
}

#[test]
fn exec_handlers_receive_source_provenance_in_demand_chain() {
    let source = r#"
fn observe(process: Process, tree: Tree) { process.exit_code }

fn build(input: Blob) {
  exec ["/bin/true", input] where { observer: observe }
}
"#;
    let parse = vx_parser::parse(source);
    assert_eq!(parse.errors(), []);
    let file = SourceFile::cast_root(parse.syntax());
    let function = match file.items().nth(1).expect("build function item") {
        vx_parser::Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let outer_range = match function.body().expect("function body").statements().next() {
        Some(vx_parser::Stmt::Expr(vx_parser::Expr::Where(where_expr))) => {
            where_expr.syntax().text_range()
        }
        other => panic!("expected parsed where expr, got {other:?}"),
    };
    let captured = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&captured);
    let engine = engine_with_source_id(source, "/workspace/build.vx").with_exec_handler(Arc::new(
        move |request| {
            *sink.lock().unwrap() = Some(request);
            Ok(Value::U64(0))
        },
    ));

    let value = engine
        .call("build", vec![Value::Blob(BlobValue::from_string("hello"))])
        .unwrap();
    assert_eq!(value, Value::U64(0));

    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("exec request should be captured");
    let call_entry = request
        .demand_chain
        .entries
        .iter()
        .find(|entry| entry.description == "call(..)")
        .expect("call demand entry");
    let provenance = call_entry.provenance.as_ref().expect("call provenance");
    assert_eq!(provenance.source_id.as_ref(), "/workspace/build.vx");
    assert_eq!(provenance.label, "call(..)");
    assert_eq!(provenance.span.start, u32::from(outer_range.start()));
    assert_eq!(provenance.span.end, u32::from(outer_range.end()));
}

#[test]
fn discover_handlers_receive_source_provenance_in_demand_chain() {
    let source = r#"
fn probe() {
  discover(executor_class("default"), (host) => host.read("/tmp/tool"))
}
"#;
    let parse = vx_parser::parse(source);
    assert_eq!(parse.errors(), []);
    let file = SourceFile::cast_root(parse.syntax());
    let function = match file.items().next().expect("probe function item") {
        vx_parser::Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let parsed_outer_call = match function.body().expect("function body").statements().next() {
        Some(vx_parser::Stmt::Expr(vx_parser::Expr::Call(call))) => call,
        other => panic!("expected parsed call, got {other:?}"),
    };
    let outer_range = parsed_outer_call.syntax().text_range();
    let captured = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&captured);
    let engine = engine_with_source_id(source, "/workspace/build.vx").with_discover_handler(
        Arc::new(move |request| {
            *sink.lock().unwrap() = Some(request);
            Ok(Value::Blob(BlobValue::from_string("probe result")))
        }),
    );

    let value = engine.call("probe", vec![]).unwrap();
    assert_eq!(value, Value::Blob(BlobValue::from_string("probe result")));

    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("discover request should be captured");
    let call_entry = request
        .demand_chain
        .entries
        .iter()
        .find(|entry| entry.description == "call(..)")
        .expect("call demand entry");
    let provenance = call_entry.provenance.as_ref().expect("call provenance");
    assert_eq!(provenance.source_id.as_ref(), "/workspace/build.vx");
    assert_eq!(provenance.label, "call(..)");
    assert_eq!(provenance.span.start, u32::from(outer_range.start()));
    assert_eq!(provenance.span.end, u32::from(outer_range.end()));
}

#[test]
fn acquire_handlers_receive_keyword_payloads_and_source_provenance() {
    let source = r#"
fn fetch() {
  acquire("https://example.test/src.tar.gz") where {
    digest: "sha256:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
    extract: "tar.gz",
  }
}
"#;
    let parse = vx_parser::parse(source);
    assert_eq!(parse.errors(), []);
    let file = SourceFile::cast_root(parse.syntax());
    let function = match file.items().next().expect("fetch function item") {
        vx_parser::Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let outer_range = match function.body().expect("function body").statements().next() {
        Some(vx_parser::Stmt::Expr(vx_parser::Expr::Where(where_expr))) => {
            where_expr.syntax().text_range()
        }
        other => panic!("expected parsed where expr, got {other:?}"),
    };

    let captured = Arc::new(Mutex::new(None::<AcquireRequest>));
    let sink = Arc::clone(&captured);
    let expected = Value::Blob(BlobValue::from_string("cached source"));
    let engine = engine_with_source_id(source, "/workspace/build.vx").with_acquire_handler(
        Arc::new(move |request| {
            *sink.lock().unwrap() = Some(request);
            Ok(expected.clone())
        }),
    );

    let value = engine.call("fetch", vec![]).unwrap();
    assert_eq!(value, Value::Blob(BlobValue::from_string("cached source")));

    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("acquire request should be captured");
    assert_eq!(request.url, "https://example.test/src.tar.gz");
    assert_eq!(
        request.digest,
        "sha256:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
    );
    assert_eq!(request.extract.as_deref(), Some("tar.gz"));

    let call_entry = request
        .demand_chain
        .entries
        .iter()
        .find(|entry| entry.description == "call(..)")
        .expect("call demand entry");
    let provenance = call_entry.provenance.as_ref().expect("call provenance");
    assert_eq!(provenance.source_id.as_ref(), "/workspace/build.vx");
    assert_eq!(provenance.label, "call(..)");
    assert_eq!(provenance.span.start, u32::from(outer_range.start()));
    assert_eq!(provenance.span.end, u32::from(outer_range.end()));
}

#[test]
fn render_handlers_receive_template_bindings_and_source_provenance() {
    let source = r#"
fn render_header() {
  return render(@templates/header.h) where {
    bindings: %{
      "NAME" => "libpng",
      "ENABLED" => true,
    },
  };
}
"#;
    let parse = vx_parser::parse(source);
    assert_eq!(parse.errors(), []);
    let file = SourceFile::cast_root(parse.syntax());
    let module = lower_source_file_with_source_id(&file, "/workspace/build.vx");
    assert!(
        matches!(
            module.functions[0].body.stmts.as_slice(),
            [crate::instr::Stmt::Return(crate::instr::Expr::Call { .. })]
        ),
        "expected lowered render function to return a call, got {:?}",
        module.functions[0].body.stmts
    );
    let function = match file.items().next().expect("render_header function item") {
        vx_parser::Item::FnDecl(node) => node,
        item => panic!("expected function decl, got {item:?}"),
    };
    let outer_range = match function.body().expect("function body").statements().next() {
        Some(vx_parser::Stmt::Return(stmt)) => match stmt.expr() {
            Some(vx_parser::Expr::Where(where_expr)) => where_expr.syntax().text_range(),
            other => panic!("expected return where expr, got {other:?}"),
        },
        other => panic!("expected parsed return where expr, got {other:?}"),
    };

    let captured = Arc::new(Mutex::new(None::<RenderRequest>));
    let sink = Arc::clone(&captured);
    let expected = Value::Blob(BlobValue::from_string("rendered header"));
    let engine = engine_with_source_id(source, "/workspace/build.vx").with_render_handler(
        Arc::new(move |request| {
            *sink.lock().unwrap() = Some(request);
            Ok(expected.clone())
        }),
    );

    let value = engine.call("render_header", vec![]).unwrap();
    assert!(
        captured.lock().unwrap().is_some(),
        "render handler should be called"
    );
    assert_eq!(
        value,
        Value::Blob(BlobValue::from_string("rendered header"))
    );

    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("render request should be captured");
    assert_eq!(request.template, "templates/header.h");
    let Value::Map(bindings) = request.bindings else {
        panic!("expected render bindings to be a map");
    };
    let rendered_bindings = bindings
        .entries
        .iter()
        .map(|(key, value)| {
            let Value::String(key) = key else {
                panic!("expected string render binding key");
            };
            (key.to_string(), value.clone())
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        rendered_bindings.get("NAME"),
        Some(&Value::String(Arc::from("libpng")))
    );
    assert_eq!(rendered_bindings.get("ENABLED"), Some(&Value::Bool(true)));

    let call_entry = request
        .demand_chain
        .entries
        .iter()
        .find(|entry| entry.description == "call(..)")
        .expect("call demand entry");
    let provenance = call_entry.provenance.as_ref().expect("call provenance");
    assert_eq!(provenance.source_id.as_ref(), "/workspace/build.vx");
    assert_eq!(provenance.label, "call(..)");
    assert_eq!(provenance.span.start, u32::from(outer_range.start()));
    assert_eq!(provenance.span.end, u32::from(outer_range.end()));
}

#[test]
fn stabilize_handlers_receive_snapshotted_witness_and_shipped_resolve() {
    let workspace = TempDir::new().unwrap();
    fs::write(workspace.path().join("input.txt"), "hello stabilize\n").unwrap();

    let source = r#"
fn build(workspace: Path) {
  stabilize(
    () => Tree.from_path(workspace).file(@input.txt),
    (input) => input.utf8(),
  )
}
"#;

    let captured = Arc::new(Mutex::new(None::<StabilizeRequest>));
    let sink = Arc::clone(&captured);
    let engine = engine_with_source_id(source, "/workspace/build.vx").with_stabilize_handler(
        Arc::new(move |request| {
            let resolved = Engine::invoke_shipped(
                &request.module,
                &request.resolve,
                vec![request.witness.clone()],
            )?;
            *sink.lock().unwrap() = Some(request);
            Ok(resolved)
        }),
    );

    let value = engine
        .call(
            "build",
            vec![Value::Path(PathValue::new(
                workspace.path().to_string_lossy().as_ref(),
            ))],
        )
        .unwrap();
    assert_eq!(value, Value::String(Arc::from("hello stabilize\n")));

    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("stabilize request should be captured");
    let Value::Blob(BlobValue::Inline { bytes, .. }) = request.witness else {
        panic!("expected snapshotted inline blob witness");
    };
    assert_eq!(bytes.as_ref(), b"hello stabilize\n");
    assert!(!request.resolve.code_identity.is_empty());
}

#[test]
fn render_preserves_obligations_from_discovered_bindings() {
    let source = r#"
fn render_header(flag: Bool) {
  return render(@templates/header.h) where {
    bindings: %{ "FLAG" => flag },
  };
}
"#;
    let engine = engine(source).with_render_handler(Arc::new(|request| {
        let Value::Map(bindings) = request.bindings else {
            panic!("expected render bindings to be a map");
        };
        let flag = bindings
            .entries
            .iter()
            .find_map(|(key, value)| match key {
                Value::String(name) if name.as_ref() == "FLAG" => Some(value.clone()),
                _ => None,
            })
            .expect("FLAG binding");
        let text = match flag {
            Value::Discovered(discovered) => match discovered.value.as_ref() {
                Value::Bool(true) => "flag:on",
                Value::Bool(false) => "flag:off",
                other => panic!("expected boolean flag binding, got {other:?}"),
            },
            Value::Bool(true) => "flag:on",
            Value::Bool(false) => "flag:off",
            other => panic!("expected boolean flag binding, got {other:?}"),
        };
        Ok(Value::Blob(BlobValue::from_string(text)))
    }));
    let observation = HostObservation::Which {
        name: "cc".to_string(),
        expected_path: Some("/usr/bin/cc".to_string()),
    };
    let flag = Value::Discovered(DiscoveredValue {
        value: Box::new(Value::Bool(true)),
        obligations: Arc::from([observation.clone()]),
    });

    let value = engine.call("render_header", vec![flag]).unwrap();
    assert_eq!(
        value,
        Value::Discovered(DiscoveredValue {
            value: Box::new(Value::Blob(BlobValue::from_string("flag:on"))),
            obligations: Arc::from([observation]),
        })
    );
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
fn call_boundary_forces_independent_children_concurrently() {
    let engine = engine("fn pair(x: U64, y: U64) { Pair { left: x, right: y } }");

    let start = Instant::now();
    let value = engine
        .call(
            "pair",
            vec![
                engine.sleep_value(200, Value::U64(1)),
                engine.sleep_value(200, Value::U64(2)),
            ],
        )
        .unwrap();
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
fn array_map_schedules_callback_bodies_concurrently() {
    let engine = engine(
        "struct Item { wait: U64, value: U64 }
         fn project(item: Item) {
           item.wait;
           item.value
         }
         fn main(items: Array) { items.map(project) }",
    );
    let item = |wait, value| {
        Value::Struct(StructValue {
            type_name: Arc::from("Item"),
            fields: Arc::from([(Arc::from("wait"), wait), (Arc::from("value"), value)]),
        })
    };

    let start = Instant::now();
    let value = engine
        .call(
            "main",
            vec![Value::Array(Arc::from([
                item(engine.sleep_value(200, Value::U64(0)), Value::U64(1)),
                item(engine.sleep_value(200, Value::U64(0)), Value::U64(2)),
            ]))],
        )
        .unwrap();
    let elapsed = start.elapsed();

    assert_eq!(
        value,
        Value::Array(Arc::from([Value::U64(1), Value::U64(2)]))
    );
    assert!(elapsed.as_millis() < 350, "elapsed = {elapsed:?}");
}

#[test]
fn force_fully_uses_scheduler_for_pending_call_nodes() {
    let engine = engine(
        "struct Item { wait: U64, value: U64 }
         fn project(item: Item) {
           item.wait;
           item.value
         }
         fn main(items: Array) { items.map(project) }",
    );
    let item = |wait, value| {
        Value::Struct(StructValue {
            type_name: Arc::from("Item"),
            fields: Arc::from([(Arc::from("wait"), wait), (Arc::from("value"), value)]),
        })
    };
    let call = engine
        .call_unforced(
            "main",
            vec![Value::Array(Arc::from([
                item(engine.sleep_value(200, Value::U64(0)), Value::U64(1)),
                item(engine.sleep_value(200, Value::U64(0)), Value::U64(2)),
            ]))],
        )
        .unwrap();

    let start = Instant::now();
    let value = engine.force_fully(call).unwrap();
    let elapsed = start.elapsed();

    assert_eq!(
        value,
        Value::Array(Arc::from([Value::U64(1), Value::U64(2)]))
    );
    assert!(elapsed.as_millis() < 350, "elapsed = {elapsed:?}");
}

#[test]
fn array_flatten_schedules_item_containers_concurrently() {
    let engine = engine("fn main(items: Array) { items.flatten() }");

    let start = Instant::now();
    let value = engine
        .call(
            "main",
            vec![Value::Array(Arc::from([
                engine.sleep_value(200, Value::Array(Arc::from([Value::U64(1)]))),
                engine.sleep_value(200, Value::Array(Arc::from([Value::U64(2)]))),
            ]))],
        )
        .unwrap();
    let elapsed = start.elapsed();

    assert_eq!(
        value,
        Value::Array(Arc::from([Value::U64(1), Value::U64(2)]))
    );
    assert!(elapsed.as_millis() < 350, "elapsed = {elapsed:?}");
}

#[test]
fn exec_schedules_argument_resolution_concurrently() {
    let engine = engine(
        r#"fn observe(process: Process, tree: Tree) { process.exit_code }
           fn build(args: Array) {
             exec(args) where { observer: observe }
           }"#,
    );

    let start = Instant::now();
    let value = engine
        .call(
            "build",
            vec![Value::Array(Arc::from([
                engine.sleep_value(200, Value::String(Arc::from("/bin/sh"))),
                engine.sleep_value(200, Value::String(Arc::from("-c"))),
                Value::String(Arc::from("true")),
            ]))],
        )
        .unwrap();
    let elapsed = start.elapsed();

    assert_eq!(value, Value::U64(0));
    assert!(elapsed.as_millis() < 350, "elapsed = {elapsed:?}");
}

#[test]
fn array_merge_forces_tree_items_concurrently() {
    let engine = engine("fn main(items: Array) { items.merge() }");
    let left = Value::Tree(TreeValue::Inline(Arc::from([(
        Arc::from("left.txt"),
        Value::Blob(BlobValue::from_string("left")),
    )])));
    let right = Value::Tree(TreeValue::Inline(Arc::from([(
        Arc::from("right.txt"),
        Value::Blob(BlobValue::from_string("right")),
    )])));
    let call = engine
        .call_unforced(
            "main",
            vec![Value::Array(Arc::from([
                engine.sleep_value(200, left),
                engine.sleep_value(200, right),
            ]))],
        )
        .unwrap();

    let start = Instant::now();
    let value = engine.force_fully(call).unwrap();
    let elapsed = start.elapsed();

    assert_eq!(
        value,
        Value::Tree(TreeValue::Inline(Arc::from([
            (
                Arc::from("left.txt"),
                Value::Blob(BlobValue::from_string("left")),
            ),
            (
                Arc::from("right.txt"),
                Value::Blob(BlobValue::from_string("right")),
            ),
        ])))
    );
    assert!(elapsed.as_millis() < 350, "elapsed = {elapsed:?}");
}

#[test]
fn fallible_suffix_schedules_independent_operands_concurrently() {
    let engine = engine("fn main(x: U64, y: U64) { [x?, y?] }");

    let start = Instant::now();
    let value = engine
        .call(
            "main",
            vec![
                engine.sleep_value(200, Value::U64(1)),
                engine.sleep_value(200, Value::U64(2)),
            ],
        )
        .unwrap();
    let elapsed = start.elapsed();

    assert_eq!(
        value,
        Value::Array(Arc::from([
            Value::Struct(StructValue {
                type_name: Arc::from("Some"),
                fields: Arc::from([(Arc::from("value"), Value::U64(1))]),
            }),
            Value::Struct(StructValue {
                type_name: Arc::from("Some"),
                fields: Arc::from([(Arc::from("value"), Value::U64(2))]),
            }),
        ]))
    );
    assert!(elapsed.as_millis() < 350, "elapsed = {elapsed:?}");
}

#[test]
fn tree_constructor_schedules_map_key_resolution_concurrently() {
    let engine = engine("fn main(entries: Map) { Tree(entries) }");
    let entries = Value::Map(MapValue {
        entries: Arc::from([
            (
                engine.sleep_value(200, Value::Path(PathValue::new("left.txt"))),
                Value::Blob(BlobValue::from_string("left")),
            ),
            (
                engine.sleep_value(200, Value::Path(PathValue::new("right.txt"))),
                Value::Blob(BlobValue::from_string("right")),
            ),
        ]),
    });

    let start = Instant::now();
    let value = engine.call("main", vec![entries]).unwrap();
    let elapsed = start.elapsed();

    assert_eq!(
        value,
        Value::Tree(TreeValue::Inline(Arc::from([
            (
                Arc::from("left.txt"),
                Value::Blob(BlobValue::from_string("left")),
            ),
            (
                Arc::from("right.txt"),
                Value::Blob(BlobValue::from_string("right")),
            ),
        ])))
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
        engine.function_identity_string("even").unwrap(),
        engine.function_identity_string("odd").unwrap()
    );
}

#[test]
// r[verify store.path]
fn path_values_join_with_normalized_slashes() {
    let engine = engine(r#"fn main(root: Path) { @({root}/src/./lib.rs) }"#);
    let value = engine
        .call("main", vec![Value::Path(PathValue::new("/workspace//"))])
        .unwrap();

    assert_eq!(value, Value::Path(PathValue::new("/workspace/src/lib.rs")));
}

#[test]
fn interpolated_path_literal_with_tree_yields_exec_path() {
    let engine = engine(r#"fn main(input: Tree) { @({input}/include/lua.h) }"#);
    let input = TreeValue::Inline(Arc::from([(
        Arc::from("include"),
        Value::Tree(TreeValue::Inline(Arc::from([(
            Arc::from("lua.h"),
            Value::Blob(BlobValue::from_string("header")),
        )]))),
    )]));
    let value = engine
        .call("main", vec![Value::Tree(input.clone())])
        .unwrap();

    assert_eq!(
        value,
        Value::ExecPath(ExecPathValue::InTree {
            tree: input,
            suffix: PathValue::new("include/lua.h"),
        })
    );
}

#[test]
// r[verify store.blob]
// r[verify store.blob.utf8]
fn blob_from_string_round_trips_text() {
    let value = eval(r#"fn main() { Blob("hello").utf8() }"#, "main").unwrap();
    assert_eq!(value, Value::String(Arc::from("hello")));
}

#[test]
fn blob_executable_marks_blob_as_executable() {
    let value = eval(r#"fn main() { Blob("hello").executable() }"#, "main").unwrap();
    assert_eq!(
        value,
        Value::Blob(BlobValue::Inline {
            bytes: Arc::from("hello".as_bytes()),
            executable: true,
        })
    );
}

#[test]
// r[verify store.tree]
// r[verify store.tree.file]
fn tree_of_and_file_only_force_the_selected_entry() {
    let engine = engine(indoc! {r#"
        fn main(other: Blob) {
          Tree(%{
            @a.txt => Blob("alpha"),
            @b.txt => other,
          }).file("a.txt").utf8()
        }
    "#});
    let value = engine
        .call("main", vec![engine.poison_value("unused tree entry")])
        .unwrap();

    assert_eq!(value, Value::String(Arc::from("alpha")));
}

#[test]
fn tree_subtree_selects_nested_directories() {
    let value = eval(
        indoc! {r#"
            fn main() {
              Tree(%{
                @include => Tree(%{
                  @lua.h => Blob("header"),
                }),
              }).subtree("include").file("lua.h").utf8()
            }
        "#},
        "main",
    )
    .unwrap();

    assert_eq!(value, Value::String(Arc::from("header")));
}

#[test]
fn tree_merge_combines_directories_recursively() {
    let value = eval(
        indoc! {r#"
            fn main() {
              let base = Tree(%{
                @include => Tree(%{
                  @lua.h => Blob("lua header"),
                }),
                @liblua.a => Blob("old archive"),
              });
              let overlay = Tree(%{
                @include => Tree(%{
                  @lauxlib.h => Blob("aux header"),
                }),
                @liblua.a => Blob("new archive"),
              });
              let merged = base.merge(overlay);
              merged.file("include/lua.h").utf8()
                + "|"
                + merged.file("include/lauxlib.h").utf8()
                + "|"
                + merged.file("liblua.a").utf8()
            }
        "#},
        "main",
    )
    .unwrap();

    assert_eq!(
        value,
        Value::String(Arc::from("lua header|aux header|new archive"))
    );
}

#[test]
// r[verify store.tree]
// r[verify store.tree.file]
// r[verify store.blob.utf8]
fn fs_backed_trees_read_files_on_demand() {
    let tempdir = TempDir::new().unwrap();
    let file_path = tempdir.path().join("hello.txt");
    fs::write(&file_path, "hello from fs").unwrap();

    let engine = engine(r#"fn main(tree: Tree) { tree.file("hello.txt").utf8() }"#);
    let tree = Value::Tree(TreeValue::FsDir(PathValue::new(
        tempdir.path().to_string_lossy(),
    )));
    let value = engine.call("main", vec![tree]).unwrap();

    assert_eq!(value, Value::String(Arc::from("hello from fs")));
}

#[test]
fn tree_from_path_creates_a_lazy_fs_backed_tree() {
    let tempdir = TempDir::new().unwrap();
    let file_path = tempdir.path().join("hello.txt");
    fs::write(&file_path, "hello from from_path").unwrap();

    let engine = engine(r#"fn main(workspace: Path) { Tree(workspace).file("hello.txt").utf8() }"#);
    let value = engine
        .call(
            "main",
            vec![Value::Path(PathValue::new(
                tempdir.path().to_string_lossy(),
            ))],
        )
        .unwrap();

    assert_eq!(value, Value::String(Arc::from("hello from from_path")));
}

#[test]
// r[verify eval.entry]
fn call_entry_passes_workspace_as_a_path() {
    let engine = engine(
        r#"fn build(workspace: Path, args: Array[String], env: Map[String, String]) {
               Tree %{
                 @workspace.txt => Blob @({workspace}/src/main.rs),
               }
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
               Tree %{
                 @stdout.txt => process.stdout.collect().blob(),
                 @output.txt => tree.file("out/output.txt"),
               }
             }

             fn build(workspace: Path) {
               exec ["/bin/sh", "-c", "mkdir -p out && cat src/input.txt > out/output.txt; printf done"] where {
                 mounts: %{ "src" => Tree(workspace) },
                 observer: observe,
               }
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
    let by_name = entries
        .iter()
        .map(|(name, value)| (name.as_ref(), value))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        by_name.get("stdout.txt"),
        Some(&&Value::Blob(BlobValue::from_string("done")))
    );
    assert_eq!(
        by_name.get("output.txt"),
        Some(&&Value::Blob(BlobValue::from_string(
            "hello from workspace"
        )))
    );
}

#[test]
fn exec_verification_failures_poison_locally() {
    let tempdir = TempDir::new().unwrap();
    let input_path = tempdir.path().join("tool.txt");
    fs::write(&input_path, "toolchain-v1").unwrap();
    let source = format!(
        r#"fn probe() #discover {{
               discover(executor_class("default"), (host) => host.read({path:?}))
             }}

             fn observe(process: Process, tree: Tree) {{
               process.stdout.collect().utf8()
             }}

             fn build(input: Blob) {{
               exec ["/bin/cat", input] where {{
                 observer: observe,
               }}
             }}"#,
        path = input_path.to_string_lossy(),
    );
    let engine = engine(&source);
    let discovered = engine.call("probe", vec![]).unwrap();
    assert!(matches!(discovered, Value::Discovered(_)));

    fs::write(&input_path, "toolchain-v2").unwrap();

    let err = engine.call("build", vec![discovered]).unwrap_err();
    assert_eq!(err.kind, PoisonKind::VerificationFailure);
    assert!(err.message.contains("host.read"));
}

#[test]
fn observer_functions_can_return_tree_results_for_exec() {
    let tempdir = TempDir::new().unwrap();
    fs::create_dir_all(tempdir.path().join("out")).unwrap();
    fs::write(
        tempdir.path().join("out/output.txt"),
        "hello from workspace",
    )
    .unwrap();

    let engine = engine(
        r#"fn observe(process: Process, tree: Tree) {
               Tree %{
                 @stdout.txt => process.stdout.collect().blob(),
                 @output.txt => tree.file("out/output.txt"),
               }
             }"#,
    );

    let process = Value::Struct(StructValue {
        type_name: Arc::from("Process"),
        fields: Arc::from([
            (Arc::from("exit_code"), Value::U64(0)),
            (
                Arc::from("stdout"),
                Value::Bytes(Arc::from(b"done".as_slice())),
            ),
            (Arc::from("stderr"), Value::Bytes(Arc::from(b"".as_slice()))),
        ]),
    });
    let tree = Value::Tree(TreeValue::FsDir(PathValue::new(
        tempdir.path().to_string_lossy(),
    )));

    let value = engine.call("observe", vec![process, tree]).unwrap();

    let Value::Tree(TreeValue::Inline(entries)) = value else {
        panic!("expected tree result");
    };
    assert_eq!(entries.len(), 2);
    let by_name = entries
        .iter()
        .map(|(name, value)| (name.as_ref(), value))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        by_name.get("stdout.txt"),
        Some(&&Value::Blob(BlobValue::from_string("done")))
    );
    assert!(matches!(
        by_name.get("output.txt"),
        Some(&Value::Blob(BlobValue::FsFile { .. }))
    ));
}

#[test]
fn exec_handler_receives_verification_obligations_from_discovered_values() {
    let tempdir = TempDir::new().unwrap();
    let input_path = tempdir.path().join("tool.txt");
    fs::write(&input_path, "toolchain-v1").unwrap();
    let expected_hash = blake3_hash(b"toolchain-v1").to_hex().to_string();
    let captured = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&captured);
    let source = format!(
        r#"fn probe() #discover {{
               discover(executor_class("default"), (host) => host.read({path:?}))
             }}

             fn observe(process: Process, tree: Tree) {{
               process.exit_code
             }}

             fn build(input: Blob) {{
               exec ["/bin/true", input] where {{
                 observer: observe,
               }}
             }}"#,
        path = input_path.to_string_lossy(),
    );
    let engine = engine(&source).with_exec_handler(Arc::new(move |request| {
        *sink.lock().unwrap() = Some(request);
        Ok(Value::U64(0))
    }));

    let discovered = engine.call("probe", vec![]).unwrap();
    let value = engine.call("build", vec![discovered]).unwrap();
    assert_eq!(value, Value::U64(0));

    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("exec request should be captured");
    assert_eq!(request.observer.def.name, "observe");
    assert_eq!(
        request.verification_obligations,
        vec![HostObservation::Read {
            path: input_path.to_string_lossy().to_string(),
            content_hash: expected_hash,
        }]
    );
}

#[test]
fn exec_rejects_raw_path_mount_values() {
    let tempdir = TempDir::new().unwrap();
    fs::write(tempdir.path().join("input.txt"), "hello from path mount").unwrap();
    let engine = engine(
        r#"fn observe(process: Process, tree: Tree) {
               Tree.empty()
             }

             fn build(workspace: Path) {
               exec ["/bin/true"] where {
                 mounts: %{ @src => workspace },
                 observer: observe,
               }
             }"#,
    );

    let err = engine
        .call(
            "build",
            vec![Value::Path(PathValue::new(
                tempdir.path().to_string_lossy(),
            ))],
        )
        .unwrap_err();
    assert_eq!(err.kind, PoisonKind::TypeError);
    assert!(
        err.message
            .contains("exec mount values must be Blob or Tree, got Path")
    );
}

#[test]
fn discover_records_host_obligations_for_demanded_values() {
    let tempdir = TempDir::new().unwrap();
    let tool_path = tempdir.path().join("tool.sh");
    let tool_script = "#!/bin/sh\nprintf toolchain-v1\n";
    fs::write(&tool_path, tool_script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = fs::metadata(&tool_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&tool_path, perms).unwrap();
    }
    let expected_hash = blake3_hash(tool_script.as_bytes()).to_hex().to_string();
    let source = format!(
        r#"fn probe() #discover {{
               discover(executor_class("default"), (host) => {{
                 host.hash(host.which({path:?}))
               }})
             }}"#,
        path = tool_path.to_string_lossy(),
    );
    let engine = engine(&source);

    let value = engine.call("probe", vec![]).unwrap();
    let Value::Discovered(discovered) = value else {
        panic!("expected discovered value");
    };
    assert_eq!(
        *discovered.value,
        Value::Blob(BlobValue::from_string(expected_hash.clone()))
    );
    assert_eq!(
        discovered.obligations.as_ref(),
        &[
            HostObservation::Hash {
                path: tool_path.to_string_lossy().to_string(),
                content_hash: expected_hash.clone(),
            },
            HostObservation::Which {
                name: tool_path.to_string_lossy().to_string(),
                expected_path: Some(tool_path.to_string_lossy().to_string()),
            },
        ]
    );
}

#[test]
fn host_which_poisons_by_default_and_fallible_calls_return_options() {
    let tempdir = TempDir::new().unwrap();
    let tool_path = tempdir.path().join("tool.sh");
    let expected_script = "#!/bin/sh\nprintf tool\n";
    fs::write(&tool_path, expected_script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = fs::metadata(&tool_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&tool_path, perms).unwrap();
    }

    let missing_path = tempdir.path().join("missing.sh");
    let source = format!(
        r##"fn found() #discover {{
               discover(executor_class("default"), (host) => host.which({tool_path:?})?)
             }}

             fn missing() #discover {{
               discover(executor_class("default"), (host) => host.which({missing:?}))
             }}

             fn missing_fallible() #discover {{
               discover(executor_class("default"), (host) => host.which({missing:?})?)
             }}"##,
        tool_path = tool_path.to_string_lossy(),
        missing = missing_path.to_string_lossy(),
    );
    let engine = engine(&source);

    let found = engine.call("found", vec![]).unwrap();
    let Value::Discovered(found) = found else {
        panic!("expected discovered value");
    };
    assert_eq!(
        *found.value,
        Value::Struct(StructValue {
            type_name: Arc::from("Some"),
            fields: Arc::from([(
                Arc::from("value"),
                Value::String(Arc::from(tool_path.to_string_lossy().to_string())),
            )]),
        })
    );
    assert_eq!(
        found.obligations.as_ref(),
        &[HostObservation::Which {
            name: tool_path.to_string_lossy().to_string(),
            expected_path: Some(tool_path.to_string_lossy().to_string()),
        }]
    );

    let err = engine.call("missing", vec![]).unwrap_err();
    assert_eq!(err.kind, PoisonKind::UserFail);
    assert!(err.message.contains("host.which could not find"));

    let missing_value = engine.call("missing_fallible", vec![]).unwrap();
    let Value::Discovered(missing_value) = missing_value else {
        panic!("expected discovered value");
    };
    assert_eq!(
        *missing_value.value,
        Value::Struct(StructValue {
            type_name: Arc::from("None"),
            fields: Arc::from([]),
        })
    );
    assert_eq!(
        missing_value.obligations.as_ref(),
        &[HostObservation::Which {
            name: missing_path.to_string_lossy().to_string(),
            expected_path: None,
        }]
    );
}

#[test]
fn host_os_returns_discovered_string() {
    let source = r#"fn probe() #discover {
           discover(executor_class("default"), (host) => host.os())
         }"#;
    let engine = engine(source);

    let value = engine.call("probe", vec![]).unwrap();
    let Value::Discovered(discovered) = value else {
        panic!("expected discovered value");
    };
    assert_eq!(
        *discovered.value,
        Value::String(Arc::from(match std::env::consts::OS {
            "macos" => "darwin",
            other => other,
        }))
    );
    assert_eq!(
        discovered.obligations.as_ref(),
        &[HostObservation::Os {
            value: match std::env::consts::OS {
                "macos" => "darwin".to_string(),
                other => other.to_string(),
            },
        }]
    );
}

#[test]
fn host_arch_returns_discovered_string() {
    let source = r#"fn probe() #discover {
           discover(executor_class("default"), (host) => host.arch())
         }"#;
    let engine = engine(source);

    let value = engine.call("probe", vec![]).unwrap();
    let Value::Discovered(discovered) = value else {
        panic!("expected discovered value");
    };
    assert_eq!(
        *discovered.value,
        Value::String(Arc::from(std::env::consts::ARCH))
    );
    assert_eq!(
        discovered.obligations.as_ref(),
        &[HostObservation::Arch {
            value: std::env::consts::ARCH.to_string(),
        }]
    );
}

#[test]
fn discovered_if_condition_accepts_discovered_bool() {
    let source = r#"fn probe() #discover {
           discover(executor_class("default"), (host) => if host.os() == "windows" {
             Blob("windows")
           } else {
             Blob("unix")
           })
         }"#;
    let engine = engine(source);

    let value = engine.call("probe", vec![]).unwrap();
    let Value::Discovered(discovered) = value else {
        panic!("expected discovered value");
    };
    assert_eq!(
        *discovered.value,
        Value::Blob(BlobValue::from_string(match std::env::consts::OS {
            "windows" => "windows",
            _ => "unix",
        }))
    );
    assert_eq!(
        discovered.obligations.as_ref(),
        &[HostObservation::Os {
            value: match std::env::consts::OS {
                "macos" => "darwin".to_string(),
                other => other.to_string(),
            },
        }]
    );
}

#[test]
fn fallible_discovery_preserves_obligations_from_discovered_inputs() {
    let tempdir = TempDir::new().unwrap();
    let input_path = tempdir.path().join("input.bin");
    let bytes = [0xff, 0xfe, 0xfd];
    fs::write(&input_path, bytes).unwrap();
    let expected_hash = blake3_hash(&bytes).to_hex().to_string();
    let source = format!(
        r#"fn probe() #discover {{
               discover(executor_class("default"), (host) => host.read({path:?}).utf8()?)
             }}"#,
        path = input_path.to_string_lossy(),
    );
    let engine = engine(&source);

    let value = engine.call("probe", vec![]).unwrap();
    let Value::Discovered(discovered) = value else {
        panic!("expected discovered value");
    };
    assert_eq!(
        *discovered.value,
        Value::Struct(StructValue {
            type_name: Arc::from("None"),
            fields: Arc::from([]),
        })
    );
    assert_eq!(
        discovered.obligations.as_ref(),
        &[HostObservation::Read {
            path: input_path.to_string_lossy().to_string(),
            content_hash: expected_hash,
        }]
    );
}

#[test]
// r[verify exec.arg.auto-mount]
fn exec_implicitly_mounts_blob_args() {
    let engine = engine(
        r#"fn observe(process: Process, tree: Tree) {
               process.stdout.collect().utf8()
             }

             fn build(input: Blob) {
               exec ["/bin/cat", input] where { observer: observe }
             }"#,
    );

    let value = engine
        .call(
            "build",
            vec![Value::Blob(BlobValue::from_string("hello blob"))],
        )
        .unwrap();

    assert_eq!(value, Value::String(Arc::from("hello blob")));
}

#[test]
// r[verify exec.arg.auto-mount]
fn exec_implicitly_mounts_tree_args() {
    let engine = engine(
        r#"fn observe(process: Process, tree: Tree) {
               process.stdout.collect().utf8()
             }

             fn build(input: Tree) {
               exec ["/bin/sh", "-c", "cat \"$1/input.txt\"", "sh", input] where {
                 observer: observe,
               }
             }"#,
    );
    let input = Value::Tree(TreeValue::Inline(Arc::from([(
        Arc::from("input.txt"),
        Value::Blob(BlobValue::from_string("hello tree")),
    )])));

    let value = engine.call("build", vec![input]).unwrap();

    assert_eq!(value, Value::String(Arc::from("hello tree")));
}

#[test]
// r[verify exec.arg.interpolation]
fn exec_mounts_tree_paths_mentioned_in_path_literals() {
    let engine = engine(
        r#"fn observe(process: Process, tree: Tree) {
               process.stdout.collect().utf8()
             }

             fn build(input: Tree) {
               exec ["/bin/cat", @({input}/input.txt)] where { observer: observe }
             }"#,
    );
    let input = Value::Tree(TreeValue::Inline(Arc::from([(
        Arc::from("input.txt"),
        Value::Blob(BlobValue::from_string("hello via execpath")),
    )])));

    let value = engine.call("build", vec![input]).unwrap();

    assert_eq!(value, Value::String(Arc::from("hello via execpath")));
}

#[test]
// r[verify exec.arg.spread]
fn exec_spreads_tree_args_in_canonical_path_order() {
    let captured = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&captured);
    let engine = engine(
        r#"fn observe(process: Process, tree: Tree) {
               process.exit_code
             }

             fn build(input: Tree) {
               exec ["tool", ..input] where { observer: observe }
             }"#,
    )
    .with_exec_handler(Arc::new(move |request| {
        *sink.lock().unwrap() = Some(request);
        Ok(Value::U64(0))
    }));
    let input = Value::Tree(TreeValue::Inline(Arc::from([
        (
            Arc::from("z.o"),
            Value::Blob(BlobValue::from_string("z object")),
        ),
        (
            Arc::from("dir"),
            Value::Tree(TreeValue::Inline(Arc::from([
                (
                    Arc::from("b.o"),
                    Value::Blob(BlobValue::from_string("nested b")),
                ),
                (
                    Arc::from("a.o"),
                    Value::Blob(BlobValue::from_string("nested a")),
                ),
            ]))),
        ),
        (
            Arc::from("a.o"),
            Value::Blob(BlobValue::from_string("a object")),
        ),
    ])));

    let value = engine.call("build", vec![input]).unwrap();
    assert_eq!(value, Value::U64(0));

    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("exec request should be captured");
    assert_eq!(request.mounts.len(), 1);

    let mount_root = request.mounts[0].path.clone();
    let expected_paths = vec![
        mount_root.join("a.o"),
        mount_root.join("dir/a.o"),
        mount_root.join("dir/b.o"),
        mount_root.join("z.o"),
    ];

    let [
        ExecArgValue::Literal(program),
        ExecArgValue::Path(first),
        ExecArgValue::Path(second),
        ExecArgValue::Path(third),
        ExecArgValue::Path(fourth),
    ] = request.args.as_slice()
    else {
        panic!("expected literal program plus four path args");
    };
    assert_eq!(program, "tool");
    assert_eq!(
        vec![first.clone(), second.clone(), third.clone(), fourth.clone()],
        expected_paths
    );
}

#[test]
fn exec_mounts_subtrees_instead_of_the_whole_tree() {
    let captured = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&captured);
    let engine = engine(indoc! {r#"
            fn observe(process: Process, tree: Tree) {
              process.exit_code
            }

            fn build(input: Tree) {
              exec(["tool", input.subtree("dir")]) where { observer: observe }
            }
        "#})
    .with_exec_handler(Arc::new(move |request| {
        *sink.lock().unwrap() = Some(request);
        Ok(Value::U64(0))
    }));
    let input = Value::Tree(TreeValue::Inline(Arc::from([
        (
            Arc::from("dir"),
            Value::Tree(TreeValue::Inline(Arc::from([
                (
                    Arc::from("a.o"),
                    Value::Blob(BlobValue::from_string("nested a")),
                ),
                (
                    Arc::from("b.o"),
                    Value::Blob(BlobValue::from_string("nested b")),
                ),
            ]))),
        ),
        (
            Arc::from("top.o"),
            Value::Blob(BlobValue::from_string("top object")),
        ),
    ])));

    let value = engine.call("build", vec![input]).unwrap();
    assert_eq!(value, Value::U64(0));

    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("exec request should be captured");
    assert_eq!(request.mounts.len(), 1);
    let Value::Tree(TreeValue::Inline(entries)) = &request.mounts[0].value else {
        panic!("expected subtree mount");
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0.as_ref(), "a.o");
    assert_eq!(entries[1].0.as_ref(), "b.o");
}

#[test]
fn exec_mounts_merged_trees() {
    let captured = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&captured);
    let engine = engine(indoc! {r#"
            fn observe(process: Process, tree: Tree) {
              process.exit_code
            }

            fn build(base: Tree, overlay: Tree) {
              exec(["tool", base.merge(overlay)]) where { observer: observe }
            }
        "#})
    .with_exec_handler(Arc::new(move |request| {
        *sink.lock().unwrap() = Some(request);
        Ok(Value::U64(0))
    }));
    let base = Value::Tree(TreeValue::Inline(Arc::from([
        (
            Arc::from("include"),
            Value::Tree(TreeValue::Inline(Arc::from([(
                Arc::from("lua.h"),
                Value::Blob(BlobValue::from_string("lua header")),
            )]))),
        ),
        (
            Arc::from("liblua.a"),
            Value::Blob(BlobValue::from_string("old archive")),
        ),
    ])));
    let overlay = Value::Tree(TreeValue::Inline(Arc::from([
        (
            Arc::from("include"),
            Value::Tree(TreeValue::Inline(Arc::from([(
                Arc::from("lauxlib.h"),
                Value::Blob(BlobValue::from_string("aux header")),
            )]))),
        ),
        (
            Arc::from("liblua.a"),
            Value::Blob(BlobValue::from_string("new archive")),
        ),
    ])));

    let value = engine.call("build", vec![base, overlay]).unwrap();
    assert_eq!(value, Value::U64(0));

    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("exec request should be captured");
    assert_eq!(request.mounts.len(), 1);
    let Value::Tree(TreeValue::Inline(entries)) = &request.mounts[0].value else {
        panic!("expected merged tree mount");
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0.as_ref(), "include");
    assert_eq!(entries[1].0.as_ref(), "liblua.a");
    assert_eq!(
        entries[1].1,
        Value::Blob(BlobValue::from_string("new archive"))
    );
}

#[test]
// r[verify exec.arg.auto-mount]
// r[verify exec.mount]
fn exec_deduplicates_implicit_blob_mounts_in_host_requests() {
    let captured = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&captured);
    let engine = engine(
        r#"fn observe(process: Process, tree: Tree) {
               process.exit_code
             }

             fn build(input: Blob) {
               exec ["tool", input, input] where { observer: observe }
             }"#,
    )
    .with_exec_handler(Arc::new(move |request| {
        *sink.lock().unwrap() = Some(request);
        Ok(Value::U64(0))
    }));

    let value = engine
        .call(
            "build",
            vec![Value::Blob(BlobValue::from_string("hello blob"))],
        )
        .unwrap();
    assert_eq!(value, Value::U64(0));

    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("exec request should be captured");
    assert_eq!(request.mounts.len(), 1);
    assert_eq!(
        request.mounts[0].value,
        Value::Blob(BlobValue::from_string("hello blob"))
    );

    let [
        ExecArgValue::Literal(program),
        ExecArgValue::Path(first),
        ExecArgValue::Path(second),
    ] = request.args.as_slice()
    else {
        panic!("expected literal program plus two path args");
    };
    assert_eq!(program, "tool");
    assert_eq!(first, second);
    assert_eq!(&request.mounts[0].path, first);
    assert!(first.as_str().starts_with(".vx/implicit/"));
}

#[test]
fn exec_can_be_routed_through_a_host_handler() {
    let tempdir = TempDir::new().unwrap();
    fs::write(tempdir.path().join("input.txt"), "hello from handler").unwrap();
    let captured = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&captured);
    let engine = engine(
        r#"fn observe(process: Process, tree: Tree) {
               process.exit_code
             }

             fn build(workspace: Path) {
               exec ["/bin/cp", "src/input.txt", "out/output.txt"] where {
                 env: %{ "INPUT" => "src/input.txt" },
                 mounts: %{ "src" => Tree(workspace) },
                 observer: observe,
               }
             }"#,
    )
    .with_exec_handler(Arc::new(move |request| {
        *sink.lock().unwrap() = Some(request);
        Ok(Value::U64(7))
    }));

    let value = engine
        .call(
            "build",
            vec![Value::Path(PathValue::new(
                tempdir.path().to_string_lossy(),
            ))],
        )
        .unwrap();
    assert_eq!(value, Value::U64(7));

    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("exec request should be captured");
    assert_eq!(
        request.args,
        vec![
            ExecArgValue::Literal("/bin/cp".to_string()),
            ExecArgValue::Literal("src/input.txt".to_string()),
            ExecArgValue::Literal("out/output.txt".to_string()),
        ]
    );
    assert_eq!(
        request.env,
        vec![(
            "INPUT".to_string(),
            ExecArgValue::Literal("src/input.txt".to_string())
        )]
    );
    assert_eq!(request.mounts.len(), 1);
    assert_eq!(request.mounts[0].path, PathValue::new("src"));
    let Value::Tree(TreeValue::FsDir(root)) = &request.mounts[0].value else {
        panic!("expected preserved workspace tree handle");
    };
    assert_eq!(root, &PathValue::new(tempdir.path().to_string_lossy()));
    assert!(request.writable_roots.is_empty());
    assert_eq!(request.observer.def.name, "observe");
    assert!(request.verification_obligations.is_empty());
    assert_eq!(request.module.functions.len(), 2);
}

#[test]
fn exec_captures_explicit_create_dirs() {
    let captured = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&captured);
    let engine = engine(indoc! {r#"
            fn observe(process: Process, tree: Tree) {
              process.exit_code
            }

            fn build() {
              exec(["cl.exe", "/nologo", "/c", @src/hello.c, "/Foout\\obj\\hello.obj"]) where {
                mounts: %{ @src/hello.c => Blob("int main(void) { return 0; }\n") },
                writable: [@out],
                create_dirs: [@out/obj],
                observer: observe,
              }
            }
        "#})
    .with_exec_handler(Arc::new(move |request| {
        *sink.lock().unwrap() = Some(request);
        Ok(Value::U64(0))
    }));

    let value = engine.call("build", vec![]).unwrap();
    assert_eq!(value, Value::U64(0));

    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("exec request should be captured");
    assert_eq!(request.writable_roots, vec![PathValue::new("out")]);
    assert_eq!(request.create_dirs, vec![PathValue::new("out/obj")]);
}

#[test]
fn exec_infers_writable_roots_only_for_unmounted_paths() {
    let captured = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&captured);
    let engine = engine(indoc! {r#"
            fn observe(process: Process, tree: Tree) {
              process.exit_code
            }

            fn build() {
              let objs = Tree %{
                @obj => Tree %{
                  @a.o => Blob("object"),
                },
              };

              exec(["ar", "rcs", @out/lib.a, ..objs]) where {
                observer: observe,
              }
            }
        "#})
    .with_exec_handler(Arc::new(move |request| {
        *sink.lock().unwrap() = Some(request);
        Ok(Value::U64(0))
    }));

    let value = engine.call("build", vec![]).unwrap();
    assert_eq!(value, Value::U64(0));

    let request = captured
        .lock()
        .unwrap()
        .clone()
        .expect("exec request should be captured");
    assert_eq!(request.writable_roots, vec![PathValue::new("out")]);
    assert_eq!(request.mounts.len(), 1);
    assert_eq!(request.mounts[0].path, PathValue::new(".vx/implicit/0"));
    assert_eq!(
        request.args,
        vec![
            ExecArgValue::Literal("ar".to_string()),
            ExecArgValue::Literal("rcs".to_string()),
            ExecArgValue::Path(PathValue::new("out/lib.a")),
            ExecArgValue::Path(PathValue::new(".vx/implicit/0/obj/a.o")),
        ]
    );
}
