//! Minimal `VxdService` shell during the evaluator purge.

use vx_services::{Daemon, VXD_PROTOCOL_VERSION};

#[derive(Clone)]
pub struct DaemonService {}

impl Daemon for DaemonService {}
