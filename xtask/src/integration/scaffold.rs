use super::context::IntegrationContext;

pub(crate) fn create_hello_world(context: &IntegrationContext) -> eyre::Result<()> {
    context.write_project_file(
        "Cargo.toml",
        r#"[package]
name = "hello"
version = "0.1.0"
edition = "2021"
"#,
    )?;

    context.write_project_file(
        "src/main.rs",
        r#"fn main() {
    println!("Hello, world!");
}
"#,
    )?;

    Ok(())
}

pub(crate) fn create_hello_world_at(
    context: &IntegrationContext,
    subdir: &str,
) -> eyre::Result<()> {
    context.write_project_file(
        &format!("{subdir}/Cargo.toml"),
        r#"[package]
name = "hello"
version = "0.1.0"
edition = "2021"
"#,
    )?;

    context.write_project_file(
        &format!("{subdir}/src/main.rs"),
        r#"fn main() {
    println!("Hello, world!");
}
"#,
    )?;

    Ok(())
}

pub(crate) fn create_project_with_deps(context: &IntegrationContext) -> eyre::Result<()> {
    context.write_project_file(
        "Cargo.toml",
        r#"[package]
name = "withdeps"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = "1.0"
"#,
    )?;

    context.write_project_file("src/main.rs", "fn main() {}\n")?;
    Ok(())
}

pub(crate) fn create_workspace(context: &IntegrationContext) -> eyre::Result<()> {
    context.write_project_file(
        "Cargo.toml",
        r#"[workspace]
members = ["crates/*"]
"#,
    )?;
    Ok(())
}

pub(crate) fn create_project_with_build_script(context: &IntegrationContext) -> eyre::Result<()> {
    context.write_project_file(
        "Cargo.toml",
        r#"[package]
name = "withbuild"
version = "0.1.0"
edition = "2021"
build = "build.rs"
"#,
    )?;

    context.write_project_file("src/main.rs", "fn main() {}\n")?;
    context.write_project_file("build.rs", "fn main() {}\n")?;
    Ok(())
}

pub(crate) fn create_multi_crate_project(context: &IntegrationContext) -> eyre::Result<()> {
    context.write_project_file(
        "util/Cargo.toml",
        r#"[package]
name = "util"
version = "0.1.0"
edition = "2021"
"#,
    )?;
    context.write_project_file(
        "util/src/lib.rs",
        r#"pub fn greet() -> &'static str {
    "Hello from util!"
}
"#,
    )?;

    context.write_project_file(
        "app/Cargo.toml",
        r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
util = { path = "../util" }
"#,
    )?;
    context.write_project_file(
        "app/src/main.rs",
        r#"fn main() {
    println!("{}", util::greet());
}
"#,
    )?;

    Ok(())
}

pub(crate) fn create_diamond_dependency_project(context: &IntegrationContext) -> eyre::Result<()> {
    context.write_project_file(
        "common/Cargo.toml",
        r#"[package]
name = "common"
version = "0.1.0"
edition = "2021"
"#,
    )?;
    context.write_project_file(
        "common/src/lib.rs",
        r#"pub fn version() -> &'static str {
    "1.0.0"
}
"#,
    )?;

    context.write_project_file(
        "helper/Cargo.toml",
        r#"[package]
name = "helper"
version = "0.1.0"
edition = "2021"

[dependencies]
common = { path = "../common" }
"#,
    )?;
    context.write_project_file(
        "helper/src/lib.rs",
        r#"pub fn describe() -> String {
    format!("Helper v{}", common::version())
}
"#,
    )?;

    context.write_project_file(
        "app/Cargo.toml",
        r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
common = { path = "../common" }
helper = { path = "../helper" }
"#,
    )?;
    context.write_project_file(
        "app/src/main.rs",
        r#"fn main() {
    println!("Version: {}", common::version());
    println!("{}", helper::describe());
}
"#,
    )?;

    Ok(())
}
