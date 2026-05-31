//! Declaration-only standard library surface for Vixen.
//!
//! The canonical header files live under `std/` and are intended to become
//! the source of truth for builtin signatures in the frontend and LSP.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy)]
pub struct StdlibSource {
    pub logical_path: &'static str,
    pub text: &'static str,
}

const STD_CORE: StdlibSource = StdlibSource {
    // TODO: what is the logical path meant to be exactly?
    logical_path: "crates/vx-stdlib/std/core.vx",
    text: include_str!("../std/core.vx"),
};

const STD_BUILD: StdlibSource = StdlibSource {
    logical_path: "crates/vx-stdlib/std/build.vx",
    text: include_str!("../std/build.vx"),
};

/// Ambient bodied helpers (`assert`/`assert_eq`) — pure Vixen, unlike the
/// declaration-only `core.vx`. Injected as prelude so they resolve un-imported.
const STD_PRELUDE: StdlibSource = StdlibSource {
    logical_path: "crates/vx-stdlib/std/prelude.vx",
    text: include_str!("../std/prelude.vx"),
};

pub fn stdlib_sources() -> &'static [StdlibSource; 3] {
    &[STD_CORE, STD_BUILD, STD_PRELUDE]
}

pub fn source_disk_path(relative: &str) -> PathBuf {
    workspace_root().join(relative)
}

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
}
