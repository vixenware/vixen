//! User-session VixenFS backend probe.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eyre::{Result, WrapErr, bail, eyre};
use tokio::process::Command;
use tokio::time::timeout;
use vox::{MetadataEntry, NoopClient};
use vx_fskit_types::StatusPayload;
use vx_services::{
    CreateResult, DirEntry, GetAttributesResult, ItemAttributes, ItemId, ItemType, LookupResult,
    ReadDirResult, ReadResult, SetAttributesParams, Vfs, VfsDiskNotify, VfsDispatcher, VfsResult,
    WriteResult, errno,
};

const ROOT_ID: ItemId = 1;
const PROBE_FILE_ID: ItemId = 2;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const WAIT_FOR_SERVICE: Duration = Duration::from_secs(2);

pub fn probe_vfs() -> Result<()> {
    tracing::info!(
        mount_point = crate::mount::MOUNT_POINT,
        "VixenFS probe starting"
    );
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .wrap_err("building VixenFS probe runtime")?
        .block_on(probe_vfs_async())
}

async fn probe_vfs_async() -> Result<()> {
    let status = read_status().await?;
    let socket_path = status
        .socket_path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| eyre!("VixenFS status is missing socket_path"))?;
    let addr = format!("local://{socket_path}");
    let probe_name = format!(".vixenfs-app-probe-{}-{}", std::process::id(), unix_nanos());
    let probe_body = format!("vixenfs probe {probe_name}\n");
    let vfs = ProbeVfs {
        name: Arc::from(probe_name.as_str()),
        body: Arc::from(probe_body.as_bytes()),
    };

    tracing::info!(addr, probe_name, "connecting VixenFS probe backend");
    let client = vox::connect::<NoopClient>(&addr)
        .metadata(vec![
            MetadataEntry::str("probe", "vixen-management"),
            MetadataEntry::str("probe_name", probe_name.as_str()),
        ])
        .on_connection(VfsDispatcher::new(vfs))
        .connect_timeout(CONNECT_TIMEOUT)
        .wait_for_service(WAIT_FOR_SERVICE)
        .establish()
        .await
        .wrap_err_with(|| format!("connecting VixenFS backend probe at {addr}"))?;

    let probe_path = format!("{}/{}", crate::mount::MOUNT_POINT, probe_name);
    let output = run_output("/bin/cat", &[probe_path.as_str()]).await?;
    if output.as_bytes() != probe_body.as_bytes() {
        bail!("VixenFS probe read unexpected payload from {probe_path}: {output:?}");
    }

    drop(client);
    writeln!(std::io::stdout(), "VixenFS probe: ok").wrap_err("writing VixenFS probe result")?;
    tracing::info!(probe_path, "VixenFS probe finished");
    Ok(())
}

async fn read_status() -> Result<StatusPayload> {
    let status_path = format!(
        "{}/{}",
        crate::mount::MOUNT_POINT,
        vx_vfs_types::VIXENFS_STATUS_RELATIVE_PATH
    );
    let json = run_output("/bin/cat", &[status_path.as_str()]).await?;
    facet_json::from_str(&json).wrap_err_with(|| format!("parsing {status_path}"))
}

async fn run_output(program: &str, args: &[&str]) -> Result<String> {
    tracing::info!(program, args = ?args, "running VixenFS probe child");
    let output = timeout(
        COMMAND_TIMEOUT,
        Command::new(program).args(args).kill_on_drop(true).output(),
    )
    .await
    .wrap_err_with(|| format!("{program} timed out during VixenFS probe"))?
    .wrap_err_with(|| format!("running {program} during VixenFS probe"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{program} failed during VixenFS probe with status {:?}: {}",
            output.status.code(),
            stderr.trim()
        );
    }

    String::from_utf8(output.stdout).wrap_err_with(|| format!("{program} returned invalid UTF-8"))
}

#[derive(Clone)]
struct ProbeVfs {
    name: Arc<str>,
    body: Arc<[u8]>,
}

