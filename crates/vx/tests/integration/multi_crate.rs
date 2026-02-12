//! Multi-crate build tests
//!
//! These tests verify that vx correctly handles projects with path dependencies:
//! - Building a bin crate that depends on a lib crate
//! - Caching works correctly (deps before dependents)
//! - Changes to deps trigger rebuilds of dependents
//! - Changes to app only rebuild app
//! - Path independence (same content, different checkout location = cache hit)

use crate::harness::{TestEnv, create_diamond_dependency_project, create_multi_crate_project};

#[test]
fn multi_crate_build_succeeds() {
    let env = TestEnv::new();
    create_multi_crate_project(&env);

    env.build().in_subdir("app").run().assert_not_cached();
}

#[test]
fn multi_crate_second_build_is_cache_hit() {
    let env = TestEnv::new();
    create_multi_crate_project(&env);

    // First build
    env.build().in_subdir("app").run().assert_not_cached();

    // Second build - should be fully cached
    env.build().in_subdir("app").run().assert_cached();
}

#[test]
fn dep_change_rebuilds_both() {
    let env = TestEnv::new();
    create_multi_crate_project(&env);

    // First build
    env.build().in_subdir("app").run().assert_not_cached();

    // Modify the util library
    env.write_file(
        "util/src/lib.rs",
        r#"pub fn greet() -> &'static str {
    "Hello from MODIFIED util!"
}
"#,
    );

    // Second build - should rebuild both util and app
    env.build().in_subdir("app").run().assert_not_cached();
}

#[test]
fn app_change_rebuilds_only_app() {
    let env = TestEnv::new();
    create_multi_crate_project(&env);

    // First build
    env.build().in_subdir("app").run().assert_success();

    // Modify only the app
    env.write_file(
        "app/src/main.rs",
        r#"fn main() {
    println!("Modified: {}", util::greet());
}
"#,
    );

    // Second build - util should still be cached, only app rebuilds
    // The overall build is not fully cached since app changed
    env.build().in_subdir("app").run().assert_not_cached();
}

#[test]
fn module_rename_invalidates_cache() {
    let env = TestEnv::new();
    create_multi_crate_project(&env);

    // Add a module to util
    env.write_file(
        "util/src/lib.rs",
        r#"mod helper;
pub use helper::greet;
"#,
    );
    env.write_file(
        "util/src/helper.rs",
        r#"pub fn greet() -> &'static str {
    "Hello from helper module!"
}
"#,
    );

    // First build
    env.build().in_subdir("app").run().assert_success();

    // Second build - should be cached
    env.build().in_subdir("app").run().assert_cached();

    // Rename the module file (helper.rs -> greeter.rs)
    std::fs::remove_file(env.project_path().join("util/src/helper.rs")).unwrap();
    env.write_file(
        "util/src/greeter.rs",
        r#"pub fn greet() -> &'static str {
    "Hello from greeter module!"
}
"#,
    );
    env.write_file(
        "util/src/lib.rs",
        r#"mod greeter;
pub use greeter::greet;
"#,
    );

    // Third build - should NOT be cached (module path changed)
    env.build().in_subdir("app").run().assert_not_cached();
}

#[test]
fn diamond_dependency_builds() {
    let env = TestEnv::new();
    create_diamond_dependency_project(&env);

    env.build().in_subdir("app").run().assert_success();
}

#[test]
fn diamond_dependency_cache_hit() {
    let env = TestEnv::new();
    create_diamond_dependency_project(&env);

    // First build
    env.build().in_subdir("app").run().assert_success();

    // Second build - should be cached
    env.build().in_subdir("app").run().assert_cached();
}

#[test]
fn diamond_shared_dep_change_rebuilds_dependents() {
    let env = TestEnv::new();
    create_diamond_dependency_project(&env);

    // First build
    env.build().in_subdir("app").run().assert_success();

    // Modify the common library (shared dep)
    env.write_file(
        "common/src/lib.rs",
        r#"pub fn version() -> &'static str {
    "2.0.0"
}
"#,
    );

    // Second build - should rebuild common, helper, and app
    env.build().in_subdir("app").run().assert_not_cached();
}
