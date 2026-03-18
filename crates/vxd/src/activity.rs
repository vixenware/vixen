use ::moire::sync::SyncMutex;
use std::sync::Arc;
use std::time::Instant;
use vxd_proto::PsEntry;

#[derive(Debug, Clone)]
struct ActivityRecord {
    id: u64,
    kind: String,
    label: String,
    executor: Option<String>,
    started_at: Instant,
    last_update_at: Instant,
    last_message: Option<String>,
    last_remote_update_at: Option<Instant>,
    last_remote_message: Option<String>,
}

#[derive(Debug, Default)]
struct ActivityState {
    next_id: u64,
    active: Vec<ActivityRecord>,
}

#[derive(Clone)]
pub(crate) struct ActivityRegistry {
    state: Arc<SyncMutex<ActivityState>>,
}

impl ActivityRegistry {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(SyncMutex::new(
                "vxd/activity_registry",
                ActivityState {
                    next_id: 1,
                    active: Vec::new(),
                },
            )),
        }
    }

    pub(crate) fn start(
        &self,
        kind: impl Into<String>,
        label: impl Into<String>,
        executor: Option<String>,
    ) -> ActivityHandle {
        let now = Instant::now();
        let mut state = self.state.lock();
        let id = state.next_id;
        state.next_id += 1;
        state.active.push(ActivityRecord {
            id,
            kind: kind.into(),
            label: label.into(),
            executor,
            started_at: now,
            last_update_at: now,
            last_message: None,
            last_remote_update_at: None,
            last_remote_message: None,
        });
        ActivityHandle {
            registry: self.clone(),
            id,
        }
    }

    pub(crate) fn snapshot(&self) -> Vec<PsEntry> {
        let now = Instant::now();
        let mut entries = self
            .state
            .lock()
            .active
            .iter()
            .map(|record| PsEntry {
                id: record.id,
                kind: record.kind.clone(),
                label: record.label.clone(),
                executor: record.executor.clone(),
                age_ms: duration_millis(now.saturating_duration_since(record.started_at)),
                idle_ms: duration_millis(now.saturating_duration_since(record.last_update_at)),
                last_message: record.last_message.clone(),
                remote_idle_ms: record
                    .last_remote_update_at
                    .map(|instant| duration_millis(now.saturating_duration_since(instant))),
                last_remote_message: record.last_remote_message.clone(),
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.id.cmp(&right.id));
        entries
    }

    fn update_message(&self, id: u64, message: String) {
        let now = Instant::now();
        if let Some(record) = self
            .state
            .lock()
            .active
            .iter_mut()
            .find(|record| record.id == id)
        {
            record.last_update_at = now;
            record.last_message = Some(message);
        }
    }

    fn update_remote_message(&self, id: u64, message: String) {
        let now = Instant::now();
        if let Some(record) = self
            .state
            .lock()
            .active
            .iter_mut()
            .find(|record| record.id == id)
        {
            record.last_update_at = now;
            record.last_message = Some(message.clone());
            record.last_remote_update_at = Some(now);
            record.last_remote_message = Some(message);
        }
    }

    fn complete(&self, id: u64) {
        self.state.lock().active.retain(|record| record.id != id);
    }
}

#[derive(Clone)]
pub(crate) struct ActivityHandle {
    registry: ActivityRegistry,
    id: u64,
}

impl ActivityHandle {
    pub(crate) fn update(&self, message: impl Into<String>) {
        self.registry.update_message(self.id, message.into());
    }

    pub(crate) fn update_remote(&self, message: impl Into<String>) {
        self.registry.update_remote_message(self.id, message.into());
    }

    pub(crate) fn complete(&self) {
        self.registry.complete(self.id);
    }
}

impl Drop for ActivityHandle {
    fn drop(&mut self) {
        self.complete();
    }
}

fn duration_millis(duration: std::time::Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::ActivityRegistry;

    #[test]
    fn activity_registry_tracks_latest_message_and_removes_completed_entries() {
        let registry = ActivityRegistry::new();
        let activity = registry.start("exec", "run cc", Some("class:default".to_string()));
        activity.update("prepared exec inputs");

        let entries = registry.snapshot();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, "exec");
        assert_eq!(entries[0].executor.as_deref(), Some("class:default"));
        assert_eq!(
            entries[0].last_message.as_deref(),
            Some("prepared exec inputs")
        );
        assert_eq!(entries[0].last_remote_message, None);

        activity.update_remote("stdout: hello");
        let entries = registry.snapshot();
        assert_eq!(
            entries[0].last_remote_message.as_deref(),
            Some("stdout: hello")
        );
        assert!(entries[0].remote_idle_ms.is_some());

        activity.complete();
        drop(activity);

        assert!(registry.snapshot().is_empty());
    }
}
