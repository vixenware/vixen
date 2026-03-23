use super::*;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use vx_eval2::ValueRep;

use crate::eval_runtime::build_eval_runtime;

#[test]
fn parse_eval_arg_converts_nested_json_values() {
    let value = parse_eval_arg(r#"{"name":"serde","versions":[1,2]}"#).expect("parse arg");
    let ValueRep::Map(map) = &value.rep else {
        panic!("expected map arg, got {:?}", value.rep);
    };

    assert_eq!(map.entries.len(), 2);
    assert!(map.entries.iter().any(|(key, value)| {
        matches!((&key.rep, &value.rep), (ValueRep::String(key), ValueRep::String(value)) if key.as_ref() == "name" && value.as_ref() == "serde")
    }));
    assert!(map.entries.iter().any(|(key, value)| {
        matches!((&key.rep, &value.rep), (ValueRep::String(key), ValueRep::Array(items)) if key.as_ref() == "versions" && items.len() == 2)
    }));
}

#[test]
fn parse_eval_arg_supports_external_path_shorthand() {
    let value =
        parse_eval_arg(r#"{"$path":"specs/eval/run/cargo_sha2_hello"}"#).expect("parse path arg");
    let ValueRep::External(external) = &value.rep else {
        panic!("expected external path arg, got {:?}", value.rep);
    };
    assert_eq!(external.kind.as_ref(), "Path");
    assert_eq!(
        external.descriptor.as_ref(),
        "specs/eval/run/cargo_sha2_hello"
    );
}

#[test]
fn read_eval_source_uses_parent_directory_for_runtime_root() {
    let (_source, source_id, root_dir) = read_eval_source("Cargo.toml").expect("read source");

    assert!(source_id.ends_with("Cargo.toml"));
    assert_eq!(root_dir, std::env::current_dir().expect("cwd"));
}

#[test]
fn local_eval_runtime_supports_read_file() {
    let runtime = build_eval_runtime(
        "local",
        std::env::current_dir().expect("source dir"),
        std::env::current_dir().expect("cwd"),
    )
    .expect("build local eval runtime");
    let resolution = vx_eval2::eval_source_entry_at_place_with_args_with_source_id(
        "fn build() { read_file(\"Cargo.toml\") }",
        "local-eval-runtime-supports-read-file",
        "build",
        &[],
        &runtime,
        "local",
    )
    .expect("evaluate read_file");
    let Resolution::Ready(value) = resolution else {
        panic!("expected ready outcome, got {resolution:?}");
    };
    assert!(matches!(value.rep, ValueRep::External(_)));
}

#[test]
fn cargo_sketch_runtime_supports_tree_file_from_path_receiver() {
    let root = tempfile::tempdir().expect("tempdir");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::write(workspace.join("Cargo.toml"), "[package]\nname = \"demo\"\n")
        .expect("write Cargo.toml");

    let runtime = build_eval_runtime(
        "cargo-sketch",
        std::env::current_dir().expect("source dir"),
        root.path().to_path_buf(),
    )
    .expect("build cargo-sketch eval runtime");
    let resolution = vx_eval2::eval_source_entry_at_place_with_args_with_source_id(
        "fn build() { Tree(Path(\"workspace\")).file(Path(\"Cargo.toml\")) }",
        "cargo-sketch-runtime-tree-file",
        "build",
        &[],
        &runtime,
        "local",
    )
    .expect("evaluate Tree.file");
    let Resolution::Ready(blob) = resolution else {
        panic!("expected ready Blob, got {resolution:?}");
    };
    let ValueRep::External(external) = &blob.rep else {
        panic!("expected external blob, got {:?}", blob.rep);
    };
    assert_eq!(external.kind.as_ref(), "Blob");
    assert!(external.descriptor.contains("name = \"demo\""));
}

#[test]
fn cargo_sketch_runtime_build_materializes_local_vendor_tree() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/cargo-resolve.vx"),
    )
    .expect("read zoo/cargo-resolve.vx");
    let runtime = build_eval_runtime(
        "cargo-sketch",
        std::env::current_dir().expect("source dir"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
    )
    .expect("build cargo-sketch eval runtime");
    let resolution = vx_eval2::eval_source_entry_at_place_with_args_with_source_id(
        &zoo_source,
        "cargo-sketch-build-local-vendor-tree",
        "build",
        &[
            parse_eval_arg(r#"{"$path":"specs/eval/run/cargo_local_build_hello"}"#)
                .expect("parse workspace path"),
            parse_eval_arg("[]").expect("parse empty args"),
            parse_eval_arg("{}").expect("parse empty env"),
        ],
        &runtime,
        "local",
    )
    .expect("evaluate build vertical slice");
    let Resolution::Ready(tree) = resolution else {
        panic!("expected ready build tree, got {resolution:?}");
    };
    let ValueRep::External(external) = &tree.rep else {
        panic!("expected external tree, got {:?}", tree.rep);
    };
    assert_eq!(external.kind.as_ref(), "Tree");

    let root = PathBuf::from(external.descriptor.as_ref());
    assert!(root.join("Cargo.lock").is_file());
    assert!(root.join("vendor/helper-local-0.1.0/Cargo.toml").is_file());
    assert!(root.join("vendor/helper-local-0.1.0/src/lib.rs").is_file());
    assert!(!root.join("vendor/vendor").exists());
    assert!(!root.join("vendor/{pkg.name}-{pkg.version}").exists());

    let binary = root.join("out/hello-local-build");
    assert!(
        binary.is_file(),
        "expected built binary at {}",
        binary.display()
    );

    let output = Command::new(&binary)
        .output()
        .expect("run built local hello");
    assert!(
        output.status.success(),
        "built local hello should run successfully"
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hello\n");
}

#[test]
fn build_sketch_runtime_merges_tree_streams() {
    let root = tempfile::tempdir().expect("tempdir");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(workspace.join("one")).expect("create workspace/one");
    std::fs::create_dir_all(workspace.join("two")).expect("create workspace/two");
    std::fs::write(workspace.join("one/hello.txt"), "hello from one").expect("write one");
    std::fs::write(workspace.join("two/goodbye.txt"), "goodbye from two").expect("write two");

    let runtime = build_eval_runtime(
        "build-sketch",
        std::env::current_dir().expect("source dir"),
        root.path().to_path_buf(),
    )
    .expect("build build-sketch eval runtime");
    let resolution = vx_eval2::eval_source_entry_at_place_with_args_with_source_id(
        r#"
fn build() {
  [Path("workspace/one"), Path("workspace/two")]
    .map((path) => Tree(path))
    .merge()
    .file(Path("hello.txt"))
}
"#,
        "build-sketch-stream-merge",
        "build",
        &[],
        &runtime,
        "local",
    )
    .expect("evaluate merged tree stream");
    let Resolution::Ready(blob) = resolution else {
        panic!("expected ready Blob, got {resolution:?}");
    };
    let ValueRep::External(external) = &blob.rep else {
        panic!("expected external blob, got {:?}", blob.rep);
    };
    assert_eq!(external.kind.as_ref(), "Blob");
    assert_eq!(external.descriptor.as_ref(), "hello from one");
}

#[test]
fn build_sketch_runtime_merges_tree_arrays() {
    let root = tempfile::tempdir().expect("tempdir");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(workspace.join("one")).expect("create workspace/one");
    std::fs::create_dir_all(workspace.join("two")).expect("create workspace/two");
    std::fs::write(workspace.join("one/hello.txt"), "hello from one").expect("write one");
    std::fs::write(workspace.join("two/goodbye.txt"), "goodbye from two").expect("write two");

    let runtime = build_eval_runtime(
        "build-sketch",
        std::env::current_dir().expect("source dir"),
        root.path().to_path_buf(),
    )
    .expect("build build-sketch eval runtime");
    let resolution = vx_eval2::eval_source_entry_at_place_with_args_with_source_id(
        r#"
fn build() {
  [Tree(Path("workspace/one")), Tree(Path("workspace/two"))]
    .merge()
    .file(Path("goodbye.txt"))
}
"#,
        "build-sketch-array-merge",
        "build",
        &[],
        &runtime,
        "local",
    )
    .expect("evaluate merged tree array");
    let Resolution::Ready(blob) = resolution else {
        panic!("expected ready Blob, got {resolution:?}");
    };
    let ValueRep::External(external) = &blob.rep else {
        panic!("expected external blob, got {:?}", blob.rep);
    };
    assert_eq!(external.kind.as_ref(), "Blob");
    assert_eq!(external.descriptor.as_ref(), "goodbye from two");
}

#[test]
fn build_sketch_runtime_rejects_conflicting_tree_stream_merges() {
    let root = tempfile::tempdir().expect("tempdir");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(workspace.join("one")).expect("create workspace/one");
    std::fs::create_dir_all(workspace.join("two")).expect("create workspace/two");
    std::fs::write(workspace.join("one/conflict.txt"), "one").expect("write one");
    std::fs::write(workspace.join("two/conflict.txt"), "two").expect("write two");

    let runtime = build_eval_runtime(
        "build-sketch",
        std::env::current_dir().expect("source dir"),
        root.path().to_path_buf(),
    )
    .expect("build build-sketch eval runtime");
    let error = vx_eval2::eval_source_entry_at_place_with_args_with_source_id(
        r#"
fn build() {
  [Path("workspace/one"), Path("workspace/two")]
    .map((path) => Tree(path))
    .merge()
    .file(Path("conflict.txt"))
}
"#,
        "build-sketch-stream-merge-conflict",
        "build",
        &[],
        &runtime,
        "local",
    )
    .expect_err("conflicting merged tree stream should fail");
    assert!(
        error.message.contains("tree merge conflict"),
        "expected merge conflict error, got {error:?}"
    );
}

#[test]
fn build_sketch_runtime_mounts_binary_tree_outputs_between_execs() {
    let source = r#"
fn build() -> Tree (
  let first =
    exec ["/bin/sh", "-euc", "cp /bin/sh out/copied"] where {
      writable: [@out],
      observer: (proc, vfs) => (
        if proc.exit_code != 0 (
          fail proc.stderr.collect().utf8_lossy()
        ) else (
          vfs
        )
      ),
    };

  exec ["/bin/sh", "-euc", "test -f prev/out/copied && cp prev/out/copied out/copied2"] where {
    mounts: %{ @prev => first },
    writable: [@out],
    observer: (proc, vfs) => (
      if proc.exit_code != 0 (
        fail proc.stderr.collect().utf8_lossy()
      ) else (
        vfs.subtree(@out)
      )
    ),
  }
)
"#;
    let runtime = build_eval_runtime(
        "build-sketch",
        std::env::current_dir().expect("source dir"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
    )
    .expect("build build-sketch eval runtime");
    let resolution = vx_eval2::eval_source_entry_at_place_with_args_with_source_id(
        source,
        "build-sketch-binary-tree-handoff",
        "build",
        &[],
        &runtime,
        "local",
    )
    .expect("evaluate binary tree handoff");
    let Resolution::Ready(tree) = resolution else {
        panic!("expected ready build tree, got {resolution:?}");
    };
    let ValueRep::External(external) = &tree.rep else {
        panic!("expected external tree, got {:?}", tree.rep);
    };
    assert_eq!(external.kind.as_ref(), "Tree");

    let root = PathBuf::from(external.descriptor.as_ref());
    let copied = root.join("copied2");
    assert!(
        copied.is_file(),
        "expected copied binary at {}",
        copied.display()
    );
}

#[test]
fn build_sketch_runtime_compiles_and_runs_rustc_hello() {
    let zoo_source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../zoo/rustc-hello.vx"),
    )
    .expect("read zoo/rustc-hello.vx");
    let runtime = build_eval_runtime(
        "build-sketch",
        std::env::current_dir().expect("source dir"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
    )
    .expect("build build-sketch eval runtime");
    let resolution = vx_eval2::eval_source_entry_at_place_with_args_with_source_id(
        &zoo_source,
        "build-sketch-rustc-hello",
        "build",
        &[
            parse_eval_arg(r#"{"$path":"."}"#).expect("parse workspace path"),
            parse_eval_arg("[]").expect("parse empty args"),
            parse_eval_arg("{}").expect("parse empty env"),
        ],
        &runtime,
        "local",
    )
    .expect("evaluate rustc hello build");
    let Resolution::Ready(tree) = resolution else {
        panic!("expected ready build tree, got {resolution:?}");
    };
    let ValueRep::External(external) = &tree.rep else {
        panic!("expected external tree, got {:?}", tree.rep);
    };
    assert_eq!(external.kind.as_ref(), "Tree");

    let root = PathBuf::from(external.descriptor.as_ref());
    let binary = root.join("out/hello");
    assert!(
        binary.is_file(),
        "expected built binary at {}",
        binary.display()
    );

    let output = Command::new(&binary).output().expect("run built hello");
    assert!(
        output.status.success(),
        "built hello should run successfully"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "hello from vx eval\n"
    );
}
