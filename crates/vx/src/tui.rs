//! TUI for build progress tracking
//!
//! Uses indicatif MultiProgress with:
//! - One progress bar showing overall completion
//! - 3 spinner bars for longest-running active actions
//! - 10 text bars for scrolling log buffer (last 10 messages)

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use moire::sync::{SyncMutex, oneshot};
use moire::task::FutureExt;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing_subscriber::Layer;
use vx_services::{ActionProgress, ActionType};

/// Tracks a single action
#[derive(Debug, Clone)]
struct Action {
    action_type: ActionType,
    started_at: Instant,
    progress: Option<ActionProgress>,
}

impl Action {
    fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }
}

/// OSC 9;4 progress state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OscProgressState {
    /// Remove progress indicator
    Remove = 0,
    /// Set progress (0-100)
    Set = 1,
    /// Error state (red)
    Error = 2,
    /// Indeterminate (pulsing)
    Indeterminate = 3,
}

/// Emit an OSC 9;4 terminal progress escape sequence.
///
/// Terminals that support this protocol (Ghostty 1.2+, Windows Terminal,
/// ConEmu, WezTerm) render a native progress bar in the title bar, tab,
/// or taskbar. Unsupported terminals silently ignore the sequence.
pub(crate) fn emit_osc_progress(state: OscProgressState, progress: Option<u8>) {
    use std::io::Write;

    let progress_val = progress.unwrap_or(0);
    // ESC ] 9 ; 4 ; <state> ; <progress> BEL
    let _ = write!(
        std::io::stderr(),
        "\x1b]9;4;{};{}\x07",
        state as u8,
        progress_val
    );
    let _ = std::io::stderr().flush();
}

/// TUI state shared between the service and the display task
struct TuiState {
    /// Active actions (action_id -> Action)
    active: HashMap<u64, Action>,
    /// Total number of actions we know about
    total: usize,
    /// Number of completed actions
    completed: usize,
    /// Next action ID
    next_id: u64,
    /// Recent log messages (circular buffer, keep last 100, show last 10)
    logs: VecDeque<String>,
    /// Shutdown flag to stop the display task
    shutdown: bool,
    /// Build outcome: None while running, Some(true) on success, Some(false) on failure
    build_outcome: Option<bool>,
}

impl TuiState {
    fn new() -> Self {
        Self {
            active: HashMap::new(),
            total: 0,
            completed: 0,
            next_id: 1,
            logs: VecDeque::with_capacity(100),
            shutdown: false,
            build_outcome: None,
        }
    }

    fn pending(&self) -> usize {
        self.total
            .saturating_sub(self.completed)
            .saturating_sub(self.active.len())
    }
}

/// Handle for interacting with the TUI
pub struct TuiHandle {
    state: Arc<SyncMutex<TuiState>>,
    /// Receiver that signals when display task has finished cleanup
    shutdown_rx: Arc<SyncMutex<Option<oneshot::Receiver<()>>>>,
}

impl Clone for TuiHandle {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            shutdown_rx: self.shutdown_rx.clone(),
        }
    }
}

impl TuiHandle {
    /// Create a new TUI handle and spawn the display task
    pub fn new() -> Self {
        let state = Arc::new(SyncMutex::new("vx/tui_state", TuiState::new()));
        let (shutdown_tx, shutdown_rx) = oneshot::channel("tui_shutdown");

        let handle = Self {
            state: state.clone(),
            shutdown_rx: Arc::new(SyncMutex::new("vx/tui_shutdown", Some(shutdown_rx))),
        };

        // Spawn the display task
        moire::task::spawn(Self::display_task(state, shutdown_tx).named("vx_tui_display"));

        handle
    }

    /// Set the total number of actions
    pub fn set_total(&self, total: usize) {
        let mut state = self.state.lock();
        state.total = total;
    }

    /// Start tracking an action, returning its ID
    pub fn start_action(&self, action_type: ActionType) -> u64 {
        let mut state = self.state.lock();
        let id = state.next_id;
        state.next_id += 1;

        state.active.insert(
            id,
            Action {
                action_type,
                started_at: Instant::now(),
                progress: None,
            },
        );

        id
    }

