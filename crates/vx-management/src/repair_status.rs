//! Progress status for the app-visible VixenFS repair transaction.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use eyre::{Result, WrapErr};

use crate::queue::SUPPORT_DIR;

const REPAIR_STATUS_PATH: &str = "/Library/Application Support/Vixen/management/repair-status.json";

#[derive(facet::Facet)]
pub struct RepairStatus {
    pub active: bool,
    pub step: String,
    pub state: String,
    pub message: String,
    pub updated_unix_millis: u64,
}

pub fn running(step: &str, message: &str) {
    publish(false, step, "running", message);
}

pub fn ok(step: &str, message: &str) {
    publish(false, step, "ok", message);
}

pub fn failed(step: &str, message: &str) {
    publish(false, step, "failed", message);
}

pub fn finish_ok(message: &str) {
    publish(true, "complete", "ok", message);
}

pub fn finish_failed(message: &str) {
    publish(true, "complete", "failed", message);
}

fn publish(finished: bool, step: &str, state: &str, message: &str) {
    let status = RepairStatus {
        active: !finished,
        step: step.to_string(),
        state: state.to_string(),
        message: message.to_string(),
        updated_unix_millis: unix_millis(),
    };
    if let Err(err) = write(&status) {
        tracing::warn!(error = %err, "failed to publish repair progress");
    }
}

fn write(status: &RepairStatus) -> Result<()> {
    let json = facet_json::to_string(status).wrap_err("serializing repair status")?;
    let path = Path::new(REPAIR_STATUS_PATH);
    std::fs::create_dir_all(SUPPORT_DIR).wrap_err("creating management support dir")?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).wrap_err("writing repair status")?;
    std::fs::rename(&tmp, path).wrap_err("publishing repair status")?;
    tracing::info!(
        path = REPAIR_STATUS_PATH,
        active = status.active,
        step = status.step,
        state = status.state,
        message = %status.message,
        "published repair progress"
    );
    Ok(())
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or_default()
}
