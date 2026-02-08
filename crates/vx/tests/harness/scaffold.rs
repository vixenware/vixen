use crate::harness::TestEnv;

/// Create a minimal hello world project
pub fn create_hello_world(env: &TestEnv) {
    env.write_file(
        "Cargo.toml",
        r#"[package]
name = "hello"
version = "0.1.0"
edition = "2021"
"#,
    );

    env.write_file(
        "src/main.rs",
        r#"fn main() {
    println!("Hello, world!");
}
"#,
    );
}

/// Create a minimal hello world project
pub fn create_hello_world_at(env: &TestEnv, subdir: &str) {
    env.write_file(
        &format!("{subdir}/Cargo.toml"),
        r#"[package]
name = "hello"
version = "0.1.0"
edition = "2021"
"#,
    );

    env.write_file(
        &format!("{subdir}/src/main.rs"),
        r#"fn main() {
    println!("Hello, world!");
}
"#,
    );
}

/// Create a project with dependencies (should fail)
pub fn create_project_with_deps(env: &TestEnv) {
    env.write_file(
        "Cargo.toml",
        r#"[package]
name = "withdeps"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = "1.0"
"#,
    );

    env.write_file("src/main.rs", "fn main() {}\n");
}

/// Create a workspace project (should fail)
pub fn create_workspace(env: &TestEnv) {
    env.write_file(
        "Cargo.toml",
        r#"[workspace]
members = ["crates/*"]
"#,
    );
}

/// Create a project with build script (should fail)
pub fn create_project_with_build_script(env: &TestEnv) {
    env.write_file(
        "Cargo.toml",
        r#"[package]
name = "withbuild"
version = "0.1.0"
edition = "2021"
build = "build.rs"
"#,
    );

    env.write_file("src/main.rs", "fn main() {}\n");
    env.write_file("build.rs", "fn main() {}\n");
}

/// Create a multi-crate project with app (bin) depending on util (lib)
///
/// Layout:
/// ```
/// project/
/// ├── app/
/// │   ├── Cargo.toml    # [dependencies] util = { path = "../util" }
/// │   └── src/main.rs   # use util::greet;
/// └── util/
///     ├── Cargo.toml
///     └── src/lib.rs    # pub fn greet() -> &'static str
/// ```
pub fn create_multi_crate_project(env: &TestEnv) {
    // util library crate
    env.write_file(
        "util/Cargo.toml",
        r#"[package]
name = "util"
version = "0.1.0"
edition = "2021"
"#,
    );
    env.write_file(
        "util/src/lib.rs",
        r#"pub fn greet() -> &'static str {
    "Hello from util!"
}
"#,
    );

    // app binary crate that depends on util
    env.write_file(
        "app/Cargo.toml",
        r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
util = { path = "../util" }
"#,
    );
    env.write_file(
        "app/src/main.rs",
        r#"fn main() {
    println!("{}", util::greet());
}
"#,
    );
}

/// Create a diamond dependency project: app -> {common, helper}, helper -> common
///
/// Layout:
/// ```
/// project/
/// ├── app/         # bin, depends on common and helper
/// ├── helper/      # lib, depends on common
/// └── common/      # lib, no deps
/// ```
pub fn create_diamond_dependency_project(env: &TestEnv) {
    // common library (no deps)
    env.write_file(
        "common/Cargo.toml",
        r#"[package]
name = "common"
version = "0.1.0"
edition = "2021"
"#,
    );
    env.write_file(
        "common/src/lib.rs",
        r#"pub fn version() -> &'static str {
    "1.0.0"
}
"#,
    );

    // helper library (depends on common)
    env.write_file(
        "helper/Cargo.toml",
        r#"[package]
name = "helper"
version = "0.1.0"
edition = "2021"

[dependencies]
common = { path = "../common" }
"#,
    );
    env.write_file(
        "helper/src/lib.rs",
        r#"pub fn describe() -> String {
    format!("Helper v{}", common::version())
}
"#,
    );

    // app binary (depends on both common and helper)
    env.write_file(
        "app/Cargo.toml",
        r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
common = { path = "../common" }
helper = { path = "../helper" }
"#,
    );
    env.write_file(
        "app/src/main.rs",
        r#"fn main() {
    println!("Version: {}", common::version());
    println!("{}", helper::describe());
}
"#,
    );
}
