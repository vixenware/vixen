/// Status payload served by the extension at `/.vixenfs/status.json`.
#[derive(Debug, facet::Facet)]
#[facet(rename_all = "snake_case")]
pub struct StatusPayload {
    pub connected: bool,

    #[facet(default)]
    pub bundle_version: Option<String>,

    #[facet(default)]
    pub bundle_identifier: Option<String>,

    #[facet(default)]
    pub ever_connected: bool,

    #[facet(default)]
    pub app_group_path: Option<String>,

    #[facet(default)]
    pub socket_path: Option<String>,

    #[facet(default)]
    pub extension_pid: Option<u32>,

    #[facet(default)]
    pub connected_at: Option<String>,

    #[facet(default)]
    pub disconnected_at: Option<String>,

    #[facet(default)]
    pub uptime_seconds: Option<f64>,

    #[facet(default)]
    pub disconnect_reason: Option<String>,
}
