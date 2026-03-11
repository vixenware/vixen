use std::fs;
use std::path::Path;
use std::process::Command;

use eyre::{Context, Result, bail};
use tempfile::tempdir;

use super::{
    ScenarioFuture,
    context::{BuildOptions, IntegrationContext},
};

const LUA_SKETCH: &str = include_str!("../../../zoo/lua.vx");
const LUA_VERSION: &str = "5.4.8";
const LUA_ARCHIVE_URL: &str = "https://www.lua.org/ftp/lua-5.4.8.tar.gz";

pub(crate) fn run(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        #[cfg(not(unix))]
        eyre::bail!("the lua integration scenario currently requires a unix-like host");

        #[cfg(unix)]
        {
            context.write_project_file("build.vx", LUA_SKETCH)?;
            stage_lua_sources(context).context("failed to stage real Lua sources")?;

            let build = context.run_vx_build(BuildOptions::default()).await?;
            build.assert_success("vx build")?;

            if !context.file_exists(".vx/build/bin/lua") {
                bail!("expected .vx/build/bin/lua to exist after build");
            }
            if !context.file_exists(".vx/build/lib/liblua.a") {
                bail!("expected .vx/build/lib/liblua.a to exist after build");
            }

            let lua_binary = context.project_path(".vx/build/bin/lua");
            let output = Command::new(&lua_binary)
                .arg("-v")
                .output()
                .with_context(|| format!("failed to run {}", lua_binary.display()))?;
            if !output.status.success() {
                bail!(
                    "built lua binary exited with {}\nstdout:\n{}\nstderr:\n{}",
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            if !stdout.contains(&format!("Lua {LUA_VERSION}")) {
                bail!(
                    "expected built lua binary to report version {LUA_VERSION}\nstdout:\n{}\nstderr:\n{}",
                    stdout,
                    String::from_utf8_lossy(&output.stderr),
                );
            }

            Ok(())
        }
    })
}

fn stage_lua_sources(context: &IntegrationContext) -> Result<()> {
    let download_dir = tempdir().context("failed to create Lua download temp dir")?;
    let archive_path = download_dir.path().join("lua.tar.gz");

    run_host_command(
        Command::new("curl")
            .arg("-fsSL")
            .arg(LUA_ARCHIVE_URL)
            .arg("-o")
            .arg(&archive_path),
        "download Lua source archive",
    )?;

    run_host_command(
        Command::new("tar")
            .arg("-xzf")
            .arg(&archive_path)
            .arg("-C")
            .arg(download_dir.path()),
        "extract Lua source archive",
    )?;

    let extracted = download_dir.path().join(format!("lua-{LUA_VERSION}/src"));
    if !extracted.exists() {
        bail!("expected extracted Lua sources at {}", extracted.display());
    }

    let project_sources = context.project_path("zoo/lua-src");
    fs::create_dir_all(&project_sources)
        .with_context(|| format!("failed to create {}", project_sources.display()))?;
    copy_dir(&extracted, &project_sources)?;

    Ok(())
}

fn run_host_command(cmd: &mut Command, label: &str) -> Result<()> {
    let output = cmd.output().with_context(|| format!("failed to {label}"))?;
    if output.status.success() {
        return Ok(());
    }

    bail!(
        "{label} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    for entry in
        fs::read_dir(src).with_context(|| format!("failed to read dir {}", src.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read entry under {}", src.display()))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to stat {}", src_path.display()))?;

        if file_type.is_dir() {
            fs::create_dir_all(&dst_path)
                .with_context(|| format!("failed to create {}", dst_path.display()))?;
            copy_dir(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
        }
    }

    Ok(())
}
