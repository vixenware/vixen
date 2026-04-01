//! Minimal `VxdService` shell during the evaluator purge.

use vx_services::Daemon;

#[derive(Clone)]
pub struct DaemonService {}

impl Daemon for DaemonService {}
