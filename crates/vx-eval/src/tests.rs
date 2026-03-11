//! Tests for the evaluator.
//!
//! Each test parses Vixen source, lowers it, and evaluates a function call,
//! asserting the result.

use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use blake3::hash as blake3_hash;
use tempfile::TempDir;
use vx_parser::{Pattern as AstPattern, SourceFile, Stmt as AstStmt};
use vx_store_proto::{Blake3Hash, BlobRef, ChunkHash, NodeHash};

use crate::exec::ExecArgValue;
use crate::lower::lower_source_file;
use crate::poison::PoisonKind;
use crate::transport::TransportValue;
use crate::value::{
    BlobValue, ExecPathValue, HostObservation, HostValue, MapValue, PathValue, StructValue,
    TreeValue, Value,
};
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

static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

const LUA_SKETCH_TAIL: &str = "  let objects = (all_lib_sources.map(compile)).merge();\n  let liblua = archive(objects);\n\n  let lua_exe = link(Tree [compile @lua.c, liblua]);\n\n  Tree {\n    @bin/lua: lua_exe.file(@bin/lua),\n    @lib/liblua.a: liblua.file(@liblua.a),\n  }\n";

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
fn option_unwrap_or_returns_the_inner_value() {
    let val = eval(r#"fn main() { (Some { value: 7 }).unwrap_or 9 }"#, "main").unwrap();
    assert_eq!(val, Value::U64(7));
}

#[test]
fn option_unwrap_or_returns_the_default_for_none() {
    let val = eval(r#"fn main() { None.unwrap_or 9 }"#, "main").unwrap();
    assert_eq!(val, Value::U64(9));
}

#[test]
fn option_unwrap_or_does_not_force_the_default_when_some() {
    let val = eval(
        r#"fn main() { (Some { value: 7 }).unwrap_or (fail "should not be forced") }"#,
        "main",
    )
    .unwrap();
    assert_eq!(val, Value::U64(7));
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
        "ltests.c",
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
    fs::write(
        &ar_script,
        r#"#!/bin/sh
out="$2"
mkdir -p "$(dirname "$out")"
printf 'archive\n' > "$out"
"#,
    )
    .unwrap();
    fs::write(
        &ranlib_script,
        r#"#!/bin/sh
exit 0
"#,
    )
    .unwrap();
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
        "ltests.c",
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
        "ltests.c",
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
        "ltests.c",
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
    fs::write(
        &ar_script,
        r#"#!/bin/sh
out="$2"
mkdir -p "$(dirname "$out")"
printf 'archive\n' > "$out"
"#,
    )
    .unwrap();
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
        "ltests.c",
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
    fs::write(
        &ar_script,
        r#"#!/bin/sh
out="$2"
mkdir -p "$(dirname "$out")"
printf 'archive\n' > "$out"
"#,
    )
    .unwrap();
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
        "ltests.c",
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
    fs::write(
        &ar_script,
        r#"#!/bin/sh
out="$2"
mkdir -p "$(dirname "$out")"
printf 'archive\n' > "$out"
"#,
    )
    .unwrap();
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
        "ltests.c",
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
    fs::write(
        &ar_script,
        r#"#!/bin/sh
out="$2"
mkdir -p "$(dirname "$out")"
printf 'archive\n' > "$out"
"#,
    )
    .unwrap();
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
    mounts: { @src/input.txt: input },
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
    let parse = vx_parser::parse(r#"fn main(root: Path) { @({root}/src/./lib.rs) }"#);
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
        [crate::instr::Stmt::Expr(crate::instr::Expr::PathTemplate(_))] => {}
        other => panic!("unexpected lowered body: {other:?}"),
    }
}

#[test]
fn lowering_keeps_map_payloads_for_tree_constructor_calls() {
    let parse = vx_parser::parse(
        r#"fn main(other: Blob) {
               Tree {
                 @a.txt: Blob "alpha",
                 @b.txt: other,
               }
             }"#,
    );
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
    let parse = vx_parser::parse(
        r#"fn observe(process: Process, tree: Tree) {
               process.exit_code
             }

             fn build() {
               exec ["/bin/true"] where {
                 observer: observe,
               }
             }"#,
    );
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
        [crate::instr::Stmt::Expr(crate::instr::Expr::Call { args, kwargs, .. })] => {
            assert_eq!(args.len(), 1);
            assert_eq!(kwargs.len(), 1);
            assert_eq!(kwargs[0].0, "observer");
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
fn top_level_expression_statements_run_before_entry_calls() {
    let err = eval(
        r#"
fail "boom";
fn main() { 42 }
"#,
        "main",
    )
    .unwrap_err();

    assert_eq!(err.kind, PoisonKind::UserFail);
    assert!(err.message.contains("boom"));
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
         fn main() { double 21 }",
        "main",
    )
    .unwrap();
    assert_eq!(val, Value::U64(42));
}

#[test]
fn nested_function_calls() {
    let val = eval(
        "fn add(a: U64, b: U64) { a + b }
         fn main() { add (add 1 2) (add 3 4) }",
        "main",
    )
    .unwrap();
    assert_eq!(val, Value::U64(10));
}

#[test]
fn recursive_function() {
    let val = eval(
        "fn factorial(n: U64) {
           if n <= 1 { 1 } else { n * factorial (n - 1) }
         }
         fn main() { factorial 5 }",
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
         fn apply(f: Fn(U64, U64) -> U64, x: U64, y: U64) { f x y }
         fn main() { apply add 3 4 }",
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
        "fn apply(f: Fn(U64) -> U64, x: U64) { f x }
         fn main() {
           let offset = 40;
           let add = (x) => {
             let y = offset + x;
             y
           };
           let offset = 1;
           apply add 2
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
           (x) => helper (base + offset + x)
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
    let engine = engine(
        r#"fn pack(root: Path) {
               {
                 "root": root,
                 "tree": Tree {
                   @hello.txt: Blob "hello",
                 },
                 "values": [41, 42],
               }
             }"#,
    );
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
    let blob = Value::Blob(BlobValue::Store(BlobRef::Small {
        chunk: ChunkHash(Blake3Hash([0x11; 32])),
        size: 3,
    }));
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
         fn main() { exec! }",
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
         fn main() { check 20 }",
        "main",
    )
    .unwrap();
    assert_eq!(val, Value::U64(999));
}

