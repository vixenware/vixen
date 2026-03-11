use crate::harness::{TestEnv, init_test_tracing};
use std::env;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

const LUA_SKETCH: &str = include_str!("../../../../zoo/lua.vx");

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

#[test]
fn lua_sketch_builds_through_build_vx() {
    init_test_tracing();

    let env = TestEnv::new();
    env.write_file("build.vx", LUA_SKETCH);

    for source in LUA_SOURCES {
        env.write_file(
            &format!("zoo/lua-src/{source}"),
            "int main(void) { return 0; }\n",
        );
    }

    let fake_bin = env.project_path().join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("failed to create fake-bin");
    write_executable(
        &fake_bin.join("cc"),
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
    );
    write_executable(
        &fake_bin.join("ar"),
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
    );
    write_executable(
        &fake_bin.join("ranlib"),
        r#"#!/bin/sh
exit 0
"#,
    );

    let path = env::var("PATH").expect("PATH should be set");
    let prefixed_path = format!("{}:{path}", fake_bin.display());
    let vx_home = env.project_path().join(".vx-home");
    fs::create_dir_all(&vx_home).expect("failed to create isolated VX_HOME");
    let vxd = vx_home.join("vxd.sock");

    let mut start = Command::new(TestEnv::vx_binary());
    start
        .current_dir(env.project_path())
        .env("PATH", &prefixed_path)
        .env("VX_HOME", &vx_home)
        .env("VX_TEST", "1")
        .arg("start")
        .arg("--no-tui")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _daemon = start.spawn().expect("failed to start isolated vxd");

    let mut ready = false;
    for _ in 0..100 {
        let status = Command::new(TestEnv::vx_binary())
            .current_dir(env.project_path())
            .env("VX_HOME", &vx_home)
            .env("VXD", &vxd)
            .arg("ping")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("failed to ping isolated vxd");
        if status.success() {
            ready = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(ready, "isolated vxd did not become ready in time");

    let result = env
        .build()
        .env("PATH", &prefixed_path)
        .env("VX_HOME", &vx_home.display().to_string())
        .env("VXD", &vxd.display().to_string())
        .run();
    result.assert_success();

    assert_eq!(
        fs::read_to_string(env.project_path().join(".vx/build/bin/lua"))
            .expect("lua binary should exist"),
        "fake cc output\n"
    );
    assert_eq!(
        fs::read_to_string(env.project_path().join(".vx/build/lib/liblua.a"))
            .expect("archive should exist"),
        "fake archive\n"
    );
}

fn write_executable(path: &std::path::Path, contents: &str) {
    fs::write(path, contents).expect("failed to write executable");
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(path)
            .expect("script metadata should exist")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).expect("failed to chmod executable");
    }
}
