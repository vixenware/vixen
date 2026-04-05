//! Rust-backed FSKit runtime for the macOS extension.

use std::ffi::c_void;

#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::rc::Rc;

#[cfg(test)]
use block2::RcBlock;
use objc2::rc::Retained;
#[cfg(test)]
use objc2_foundation::NSError;
use objc2_fs_kit::FSUnaryFileSystem;
#[cfg(test)]
use objc2_fs_kit::{FSFileName, FSItem, FSUnaryFileSystemOperations, FSVolumeOperations};

mod filesystem;
use filesystem::RustUnaryFileSystem;

mod item;
mod logging;
mod root_peer;
use root_peer::root_peer_state;

mod support;
#[cfg(test)]
use support::{VIXENFS_DIR_NAME, fs_filename};

mod volume;
#[cfg(test)]
use volume::RustVolume;

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

#[cfg(test)]
pub fn run_fskit_smoke() {
    root_peer_state();
    log_event("info", "vixen-fskit smoke begin");

    let fs = RustUnaryFileSystem::new();
    unsafe { FSUnaryFileSystemOperations::didFinishLoading(&*fs) };

    let volume = RustVolume::new(fs.as_super());
    let root = volume.root_item();
    let lookup_name = fs_filename(VIXENFS_DIR_NAME);
    let looked_up_item = Rc::new(RefCell::new(None::<Retained<FSItem>>));
    let looked_up_name = Rc::new(RefCell::new(None::<Retained<FSFileName>>));
    let lookup_error = Rc::new(RefCell::new(None::<Retained<NSError>>));

    let item_slot = looked_up_item.clone();
    let name_slot = looked_up_name.clone();
    let error_slot = lookup_error.clone();
    let reply = RcBlock::new(
        move |item: *mut FSItem, name: *mut FSFileName, error: *mut NSError| {
            if let Some(item) = unsafe { Retained::retain(item) } {
                *item_slot.borrow_mut() = Some(item);
            }
            if let Some(name) = unsafe { Retained::retain(name) } {
                *name_slot.borrow_mut() = Some(name);
            }
            if let Some(error) = unsafe { Retained::retain(error) } {
                *error_slot.borrow_mut() = Some(error);
            }
        },
    );

    unsafe {
        FSVolumeOperations::lookupItemNamed_inDirectory_replyHandler(
            &*volume,
            &lookup_name,
            &root,
            &reply,
        );
    }

    let found_name = looked_up_name
        .borrow()
        .as_ref()
        .map(|name| support::fs_filename_string(name))
        .unwrap_or_default();
    let failed = lookup_error.borrow().is_some();
    log_event(
        "info",
        format!(
            "vixen-fskit smoke lookup failed={failed} found_name={found_name} status_len={}",
            root_peer_state().status_json_bytes().len()
        ),
    );
}
