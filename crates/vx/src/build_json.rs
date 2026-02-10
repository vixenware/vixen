//! NDJSON event types for `vx build --json`
//!
//! Each event is a flat struct with an `event` discriminator field.
//! One JSON object per line (newline-delimited JSON).

use facet::Facet;

#[derive(Facet)]
pub struct BuildStarted {
    pub event: String,
    pub total_actions: u64,
}

#[derive(Facet)]
pub struct ActionStarted {
    pub event: String,
    pub action_id: u64,
    pub action: String,
}

#[derive(Facet)]
pub struct ActionProgress {
    pub event: String,
    pub action_id: u64,
    pub message: String,
    pub current: Option<u64>,
    pub total: Option<u64>,
}

#[derive(Facet)]
pub struct ActionCompleted {
    pub event: String,
    pub action_id: u64,
}

#[derive(Facet)]
pub struct LogMessage {
    pub event: String,
    pub message: String,
}

#[derive(Facet)]
pub struct BuildFinished {
    pub event: String,
    pub success: bool,
    pub cached: bool,
    pub duration_ms: u64,
    pub total_actions: u64,
    pub cache_hits: u64,
    pub rebuilt: u64,
    pub cancelled: u64,
    pub output_path: Option<String>,
    pub error: Option<String>,
}

pub fn emit<T: for<'a> facet::Facet<'a>>(event: &T) {
    let line = facet_json::to_string(event).expect("failed to serialize build event");
    println!("{line}");
}
