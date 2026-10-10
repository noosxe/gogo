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

#[derive(Debug, Clone, serde::Serialize)]
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

/// Drive a consumer over the event stream until the engine's terminal `Done`
/// event — or until `done` resolves, for command paths that end without one
/// (e.g. `--dry-run` returns right after planning). A dropped `done` sender
/// also resolves, so the loop always terminates. `Lagged` batches are skipped
/// (progress is lossy by design); `emit` is the frontend (NDJSON, bars).
pub async fn render(
    mut rx: tokio::sync::broadcast::Receiver<ProgressEvent>,
    done: tokio::sync::oneshot::Receiver<()>,
    mut emit: impl FnMut(&ProgressEvent),
) {
    tokio::select! {
        _ = done => {}
        _ = async {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        let terminal = matches!(ev, ProgressEvent::Done { .. });
                        emit(&ev);
                        if terminal {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        } => {}
    }
}

/// Broadcast channel; lagging receivers drop events by design (progress is lossy).
pub fn channel() -> (
    tokio::sync::broadcast::Sender<ProgressEvent>,
    tokio::sync::broadcast::Receiver<ProgressEvent>,
) {
    tokio::sync::broadcast::channel(64)
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use std::time::Duration;

    fn tag(ev: &ProgressEvent) -> &'static str {
        match ev {
            ProgressEvent::PlanReady { .. } => "PlanReady",
            ProgressEvent::FileStart { .. } => "FileStart",
            ProgressEvent::ChunkDone { .. } => "ChunkDone",
            ProgressEvent::FileDone { .. } => "FileDone",
            ProgressEvent::FileSkipped { .. } => "FileSkipped",
            ProgressEvent::FileFailed { .. } => "FileFailed",
            ProgressEvent::Warn { .. } => "Warn",
            ProgressEvent::Done { .. } => "Done",
        }
    }

    #[tokio::test]
    async fn render_stops_on_done_event() {
        let (tx, rx) = channel();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let render = render(rx, done_rx, move |ev| sink.lock().unwrap().push(tag(ev)));
        let _ = tx.send(ProgressEvent::PlanReady {
            files: 1,
            total_bytes: 1,
            skipped_bytes: 0,
        });
        let _ = tx.send(ProgressEvent::Done {
            ok: 1,
            failed: 0,
            skipped: 0,
        });
        // After Done the loop is gone: this event must never be delivered.
        let _ = tx.send(ProgressEvent::Warn {
            message: "after".into(),
        });
        tokio::time::timeout(Duration::from_secs(2), render)
            .await
            .expect("render terminates on Done");
        drop(done_tx);
        assert_eq!(*seen.lock().unwrap(), vec!["PlanReady", "Done"]);
    }

    #[tokio::test]
    async fn render_stops_when_command_ends_without_done() {
        // dry-run path: no Done event, sender still alive — `done` must end it.
        let (tx, rx) = channel();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let render = render(rx, done_rx, |_ev| {});
        let _ = tx.send(ProgressEvent::Warn {
            message: "x".into(),
        });
        let _ = done_tx.send(());
        tokio::time::timeout(Duration::from_secs(2), render)
            .await
            .expect("render terminates on done signal");
    }
}
