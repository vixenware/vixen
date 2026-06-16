//! Rust-backed FSKit runtime for the macOS extension.

use std::ffi::{CStr, c_char, c_void};
use std::path::PathBuf;

/// Configuration passed from Swift during extension bootstrap.
/// Must match the Swift-side struct layout exactly.
#[repr(C)]
pub struct VixenFsConfig {
    pub bundle_version: *const c_char,
    pub bundle_identifier: *const c_char,
    pub app_group_path: *const c_char,
    pub socket_path: *const c_char,
}

impl VixenFsConfig {
    /// Safely read C string pointers into owned Rust strings.
    /// Panics if any required pointer is null.
    pub fn to_owned(&self) -> OwnedVixenFsConfig {
        let bundle_version = unsafe {
            CStr::from_ptr(self.bundle_version)
                .to_string_lossy()
                .into_owned()
        };
        let bundle_identifier = unsafe {
            CStr::from_ptr(self.bundle_identifier)
                .to_string_lossy()
                .into_owned()
        };
        let app_group_path = unsafe {
            PathBuf::from(
                CStr::from_ptr(self.app_group_path)
                    .to_string_lossy()
                    .into_owned(),
            )
        };
        let socket_path = unsafe {
            PathBuf::from(
                CStr::from_ptr(self.socket_path)
                    .to_string_lossy()
                    .into_owned(),
            )
        };
        OwnedVixenFsConfig {
            bundle_version,
            bundle_identifier,
            app_group_path,
            socket_path,
        }
    }
}

pub struct OwnedVixenFsConfig {
    pub bundle_version: String,
    pub bundle_identifier: String,
    pub app_group_path: PathBuf,
    pub socket_path: PathBuf,
}

use objc2::rc::Retained;
use objc2_fs_kit::FSUnaryFileSystem;

mod filesystem;
use filesystem::RustUnaryFileSystem;

mod item;
mod logging;
mod root_peer;
use root_peer::init_root_peer_state;

mod support;
mod volume;

use self::logging::{init_logging, log_event};

/// # Safety
///
/// `config` must be valid yada yada.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn VixenRustCreateFileSystem(config: *const VixenFsConfig) -> *mut c_void {
    let config = unsafe { &*config };
    let owned = config.to_owned();
    init_logging(&owned.app_group_path);
    let file_system: Retained<FSUnaryFileSystem> = create_file_system(&owned);
    log_event(
        "info",
        format!(
            "created Rust FSKit filesystem object bundle_version={}",
            owned.bundle_version
        ),
    );
    Retained::into_raw(file_system).cast()
}

pub fn create_file_system(config: &OwnedVixenFsConfig) -> Retained<FSUnaryFileSystem> {
    init_root_peer_state(config);
    RustUnaryFileSystem::new().into_super()
}
