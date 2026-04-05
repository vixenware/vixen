//! Rust-backed FSKit runtime for the macOS extension.

use std::ffi::c_void;

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
pub extern "C" fn VixenRustCreateFileSystem() -> *mut c_void {
    init_logging();
    let file_system: Retained<FSUnaryFileSystem> = create_file_system();
    log_event("info", "created Rust FSKit filesystem object");
    Retained::into_raw(file_system).cast()
}

pub fn create_file_system() -> Retained<FSUnaryFileSystem> {
    root_peer_state();
    RustUnaryFileSystem::new().into_super()
}
