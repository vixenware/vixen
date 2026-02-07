use facet::Facet;

pub const VIXEN_EXTENSION_IDENTIFIER: &str = "eu.bearcove.vixenfs";
pub const VIXEN_APP_PATH: &str = "/Applications/Vixen.app";
pub const VIXEN_MOUNT_HELPER_PATH: &str = "/Applications/Vixen.app/Contents/Helpers/vixenfs-mount";
pub const VIXEN_DOWNLOAD_URL: &str = "https://vixen.rs/download/";

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

#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum VfsAction {
    RetryBuild,
    RunVxApp,
    OpenDownloadUrl(String),
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
