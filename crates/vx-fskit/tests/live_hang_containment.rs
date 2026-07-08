#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vox::{ConnectionSettings, Driver, FromVoxLane, Parity, ServerLogging, ServerLoggingOptions};
use vx_fskit_types::StatusPayload;
use vx_services::{
    CreateResult, DirEntry, GetAttributesResult, ItemAttributes, ItemId, ItemType, LookupResult,
    ReadDirResult, ReadResult, SetAttributesParams, Vfs, VfsClient, VfsDiskNotify, VfsDispatcher,
    VfsResult, WriteResult,
};

const DEFAULT_NATIVE_VFS_MOUNT: &str = "/Volumes/Vixen/vfs";
const STALL_FILE_ID: ItemId = 2;

#[derive(Clone)]
struct StallingReadVfs;

impl Vfs for StallingReadVfs {
    async fn lookup(&self, parent_id: ItemId, name: String) -> LookupResult {
        if parent_id == 1 && name == "stall.txt" {
            return LookupResult {
                attrs: stall_file_attrs(),
                error: vx_services::errno::OK,
            };
        }

        LookupResult {
            attrs: missing_attrs(),
            error: vx_services::errno::ENOENT,
        }
    }

    async fn get_attributes(&self, item_id: ItemId) -> GetAttributesResult {
        let (attrs, error) = match item_id {
            1 => (root_attrs(), vx_services::errno::OK),
            STALL_FILE_ID => (stall_file_attrs(), vx_services::errno::OK),
            _ => (missing_attrs(), vx_services::errno::ENOENT),
        };
        GetAttributesResult { attrs, error }
    }

    async fn read_dir(&self, item_id: ItemId, _cookie: u64, verifier: u64) -> ReadDirResult {
        if item_id != 1 {
            return ReadDirResult {
                entries: Vec::new(),
                next_cookie: 0,
                verifier,
                error: vx_services::errno::ENOENT,
            };
        }

        ReadDirResult {
            entries: vec![DirEntry {
                name: "stall.txt".to_owned(),
                item_id: STALL_FILE_ID,
                item_type: ItemType::File,
                size: 8,
                modified_time: 0,
                created_time: 0,
                mode: 0o644,
            }],
            next_cookie: 0,
            verifier,
            error: vx_services::errno::OK,
        }
    }

    async fn read(&self, _item_id: ItemId, _offset: u64, _len: u64) -> ReadResult {
        std::future::pending::<ReadResult>().await
    }

    async fn write(&self, _item_id: ItemId, _offset: u64, _data: Vec<u8>) -> WriteResult {
        WriteResult {
            bytes_written: 0,
            error: vx_services::errno::EIO,
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
            error: vx_services::errno::EIO,
        }
    }

    async fn delete(&self, _item_id: ItemId) -> VfsResult {
        VfsResult {
            error: vx_services::errno::EIO,
        }
    }

    async fn rename(
        &self,
        _item_id: ItemId,
        _new_parent_id: ItemId,
        _new_name: String,
    ) -> VfsResult {
        VfsResult {
            error: vx_services::errno::EIO,
        }
    }

    async fn set_attributes(&self, _item_id: ItemId, _params: SetAttributesParams) -> VfsResult {
        VfsResult {
            error: vx_services::errno::EIO,
        }
    }

    async fn ping(&self) -> String {
        "pong".to_owned()
    }
}

impl VfsDiskNotify for StallingReadVfs {}

#[tokio::test]
async fn stalled_provider_read_returns_error_and_mount_stays_responsive() {
    if std::env::var_os("VX_FSKIT_HARDENING_LIVE_TEST").is_none() {
        return;
    }

    let mount_path = std::env::var("VX_FSKIT_HARDENING_MOUNT")
        .unwrap_or_else(|_| DEFAULT_NATIVE_VFS_MOUNT.into());
    if !Path::new(&mount_path).exists() {
        return;
    }

    let status = read_status(&mount_path);
    let socket_path = status
        .socket_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .expect("status.json must expose socket_path");
    let prefix = format!("hang-{}-{}", std::process::id(), unix_nanos());
    let connection = vox::connect(format!("local://{socket_path}"))
        .connect_timeout(Duration::from_secs(2))
        .wait_for_service(Duration::from_secs(10))
        .establish()
        .await
        .expect("connect to VixenFS root listener");
    let provider_metadata = vox::metadata()
        .str("vox-service", <VfsClient as FromVoxLane>::SERVICE_NAME)
        .str("prefix", &prefix)
        .build();
    let provider_lane = connection
        .open_lane_handle(
            ConnectionSettings {
                parity: Parity::Odd,
                max_concurrent_requests: 64,
                initial_channel_credit: 16,
            },
            provider_metadata,
        )
        .await
        .expect("claim hardening provider prefix");
    let mut driver = Driver::new(
        provider_lane,
        VfsDispatcher::new(StallingReadVfs).with_middleware(ServerLogging::new(
            ServerLoggingOptions {
                log_metadata: true,
                ..Default::default()
            },
        )),
    );
    let driver_task = tokio::spawn(async move {
        driver.run().await;
    });

    let stall_path = PathBuf::from(&mount_path).join(&prefix).join("stall.txt");
    wait_for_lookup(&stall_path).await;

    let started = Instant::now();
    let output = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::process::Command::new("/bin/cat")
            .arg(&stall_path)
            .output()
            .await
    })
    .await
    .expect("stalled read did not return through the FSKit deadline")
    .expect("spawn /bin/cat for stalled read");

    assert!(
        !output.status.success(),
        "stalled read unexpectedly succeeded with stdout={:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "stalled read returned only after outer test timeout budget"
    );

    let status_path = PathBuf::from(&mount_path).join(vx_vfs_types::VIXENFS_STATUS_RELATIVE_PATH);
    let status_bytes = tokio::fs::read(status_path)
        .await
        .expect("mount should remain responsive after stalled read");
    assert!(!status_bytes.is_empty());

    driver_task.abort();
    drop(connection);
}

async fn wait_for_lookup(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if tokio::fs::metadata(path).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "provider prefix did not become visible at {}",
        path.display()
    );
}

fn read_status(mount_path: &str) -> StatusPayload {
    let status_path = PathBuf::from(mount_path).join(vx_vfs_types::VIXENFS_STATUS_RELATIVE_PATH);
    let json = std::fs::read_to_string(&status_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", status_path.display()));
    facet_json::from_str(&json)
        .unwrap_or_else(|error| panic!("parse {}: {error}", status_path.display()))
}

fn unix_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn root_attrs() -> ItemAttributes {
    ItemAttributes {
        item_id: 1,
        item_type: ItemType::Directory,
        size: 0,
        modified_time: 0,
        created_time: 0,
        mode: 0o755,
    }
}

fn stall_file_attrs() -> ItemAttributes {
    ItemAttributes {
        item_id: STALL_FILE_ID,
        item_type: ItemType::File,
        size: 8,
        modified_time: 0,
        created_time: 0,
        mode: 0o644,
    }
}

fn missing_attrs() -> ItemAttributes {
    ItemAttributes {
        item_id: 0,
        item_type: ItemType::File,
        size: 0,
        modified_time: 0,
        created_time: 0,
        mode: 0,
    }
}