#[test]
fn fibonacci() {
    let val = eval(
        "fn fib(n: U64) {
           if n <= 1 { n } else { fib (n - 1) + fib (n - 2) }
         }
         fn main() { fib 10 }",
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
           if choose flag poison { 1 } else { poison }
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
        "fn loop_forever(n: U64) { loop_forever n }
         fn main() { loop_forever 1 }",
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
    let source_a = "fn helper() { 1 } fn wrapper() { helper! }";
    let source_b = "fn helper() { 2 } fn wrapper() { helper! }";

    let key_a = engine(source_a).function_key("wrapper").unwrap();
    let key_b = engine(source_b).function_key("wrapper").unwrap();

    assert_ne!(key_a, key_b);
}

#[test]
// r[verify fn.identity.overloads]
fn all_visible_overloads_contribute_to_caller_identity() {
    let source_a = "fn helper(x: U64) { 1 }
                    fn helper(x: Bool) { 2 }
                    fn wrapper() { helper 1 }";
    let source_b = "fn helper(x: U64) { 1 }
                    fn helper(x: Bool) { 99 }
                    fn wrapper() { helper 1 }";

    let key_a = engine(source_a).function_key("wrapper").unwrap();
    let key_b = engine(source_b).function_key("wrapper").unwrap();

    assert_ne!(key_a, key_b);
}

#[test]
// r[verify fn.identity.stability]
fn comments_and_whitespace_do_not_change_function_identity() {
    let source_a = "fn helper() { 1 }\nfn wrapper() { helper! }";
    let source_b = r#"
// wrapper should keep the same identity
fn helper() {
  // this comment is ignored by lowering
  1
}

fn wrapper() {
  helper!
}
"#;

    let key_a = engine(source_a).function_key("wrapper").unwrap();
    let key_b = engine(source_b).function_key("wrapper").unwrap();

    assert_eq!(key_a, key_b);
}

#[test]
// r[verify fn.identity.scc]
fn mutually_recursive_functions_share_one_code_identity() {
    let source = "fn even(n: U64) { if n == 0 { true } else { odd (n - 1) } }
                  fn odd(n: U64) { if n == 0 { false } else { even (n - 1) } }";
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
    let value = eval(r#"fn main() { (Blob "hello").to_string! }"#, "main").unwrap();
    assert_eq!(value, Value::String(Arc::from("hello")));
}

#[test]
// r[verify store.tree]
// r[verify store.tree.file]
fn tree_of_and_file_only_force_the_selected_entry() {
    let engine = engine(
        r#"fn main(other: Blob) {
               ((Tree {
                 @a.txt: Blob "alpha",
                 @b.txt: other,
               }).file "a.txt").to_string!
             }"#,
    );
    let value = engine
        .call("main", vec![engine.poison_value("unused tree entry")])
        .unwrap();

    assert_eq!(value, Value::String(Arc::from("alpha")));
}

#[test]
fn tree_subtree_selects_nested_directories() {
    let value = eval(
        r#"fn main() {
               (((Tree {
                 @include: Tree {
                   @lua.h: Blob "header",
                 },
               }).subtree "include").file "lua.h").to_string!
             }"#,
        "main",
    )
    .unwrap();

    assert_eq!(value, Value::String(Arc::from("header")));
}

