use super::ScenarioFuture;
use super::context::{BuildOptions, IntegrationContext};
use super::scaffold::{
    create_project_with_build_script, create_project_with_deps, create_workspace,
};

pub(crate) fn rejects_registry_deps_without_lockfile(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_project_with_deps(context)?;
        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_failure_contains("vx build", "Cargo.lock")
    })
}

pub(crate) fn rejects_workspace(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_workspace(context)?;
        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_failure_contains("vx build", "workspace")
    })
}

pub(crate) fn rejects_build_script(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_project_with_build_script(context)?;
        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_failure_contains("vx build", "build script")
    })
}

pub(crate) fn rejects_features(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        context.write_project_file(
            "Cargo.toml",
            r#"[package]
name = "withfeatures"
version = "0.1.0"
edition = "2021"

[features]
default = []
foo = []
"#,
        )?;
        context.write_project_file("src/main.rs", "fn main() {}\n")?;

        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_failure_contains("vx build", "features")
    })
}

pub(crate) fn rejects_proc_macros(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        context.write_project_file(
            "Cargo.toml",
            r#"[package]
name = "procmacro"
version = "0.1.0"
edition = "2021"

[lib]
proc-macro = true
"#,
        )?;
        context.write_project_file("src/lib.rs", "")?;

        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_failure_contains("vx build", "proc-macro")
    })
}

pub(crate) fn invalid_build_vx_reports_parse_errors(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        context.write_project_file("build.vx", "fn build(")?;
        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_failure_contains("vx build", "failed to parse build.vx")
    })
}

pub(crate) fn build_vx_requires_tree_output(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        context.write_project_file(
            "build.vx",
            r#"fn build(workspace: Path, args: Array[String], env: Map[String, String]) {
  42
}
"#,
        )?;

        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_failure_contains("vx build", "must return Tree")
    })
}
