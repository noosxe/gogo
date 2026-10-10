//! Engine event channel (`docs/design/01` §4).
//!
//! The engine talks to frontends **only** through typed events — the v2 TUI consumes the
//! same stream the CLI renders. Transport: `tokio::sync::broadcast`; `--json-progress`
//! prints the same events as NDJSON on stdout (docs/design/03` §4).
//!
//! Review D2: `ChunkDone` carries `file_bytes` (cumulative per-file progress) so a
//! consumer that missed earlier events — or hit broadcast `Lagged` — can recompute its
//! bar position from any single event instead of accumulating deltas.

use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum ProgressEvent {
    PlanReady {
        files: u32,
        total_bytes: u64,
        skipped_bytes: u64,
    },
    FileStart {
        id: Uuid,
        name: String,
        total_bytes: u64,
    },
    ChunkDone {
        id: Uuid,
        chunk: u32,
        bytes: u32,
        /// Cumulative bytes on disk for this file (D2 lag self-heal).
        file_bytes: u64,
        md5_ok: bool,
    },
    FileDone {
        id: Uuid,
        path: std::path::PathBuf,
    },
    FileSkipped {
        id: Uuid,
        reason: String,
    },
    FileFailed {
        id: Uuid,
        error: String,
    },
    /// e.g. "bonus content 403 → skipped (bundled)".
    Warn {
        message: String,
    },
    Done {
        ok: u32,
        failed: u32,
        skipped: u32,
    },
}

/// Broadcast channel; lagging receivers drop events by design (progress is lossy).
pub fn channel() -> (
    tokio::sync::broadcast::Sender<ProgressEvent>,
    tokio::sync::broadcast::Receiver<ProgressEvent>,
) {
    tokio::sync::broadcast::channel(64)
}
