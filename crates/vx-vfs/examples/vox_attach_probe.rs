use std::time::Duration;

use vx_services::{
    CreateResult, DirEntry, GetAttributesResult, ItemAttributes, ItemId, ItemType, LookupResult,
    ReadDirResult, ReadResult, SetAttributesParams, Vfs, VfsDiskNotify, VfsDispatcher, VfsResult,
    WriteResult,
};

#[derive(Clone)]
struct DummyVfs;

impl Vfs for DummyVfs {
    async fn lookup(&self, _parent_id: ItemId, _name: String) -> LookupResult {
        LookupResult {
            attrs: missing_file_attrs(),
            error: vx_services::errno::ENOENT,
        }
    }

    async fn get_attributes(&self, _item_id: ItemId) -> GetAttributesResult {
        GetAttributesResult {
            attrs: missing_file_attrs(),
            error: vx_services::errno::ENOENT,
        }
    }

    async fn read_dir(&self, _item_id: ItemId, _cookie: u64, _verifier: u64) -> ReadDirResult {
        ReadDirResult {
            entries: Vec::<DirEntry>::new(),
            next_cookie: 0,
            verifier: 0,
            error: vx_services::errno::ENOENT,
        }
    }

    async fn read(&self, _item_id: ItemId, _offset: u64, _len: u64) -> ReadResult {
        ReadResult {
            data: Vec::new(),
            error: vx_services::errno::ENOENT,
        }
    }

    async fn write(&self, _item_id: ItemId, _offset: u64, _data: Vec<u8>) -> WriteResult {
        WriteResult {
            bytes_written: 0,
            error: vx_services::errno::ENOENT,
        }
    }

    async fn create(
        &self,
        _parent_id: ItemId,
        _name: String,
        _item_type: ItemType,
        _mode: u32,
    ) -> CreateResult {
        CreateResult {
            item_id: 0,
            error: vx_services::errno::ENOENT,
        }
    }

    async fn delete(&self, _item_id: ItemId) -> VfsResult {
        VfsResult {
            error: vx_services::errno::ENOENT,
        }
    }

    async fn rename(
        &self,
        _item_id: ItemId,
        _new_parent_id: ItemId,
        _new_name: String,
    ) -> VfsResult {
        VfsResult {
            error: vx_services::errno::ENOENT,
        }
    }

    async fn set_attributes(&self, _item_id: ItemId, _params: SetAttributesParams) -> VfsResult {
        VfsResult {
            error: vx_services::errno::ENOENT,
        }
    }

    async fn ping(&self) -> String {
        "pong".to_string()
    }
}

impl VfsDiskNotify for DummyVfs {}

fn missing_file_attrs() -> ItemAttributes {
    ItemAttributes {
        item_id: 0,
        item_type: ItemType::File,
        size: 0,
        modified_time: 0,
        created_time: 0,
        mode: 0,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().ok_or(
        "usage: cargo run -p vx-vfs --example vox_attach_probe -- <noop|vfs> /tmp/vox-attach-probe.sock",
    )?;
    let socket_path = args.next().ok_or(
        "usage: cargo run -p vx-vfs --example vox_attach_probe -- <noop|vfs> /tmp/vox-attach-probe.sock",
    )?;
    let addr = format!("local://{socket_path}");

    let builder = vox::connect(&addr)
        .metadata(vox::metadata().str("probe", "rust").build())
        .connect_timeout(Duration::from_secs(5));

    let connection = match mode.as_str() {
        "noop" => builder.establish().await?,
        "vfs" => {
            builder
                .on_lane(VfsDispatcher::new(DummyVfs))
                .establish()
                .await?
        }
        _ => return Err(format!("unknown mode: {mode}").into()),
    };

    println!("connected {addr}");
    connection.closed().await;
    println!("closed");
    Ok(())
}
