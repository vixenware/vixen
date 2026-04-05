#![allow(non_snake_case)]

use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::NSObjectProtocol;
use objc2_fs_kit::{FSFileName, FSItem, FSItemAttributes, FSItemGetAttributesRequest, FSItemType};
use vx_services::ItemAttributes as VfsItemAttributes;

use super::support::{
    ROOT_BACKEND_ID, ROOT_LOCAL_ID, STATUS_JSON_ID, STATUS_JSON_NAME, VIXENFS_DIR_ID,
    VIXENFS_DIR_NAME, fs_filename, fs_filename_string, fs_item_attributes, fs_item_type, unix_time,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum VirtualKind {
    Root,
    MetaDir,
    StatusJson,
    None,
}

struct CachedState {
    name: Retained<FSFileName>,
    parent_local_id: u64,
    item_type: FSItemType,
    size: u64,
    modified_time: u64,
    created_time: u64,
    mode: u32,
}

pub(super) struct ItemIvars {
    local_item_id: u64,
    backend_item_id: u64,
    connection_generation: u64,
    virtual_kind: VirtualKind,
    state: Mutex<CachedState>,
}

define_class!(
    #[unsafe(super = FSItem)]
    #[ivars = ItemIvars]
    pub(super) struct RustItem;

    unsafe impl NSObjectProtocol for RustItem {}
);

impl RustItem {
    fn new(
        local_item_id: u64,
        backend_item_id: u64,
        connection_generation: u64,
        parent_local_id: u64,
        name: &str,
        item_type: FSItemType,
        size: u64,
        modified_time: u64,
        created_time: u64,
        mode: u32,
        virtual_kind: VirtualKind,
    ) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ItemIvars {
            local_item_id,
            backend_item_id,
            connection_generation,
            virtual_kind,
            state: Mutex::new(CachedState {
                name: fs_filename(name),
                parent_local_id,
                item_type,
                size,
                modified_time,
                created_time,
                mode,
            }),
        });
        unsafe { msg_send![super(this), init] }
    }

    pub(super) fn root(connection_generation: u64) -> Retained<Self> {
        Self::new(
            ROOT_LOCAL_ID,
            ROOT_BACKEND_ID,
            connection_generation,
            ROOT_LOCAL_ID,
            "/",
            FSItemType::Directory,
            0,
            0,
            0,
            0o755,
            VirtualKind::Root,
        )
    }

    pub(super) fn meta_dir() -> Retained<Self> {
        Self::new(
            VIXENFS_DIR_ID,
            VIXENFS_DIR_ID,
            0,
            ROOT_LOCAL_ID,
            VIXENFS_DIR_NAME,
            FSItemType::Directory,
            0,
            0,
            0,
            0o755,
            VirtualKind::MetaDir,
        )
    }

    pub(super) fn status_json(len: usize) -> Retained<Self> {
        let now = unix_time();
        Self::new(
            STATUS_JSON_ID,
            STATUS_JSON_ID,
            0,
            VIXENFS_DIR_ID,
            STATUS_JSON_NAME,
            FSItemType::File,
            len as u64,
            now,
            0,
            0o444,
            VirtualKind::StatusJson,
        )
    }

    pub(super) fn from_backend(
        local_item_id: u64,
        parent_local_id: u64,
        connection_generation: u64,
        name: &str,
        attrs: &VfsItemAttributes,
    ) -> Retained<Self> {
        Self::new(
            local_item_id,
            attrs.item_id,
            connection_generation,
            parent_local_id,
            name,
            fs_item_type(attrs.item_type),
            attrs.size,
            attrs.modified_time,
            attrs.created_time,
            attrs.mode,
            VirtualKind::None,
        )
    }

    pub(super) fn item_id(&self) -> u64 {
        self.ivars().local_item_id
    }

    pub(super) fn backend_item_id(&self) -> u64 {
        self.ivars().backend_item_id
    }

    pub(super) fn generation(&self) -> u64 {
        self.ivars().connection_generation
    }

    pub(super) fn virtual_kind(&self) -> VirtualKind {
        self.ivars().virtual_kind
    }

    pub(super) fn parent_local_id(&self) -> u64 {
        self.ivars()
            .state
            .lock()
            .expect("item state mutex poisoned")
            .parent_local_id
    }

    pub(super) fn fs_name(&self) -> Retained<FSFileName> {
        self.ivars()
            .state
            .lock()
            .expect("item state mutex poisoned")
            .name
            .clone()
    }

    pub(super) fn attributes_for_request(
        &self,
        request: Option<&FSItemGetAttributesRequest>,
    ) -> Retained<FSItemAttributes> {
        let state = self
            .ivars()
            .state
            .lock()
            .expect("item state mutex poisoned");
        fs_item_attributes(
            request,
            self.item_id(),
            state.parent_local_id,
            state.item_type,
            state.size,
            state.modified_time,
            state.created_time,
            state.mode,
        )
    }

    pub(super) fn update_from_backend(
        &self,
        parent_local_id: u64,
        name: &str,
        attrs: &VfsItemAttributes,
    ) {
        let mut state = self
            .ivars()
            .state
            .lock()
            .expect("item state mutex poisoned");
        state.name = fs_filename(name);
        state.parent_local_id = parent_local_id;
        state.item_type = fs_item_type(attrs.item_type);
        state.size = attrs.size;
        state.modified_time = attrs.modified_time;
        state.created_time = attrs.created_time;
        state.mode = attrs.mode;
    }

    pub(super) fn update_name_and_parent(&self, parent_local_id: u64, name: &str) {
        let mut state = self
            .ivars()
            .state
            .lock()
            .expect("item state mutex poisoned");
        state.name = fs_filename(name);
        state.parent_local_id = parent_local_id;
    }

    pub(super) fn update_status_len(&self, len: usize) {
        let mut state = self
            .ivars()
            .state
            .lock()
            .expect("item state mutex poisoned");
        state.size = len as u64;
        state.modified_time = unix_time();
    }

    pub(super) fn debug_summary(&self) -> String {
        let state = self
            .ivars()
            .state
            .lock()
            .expect("item state mutex poisoned");
        format!(
            "local={} backend={} parent={} gen={} name={} kind={} type={:?} mode={:#o} size={}",
            self.ivars().local_item_id,
            self.ivars().backend_item_id,
            state.parent_local_id,
            self.ivars().connection_generation,
            fs_filename_string(&state.name),
            self.virtual_kind().label(),
            state.item_type,
            state.mode,
            state.size
        )
    }
}

impl VirtualKind {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::MetaDir => "meta_dir",
            Self::StatusJson => "status_json",
            Self::None => "backend",
        }
    }
}

pub(super) fn as_rust_item(item: &FSItem) -> Option<&RustItem> {
    let object: &AnyObject = item.as_ref();
    object.downcast_ref::<RustItem>()
}
