use facet::Facet;

pub const VIXEN_EXTENSION_IDENTIFIER: &str = "eu.bearcove.vixenfs";
pub const VIXEN_APP_PATH: &str = "/Applications/Vixen.app";
pub const VIXEN_FS_APPEX_PATH: &str = "/Applications/Vixen.app/Contents/Extensions/VixenFS.appex";
pub const VIXEN_DOWNLOAD_URL: &str = "https://vixen.rs/download/";
pub const VIXENFS_STATUS_RELATIVE_PATH: &str = ".vixenfs/status.json";
pub const DEFAULT_CONFIGURED_VFS_MOUNT_PATH: &str = "/tmp/vx/vfs";

/// Plain APFS directory under `/Volumes`; the FSKit mount lives below it so
/// enumerating `/Volumes` does not have to touch a wedged FSKit root.
pub const MACOS_VFS_MOUNT_PARENT: &str = "/Volumes/Vixen";
pub const MACOS_VFS_MOUNT_POINT: &str = "/Volumes/Vixen/vfs";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum VfsIssueCode {
    UnsupportedPlatform,
    AppMissing,
    ExtensionNotInstalled,
    ExtensionDisabled,
    HealthCheckFailed,
    MountFailed,
}

impl core::fmt::Display for VfsIssueCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            VfsIssueCode::UnsupportedPlatform => write!(f, "Unsupported platform"),
            VfsIssueCode::AppMissing => write!(f, "Vixen app is missing"),
            VfsIssueCode::ExtensionNotInstalled => write!(f, "VFS extension is not installed"),
            VfsIssueCode::ExtensionDisabled => write!(f, "VFS extension is disabled"),
            VfsIssueCode::HealthCheckFailed => write!(f, "VFS health check failed"),
            VfsIssueCode::MountFailed => write!(f, "VFS mount failed"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum VfsAction {
    RetryBuild,
    RunVxApp,
    OpenDownloadUrl(String),
    RunCommand(String),
}

#[derive(Debug, Clone, Facet)]
pub struct VfsDiagnostic {
    pub code: VfsIssueCode,
    pub summary: String,
    pub detail: Option<String>,
    pub actions: Vec<VfsAction>,
}

#[derive(Debug, Clone, Facet)]
#[facet(rename_all = "camelCase")]
pub struct VfsHealth {
    pub extension_installed: bool,
    pub extension_enabled: bool,
}