    /// Update progress on a running action
    pub fn update_progress(&self, id: u64, progress: ActionProgress) {
        let mut state = self.state.lock();
        if let Some(action) = state.active.get_mut(&id) {
            action.progress = Some(progress);
        }
    }

    /// Mark an action as completed
    pub fn complete_action(&self, id: u64) {
        let mut state = self.state.lock();
        if state.active.remove(&id).is_some() {
            state.completed += 1;
        }
    }

    /// Add a log message to the TUI
    pub fn log_message(&self, message: String) {
        let mut state = self.state.lock();
        state.logs.push_back(message);
        // Keep only last 100 messages
        if state.logs.len() > 100 {
            state.logs.pop_front();
        }
    }

    /// Signal the build outcome so the terminal progress shows the right final state
    pub fn set_build_outcome(&self, success: bool) {
        let mut state = self.state.lock();
        state.build_outcome = Some(success);
    }

    /// Shutdown the TUI and wait for cleanup to complete
    pub async fn shutdown(&self) {
        // Set the shutdown flag
        {
            let mut state = self.state.lock();
            state.shutdown = true;
        }

        // Wait for the display task to signal completion
        let rx = self.shutdown_rx.lock().take();
        if let Some(rx) = rx {
            let _ = rx.await; // Ignore errors (channel closed is fine)
        }
    }

