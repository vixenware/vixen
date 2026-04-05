#![allow(non_snake_case)]

use std::ptr;

use objc2::rc::Retained;
use objc2::{AnyThread, ClassType, define_class, msg_send};
use objc2_foundation::NSObjectProtocol;
use objc2_fs_kit::{
    FSContainerStatus, FSFileSystemBase, FSProbeResult, FSResource, FSTaskOptions,
    FSUnaryFileSystem, FSUnaryFileSystemOperations, FSVolume,
};

use super::log_event;
use super::root_peer::root_peer_state;
use super::support::posix_error;
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
            root_peer_state();
            log_event("info", "vixen-fskit probeResource");
            let result = unsafe { FSProbeResult::usableButLimitedProbeResult() };
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
            root_peer_state();
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
            let file_system = self.as_super();
            let status =
                unsafe { FSContainerStatus::notReadyWithStatus(&posix_error(libc::ENOTCONN)) };
            unsafe { FSFileSystemBase::setContainerStatus(file_system, &status) };
            reply.call((ptr::null_mut(),));
        }

        #[unsafe(method(didFinishLoading))]
        fn didFinishLoading(&self) {
            root_peer_state();
            log_event("info", "vixen-fskit didFinishLoading");
        }
    }
);

impl RustUnaryFileSystem {
    pub(super) fn new() -> Retained<Self> {
        unsafe { msg_send![super(Self::alloc().set_ivars(())), init] }
    }
}
