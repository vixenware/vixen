use super::ScenarioFuture;
use super::context::{BuildOptions, IntegrationContext};
use super::scaffold::{create_diamond_dependency_project, create_multi_crate_project};

pub(crate) fn multi_crate_build_succeeds(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_multi_crate_project(context)?;
        context
            .run_vx_build(BuildOptions::default().subdir("app"))
            .await?
            .assert_not_cached()
    })
}

pub(crate) fn multi_crate_second_build_is_cache_hit(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_multi_crate_project(context)?;
        context
            .run_vx_build(BuildOptions::default().subdir("app"))
            .await?
            .assert_not_cached()?;
        context
            .run_vx_build(BuildOptions::default().subdir("app"))
            .await?
            .assert_cached()
    })
}

pub(crate) fn dep_change_rebuilds_both(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_multi_crate_project(context)?;
        context
            .run_vx_build(BuildOptions::default().subdir("app"))
            .await?
            .assert_not_cached()?;

        context.write_project_file(
            "util/src/lib.rs",
            r#"pub fn greet() -> &'static str {
    "Hello from MODIFIED util!"
}
"#,
        )?;

        context
            .run_vx_build(BuildOptions::default().subdir("app"))
            .await?
            .assert_not_cached()
    })
}

pub(crate) fn app_change_rebuilds_only_app(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_multi_crate_project(context)?;
        context
            .run_vx_build(BuildOptions::default().subdir("app"))
            .await?
            .assert_success("vx build")?;

        context.write_project_file(
            "app/src/main.rs",
            r#"fn main() {
    println!("Modified: {}", util::greet());
}
"#,
        )?;

        context
            .run_vx_build(BuildOptions::default().subdir("app"))
            .await?
            .assert_not_cached()
    })
}

pub(crate) fn module_rename_invalidates_cache(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_multi_crate_project(context)?;

        context.write_project_file(
            "util/src/lib.rs",
            r#"mod helper;
pub use helper::greet;
"#,
        )?;
        context.write_project_file(
            "util/src/helper.rs",
            r#"pub fn greet() -> &'static str {
    "Hello from helper module!"
}
"#,
        )?;

        context
            .run_vx_build(BuildOptions::default().subdir("app"))
            .await?
            .assert_success("vx build")?;
        context
            .run_vx_build(BuildOptions::default().subdir("app"))
            .await?
            .assert_cached()?;

        context.remove_project_file("util/src/helper.rs")?;
        context.write_project_file(
            "util/src/greeter.rs",
            r#"pub fn greet() -> &'static str {
    "Hello from greeter module!"
}
"#,
        )?;
        context.write_project_file(
            "util/src/lib.rs",
            r#"mod greeter;
pub use greeter::greet;
"#,
        )?;

        context
            .run_vx_build(BuildOptions::default().subdir("app"))
            .await?
            .assert_not_cached()
    })
}

pub(crate) fn diamond_dependency_builds(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_diamond_dependency_project(context)?;
        context
            .run_vx_build(BuildOptions::default().subdir("app"))
            .await?
            .assert_success("vx build")
    })
}

pub(crate) fn diamond_dependency_cache_hit(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_diamond_dependency_project(context)?;
        context
            .run_vx_build(BuildOptions::default().subdir("app"))
            .await?
            .assert_success("vx build")?;
        context
            .run_vx_build(BuildOptions::default().subdir("app"))
            .await?
            .assert_cached()
    })
}

pub(crate) fn diamond_shared_dep_change_rebuilds_dependents(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_diamond_dependency_project(context)?;
        context
            .run_vx_build(BuildOptions::default().subdir("app"))
            .await?
            .assert_success("vx build")?;

        context.write_project_file(
            "common/src/lib.rs",
            r#"pub fn version() -> &'static str {
    "2.0.0"
}
"#,
        )?;

        context
            .run_vx_build(BuildOptions::default().subdir("app"))
            .await?
            .assert_not_cached()
    })
}