    /// Background task that updates the display at ~15fps
    async fn display_task(state: Arc<SyncMutex<TuiState>>, shutdown_tx: oneshot::Sender<()>) {
        let multi = MultiProgress::new();
        multi.set_move_cursor(false); // can't set it to true, we add/remove tasks dynamically

        // Main progress bar
        let progress_bar = multi.add(ProgressBar::new(100));
        progress_bar.set_style(
            ProgressStyle::default_bar()
                .template("{bar:40.cyan/blue} {pos}/{len} [{msg}]")
                .expect("valid template")
                .progress_chars("=>-"),
        );

        // Active action spinners (3 for longest-running actions)
        let spinner_style = ProgressStyle::with_template("  {spinner} {msg}")
            .expect("valid template")
            .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ ");

        let empty_style = ProgressStyle::with_template("").expect("valid template");

        let action_bars: Vec<_> = (0..3)
            .map(|_| {
                let bar = multi.add(ProgressBar::new_spinner());
                bar.set_style(empty_style.clone());
                // Don't enable steady tick - we'll tick manually only when active
                bar
            })
            .collect();

        // Log display bars (10 for scrolling buffer)
        let log_style = ProgressStyle::with_template("  {msg}").expect("valid template");

        let log_bars: Vec<_> = (0..10)
            .map(|_| {
                let bar = multi.add(ProgressBar::new(0));
                bar.set_style(log_style.clone());
                bar
            })
            .collect();

        // Emit indeterminate progress while the build graph is being computed
        emit_osc_progress(OscProgressState::Indeterminate, None);

        // Update loop at ~15fps (every 67ms)
        let mut interval = tokio::time::interval(Duration::from_millis(67));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            interval.tick().await;

            let state_snapshot = state.lock();

            // Update progress bar
            let total = state_snapshot.total.max(1); // Avoid div by zero
            let completed = state_snapshot.completed;
            let pending = state_snapshot.pending();
            let active = state_snapshot.active.len();

            // Emit OSC 9;4 progress (doubles as keepalive for Ghostty's 15s timeout)
            if state_snapshot.total > 0 {
                let pct = (completed as f64 / total as f64 * 100.0).min(100.0) as u8;
                emit_osc_progress(OscProgressState::Set, Some(pct));
            }

            progress_bar.set_length(total as u64);
            progress_bar.set_position(completed as u64);
            progress_bar.set_message(format!(
                "completed: {}, active: {}, pending: {}",
                completed, active, pending
            ));

            // Get the 3 longest-running actions
            let mut actions: Vec<_> = state_snapshot.active.values().collect();
            actions.sort_by_key(|a| std::cmp::Reverse(a.elapsed()));

            for (i, bar) in action_bars.iter().enumerate() {
                if let Some(action) = actions.get(i) {
                    bar.set_style(spinner_style.clone());
                    let elapsed_secs = action.elapsed().as_secs_f64();

                    let progress_str = match &action.progress {
                        Some(p) if p.current.is_some() && p.total.is_some() => {
                            let current = p.current.unwrap();
                            let total = p.total.unwrap();
                            if total > 0 {
                                let pct = (current as f64 / total as f64 * 100.0).min(100.0);
                                let filled = (pct / 100.0 * 20.0) as usize;
                                let empty = 20 - filled;
                                let current_mb = current as f64 / 1_048_576.0;
                                let total_mb = total as f64 / 1_048_576.0;
                                format!(
                                    " [{}{}] {:.0}% ({:.0}/{:.0} MB)",
                                    "█".repeat(filled),
                                    "░".repeat(empty),
                                    pct,
                                    current_mb,
                                    total_mb,
                                )
                            } else {
                                format!("  {}", p.message)
                            }
                        }
                        Some(p) if !p.message.is_empty() => {
                            format!("  {}", p.message)
                        }
                        _ => String::new(),
                    };

                    bar.set_message(format!(
                        "{:<40}{} ({:.1}s)",
                        action.action_type.display_name(),
                        progress_str,
                        elapsed_secs
                    ));
                    bar.tick(); // Manually tick to advance spinner animation
                } else {
                    // Hide inactive slots completely
                    bar.set_style(empty_style.clone());
                    bar.set_message("");
                }
            }

            // Show the last 10 log messages
            let log_count = state_snapshot.logs.len();
            let start_idx = log_count.saturating_sub(10);

            for (i, bar) in log_bars.iter().enumerate() {
                if let Some(log) = state_snapshot.logs.get(start_idx + i) {
                    bar.set_message(log.clone());
                } else {
                    bar.set_message("");
                }
            }

            // If shutdown was requested, clean up and exit
            if state_snapshot.shutdown {
                // Emit final OSC 9;4 state based on build outcome
                match state_snapshot.build_outcome {
                    Some(true) => emit_osc_progress(OscProgressState::Remove, None),
                    Some(false) => emit_osc_progress(OscProgressState::Error, None),
                    // No outcome yet (e.g. Ctrl-C) — just clear
                    None => emit_osc_progress(OscProgressState::Remove, None),
                }

                // Clear all bars in reverse order (logs, actions, progress)
                for bar in log_bars.iter().rev() {
                    bar.finish_and_clear();
                }
                for bar in action_bars.iter().rev() {
                    bar.finish_and_clear();
                }
                progress_bar.finish_and_clear();

                // Clear the MultiProgress
                drop(multi);

                // Signal that cleanup is complete
                let _ = shutdown_tx.send(());

                break;
            }
        }
    }
}

/// Custom tracing layer that sends logs to the TUI
pub struct TuiLayer {
    tui: TuiHandle,
}

impl TuiLayer {
    pub fn new(tui: TuiHandle) -> Self {
        Self { tui }
    }
}

impl<S> Layer<S> for TuiLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        use std::fmt::Write;
        use tracing::field::Visit;

        // Visitor to extract the log message
        struct MessageVisitor {
            message: String,
        }

        impl Visit for MessageVisitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    let _ = write!(self.message, "{:?}", value);
                } else {
                    let _ = write!(self.message, " {}={:?}", field.name(), value);
                }
            }

            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "message" {
                    self.message = value.to_string();
                } else {
                    let _ = write!(self.message, " {}={}", field.name(), value);
                }
            }
        }

        let mut visitor = MessageVisitor {
            message: String::new(),
        };
        event.record(&mut visitor);

        // Format with level and target
        let metadata = event.metadata();
        let level = metadata.level();
        let target = metadata.target();

        let formatted = format!("[{:<5}] {}: {}", level, target, visitor.message);

        self.tui.log_message(formatted);
    }
}
