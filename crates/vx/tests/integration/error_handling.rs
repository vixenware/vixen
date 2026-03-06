//! Error handling tests
//!
//! These tests verify that vx fails loudly and clearly on unsupported features.

use crate::harness::{
    TestEnv, create_project_with_build_script, create_project_with_deps, create_workspace,
};

#[test]
fn rejects_registry_deps_without_lockfile() {
    let env = TestEnv::new();
    create_project_with_deps(&env);

    // No Cargo.lock file - should fail with a clear error message
    let result = env.build().run();
    result.assert_failure_contains("Cargo.lock");
}

#[test]
fn rejects_workspace() {
    let env = TestEnv::new();
    create_workspace(&env);

    let result = env.build().run();
    result.assert_failure_contains("workspace");
}

#[test]
fn rejects_build_script() {
    let env = TestEnv::new();
    create_project_with_build_script(&env);

    let result = env.build().run();
    result.assert_failure_contains("build script");
}

#[test]
fn rejects_features() {
    let env = TestEnv::new();
    env.write_file(
        "Cargo.toml",
        r#"[package]
name = "withfeatures"
version = "0.1.0"
edition = "2021"

[features]
default = []
foo = []
"#,
    );
    env.write_file("src/main.rs", "fn main() {}\n");

    let result = env.build().run();
    result.assert_failure_contains("features");
}

#[test]
fn rejects_proc_macros() {
    let env = TestEnv::new();
    env.write_file(
        "Cargo.toml",
        r#"[package]
name = "procmacro"
version = "0.1.0"
edition = "2021"

[lib]
proc-macro = true
"#,
    );
    env.write_file("src/lib.rs", "");

    let result = env.build().run();
    result.assert_failure_contains("proc-macro");
}
