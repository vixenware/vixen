//! Rust-backed FSKit runtime for the macOS extension.

use std::ffi::{CStr, c_char, c_void};

use objc2::rc::Retained;
use objc2_fs_kit::FSUnaryFileSystem;

mod filesystem;
use filesystem::RustUnaryFileSystem;

mod item;
mod logging;
mod root_peer;
use root_peer::root_peer_state;

mod support;
mod volume;

use self::logging::{init_logging, log_event};

#[unsafe(no_mangle)]
pub extern "C" fn VixenRustCreateFileSystem(bundle_version: *const c_char) -> *mut c_void {
    init_logging();
    let bundle_version = unsafe {
        CStr::from_ptr(bundle_version)
            .to_string_lossy()
            .into_owned()
    };
    let file_system: Retained<FSUnaryFileSystem> = create_file_system(&bundle_version);
    log_event(
        "info",
        format!("created Rust FSKit filesystem object bundle_version={bundle_version}"),
    );
    Retained::into_raw(file_system).cast()
}

pub fn create_file_system(bundle_version: &str) -> Retained<FSUnaryFileSystem> {
    root_peer_state(bundle_version.to_owned());
    RustUnaryFileSystem::new().into_super()
}
