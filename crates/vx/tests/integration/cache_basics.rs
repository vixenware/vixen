//! Basic cache correctness tests
//!
//! These tests verify fundamental caching behavior:
//! - Fresh builds work (single and multi-file)
//! - Second build is a cache hit
//! - Source/profile/edition changes cause cache miss
//! - Changes to any module in the closure cause cache miss

use crate::harness::{TestEnv, create_hello_world, create_hello_world_at, init_test_tracing};
use std::fs;

#[test]
// r[verify exec.writable]
fn fresh_build_succeeds() {
    init_test_tracing();
    let env = TestEnv::new();
    create_hello_world(&env);

    let result = env.build().run();
    result.assert_not_cached();
}

#[test]
// r[verify eval.entry.cargo-compat]
fn cargo_compat_build_succeeds_without_build_vx() {
    let env = TestEnv::new();
    create_hello_world(&env);

    assert!(!env.file_exists("build.vx"));
    assert!(!env.file_exists("vx.styx"));

    env.build().run().assert_success();
}

#[test]
fn build_vx_is_evaluated_and_materialized_into_dot_vx_build() {
    let env = TestEnv::new();
    env.write_file("input.txt", "hello from build.vx");
    env.write_file(
        "build.vx",
        r#"fn observe(process: Process, tree: Tree) {
  Tree.of(%{
    "output.txt" => tree.file("out/output.txt"),
    "stdout.txt" => process.stdout,
  })
}

fn build(workspace: Path, args: Array[String], env: Map[String, String]) {
  exec(
    ["/bin/sh", "-c", "cat src/input.txt > out/output.txt; printf built"];
    mounts: %{ "src" => workspace },
    writable: ["out"],
    observer: observe,
  )
}
"#,
    );

    let result = env.build().run();
    result.assert_not_cached();

    assert_eq!(
        fs::read_to_string(env.project_path().join(".vx/build/output.txt")).unwrap(),
        "hello from build.vx"
    );
    assert_eq!(
        fs::read_to_string(env.project_path().join(".vx/build/stdout.txt")).unwrap(),
        "built"
    );
}

#[test]
// r[verify exec.errors]
fn build_vx_observer_can_interpret_failed_processes() {
    let env = TestEnv::new();
    env.write_file(
        "build.vx",
        r#"fn observe(process: Process, tree: Tree) {
  Tree.of(%{
    "stderr.txt" => process.stderr,
  })
}

fn build(workspace: Path, args: Array[String], env: Map[String, String]) {
  exec(
    ["/bin/sh", "-c", "printf broken >&2; exit 7"];
    observer: observe,
  )
}
"#,
    );

    let result = env.build().run();
    result.assert_not_cached();

    assert_eq!(
        fs::read_to_string(env.project_path().join(".vx/build/stderr.txt")).unwrap(),
        "broken"
    );
}

#[test]
// r[verify exec.verifier]
fn build_vx_verifier_failures_poison_exec() {
    let env = TestEnv::new();
    env.write_file(
        "build.vx",
        r#"fn verify() { fail "toolchain drift" }

fn observe(process: Process, tree: Tree) {
  Tree.of(%{
    "stdout.txt" => process.stdout,
  })
}

fn build(workspace: Path, args: Array[String], env: Map[String, String]) {
  exec(
    ["/bin/sh", "-c", "printf built"];
    observer: observe,
    verifier: verify,
  )
}
"#,
    );

    let result = env.build().run();
    result.assert_failure_contains("VerificationFailure");
    result.assert_failure_contains("toolchain drift");
}

#[test]
fn second_build_is_cache_hit() {
    let env = TestEnv::new();
    create_hello_world(&env);

    // First build
    let result1 = env.build().run();
    result1.assert_not_cached();

    // Second build — should be cached
    let result2 = env.build().run();
    result2.assert_cached();
}

#[test]
fn source_change_causes_cache_miss() {
    let env = TestEnv::new();
    create_hello_world(&env);

    // First build
    env.build().run().assert_success();

    // Modify source
    env.write_file(
        "src/main.rs",
        r#"fn main() {
    println!("Hello, modified world!");
}
"#,
    );

    // Second build — should NOT be cached
    env.build().run().assert_not_cached();
}

