use crate::{
    build_json,
    tui::{OscProgressState, emit_osc_progress},
    types::CliMode,
};
use std::sync::atomic::{self, AtomicU64};
use vxd_proto::{ActionProgress, ActionType, ProgressListenerServer as ProgressListener};

/// Progress listener that dispatches to TUI, JSON, or plain text output.
pub(crate) struct ProgressListenerImpl {
    mode: CliMode,
    next_action_id: AtomicU64,
    /// Tracks total/completed for OSC progress in Text mode
    total_actions: AtomicU64,
    completed_actions: AtomicU64,
}

impl Clone for ProgressListenerImpl {
    fn clone(&self) -> Self {
        Self {
            mode: self.mode.clone(),
            next_action_id: AtomicU64::new(self.next_action_id.load(atomic::Ordering::SeqCst)),
            total_actions: AtomicU64::new(self.total_actions.load(atomic::Ordering::SeqCst)),
            completed_actions: AtomicU64::new(
                self.completed_actions.load(atomic::Ordering::SeqCst),
            ),
        }
    }
}

impl ProgressListenerImpl {
    /// Create a new progress listener for the given CLI mode.
    pub(crate) fn new(mode: CliMode) -> Self {
        Self {
            mode,
            next_action_id: AtomicU64::new(1),
            total_actions: AtomicU64::new(0),
            completed_actions: AtomicU64::new(0),
        }
    }

    fn next_id(&self) -> u64 {
        self.next_action_id.fetch_add(1, atomic::Ordering::SeqCst)
    }

    async fn set_total_impl(&self, total: u64) {
        match &self.mode {
            CliMode::Json => {
                build_json::emit(&build_json::BuildStarted {
                    event: "build_started".into(),
                    total_actions: total,
                });
            }
            CliMode::Tui(tui) => {
                tui.set_total(total as usize);
            }
            CliMode::Text => {
                self.total_actions.store(total, atomic::Ordering::SeqCst);
                emit_osc_progress(OscProgressState::Set, Some(0));
                tracing::info!("Build: {total} actions total");
            }
        }
    }

    async fn start_action_impl(&self, action_type: ActionType) -> u64 {
        match &self.mode {
            CliMode::Json => {
                let id = self.next_id();
                build_json::emit(&build_json::ActionStarted {
                    event: "action_started".into(),
                    action_id: id,
                    action: action_type.display_name(),
                });
                id
            }
            CliMode::Tui(tui) => tui.start_action(action_type),
            CliMode::Text => {
                let id = self.next_id();
                tracing::info!("[{id}] Starting: {action_type:?}");
                id
            }
        }
    }

    async fn update_progress_impl(&self, action_id: u64, progress: ActionProgress) {
        match &self.mode {
            CliMode::Json => {
                build_json::emit(&build_json::ActionProgress {
                    event: "action_progress".into(),
                    action_id,
                    message: progress.message,
                    current: progress.current,
                    total: progress.total,
                });
            }
            CliMode::Tui(tui) => {
                tui.update_progress(action_id, progress);
            }
            CliMode::Text => {
                if let (Some(current), Some(total)) = (progress.current, progress.total) {
                    let pct = if total > 0 {
                        current as f64 / total as f64 * 100.0
                    } else {
                        0.0
                    };
                    tracing::info!(
                        "[{action_id}] {}: {pct:.0}% ({current}/{total})",
                        progress.message,
                    );
                } else {
                    tracing::info!("[{action_id}] {}", progress.message);
                }
            }
        }
    }

    async fn complete_action_impl(&self, action_id: u64) {
        match &self.mode {
            CliMode::Json => {
                build_json::emit(&build_json::ActionCompleted {
                    event: "action_completed".into(),
                    action_id,
                });
            }
            CliMode::Tui(tui) => {
                tui.complete_action(action_id);
            }
            CliMode::Text => {
                let completed = self
                    .completed_actions
                    .fetch_add(1, atomic::Ordering::SeqCst)
                    + 1;
                let total = self.total_actions.load(atomic::Ordering::SeqCst);
                if total > 0 {
                    let pct = (completed as f64 / total as f64 * 100.0).min(100.0) as u8;
                    emit_osc_progress(OscProgressState::Set, Some(pct));
                }
                tracing::info!("[{action_id}] Complete");
            }
        }
    }

    async fn log_message_impl(&self, message: String) {
        match &self.mode {
            CliMode::Json => {
                build_json::emit(&build_json::LogMessage {
                    event: "log".into(),
                    message,
                });
            }
            CliMode::Tui(tui) => {
                tui.log_message(message);
            }
            CliMode::Text => {
                tracing::info!("{message}");
            }
        }
    }
}

impl ProgressListener for ProgressListenerImpl {
    async fn set_total(&self, call: impl roam::Call<(), core::convert::Infallible>, total: u64) {
        self.set_total_impl(total).await;
        call.ok(()).await;
    }

    async fn start_action(
        &self,
        call: impl roam::Call<u64, core::convert::Infallible>,
        action_type: ActionType,
    ) {
        call.ok(self.start_action_impl(action_type).await).await;
    }

    async fn update_progress(
        &self,
        call: impl roam::Call<(), core::convert::Infallible>,
        action_id: u64,
        progress: ActionProgress,
    ) {
        self.update_progress_impl(action_id, progress).await;
        call.ok(()).await;
    }

    async fn complete_action(
        &self,
        call: impl roam::Call<(), core::convert::Infallible>,
        action_id: u64,
    ) {
        self.complete_action_impl(action_id).await;
        call.ok(()).await;
    }

    async fn log_message(
        &self,
        call: impl roam::Call<(), core::convert::Infallible>,
        message: String,
    ) {
        self.log_message_impl(message).await;
        call.ok(()).await;
    }
}