#[test]
fn tree_merge_combines_directories_recursively() {
    let value = eval(
        r#"fn main() {
               let base = Tree {
                 @include: Tree {
                   @lua.h: Blob "lua header",
                 },
                 @liblua.a: Blob "old archive",
               };
               let overlay = Tree {
                 @include: Tree {
                   @lauxlib.h: Blob "aux header",
                 },
                 @liblua.a: Blob "new archive",
               };
               let merged = base.merge overlay;
               (merged.file "include/lua.h").to_string!
                 + "|"
                 + (merged.file "include/lauxlib.h").to_string!
                 + "|"
                 + (merged.file "liblua.a").to_string!
             }"#,
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

    let engine = engine(r#"fn main(tree: Tree) { (tree.file "hello.txt").to_string! }"#);
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

    let engine = engine(
        r#"fn main(workspace: Path) { ((Tree.from_path workspace).file "hello.txt").to_string! }"#,
    );
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
               Tree {
                 @workspace.txt: Blob @({workspace}/src/main.rs),
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
               Tree {
                 @stdout.txt: process.stdout.collect().blob(),
                 @output.txt: tree.file("out/output.txt"),
               }
             }

             fn build(workspace: Path) {
               exec ["/bin/sh", "-c", "mkdir -p out && cat src/input.txt > out/output.txt; printf done"] where {
                 mounts: { "src": Tree(workspace) },
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
               discover (executor_class "default") ((host) => host.read {path:?})
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
               Tree {
                 @stdout.txt: process.stdout.collect().blob(),
                 @output.txt: tree.file("out/output.txt"),
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
        Some(&Value::Blob(BlobValue::FsFile(_)))
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
               discover (executor_class "default") ((host) => host.read {path:?})
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
               Tree.empty!
             }

             fn build(workspace: Path) {
               exec ["/bin/true"] where {
                 mounts: { "src": workspace },
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
fn discover_records_host_obligations_locally() {
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
    let tool_str = tool_path.to_string_lossy().to_string();
    let source = format!(
        r#"fn probe() #discover {{
               discover (executor_class "default") ((host) => {{
                 host.read {path:?};
                 host.hash {path:?};
                 host.which {path:?}
               }})
             }}"#,
        path = tool_path.to_string_lossy(),
    );
    let engine = engine(&source);

    let value = engine.call("probe", vec![]).unwrap();
    let Value::Discovered(discovered) = value else {
        panic!("expected discovered value");
    };
    assert_eq!(*discovered.value, Value::String(Arc::from(tool_str)));
    assert_eq!(
        discovered.obligations.as_ref(),
        &[
            HostObservation::Hash {
                path: tool_path.to_string_lossy().to_string(),
                content_hash: expected_hash.clone(),
            },
            HostObservation::Read {
                path: tool_path.to_string_lossy().to_string(),
                content_hash: expected_hash,
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

    let missing = tempdir.path().join("missing.sh");
    let source = format!(
        r##"fn found() #discover {{
               discover (executor_class "default") ((host) => host.which? {tool_path:?})
             }}

             fn missing() #discover {{
               discover (executor_class "default") ((host) => host.which {missing:?})
             }}

             fn missing_fallible() #discover {{
               discover (executor_class "default") ((host) => host.which? {missing:?})
             }}"##,
        tool_path = tool_path.to_string_lossy(),
        missing = missing.to_string_lossy(),
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

    let err = engine.call("missing", vec![]).unwrap_err();
    assert_eq!(err.kind, PoisonKind::UserFail);
    assert!(err.message.contains("host.which could not find"));

    let missing = engine.call("missing_fallible", vec![]).unwrap();
    let Value::Discovered(missing) = missing else {
        panic!("expected discovered value");
    };
    assert_eq!(
        *missing.value,
        Value::Struct(StructValue {
            type_name: Arc::from("None"),
            fields: Arc::from([]),
        })
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
    let engine = engine(
        r#"fn observe(process: Process, tree: Tree) {
               process.exit_code
             }

             fn build(input: Tree) {
               exec ["tool", input.subtree "dir"] where { observer: observe }
             }"#,
    )
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
    let engine = engine(
        r#"fn observe(process: Process, tree: Tree) {
               process.exit_code
             }

             fn build(base: Tree, overlay: Tree) {
               exec ["tool", base.merge overlay] where { observer: observe }
             }"#,
    )
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
                 env: { "INPUT": "src/input.txt" },
                 mounts: { "src": Tree.from_path workspace },
                 writable: ["out"],
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
    let Value::Tree(TreeValue::Inline(entries)) = &request.mounts[0].value else {
        panic!("expected shipped workspace tree");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0.as_ref(), "input.txt");
    assert_eq!(
        entries[0].1,
        Value::Blob(BlobValue::from_string("hello from handler"))
    );
    assert_eq!(request.writable_roots, vec![PathValue::new("out")]);
    assert_eq!(request.observer.def.name, "observe");
    assert!(request.verification_obligations.is_empty());
    assert_eq!(request.module.functions.len(), 2);
}