#[test]
fn profile_change_causes_cache_miss() {
    let env = TestEnv::new();
    create_hello_world(&env);

    // Debug build
    env.build().run().assert_not_cached();

    // Release build — should NOT be cached (different profile)
    env.build().release().run().assert_not_cached();

    // Debug build again — should still be cached from first build
    env.build().run().assert_cached();
}

#[test]
fn edition_change_causes_cache_miss() {
    let env = TestEnv::new();
    create_hello_world(&env);

    // Build with 2021 edition
    env.build().run().assert_success();

    // Change edition to 2018
    env.write_file(
        "Cargo.toml",
        r#"[package]
name = "hello"
version = "0.1.0"
edition = "2018"
"#,
    );

    // Second build — should NOT be cached
    env.build().run().assert_not_cached();
}

#[test]
fn clean_removes_project_local_vx_dir() {
    let env = TestEnv::new();
    create_hello_world(&env);

    // Build first
    env.build().run().assert_success();

    // Check .vx/build exists
    assert!(
        env.file_exists(".vx/build"),
        ".vx/build should exist after build"
    );

    // Clean
    let clean_result = env.clean();
    clean_result.assert_success();

    // Check .vx is gone
    assert!(!env.file_exists(".vx"), ".vx should not exist after clean");
}

#[test]
fn clean_cache_invalidates_last_build() {
    let env = TestEnv::new();
    create_hello_world(&env);

    // First build — cache miss
    env.build().run().assert_not_cached();

    // Second build — should be cached
    env.build().run().assert_cached();

    // Invalidate cache entries
    let clean_result = env.clean_cache();
    clean_result.assert_success();

    // Third build — should be a cache miss again (entries were invalidated)
    env.build().run().assert_not_cached();
}

#[test]
fn multi_file_crate_builds_successfully() {
    // This test verifies that crates with multiple source files
    // (using mod declarations) build correctly.
    let env = TestEnv::new();

    env.write_file(
        "Cargo.toml",
        r#"[package]
name = "multifile"
version = "0.1.0"
edition = "2021"
"#,
    );
    env.write_file(
        "src/main.rs",
        r#"mod greeter;

fn main() {
    greeter::say_hello();
}
"#,
    );
    env.write_file(
        "src/greeter.rs",
        r#"pub fn say_hello() {
    println!("Hello from greeter module!");
}
"#,
    );

    env.build().run().assert_success();
}

#[test]
fn module_change_causes_cache_miss() {
    // This test verifies that changing a non-main module file
    // correctly invalidates the cache.
    let env = TestEnv::new();

    env.write_file(
        "Cargo.toml",
        r#"[package]
name = "multifile"
version = "0.1.0"
edition = "2021"
"#,
    );
    env.write_file(
        "src/main.rs",
        r#"mod helper;

fn main() {
    helper::greet();
}
"#,
    );
    env.write_file(
        "src/helper.rs",
        r#"pub fn greet() {
    println!("Hello!");
}
"#,
    );

    // First build
    env.build().run().assert_not_cached();

    // Second build — should be cached
    env.build().run().assert_cached();

    // Modify the helper module (not main.rs)
    env.write_file(
        "src/helper.rs",
        r#"pub fn greet() {
    println!("Hello, modified!");
}
"#,
    );

    // Third build — should NOT be cached
    env.build().run().assert_not_cached();
}

#[test]
fn different_checkout_path_is_cache_hit() {
    // This test verifies that the same project checked out in different locations
    // produces a cache hit (path normalization via --remap-path-prefix)
    let env = TestEnv::new();

    create_hello_world_at(&env, "project-1");
    create_hello_world_at(&env, "project-2");

    // Build in first location
    env.build().in_subdir("project-1").run().assert_not_cached();

    // Build in second location — should be cache hit
    // because content is identical and paths are remapped
    env.build().in_subdir("project-2").run().assert_cached();
}
