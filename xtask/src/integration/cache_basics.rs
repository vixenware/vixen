use super::ScenarioFuture;
use super::context::{BuildOptions, IntegrationContext};
use super::scaffold::{create_hello_world, create_hello_world_at};

pub(crate) fn fresh_build_succeeds(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_hello_world(context)?;
        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_not_cached()
    })
}

pub(crate) fn cargo_compat_build_succeeds_without_build_vx(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_hello_world(context)?;

        assert!(!context.file_exists("build.vx"));
        assert!(!context.file_exists("vx.styx"));

        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_success("vx build")
    })
}

pub(crate) fn build_vx_is_evaluated_and_materialized_into_dot_vx_build(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        context.write_project_file("input.txt", "hello from build.vx")?;
        context.write_project_file(
            "build.vx",
            r#"fn observe(process: Process, tree: Tree) {
  Tree {
    @output.txt: tree.file(@out/output.txt),
    @stdout.txt: process.stdout,
  }
}

fn build(workspace: Path, args: Array[String], env: Map[String, String]) {
  exec(["/bin/sh", "-c", "cat src/input.txt > out/output.txt; printf built"]) where {
    mounts: { @src: Tree(workspace) },
    writable: [@out],
    observer: observe,
  }
}
"#,
        )?;

        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_not_cached()?;
        context.assert_file_contents(".vx/build/output.txt", "hello from build.vx")?;
        context.assert_file_contents(".vx/build/stdout.txt", "built")
    })
}

pub(crate) fn build_vx_observer_can_interpret_failed_processes(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        context.write_project_file(
            "build.vx",
            r#"fn observe(process: Process, tree: Tree) {
  Tree {
    @stderr.txt: process.stderr,
  }
}

fn build(workspace: Path, args: Array[String], env: Map[String, String]) {
  exec(["/bin/sh", "-c", "printf broken >&2; exit 7"]) where {
    observer: observe,
  }
}
"#,
        )?;

        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_not_cached()?;
        context.assert_file_contents(".vx/build/stderr.txt", "broken")
    })
}

pub(crate) fn second_build_is_cache_hit(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_hello_world(context)?;
        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_not_cached()?;
        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_cached()
    })
}

pub(crate) fn source_change_causes_cache_miss(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_hello_world(context)?;
        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_success("vx build")?;

        context.write_project_file(
            "src/main.rs",
            r#"fn main() {
    println!("Hello, modified world!");
}
"#,
        )?;

        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_not_cached()
    })
}

pub(crate) fn profile_change_causes_cache_miss(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_hello_world(context)?;
        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_not_cached()?;
        context
            .run_vx_build(BuildOptions::default().release())
            .await?
            .assert_not_cached()?;
        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_cached()
    })
}

pub(crate) fn edition_change_causes_cache_miss(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_hello_world(context)?;
        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_success("vx build")?;

        context.write_project_file(
            "Cargo.toml",
            r#"[package]
name = "hello"
version = "0.1.0"
edition = "2018"
"#,
        )?;

        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_not_cached()
    })
}

pub(crate) fn clean_removes_project_local_vx_dir(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_hello_world(context)?;
        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_success("vx build")?;

        assert!(
            context.file_exists(".vx/build"),
            ".vx/build should exist after build"
        );

        context
            .run_vx_clean(false)
            .await?
            .assert_success("vx clean")?;

        assert!(
            !context.file_exists(".vx"),
            ".vx should not exist after clean"
        );
        Ok(())
    })
}

pub(crate) fn clean_cache_invalidates_last_build(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_hello_world(context)?;
        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_not_cached()?;
        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_cached()?;
        context
            .run_vx_clean(true)
            .await?
            .assert_success("vx clean --cache")?;
        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_not_cached()
    })
}

pub(crate) fn multi_file_crate_builds_successfully(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        context.write_project_file(
            "Cargo.toml",
            r#"[package]
name = "multifile"
version = "0.1.0"
edition = "2021"
"#,
        )?;
        context.write_project_file(
            "src/main.rs",
            r#"mod greeter;

fn main() {
    greeter::say_hello();
}
"#,
        )?;
        context.write_project_file(
            "src/greeter.rs",
            r#"pub fn say_hello() {
    println!("Hello from greeter module!");
}
"#,
        )?;

        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_success("vx build")
    })
}

pub(crate) fn module_change_causes_cache_miss(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        context.write_project_file(
            "Cargo.toml",
            r#"[package]
name = "multifile"
version = "0.1.0"
edition = "2021"
"#,
        )?;
        context.write_project_file(
            "src/main.rs",
            r#"mod helper;

fn main() {
    helper::greet();
}
"#,
        )?;
        context.write_project_file(
            "src/helper.rs",
            r#"pub fn greet() {
    println!("Hello!");
}
"#,
        )?;

        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_not_cached()?;
        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_cached()?;

        context.write_project_file(
            "src/helper.rs",
            r#"pub fn greet() {
    println!("Hello, modified!");
}
"#,
        )?;

        context
            .run_vx_build(BuildOptions::default())
            .await?
            .assert_not_cached()
    })
}

pub(crate) fn different_checkout_path_is_cache_hit(
    context: &mut IntegrationContext,
) -> ScenarioFuture<'_> {
    Box::pin(async move {
        create_hello_world_at(context, "project-1")?;
        create_hello_world_at(context, "project-2")?;

        context
            .run_vx_build(BuildOptions::default().subdir("project-1"))
            .await?
            .assert_not_cached()?;
        context
            .run_vx_build(BuildOptions::default().subdir("project-2"))
            .await?
            .assert_cached()
    })
}