impl ProbeVfs {
    fn root_attrs(&self) -> ItemAttributes {
        ItemAttributes {
            item_id: ROOT_ID,
            item_type: ItemType::Directory,
            size: 0,
            modified_time: 0,
            created_time: 0,
            mode: 0o755,
        }
    }

    fn file_attrs(&self) -> ItemAttributes {
        ItemAttributes {
            item_id: PROBE_FILE_ID,
            item_type: ItemType::File,
            size: self.body.len() as u64,
            modified_time: 0,
            created_time: 0,
            mode: 0o444,
        }
    }
}

impl Vfs for ProbeVfs {
    async fn lookup(&self, parent_id: ItemId, name: String) -> LookupResult {
        tracing::info!(parent_id, name, "VixenFS probe lookup");
        if parent_id == ROOT_ID && name == self.name.as_ref() {
            return LookupResult {
                attrs: self.file_attrs(),
                error: errno::OK,
            };
        }
        LookupResult {
            attrs: self.file_attrs(),
            error: errno::ENOENT,
        }
    }

    async fn get_attributes(&self, item_id: ItemId) -> GetAttributesResult {
        tracing::info!(item_id, "VixenFS probe get_attributes");
        match item_id {
            ROOT_ID => GetAttributesResult {
                attrs: self.root_attrs(),
                error: errno::OK,
            },
            PROBE_FILE_ID => GetAttributesResult {
                attrs: self.file_attrs(),
                error: errno::OK,
            },
            _ => GetAttributesResult {
                attrs: self.file_attrs(),
                error: errno::ENOENT,
            },
        }
    }

    async fn read_dir(&self, item_id: ItemId, _cookie: u64, _verifier: u64) -> ReadDirResult {
        tracing::info!(item_id, "VixenFS probe read_dir");
        if item_id != ROOT_ID {
            return ReadDirResult {
                entries: Vec::new(),
                next_cookie: 0,
                verifier: 0,
                error: errno::ENOTDIR,
            };
        }

        ReadDirResult {
            entries: vec![DirEntry {
                name: self.name.to_string(),
                item_id: PROBE_FILE_ID,
                item_type: ItemType::File,
                size: self.body.len() as u64,
                modified_time: 0,
                created_time: 0,
                mode: 0o444,
            }],
            next_cookie: 0,
            verifier: 1,
            error: errno::OK,
        }
    }

    async fn read(&self, item_id: ItemId, offset: u64, len: u64) -> ReadResult {
        tracing::info!(item_id, offset, len, "VixenFS probe read");
        if item_id != PROBE_FILE_ID {
            return ReadResult {
                data: Vec::new(),
                error: errno::ENOENT,
            };
        }

        let start = offset.min(self.body.len() as u64) as usize;
        let end = (offset.saturating_add(len)).min(self.body.len() as u64) as usize;
        ReadResult {
            data: self.body[start..end].to_vec(),
            error: errno::OK,
        }
    }

    async fn write(&self, _item_id: ItemId, _offset: u64, _data: Vec<u8>) -> WriteResult {
        WriteResult {
            bytes_written: 0,
            error: errno::EACCES,
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
            error: errno::EACCES,
        }
    }

    async fn delete(&self, _item_id: ItemId) -> VfsResult {
        VfsResult {
            error: errno::EACCES,
        }
    }

    async fn rename(
        &self,
        _item_id: ItemId,
        _new_parent_id: ItemId,
        _new_name: String,
    ) -> VfsResult {
        VfsResult {
            error: errno::EACCES,
        }
    }

    async fn set_attributes(&self, _item_id: ItemId, _params: SetAttributesParams) -> VfsResult {
        VfsResult {
            error: errno::EACCES,
        }
    }

    async fn ping(&self) -> String {
        "vixenfs-probe".to_string()
    }
}

impl VfsDiskNotify for ProbeVfs {}

fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}
