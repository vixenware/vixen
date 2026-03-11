use super::{
    ScenarioFuture,
    context::{BuildOptions, IntegrationContext},
};

const LUA_SKETCH: &str = include_str!("../../../zoo/lua.vx");

const LUA_SOURCES: &[&str] = &[
    "lapi.c",
    "lcode.c",
    "lctype.c",
    "ldebug.c",
    "ldo.c",
    "ldump.c",
    "lfunc.c",
    "lgc.c",
    "llex.c",
    "lmem.c",
    "lobject.c",
    "lopcodes.c",
    "lparser.c",
    "lstate.c",
    "lstring.c",
    "ltable.c",
    "ltm.c",
    "lundump.c",
    "lvm.c",
    "lzio.c",
    "ltests.c",
    "lauxlib.c",
    "lbaselib.c",
    "ldblib.c",
    "liolib.c",
    "lmathlib.c",
    "loslib.c",
    "ltablib.c",
    "lstrlib.c",
    "lutf8lib.c",
    "loadlib.c",
    "lcorolib.c",
    "linit.c",
    "lua.c",
];

pub(crate) fn run(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        #[cfg(not(unix))]
        eyre::bail!("the lua integration scenario currently requires a unix-like host");

        #[cfg(unix)]
        {
            context.write_project_file("build.vx", LUA_SKETCH)?;

            for source in LUA_SOURCES {
                context.write_project_file(
                    &format!("zoo/lua-src/{source}"),
                    "int main(void) { return 0; }\n",
                )?;
            }

            context.write_fake_tool(
                "cc",
                r#"#!/bin/sh
out=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "-o" ]; then
    out="$arg"
    break
  fi
  prev="$arg"
done
if [ -n "$out" ]; then
  mkdir -p "$(dirname "$out")"
  printf 'fake cc output\n' > "$out"
fi
exit 0
"#,
            )?;
            context.write_fake_tool(
                "ar",
                r#"#!/bin/sh
out=""
for arg in "$@"; do
  case "$arg" in
    *.a) out="$arg" ;;
  esac
done
if [ -n "$out" ]; then
  mkdir -p "$(dirname "$out")"
  printf 'fake archive\n' > "$out"
fi
exit 0
"#,
            )?;
            context.write_fake_tool(
                "ranlib",
                r#"#!/bin/sh
exit 0
"#,
            )?;

            let build = context.run_vx_build(BuildOptions::default()).await?;
            build.assert_success("vx build")?;

            context.assert_file_contents(".vx/build/bin/lua", "fake cc output\n")?;
            context.assert_file_contents(".vx/build/lib/liblua.a", "fake archive\n")?;

            Ok(())
        }
    })
}
