//! Minimal `VxdService` shell during the evaluator purge.

use vx_services::ServiceVersion;
use vx_services::{VXD_PROTOCOL_VERSION, VxDaemon};

#[derive(Clone)]
pub struct VxdService {}

impl VxDaemon for VxdService {
    async fn version(&self) -> ServiceVersion {
        ServiceVersion {
            service: "vxd".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: VXD_PROTOCOL_VERSION,
        }
    }
}
