//! FUSE request telemetry for debugging hangs.
//!
//! Tracks in-flight requests and keeps a ring buffer of recently completed
//! operations. On SIGUSR1 (or programmatic call), dumps both to stderr.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A FUSE opcode name for display purposes.
fn opcode_name(opcode: u32) -> &'static str {
    match opcode {
        1 => "LOOKUP",
        2 => "FORGET",
        3 => "GETATTR",
        4 => "SETATTR",
        5 => "READLINK",
        6 => "SYMLINK",
        8 => "MKNOD",
        9 => "MKDIR",
        10 => "UNLINK",
        11 => "RMDIR",
        12 => "RENAME",
        13 => "LINK",
        14 => "OPEN",
        15 => "READ",
        16 => "WRITE",
        17 => "STATFS",
        18 => "RELEASE",
        20 => "FSYNC",
        21 => "SETXATTR",
        22 => "GETXATTR",
        23 => "LISTXATTR",
        24 => "REMOVEXATTR",
        25 => "FLUSH",
        26 => "INIT",
        27 => "OPENDIR",
        28 => "READDIR",
        29 => "RELEASEDIR",
        30 => "FSYNCDIR",
        31 => "GETLK",
        32 => "SETLK",
        33 => "SETLKW",
        34 => "ACCESS",
        35 => "CREATE",
        36 => "INTERRUPT",
        37 => "BMAP",
        38 => "DESTROY",
        39 => "IOCTL",
        40 => "POLL",
        41 => "NOTIFY_REPLY",
        42 => "BATCH_FORGET",
        43 => "FALLOCATE",
        44 => "READDIRPLUS",
        45 => "RENAME2",
        46 => "LSEEK",
        47 => "COPY_FILE_RANGE",
        _ => "UNKNOWN",
    }
}

/// A completed operation entry in the ring buffer.
#[derive(Debug, Clone)]
struct CompletedOp {
    opcode: u32,
    unique: u64,
    /// Duration from request arrival to response send.
    duration_us: u64,
    /// Wall-clock offset from telemetry creation.
    wall_offset_us: u64,
}

/// An in-flight request.
#[derive(Debug, Clone)]
struct InFlightOp {
    opcode: u32,
    start: Instant,
}

/// Inner state protected by a mutex.
struct Inner {
    epoch: Instant,
    /// Ring buffer of completed operations.
    completed: Vec<CompletedOp>,
    /// Write cursor into `completed`.
    completed_cursor: usize,
    completed_count: u64,
    /// Currently in-flight requests: unique → op info.
    in_flight: HashMap<u64, InFlightOp>,
    /// Total requests dispatched.
    total_dispatched: u64,
    /// Total requests completed.
    total_completed: u64,
}

/// Shared telemetry state for a FUSE session.
///
/// Clone-friendly (wraps `Arc<Mutex<...>>`). Thread-safe.
#[derive(Clone)]
pub struct TelemetryState {
    inner: Arc<Mutex<Inner>>,
}

const RING_SIZE: usize = 256;

impl TelemetryState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                epoch: Instant::now(),
                completed: Vec::with_capacity(RING_SIZE),
                completed_cursor: 0,
                completed_count: 0,
                in_flight: HashMap::new(),
                total_dispatched: 0,
                total_completed: 0,
            })),
        }
    }

    /// Record that a FUSE request has been read from the kernel.
    pub fn request_start(&self, unique: u64, opcode: u32) {
        let mut inner = self.inner.lock().unwrap();
        inner.total_dispatched += 1;
        inner.in_flight.insert(unique, InFlightOp {
            opcode,
            start: Instant::now(),
        });
    }

    /// Record that a FUSE response has been sent.
    pub fn request_end(&self, unique: u64) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(op) = inner.in_flight.remove(&unique) {
            let now = Instant::now();
            let duration_us = now.duration_since(op.start).as_micros() as u64;
            let wall_offset_us = now.duration_since(inner.epoch).as_micros() as u64;

            let entry = CompletedOp {
                opcode: op.opcode,
                unique,
                duration_us,
                wall_offset_us,
            };

            if inner.completed.len() < RING_SIZE {
                inner.completed.push(entry);
            } else {
                let cursor = inner.completed_cursor;
                inner.completed[cursor] = entry;
            }
            inner.completed_cursor = (inner.completed_cursor + 1) % RING_SIZE;
            inner.completed_count += 1;
            inner.total_completed += 1;
        }
    }

    /// Dump telemetry state to stderr. Safe to call from a signal handler context
    /// (via tokio signal task).
    pub fn dump(&self) {
        let inner = self.inner.lock().unwrap();

        eprintln!("=== FUSE TELEMETRY DUMP ===");
        eprintln!(
            "Total dispatched: {}, completed: {}, in-flight: {}",
            inner.total_dispatched,
            inner.total_completed,
            inner.in_flight.len()
        );

        // In-flight requests
        if !inner.in_flight.is_empty() {
            eprintln!("\n--- In-flight requests ({}) ---", inner.in_flight.len());
            let now = Instant::now();
            let mut entries: Vec<_> = inner.in_flight.iter().collect();
            entries.sort_by_key(|(_, op)| op.start);
            for (unique, op) in &entries {
                let age_ms = now.duration_since(op.start).as_millis();
                eprintln!(
                    "  unique={} op={} age={}ms",
                    unique,
                    opcode_name(op.opcode),
                    age_ms,
                );
            }
        }

        // Recent completed operations
        let count = inner.completed.len();
        if count > 0 {
            eprintln!("\n--- Last {} completed operations ---", count);
            // Print in chronological order
            let start = if inner.completed_count as usize > RING_SIZE {
                inner.completed_cursor
            } else {
                0
            };
            for i in 0..count {
                let idx = (start + i) % count;
                let op = &inner.completed[idx];
                eprintln!(
                    "  +{:.3}s unique={} op={} dur={}us",
                    op.wall_offset_us as f64 / 1_000_000.0,
                    op.unique,
                    opcode_name(op.opcode),
                    op.duration_us,
                );
            }
        }

        eprintln!("=== END TELEMETRY DUMP ===");
    }
}

impl std::fmt::Debug for TelemetryState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelemetryState").finish_non_exhaustive()
    }
}

impl Default for TelemetryState {
    fn default() -> Self {
        Self::new()
    }
}
