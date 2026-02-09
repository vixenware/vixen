//! Diagnostic wrappers for `parking_lot::RwLock` and `parking_lot::Mutex`.
//!
//! When the `diagnostics` feature is **disabled**, these are zero-cost wrappers
//! that compile down to plain parking_lot locks. When **enabled**, every lock
//! registers itself in a global registry and tracks:
//!
//! - Who is waiting to acquire the lock (with backtrace + duration)
//! - Who currently holds the lock (with backtrace + duration)
//!
//! Call [`dump_lock_diagnostics()`] (e.g. from a SIGUSR1 handler) to get a
//! human-readable snapshot of all contention.
//!
//! These are **synchronous** locks — no tokio semaphore, no async wakers.
//! This eliminates a class of cross-runtime deadlocks where tokio's async
//! RwLock semaphore permits can be lost when shared between runtimes.

#[cfg(feature = "diagnostics")]
mod inner {
    use std::backtrace::Backtrace;
    use std::ops::{Deref, DerefMut};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, LazyLock, Mutex as StdMutex, Weak};
    use std::time::Instant;

    // ── Global registry ──────────────────────────────────────────

    static LOCK_REGISTRY: LazyLock<StdMutex<Vec<Weak<LockInfo>>>> =
        LazyLock::new(|| StdMutex::new(Vec::new()));

    /// Snapshot all tracked locks and return a human-readable report.
    pub fn dump_lock_diagnostics() -> String {
        let Ok(registry) = LOCK_REGISTRY.lock() else {
            return "(lock registry poisoned)\n".to_string();
        };
        let mut out = String::new();

        for weak in registry.iter() {
            let Some(info) = weak.upgrade() else {
                continue;
            };

            let (Ok(waiters), Ok(holders)) =
                (info.waiters.lock(), info.holders.lock())
            else {
                out.push_str(&format!(
                    "Lock \"{}\": (diagnostics mutex poisoned)\n",
                    info.name
                ));
                continue;
            };

            // Skip quiescent locks
            if waiters.is_empty() && holders.is_empty() {
                continue;
            }

            let acquires = info.total_acquires.load(Ordering::SeqCst);
            let releases = info.total_releases.load(Ordering::SeqCst);
            let held_count = holders.len() as u64;
            let balanced = acquires == releases + held_count;
            out.push_str(&format!(
                "Lock \"{}\" (acquires={}, releases={}, holding={}{}):\n",
                info.name,
                acquires,
                releases,
                held_count,
                if balanced { "" } else { " *** MISMATCH ***" }
            ));

            if holders.is_empty() {
                out.push_str("  Holders: (none)\n");
            } else {
                out.push_str(&format!("  Holders ({}):\n", holders.len()));
                for h in holders.iter() {
                    let elapsed = h.since.elapsed();
                    let task_info = format_task_info(h.task_id, h.task_name);
                    out.push_str(&format!(
                        "    [{}]{} held for {:.3}s\n",
                        h.kind, task_info,
                        elapsed.as_secs_f64()
                    ));
                    format_backtrace(&h.backtrace, &mut out);
                }
            }

            if waiters.is_empty() {
                out.push_str("  Waiters: (none)\n");
            } else {
                out.push_str(&format!("  Waiters ({}):\n", waiters.len()));
                for w in waiters.iter() {
                    let elapsed = w.since.elapsed();
                    let task_info = format_task_info(w.task_id, w.task_name);
                    out.push_str(&format!(
                        "    [{}]{} waiting for {:.3}s\n",
                        w.kind, task_info,
                        elapsed.as_secs_f64()
                    ));
                    format_backtrace(&w.backtrace, &mut out);
                }
            }
        }

        out
    }

    fn format_task_info(task_id: Option<tokio::task::Id>, task_name: Option<&str>) -> String {
        match (task_id, task_name) {
            (Some(id), Some(name)) => format!(" task={id}({name})"),
            (Some(id), None) => format!(" task={id}"),
            _ => String::new(),
        }
    }

    fn format_backtrace(bt: &Backtrace, out: &mut String) {
        let text = format!("{bt}");
        let mut shown = 0;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.contains("vx_runner")
                || trimmed.contains("vx_vfs")
                || trimmed.contains("fuse3")
                || trimmed.contains("cas_vfs")
                || trimmed.contains("diagnostic_lock")
            {
                out.push_str(&format!("      {trimmed}\n"));
                shown += 1;
                if shown >= 8 {
                    break;
                }
            }
        }
        if shown == 0 {
            for line in text.lines().take(6) {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    out.push_str(&format!("      {trimmed}\n"));
                }
            }
        }
    }

    // ── Shared types ─────────────────────────────────────────────

    #[derive(Debug, Clone, Copy)]
    enum AcquireKind {
        Read,
        Write,
        Mutex,
    }

    impl std::fmt::Display for AcquireKind {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                AcquireKind::Read => write!(f, "read"),
                AcquireKind::Write => write!(f, "write"),
                AcquireKind::Mutex => write!(f, "mutex"),
            }
        }
    }

    struct WaiterOrHolder {
        id: u64,
        kind: AcquireKind,
        since: Instant,
        backtrace: Backtrace,
        task_id: Option<tokio::task::Id>,
        task_name: Option<&'static str>,
    }

    struct LockInfo {
        name: &'static str,
        next_id: AtomicU64,
        waiters: StdMutex<Vec<WaiterOrHolder>>,
        holders: StdMutex<Vec<WaiterOrHolder>>,
        total_acquires: AtomicU64,
        total_releases: AtomicU64,
    }

    impl LockInfo {
        fn new(name: &'static str) -> Arc<Self> {
            let info = Arc::new(Self {
                name,
                next_id: AtomicU64::new(0),
                waiters: StdMutex::new(Vec::new()),
                holders: StdMutex::new(Vec::new()),
                total_acquires: AtomicU64::new(0),
                total_releases: AtomicU64::new(0),
            });
            LOCK_REGISTRY.lock().unwrap().push(Arc::downgrade(&info));
            info
        }

        fn capture_task_info() -> (Option<tokio::task::Id>, Option<&'static str>) {
            (tokio::task::try_id(), None)
        }

        fn add_waiter(&self, kind: AcquireKind) -> u64 {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let (task_id, task_name) = Self::capture_task_info();
            self.waiters.lock().unwrap().push(WaiterOrHolder {
                id,
                kind,
                since: Instant::now(),
                backtrace: Backtrace::force_capture(),
                task_id,
                task_name,
            });
            id
        }

        fn remove_holder(&self, id: u64) {
            self.holders.lock().unwrap().retain(|h| h.id != id);
            self.total_releases.fetch_add(1, Ordering::Relaxed);
        }

        fn promote_waiter_to_holder(&self, waiter_id: u64, kind: AcquireKind) -> u64 {
            let holder_id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let (task_id, task_name) = Self::capture_task_info();

            let mut waiters = self.waiters.lock().unwrap();
            let mut holders = self.holders.lock().unwrap();

            waiters.retain(|w| w.id != waiter_id);
            holders.push(WaiterOrHolder {
                id: holder_id,
                kind,
                since: Instant::now(),
                backtrace: Backtrace::force_capture(),
                task_id,
                task_name,
            });

            self.total_acquires.fetch_add(1, Ordering::Relaxed);
            holder_id
        }
    }

    // ── DiagnosticRwLock ─────────────────────────────────────────

    pub struct DiagnosticRwLock<T> {
        inner: parking_lot::RwLock<T>,
        info: Arc<LockInfo>,
    }

    impl<T> DiagnosticRwLock<T> {
        pub fn new(name: &'static str, value: T) -> Self {
            Self {
                inner: parking_lot::RwLock::new(value),
                info: LockInfo::new(name),
            }
        }

        pub fn read(&self) -> DiagnosticRwLockReadGuard<'_, T> {
            let waiter_id = self.info.add_waiter(AcquireKind::Read);
            let guard = self.inner.read();
            let holder_id = self.info.promote_waiter_to_holder(waiter_id, AcquireKind::Read);
            DiagnosticRwLockReadGuard {
                guard: std::mem::ManuallyDrop::new(guard),
                info: &self.info,
                holder_id,
            }
        }

        pub fn write(&self) -> DiagnosticRwLockWriteGuard<'_, T> {
            let waiter_id = self.info.add_waiter(AcquireKind::Write);
            let guard = self.inner.write();
            let holder_id = self.info.promote_waiter_to_holder(waiter_id, AcquireKind::Write);
            DiagnosticRwLockWriteGuard {
                guard: std::mem::ManuallyDrop::new(guard),
                info: &self.info,
                holder_id,
            }
        }
    }

    pub struct DiagnosticRwLockReadGuard<'a, T> {
        guard: std::mem::ManuallyDrop<parking_lot::RwLockReadGuard<'a, T>>,
        info: &'a Arc<LockInfo>,
        holder_id: u64,
    }

    impl<T> Drop for DiagnosticRwLockReadGuard<'_, T> {
        fn drop(&mut self) {
            // Drop the parking_lot guard first (releases the lock),
            // then update bookkeeping.
            unsafe { std::mem::ManuallyDrop::drop(&mut self.guard) };
            self.info.remove_holder(self.holder_id);
        }
    }

    impl<T> Deref for DiagnosticRwLockReadGuard<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            &self.guard
        }
    }

    pub struct DiagnosticRwLockWriteGuard<'a, T> {
        guard: std::mem::ManuallyDrop<parking_lot::RwLockWriteGuard<'a, T>>,
        info: &'a Arc<LockInfo>,
        holder_id: u64,
    }

    impl<T> Drop for DiagnosticRwLockWriteGuard<'_, T> {
        fn drop(&mut self) {
            unsafe { std::mem::ManuallyDrop::drop(&mut self.guard) };
            self.info.remove_holder(self.holder_id);
        }
    }

    impl<T> Deref for DiagnosticRwLockWriteGuard<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            &self.guard
        }
    }

    impl<T> DerefMut for DiagnosticRwLockWriteGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            &mut self.guard
        }
    }

    // ── DiagnosticMutex ──────────────────────────────────────────

    pub struct DiagnosticMutex<T> {
        inner: parking_lot::Mutex<T>,
        info: Arc<LockInfo>,
    }

    impl<T> DiagnosticMutex<T> {
        pub fn new(name: &'static str, value: T) -> Self {
            Self {
                inner: parking_lot::Mutex::new(value),
                info: LockInfo::new(name),
            }
        }

        pub fn lock(&self) -> DiagnosticMutexGuard<'_, T> {
            let waiter_id = self.info.add_waiter(AcquireKind::Mutex);
            let guard = self.inner.lock();
            let holder_id = self.info.promote_waiter_to_holder(waiter_id, AcquireKind::Mutex);
            DiagnosticMutexGuard {
                guard: std::mem::ManuallyDrop::new(guard),
                info: &self.info,
                holder_id,
            }
        }
    }

    pub struct DiagnosticMutexGuard<'a, T> {
        guard: std::mem::ManuallyDrop<parking_lot::MutexGuard<'a, T>>,
        info: &'a Arc<LockInfo>,
        holder_id: u64,
    }

    impl<T> Drop for DiagnosticMutexGuard<'_, T> {
        fn drop(&mut self) {
            unsafe { std::mem::ManuallyDrop::drop(&mut self.guard) };
            self.info.remove_holder(self.holder_id);
        }
    }

    impl<T> Deref for DiagnosticMutexGuard<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            &self.guard
        }
    }

    impl<T> DerefMut for DiagnosticMutexGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            &mut self.guard
        }
    }
}

#[cfg(not(feature = "diagnostics"))]
mod inner {
    // Zero-cost wrappers — the compiler eliminates these entirely.

    pub struct DiagnosticRwLock<T>(parking_lot::RwLock<T>);

    impl<T> DiagnosticRwLock<T> {
        #[inline]
        pub fn new(_name: &'static str, value: T) -> Self {
            Self(parking_lot::RwLock::new(value))
        }

        #[inline]
        pub fn read(&self) -> parking_lot::RwLockReadGuard<'_, T> {
            self.0.read()
        }

        #[inline]
        pub fn write(&self) -> parking_lot::RwLockWriteGuard<'_, T> {
            self.0.write()
        }
    }

    pub struct DiagnosticMutex<T>(parking_lot::Mutex<T>);

    impl<T> DiagnosticMutex<T> {
        #[inline]
        pub fn new(_name: &'static str, value: T) -> Self {
            Self(parking_lot::Mutex::new(value))
        }

        #[inline]
        pub fn lock(&self) -> parking_lot::MutexGuard<'_, T> {
            self.0.lock()
        }
    }

    #[inline]
    pub fn dump_lock_diagnostics() -> String {
        String::new()
    }
}

pub use inner::*;
