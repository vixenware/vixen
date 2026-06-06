//! Last-action status written by the privileged daemon and read by the app.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use eyre::{Result, WrapErr};

use crate::Button;
use crate::queue::SUPPORT_DIR;

const STATUS_PATH: &str = "/Library/Application Support/Vixen/management/last-action.json";

#[derive(facet::Facet)]
pub struct LastActionStatus {
    pub button: String,
    pub ok: bool,
    pub message: String,
    pub finished_unix_seconds: u64,
}

pub fn record(button: Button, outcome: &Result<()>) -> Result<()> {
    tracing::info!(button = ?button, ok = outcome.is_ok(), "recording management action outcome");
    let status = match outcome {
        Ok(()) => LastActionStatus {
            button: button.queue_name().to_string(),
            ok: true,
            message: "ok".to_string(),
            finished_unix_seconds: unix_seconds(),
        },
        Err(err) => LastActionStatus {
            button: button.queue_name().to_string(),
            ok: false,
            message: format!("{err:#}"),
            finished_unix_seconds: unix_seconds(),
        },
    };
    write(&status)
}

fn write(status: &LastActionStatus) -> Result<()> {
    let json = facet_json::to_string(status).wrap_err("serializing last action status")?;
    let path = Path::new(STATUS_PATH);
    std::fs::create_dir_all(SUPPORT_DIR).wrap_err("creating management support dir")?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).wrap_err("writing last action status")?;
    std::fs::rename(&tmp, path).wrap_err("publishing last action status")?;
    tracing::info!(
        path = STATUS_PATH,
        button = status.button,
        ok = status.ok,
        finished_unix_seconds = status.finished_unix_seconds,
        message = %status.message,
        "published management last-action status"
    );
    Ok(())
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
