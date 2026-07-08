#![allow(non_snake_case)]

use std::ptr;

use objc2::rc::Retained;
use objc2::{AnyThread, ClassType, define_class, msg_send};
use objc2_foundation::{NSObjectProtocol, NSString, NSUUID};
use objc2_fs_kit::{
    FSContainerIdentifier, FSContainerStatus, FSFileSystemBase, FSProbeResult, FSResource,
    FSTaskOptions, FSUnaryFileSystem, FSUnaryFileSystemOperations, FSVolume,
};

use super::log_event;
use super::root_peer::root_peer_state;
use super::support::{CONTAINER_UUID, VOLUME_NAME, posix_error};
use super::volume::RustVolume;

define_class!(
    #[unsafe(super = FSUnaryFileSystem)]
    #[ivars = ()]
    pub(super) struct RustUnaryFileSystem;

    unsafe impl NSObjectProtocol for RustUnaryFileSystem {}

    unsafe impl FSUnaryFileSystemOperations for RustUnaryFileSystem {
        #[unsafe(method(probeResource:replyHandler:))]
        fn probeResource_replyHandler(
            &self,
            _resource: &FSResource,
            reply: &block2::DynBlock<dyn Fn(*mut FSProbeResult, *mut objc2_foundation::NSError)>,
        ) {
            // Do NOT start the VFS root listener here. probeResource runs in throwaway
            // probe processes (FSKit probes the module whenever something — e.g. the
            // app's FSClient.installedExtensions poll — asks about it). Binding the
            // socket on probe spawns a second process that competes for vixenfs.sock with
            // the real mount instance, so a backend connecting gets ECONNREFUSED. The
            // listener belongs only to loadResource (the actual mount), below.
            log_event("info", "vixen-fskit probeResource");
            // Block-resource FSes return a usable result carrying a name + a stable
            // container identity (name/containerID must be non-nil). Returning only a
            // recognized result leaves the FSKit mount path without a usable match.
            let result = unsafe {
                let name = NSString::from_str(VOLUME_NAME);
                let uuid = NSUUID::initWithUUIDString(
                    NSUUID::alloc(),
                    &NSString::from_str(CONTAINER_UUID),
                )
                .expect("CONTAINER_UUID is a valid UUID string");
                let container_id =
                    FSContainerIdentifier::initWithUUID(FSContainerIdentifier::alloc(), &uuid);
                FSProbeResult::usableProbeResultWithName_containerID(&name, &container_id)
            };
            log_event("info", "vixen-fskit probeResource returning usable result");
            reply.call((Retained::into_raw(result), ptr::null_mut()));
        }

        #[unsafe(method(loadResource:options:replyHandler:))]
        fn loadResource_options_replyHandler(
            &self,
            _resource: &FSResource,
            _options: &FSTaskOptions,
            reply: &block2::DynBlock<dyn Fn(*mut FSVolume, *mut objc2_foundation::NSError)>,
        ) {
            tracing::info!("in loadResource");
            // This is the mounting instance — start the VFS socket listener here (and
            // only here), so probe-only instances never bind the socket.
            root_peer_state().ensure_listening();
            tracing::info!("calling as_super()");
            let file_system = self.as_super();
            tracing::info!("calling ready()");
            let ready = unsafe { FSContainerStatus::ready() };
            tracing::info!("calling setContainerStatus()");
            unsafe { FSFileSystemBase::setContainerStatus(file_system, &ready) };
            tracing::info!("Making new RustVolume");
            let volume: Retained<FSVolume> = RustVolume::new(file_system).into_super();
            tracing::info!("reply.call()");
            reply.call((Retained::into_raw(volume), ptr::null_mut()));
        }

        #[unsafe(method(unloadResource:options:replyHandler:))]
        fn unloadResource_options_replyHandler(
            &self,
            _resource: &FSResource,
            _options: &FSTaskOptions,
            reply: &block2::DynBlock<dyn Fn(*mut objc2_foundation::NSError)>,
        ) {
            log_event("info", "vixen-fskit unloadResource");
            root_peer_state().stop_listener("filesystem unloadResource");
            let file_system = self.as_super();
            let status =
                unsafe { FSContainerStatus::notReadyWithStatus(&posix_error(libc::ENOTCONN)) };
            unsafe { FSFileSystemBase::setContainerStatus(file_system, &status) };
            reply.call((ptr::null_mut(),));
        }

        #[unsafe(method(didFinishLoading))]
        fn didFinishLoading(&self) {
            root_peer_state().ensure_listening();
            log_event("info", "vixen-fskit didFinishLoading");
        }
    }
);

impl RustUnaryFileSystem {
    pub(super) fn new() -> Retained<Self> {
        unsafe { msg_send![super(Self::alloc().set_ivars(())), init] }
    }
}
